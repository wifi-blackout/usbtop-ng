//! Physical connectors, from the sysfs port objects.
//!
//! Every hub, root hubs included, exposes one `usb_port` device per
//! downstream port under its interface directory, named after the hub:
//! `usb3/3-0:1.0/usb3-port1`, `3-1/3-1:1.0/3-1-port4`. When a port has a
//! companion on the other bus of the same xHCI controller (the USB2 and the
//! SuperSpeed halves of one physical receptacle), each carries a `peer`
//! symlink to the other. Verified against `drivers/usb/core/port.c`:
//! `usb_hub_create_port_device` names the port `"%s-port%d"` from the hub's
//! device name, and `link_peers` creates the two `peer` links, one each way.
//!
//! This module reads exactly that and nothing more: the port names under
//! each device's interface directories and the last component of each
//! `peer` link. Pairing follows the link and never infers a companion from
//! port numbers or bus adjacency: one fleet host pairs `usb4-port2` with
//! `usb3-port1`, another pairs bus 3 with bus 6.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One hub port as sysfs describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    /// The hub that owns the port: `usb3` or `3-1.4`.
    pub hub: String,
    /// The 1-based port number on that hub.
    pub number: u32,
    /// The companion port's name, from the `peer` link, when there is one.
    pub peer: Option<String>,
}

/// A port as a physical position: its name, the bus it is on, and its
/// chain (the hub's chain plus the port number: `usb3-port1` is `[1]`,
/// `3-1.4-port2` is `[1, 4, 2]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRef {
    pub name: String,
    pub bus: u8,
    pub chain: Vec<u32>,
}

/// Every port found under a set of device directories, by port name.
#[derive(Debug, Default)]
pub struct PortIndex {
    ports: BTreeMap<String, PortInfo>,
    /// The device names that own at least one port: the hubs whose driver
    /// has created its port objects.
    hubs: BTreeSet<String>,
}

impl PortIndex {
    /// Scan the interface directories of every device directory in
    /// `devices` (each an entry of `/sys/bus/usb/devices` or a fixture's
    /// `sysfs/`; root-hub symlinks are followed). Nothing deeper than
    /// `<device>/<interface>/<device>-port<N>` is visited, and nothing but
    /// the `peer` link is read. An unreadable directory is skipped.
    pub fn scan_devices<'a>(devices: impl IntoIterator<Item = &'a Path>) -> PortIndex {
        let mut index = PortIndex::default();
        for device_dir in devices {
            let Some(device) = device_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            index.scan_device(device_dir, device);
        }
        index
    }

    /// Scan every device entry of a sysfs base directory: each entry whose
    /// name has no `:` (interface entries are skipped).
    #[cfg(test)]
    pub fn scan(base: &Path) -> PortIndex {
        let mut dirs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                if !entry.file_name().to_string_lossy().contains(':') {
                    dirs.push(entry.path());
                }
            }
        }
        PortIndex::scan_devices(dirs.iter().map(std::path::PathBuf::as_path))
    }

    fn scan_device(&mut self, device_dir: &Path, device: &str) {
        let children = match std::fs::read_dir(device_dir) {
            Ok(children) => children,
            Err(e) => {
                log::debug!("{device}: cannot read {}: {e}", device_dir.display());
                return;
            }
        };
        let prefix = format!("{device}-port");
        for interface in children.flatten() {
            if !interface.file_name().to_string_lossy().contains(':') {
                continue;
            }
            let entries = match std::fs::read_dir(interface.path()) {
                Ok(entries) => entries,
                Err(e) => {
                    log::debug!("{device}: cannot read {}: {e}", interface.path().display());
                    continue;
                }
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(number) = name
                    .strip_prefix(&prefix)
                    .and_then(|n| n.parse::<u32>().ok())
                else {
                    continue;
                };
                let peer = std::fs::read_link(entry.path().join("peer"))
                    .ok()
                    .and_then(|target| {
                        target.file_name().map(|f| f.to_string_lossy().into_owned())
                    });
                self.hubs.insert(device.to_string());
                self.ports.insert(
                    name,
                    PortInfo {
                        hub: device.to_string(),
                        number,
                        peer,
                    },
                );
            }
        }
    }

    /// Whether `device` owns any port object: it is a hub whose driver has
    /// created its ports.
    pub fn is_hub(&self, device: &str) -> bool {
        self.hubs.contains(device)
    }

    /// The port `device` is attached to and, when the `peer` link is present
    /// and reciprocal, its companion. `None` when the device is a root hub,
    /// when its name does not parse, or when its port object is not in the
    /// index (a tree captured without port objects, or a hub whose driver
    /// has not created them yet).
    pub fn connector_of(&self, device: &str) -> Option<(PortRef, Option<PortRef>)> {
        let (hub, number) = port_of_device(device)?;
        let name = port_name(&hub, number);
        let own = self.port_ref(&name)?;
        let peer =
            self.ports
                .get(&name)?
                .peer
                .as_deref()
                .and_then(|peer| match self.ports.get(peer) {
                    Some(info) if info.peer.as_deref() == Some(name.as_str()) => {
                        self.port_ref(peer)
                    }
                    Some(_) => {
                        log::debug!("{name}: peer {peer} does not peer back; single port");
                        None
                    }
                    None => {
                        log::debug!("{name}: peer {peer} is not a known port; single port");
                        None
                    }
                });
        Some((own, peer))
    }

    fn port_ref(&self, name: &str) -> Option<PortRef> {
        let info = self.ports.get(name)?;
        let bus = bus_of(&info.hub)?;
        let mut chain = chain_of(&info.hub)?;
        chain.push(info.number);
        Some(PortRef {
            name: name.to_string(),
            bus,
            chain,
        })
    }

    #[cfg(test)]
    pub fn get(&self, name: &str) -> Option<&PortInfo> {
        self.ports.get(name)
    }

    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PortInfo)> {
        self.ports.iter().map(|(name, info)| (name.as_str(), info))
    }
}

/// The one parser of a sysfs USB device name. A root hub is `usb<N>`: bus
/// `N`, empty chain. Every other device is `<bus>-<p1>.<p2>…`: its port
/// chain from the root port down. Anything else, an interface name with a
/// `:`, a controller directory, a port object, garbage, is `None`. Every
/// reading of a device name in the crate (`port_of_device`, `bus_of`,
/// `chain_of`, `UsbDevice::port_chain`) goes through here, so the Port
/// column and the connector label cannot disagree about what a name means.
pub fn parse_device_name(name: &str) -> Option<(u8, Vec<u32>)> {
    if let Some(rest) = name.strip_prefix("usb") {
        return rest.parse::<u8>().ok().map(|bus| (bus, Vec::new()));
    }
    let (bus, ports) = name.split_once('-')?;
    let bus = bus.parse::<u8>().ok()?;
    let chain = ports
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    Some((bus, chain))
}

/// The hub and port number a device is attached to, from its sysfs name:
/// `3-1.4` is port 4 of hub `3-1`; `3-1` is port 1 of root hub `usb3`. A
/// root hub (`usb3`) is on no port, and anything that is not a device name
/// is `None`.
pub fn port_of_device(device: &str) -> Option<(String, u32)> {
    let (bus, chain) = parse_device_name(device)?;
    let (&last, parents) = chain.split_last()?;
    let hub = if parents.is_empty() {
        format!("usb{bus}")
    } else {
        format!(
            "{bus}-{}",
            parents
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )
    };
    Some((hub, last))
}

/// `<hub>-port<N>`, the kernel's port device name.
pub fn port_name(hub: &str, number: u32) -> String {
    format!("{hub}-port{number}")
}

/// The bus a device or hub name belongs to: `usb3` and `3-1.4` are both bus 3.
fn bus_of(name: &str) -> Option<u8> {
    parse_device_name(name).map(|(bus, _)| bus)
}

/// The port chain of a device or hub name: `usb3` is `[]`, `3-1.4` is `[1, 4]`.
fn chain_of(name: &str) -> Option<Vec<u32>> {
    parse_device_name(name).map(|(_, chain)| chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A fake `/sys/bus/usb/devices`: root hubs as symlinks into a
    /// controller directory (as real sysfs has), devices as plain
    /// directories, hub ports under `<hub>/<hub>:1.0/<hub>-port<N>/`, and
    /// `peer` links whose target's last component names the companion.
    struct Tree {
        root: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Tree {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("devices")).unwrap();
            Tree { root }
        }

        fn base(&self) -> PathBuf {
            self.root.path().join("devices")
        }

        fn root_hub(&self, controller: &str, hub: &str) -> PathBuf {
            let real = self.root.path().join(controller).join(hub);
            std::fs::create_dir_all(&real).unwrap();
            std::os::unix::fs::symlink(&real, self.base().join(hub)).unwrap();
            real
        }

        fn device(&self, name: &str) -> PathBuf {
            let dir = self.base().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn port(&self, hub_dir: &Path, hub: &str, number: u32) -> PathBuf {
            let dir = hub_dir
                .join(format!("{hub}:1.0"))
                .join(port_name(hub, number));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn peer(&self, from: &Path, to: &Path) {
            std::os::unix::fs::symlink(to, from.join("peer")).unwrap();
        }

        fn pair(&self, a: &Path, b: &Path) {
            self.peer(a, b);
            self.peer(b, a);
        }
    }

    /// Two root hubs on one controller (bus 3 at USB2, bus 4 at USB3), root
    /// ports 1 paired and port 2 on bus 3 alone, and a USB3 hub on root
    /// port 1 whose halves `3-1`/`4-1` own paired ports 1 and 2.
    fn paired_tree() -> Tree {
        let t = Tree::new();
        let usb3 = t.root_hub("0000:00:14.0", "usb3");
        let usb4 = t.root_hub("0000:00:14.0", "usb4");
        let u3p1 = t.port(&usb3, "usb3", 1);
        let u4p1 = t.port(&usb4, "usb4", 1);
        t.pair(&u3p1, &u4p1);
        t.port(&usb3, "usb3", 2);
        let hub3 = t.device("3-1");
        let hub4 = t.device("4-1");
        for n in 1..=2 {
            let a = t.port(&hub3, "3-1", n);
            let b = t.port(&hub4, "4-1", n);
            t.pair(&a, &b);
        }
        t.device("3-2");
        t.device("3-1.2");
        t
    }

    #[test]
    fn parse_device_name_reads_root_hubs_and_port_chains_and_rejects_the_rest() {
        assert_eq!(parse_device_name("usb3"), Some((3, vec![])));
        assert_eq!(parse_device_name("3-1"), Some((3, vec![1])));
        assert_eq!(parse_device_name("3-1.4.2"), Some((3, vec![1, 4, 2])));
        assert_eq!(parse_device_name("12-10.3"), Some((12, vec![10, 3])));
        for bad in [
            "",
            "garbage",
            "usb",
            "usbx",
            "usb300",
            "usb3-port1",
            "3",
            "3-",
            "-1",
            "x-1",
            "3-x",
            "3-1.",
            "3-1..2",
            "3-1:1.0",
        ] {
            assert_eq!(parse_device_name(bad), None, "{bad:?} is not a device name");
        }
    }

    #[test]
    fn port_of_device_names_the_hub_and_port() {
        assert_eq!(port_of_device("3-1"), Some(("usb3".to_string(), 1)));
        assert_eq!(port_of_device("3-1.4"), Some(("3-1".to_string(), 4)));
        assert_eq!(port_of_device("3-1.4.2"), Some(("3-1.4".to_string(), 2)));
        assert_eq!(port_of_device("usb3"), None, "a root hub is on no port");
        assert_eq!(port_of_device("3-x"), None);
        assert_eq!(
            port_of_device("x-1"),
            None,
            "a non-numeric bus is not a device name"
        );
        assert_eq!(port_of_device("garbage"), None);
        assert_eq!(port_name("3-1.4", 2), "3-1.4-port2");
    }

    #[test]
    fn scan_finds_ports_under_interface_dirs_and_reads_peers() {
        let t = paired_tree();
        let index = PortIndex::scan(&t.base());

        assert_eq!(
            index.get("usb3-port1"),
            Some(&PortInfo {
                hub: "usb3".to_string(),
                number: 1,
                peer: Some("usb4-port1".to_string()),
            })
        );
        assert_eq!(
            index.get("usb3-port2"),
            Some(&PortInfo {
                hub: "usb3".to_string(),
                number: 2,
                peer: None,
            })
        );
        assert_eq!(
            index.get("4-1-port2").and_then(|p| p.peer.as_deref()),
            Some("3-1-port2")
        );
        assert_eq!(index.iter().count(), 7, "3 root ports + 4 hub ports");
        assert!(index.is_hub("3-1"));
        assert!(index.is_hub("usb3"));
        assert!(!index.is_hub("3-2"), "a leaf owns no ports");
    }

    #[test]
    fn connector_of_pairs_through_reciprocal_peer_links() {
        let t = paired_tree();
        let index = PortIndex::scan(&t.base());

        let (own, peer) = index.connector_of("3-1").unwrap();
        assert_eq!(
            own,
            PortRef {
                name: "usb3-port1".to_string(),
                bus: 3,
                chain: vec![1],
            }
        );
        assert_eq!(
            peer,
            Some(PortRef {
                name: "usb4-port1".to_string(),
                bus: 4,
                chain: vec![1],
            })
        );

        // Entering from the other half lands on the same pair, reversed.
        let (own, peer) = index.connector_of("4-1").unwrap();
        assert_eq!(own.name, "usb4-port1");
        assert_eq!(peer.map(|p| p.name).as_deref(), Some("usb3-port1"));

        // Nested: a device on hub port 2 pairs through the hub's own ports.
        let (own, peer) = index.connector_of("3-1.2").unwrap();
        assert_eq!(own.name, "3-1-port2");
        assert_eq!(own.chain, vec![1, 2]);
        let peer = peer.unwrap();
        assert_eq!(peer.name, "4-1-port2");
        assert_eq!(peer.bus, 4);
        assert_eq!(peer.chain, vec![1, 2]);
    }

    #[test]
    fn a_port_without_a_peer_is_a_single_connector() {
        let t = paired_tree();
        let index = PortIndex::scan(&t.base());
        let (own, peer) = index.connector_of("3-2").unwrap();
        assert_eq!(own.name, "usb3-port2");
        assert_eq!(own.chain, vec![2]);
        assert_eq!(peer, None);
    }

    #[test]
    fn a_dangling_or_non_reciprocal_peer_degrades_to_a_single_port() {
        let t = Tree::new();
        let usb3 = t.root_hub("0000:00:14.0", "usb3");
        let usb4 = t.root_hub("0000:00:14.0", "usb4");
        // Dangling: names a port that does not exist anywhere.
        let u3p3 = t.port(&usb3, "usb3", 3);
        t.peer(&u3p3, &usb4.join("4-0:1.0").join("usb4-port9"));
        // Non-reciprocal: usb3-port4 says usb4-port4, which says usb3-port1.
        let u3p4 = t.port(&usb3, "usb3", 4);
        let u4p4 = t.port(&usb4, "usb4", 4);
        let u3p1 = t.port(&usb3, "usb3", 1);
        t.peer(&u3p4, &u4p4);
        t.peer(&u4p4, &u3p1);
        t.device("3-3");
        t.device("3-4");
        t.device("4-4");

        let index = PortIndex::scan(&t.base());
        assert_eq!(index.connector_of("3-3").unwrap().1, None, "dangling");
        assert_eq!(
            index.connector_of("3-4").unwrap().1,
            None,
            "non-reciprocal from the claiming side"
        );
        assert_eq!(
            index.connector_of("4-4").unwrap().1,
            None,
            "non-reciprocal from the other side"
        );
    }

    #[test]
    fn different_port_numbers_and_non_adjacent_buses_follow_the_link() {
        let t = Tree::new();
        let usb3 = t.root_hub("0000:00:14.0", "usb3");
        let usb4 = t.root_hub("0000:00:14.0", "usb4");
        let usb6 = t.root_hub("0000:00:14.0", "usb6");
        // Differing numbers, as on a fleet laptop: usb4-port2 <-> usb3-port1.
        let u3p1 = t.port(&usb3, "usb3", 1);
        let u4p2 = t.port(&usb4, "usb4", 2);
        t.pair(&u3p1, &u4p2);
        // Non-adjacent buses, as on a fleet SBC: usb3-port2 <-> usb6-port1.
        let u3p2 = t.port(&usb3, "usb3", 2);
        let u6p1 = t.port(&usb6, "usb6", 1);
        t.pair(&u3p2, &u6p1);
        t.device("4-2");
        t.device("6-1");

        let index = PortIndex::scan(&t.base());
        let (own, peer) = index.connector_of("4-2").unwrap();
        assert_eq!(
            (own.name.as_str(), own.chain.as_slice()),
            ("usb4-port2", &[2][..])
        );
        let peer = peer.unwrap();
        assert_eq!(
            (peer.name.as_str(), peer.bus, peer.chain.as_slice()),
            ("usb3-port1", 3, &[1][..])
        );

        let (own, peer) = index.connector_of("6-1").unwrap();
        assert_eq!(own.bus, 6);
        assert_eq!(
            peer.map(|p| (p.name, p.bus)),
            Some(("usb3-port2".to_string(), 3))
        );
    }

    #[test]
    fn root_hubs_and_unknown_devices_have_no_connector() {
        let t = paired_tree();
        let index = PortIndex::scan(&t.base());
        assert_eq!(
            index.connector_of("usb3"),
            None,
            "a root hub sits on no port"
        );
        assert_eq!(index.connector_of("3-9"), None, "no port object for port 9");
        assert_eq!(index.connector_of("garbage"), None);
        assert_eq!(
            PortIndex::default().connector_of("3-1"),
            None,
            "an empty index (a tree without port objects) places nothing"
        );
    }

    #[test]
    fn objects_outside_interface_dirs_are_not_scanned() {
        let t = Tree::new();
        let hub = t.device("3-1");
        // A port-shaped dir directly under the device, not under an interface.
        std::fs::create_dir_all(hub.join("3-1-port7")).unwrap();
        // A wrongly named entry under the interface.
        std::fs::create_dir_all(hub.join("3-1:1.0").join("other-port1")).unwrap();
        // A Thunderbolt-style object with its own `peer`, not a port.
        let node = t.base().join("0000:06:00.4").join("physical_node");
        std::fs::create_dir_all(&node).unwrap();
        std::os::unix::fs::symlink(&hub, node.join("peer")).unwrap();
        // An interface-looking top-level entry that must be skipped by `scan`.
        std::fs::create_dir_all(t.base().join("3-1:1.0").join("3-1-port8")).unwrap();

        let index = PortIndex::scan(&t.base());
        assert_eq!(
            index.iter().count(),
            0,
            "{:?}",
            index.iter().collect::<Vec<_>>()
        );
        assert!(!index.is_hub("3-1"));
    }

    #[test]
    fn scan_devices_skips_unreadable_and_nameless_paths() {
        let t = paired_tree();
        let missing = t.base().join("9-9");
        let dirs = [t.base().join("3-1"), missing, PathBuf::from("/")];
        let index = PortIndex::scan_devices(dirs.iter().map(PathBuf::as_path));
        assert_eq!(index.iter().count(), 2, "only 3-1's two ports");
        assert!(index.is_hub("3-1"));
    }
}

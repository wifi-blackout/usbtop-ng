//! Call-outs: devices and hubs linked below the speed they support, and
//! why. Pure over the device manager's rows and the connector index; no
//! sysfs reads. Rules and the verdict doctrine they follow are in
//! `docs/superpowers/specs/2026-09-12-capability-callouts-design.md`.
//!
//! The kernel pairs a hub's ports by port number with the ports of the hub
//! on its upstream port's peer (`drivers/usb/core/port.c`
//! `find_and_link_peer`); a hub whose two halves sit on different port
//! numbers gets its own ports left unpaired and its halves paired with
//! empty ports. `Topology::ss_half` therefore matches such halves by
//! elimination and says nothing when the match is ambiguous.

use std::collections::HashMap;

use crate::connector::{device_name_of_port, port_name, port_of_device, PortIndex};
use crate::device::manager::DeviceManager;
use crate::device::{Capability, CapabilitySource};
use crate::usbmon::parser::UsbSpeed;

/// Why a device is linked below its capability, when the topology proves it.
#[derive(Debug, Clone, PartialEq)]
pub enum Cause {
    /// The connector's SuperSpeed port is empty: the link came up at USB 2
    /// speed (a cable or port problem, or a device that never trained).
    SuperSpeedSideEmpty { peer_port: String },
    /// The root port has no SuperSpeed twin.
    Usb2OnlyHostPort,
    /// The hub above the device is itself linked below the capability.
    UpstreamHubLink { hub: String, hub_link: UsbSpeed },
    /// The root hub's own rate is below the capability.
    HostPortMax { max: UsbSpeed },
    /// The upstream allows the capability; the link still came up slower.
    UpstreamPermits,
}

impl Cause {
    /// The JSON value.
    pub fn kind(&self) -> &'static str {
        match self {
            Cause::SuperSpeedSideEmpty { .. } => "superspeed_side_empty",
            Cause::Usb2OnlyHostPort => "usb2_only_host_port",
            Cause::UpstreamHubLink { .. } => "upstream_hub_link",
            Cause::HostPortMax { .. } => "host_port_max",
            Cause::UpstreamPermits => "upstream_permits",
        }
    }

    fn reason(&self, capability: &UsbSpeed) -> String {
        match self {
            Cause::SuperSpeedSideEmpty { peer_port } => format!(
                "the SuperSpeed side of this connector ({peer_port}) is empty, so the link came up at USB 2 speed; check the cable or the port"
            ),
            Cause::Usb2OnlyHostPort => {
                "this host port is USB 2 only; move it to a USB 3 port".to_string()
            }
            Cause::UpstreamHubLink { hub, hub_link } => {
                let mut text = format!(
                    "the hub above it ({hub}) is linked at {}",
                    short_speed(hub_link)
                );
                if hub_link.to_mbps() <= HIGH_SPEED_MBPS {
                    text.push_str("; move it to a USB 3 port");
                }
                text
            }
            Cause::HostPortMax { max } => {
                format!("this host port tops out at {}", short_speed(max))
            }
            Cause::UpstreamPermits => format!(
                "the port above it allows {}; check the cable or the device",
                short_speed(capability)
            ),
        }
    }
}

/// One device linked below the speed it supports.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub bus: u8,
    pub address: u8,
    /// The sysfs name, `5-1.2`.
    pub path: String,
    /// The kernel port name the device is on, `5-1-port2`; `None` when the
    /// index does not know it.
    pub port: Option<String>,
    pub link: UsbSpeed,
    pub capability: Capability,
    pub cause: Option<Cause>,
}

impl Finding {
    /// The sentence every surface shows: `linked at 480M, supports 10G:
    /// <reason>`, with `(from bcdUSB)` after the capability when it is the
    /// fallback floor.
    pub fn message(&self) -> String {
        let source = match self.capability.source {
            CapabilitySource::Bos => "",
            CapabilitySource::BcdUsb => " (from bcdUSB)",
        };
        let reason = match &self.cause {
            Some(cause) => cause.reason(&self.capability.speed),
            None => "its port is unknown to the connector index".to_string(),
        };
        format!(
            "linked at {}, supports {}{source}: {reason}",
            short_speed(&self.link),
            short_speed(&self.capability.speed)
        )
    }
}

/// `480M`, `5G`, `10G`; `?` for an unknown rate.
pub fn short_speed(speed: &UsbSpeed) -> String {
    let mbps = speed.to_mbps();
    if mbps <= 0.0 {
        "?".to_string()
    } else if mbps >= 1000.0 {
        format!("{}G", mbps / 1000.0)
    } else {
        format!("{mbps}M")
    }
}

const HIGH_SPEED_MBPS: f64 = 480.0;

struct Row<'a> {
    name: &'a str,
    bus: u8,
    address: u8,
    link: &'a UsbSpeed,
    capability: Option<&'a Capability>,
}

impl Row<'_> {
    fn capable_above_high(&self) -> bool {
        self.capability
            .is_some_and(|c| c.speed.to_mbps() > HIGH_SPEED_MBPS)
    }
}

/// Rule H's verdict about a hub's SuperSpeed half. The distinction that
/// matters is `Missing` against `Ambiguous`: a hub whose half provably never
/// enumerated is a call-out (its SuperSpeed side really is empty), while a
/// hub whose half the topology cannot pin down says nothing at all.
#[derive(Debug, Clone)]
enum Half {
    /// That hub is this hub's SuperSpeed half.
    Known(String),
    /// This hub has no SuperSpeed half: it is a SuperSpeed hub itself, it
    /// claims no SuperSpeed capability, there is no SuperSpeed side above
    /// it, or no unclaimed SuperSpeed hub exists there for it to be.
    Missing,
    /// Two or more candidates fit, or the parent's own half is itself
    /// ambiguous: which hub is this one's half is unknowable.
    Ambiguous,
}

struct Topology<'a> {
    rows: &'a [Row<'a>],
    by_name: HashMap<&'a str, &'a Row<'a>>,
    ports: &'a PortIndex,
    ss_half: HashMap<String, Half>,
}

impl<'a> Topology<'a> {
    fn present(&self, name: &str) -> Option<&'a Row<'a>> {
        self.by_name.get(name).copied()
    }

    /// The reciprocal peer of `device`'s own port.
    fn peer_port_of(&self, device: &str) -> Option<String> {
        self.ports.connector_of(device)?.1.map(|peer| peer.name)
    }

    /// The present device on the port named `port`.
    fn device_on_port(&self, port: &str) -> Option<&'a Row<'a>> {
        let info = self.ports.get(port)?;
        let name = device_name_of_port(&info.hub, info.number)?;
        self.present(&name)
    }

    /// Whether `hub`'s own port has a reciprocal peer holding a present device.
    fn peer_holds_a_device(&self, hub: &str) -> bool {
        self.peer_port_of(hub)
            .is_some_and(|peer| self.device_on_port(&peer).is_some())
    }

    /// The present hubs attached to `parent`'s ports, in row order.
    /// Collected rather than returned lazily so the caller may hold the
    /// list across the recursive `ss_half` call.
    fn hubs_on(&self, parent: &str) -> Vec<&'a Row<'a>> {
        self.rows
            .iter()
            .filter(|row| {
                self.ports.is_hub(row.name)
                    && port_of_device(row.name).is_some_and(|(hub, _)| hub == parent)
            })
            .collect()
    }

    /// The SuperSpeed half of `hub` (rule H in the spec), memoized.
    fn ss_half(&mut self, hub: &str) -> Half {
        if let Some(known) = self.ss_half.get(hub) {
            return known.clone();
        }
        // Belt and braces against re-entering the same hub. It cannot
        // happen in any tree: the only recursion is on `port_of_device`'s
        // parent, which drops the last element of the name's port chain, so
        // the chain strictly shortens until the root-hub arm below, which
        // does not recurse at all. A name cannot be its own ancestor.
        self.ss_half.insert(hub.to_string(), Half::Ambiguous);
        let half = self.find_ss_half(hub);
        self.ss_half.insert(hub.to_string(), half.clone());
        half
    }

    fn find_ss_half(&mut self, hub: &str) -> Half {
        let Some(row) = self.present(hub) else {
            return Half::Missing;
        };
        if row.link.to_mbps() > HIGH_SPEED_MBPS {
            // A SuperSpeed hub is the SuperSpeed half, and has none of its own.
            return Half::Missing;
        }
        let Some((parent, _)) = port_of_device(hub) else {
            // A root hub: the hub owning the reciprocal peer of any of its ports.
            let peer_hubs: Vec<String> = self
                .ports
                .ports_of(hub)
                .filter_map(|(name, info)| {
                    let peer = info.peer.as_deref()?;
                    let peer_info = self.ports.get(peer)?;
                    (peer_info.peer.as_deref() == Some(name)).then(|| peer_info.hub.clone())
                })
                .collect();
            return peer_hubs
                .into_iter()
                .find(|h| self.present(h).is_some())
                .map_or(Half::Missing, Half::Known);
        };
        // Step 2: the kernel's own pairing.
        if let Some(peer) = self.peer_port_of(hub) {
            if let Some(dev) = self.device_on_port(&peer) {
                if self.ports.is_hub(dev.name) {
                    return Half::Known(dev.name.to_string());
                }
            }
        }
        // Step 3: match by elimination under the parent pair.
        if !row.capable_above_high() {
            // The hub states no SuperSpeed capability, so it has no half.
            return Half::Missing;
        }
        let parent_ss = match self.ss_half(&parent) {
            Half::Known(parent_ss) => parent_ss,
            // No SuperSpeed side above at all, so no half of this hub can be
            // enumerated anywhere: that is knowledge, not ignorance.
            Half::Missing => return Half::Missing,
            Half::Ambiguous => return Half::Ambiguous,
        };
        let unclaimed_usb2: Vec<&str> = self
            .hubs_on(&parent)
            .into_iter()
            .filter(|h| h.link.to_mbps() <= HIGH_SPEED_MBPS && h.capable_above_high())
            .filter(|h| !self.peer_holds_a_device(h.name))
            .map(|h| h.name)
            .collect();
        let unclaimed_ss: Vec<&str> = self
            .hubs_on(&parent_ss)
            .into_iter()
            .filter(|h| h.link.to_mbps() > HIGH_SPEED_MBPS)
            .filter(|h| !self.peer_holds_a_device(h.name))
            .map(|h| h.name)
            .collect();
        match (unclaimed_usb2.as_slice(), unclaimed_ss.as_slice()) {
            ([one], [half]) if *one == hub => Half::Known((*half).to_string()),
            // Nothing unclaimed on the SuperSpeed side for this hub's half
            // to be: it never enumerated.
            (_, []) => Half::Missing,
            _ => Half::Ambiguous,
        }
    }
}

/// Every present, non-root device linked below a known capability, with
/// the cause the topology proves, ordered by (bus, address).
pub fn analyze(manager: &DeviceManager, ports: &PortIndex) -> Vec<Finding> {
    let mut rows: Vec<Row> = manager
        .buses
        .values()
        .flat_map(|bus| bus.devices.values())
        .filter(|device| !device.is_disconnected)
        .filter_map(|device| {
            let name = device.sysfs_path.as_deref()?.file_name()?.to_str()?;
            Some(Row {
                name,
                bus: device.bus_id,
                address: device.device_id,
                link: &device.speed,
                capability: device.capability.as_ref(),
            })
        })
        .collect();
    rows.sort_by_key(|row| (row.bus, row.address));
    let by_name = rows.iter().map(|row| (row.name, row)).collect();
    let mut topology = Topology {
        rows: &rows,
        by_name,
        ports,
        ss_half: HashMap::new(),
    };
    let bus_speed = |bus: u8| manager.buses.get(&bus).map(|b| b.speed.clone());
    rows.iter()
        .filter_map(|row| finding_for(row, &mut topology, &bus_speed))
        .collect()
}

fn finding_for(
    row: &Row,
    topology: &mut Topology,
    bus_speed: &impl Fn(u8) -> Option<UsbSpeed>,
) -> Option<Finding> {
    let capability = row.capability?;
    let link = row.link.to_mbps();
    if link <= 0.0 || capability.speed.to_mbps() <= link {
        return None;
    }
    // A root hub is on no port and is never a finding.
    let (parent, number) = port_of_device(row.name)?;
    let port = port_name(&parent, number);
    let finding = |port: Option<String>, cause: Option<Cause>| Finding {
        bus: row.bus,
        address: row.address,
        path: row.name.to_string(),
        port,
        link: row.link.clone(),
        capability: capability.clone(),
        cause,
    };
    if topology.ports.get(&port).is_none() {
        return Some(finding(None, None));
    }
    let root_parent = port_of_device(&parent).is_none();
    if link <= HIGH_SPEED_MBPS {
        if topology.ports.is_hub(row.name) {
            match topology.ss_half(row.name) {
                // Its SuperSpeed half is up: a USB 2 half sitting at 480 is
                // how a USB 3 hub enumerates, not a fault.
                Half::Known(_) => return None,
                // Rule H cannot pin the half down, so an empty SuperSpeed
                // port opposite proves nothing -- that half may be
                // enumerated on another port number. Say nothing about the
                // hub; its children still get their own true statement.
                Half::Ambiguous => return None,
                // Rule H proved the half never enumerated: judge the hub
                // like any other device below.
                Half::Missing => {}
            }
        }
        // Only a half rule H actually found places a SuperSpeed port; both
        // `Missing` and `Ambiguous` take the "P3 unknown" branch, whose
        // verdict is true either way.
        let ss_port = match topology.ss_half(&parent) {
            Half::Known(half) => Some(port_name(&half, number)),
            Half::Missing | Half::Ambiguous => None,
        }
        .filter(|name| topology.ports.get(name).is_some());
        let cause = match ss_port {
            Some(ss_port) if topology.device_on_port(&ss_port).is_some() => return None,
            Some(ss_port) => Cause::SuperSpeedSideEmpty { peer_port: ss_port },
            None if root_parent => Cause::Usb2OnlyHostPort,
            None => Cause::UpstreamHubLink {
                hub_link: topology
                    .present(&parent)
                    .map_or(UsbSpeed::UNKNOWN, |p| p.link.clone()),
                hub: parent,
            },
        };
        return Some(finding(Some(port), Some(cause)));
    }
    let limit = if root_parent {
        bus_speed(row.bus)
    } else {
        topology.present(&parent).map(|p| p.link.clone())
    };
    let cause = match limit {
        Some(max) if max.to_mbps() > 0.0 && max.to_mbps() < capability.speed.to_mbps() => {
            if root_parent {
                Cause::HostPortMax { max }
            } else {
                Cause::UpstreamHubLink {
                    hub: parent,
                    hub_link: max,
                }
            }
        }
        _ => Cause::UpstreamPermits,
    };
    Some(finding(Some(port), Some(cause)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// SuperSpeed only: 5 Gb/s (the camera's shape).
    const SS: &[u8] = &[
        0x05, 0x0f, 0x16, 0x00, 0x02, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10, 0x03,
        0x00, 0x0c, 0x00, 0x03, 0x0a, 0xff, 0x07,
    ];
    /// SuperSpeed plus SuperSpeedPlus at 10 Gb/s (the adapter's shape).
    const SSP: &[u8] = &[
        0x05, 0x0f, 0x2a, 0x00, 0x03, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10, 0x03,
        0x00, 0x0e, 0x00, 0x03, 0x0a, 0xff, 0x07, 0x14, 0x10, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x11, 0x00, 0x00, 0x30, 0x40, 0x0a, 0x00, 0xb0, 0x40, 0x0a, 0x00,
    ];

    /// A fake `/sys/bus/usb/devices`: root hubs as symlinks into a
    /// controller directory, devices as directories with `busnum`,
    /// `devnum`, `speed`, optional `version` and `bos_descriptors`, hub
    /// ports under `<hub>/<hub>:1.0/<hub>-port<N>/` with `peer` links.
    struct Tree {
        root: tempfile::TempDir,
        next_devnum: std::cell::Cell<u8>,
    }

    impl Tree {
        fn new() -> Tree {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("devices")).unwrap();
            Tree {
                root,
                next_devnum: std::cell::Cell::new(2),
            }
        }

        fn base(&self) -> PathBuf {
            self.root.path().join("devices")
        }

        fn write(
            dir: &Path,
            bus: u8,
            devnum: u8,
            speed: &str,
            version: Option<&str>,
            bos: Option<&[u8]>,
        ) {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("busnum"), format!("{bus}\n")).unwrap();
            std::fs::write(dir.join("devnum"), format!("{devnum}\n")).unwrap();
            std::fs::write(dir.join("speed"), format!("{speed}\n")).unwrap();
            if let Some(version) = version {
                std::fs::write(dir.join("version"), format!("{version}\n")).unwrap();
            }
            if let Some(bos) = bos {
                std::fs::write(dir.join("bos_descriptors"), bos).unwrap();
            }
        }

        /// A root hub `usbN` at `speed`, devnum 1, under a controller dir.
        fn root_hub(&self, bus: u8, speed: &str) -> PathBuf {
            let real = self
                .root
                .path()
                .join("0000:00:14.0")
                .join(format!("usb{bus}"));
            Self::write(&real, bus, 1, speed, None, None);
            std::os::unix::fs::symlink(&real, self.base().join(format!("usb{bus}"))).unwrap();
            real
        }

        /// A device with the next devnum on its bus (taken from the name).
        fn device(
            &self,
            name: &str,
            speed: &str,
            version: Option<&str>,
            bos: Option<&[u8]>,
        ) -> PathBuf {
            let (bus, _) = crate::connector::parse_device_name(name).unwrap();
            let devnum = self.next_devnum.get();
            self.next_devnum.set(devnum + 1);
            let dir = self.base().join(name);
            Self::write(&dir, bus, devnum, speed, version, bos);
            dir
        }

        fn port(&self, hub_dir: &Path, hub: &str, number: u32) -> PathBuf {
            let dir = hub_dir
                .join(format!("{hub}:1.0"))
                .join(port_name(hub, number));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn pair(&self, a: &Path, b: &Path) {
            std::os::unix::fs::symlink(b, a.join("peer")).unwrap();
            std::os::unix::fs::symlink(a, b.join("peer")).unwrap();
        }

        fn analyze(&self) -> Vec<Finding> {
            let mut manager = DeviceManager::with_sysfs_base(self.base());
            manager.enumerate_present_devices();
            manager.update_bus_speeds();
            let index = PortIndex::scan_devices(
                manager
                    .buses
                    .values()
                    .flat_map(|bus| bus.devices.values())
                    .filter_map(|device| device.sysfs_path.as_deref()),
            );
            super::analyze(&manager, &index)
        }
    }

    /// Paired root hubs 3 (480) and 4 (5000) with `n` root ports paired by
    /// number.
    fn paired_roots(t: &Tree, n: u32) -> (PathBuf, PathBuf) {
        let usb3 = t.root_hub(3, "480");
        let usb4 = t.root_hub(4, "5000");
        for i in 1..=n {
            t.pair(&t.port(&usb3, "usb3", i), &t.port(&usb4, "usb4", i));
        }
        (usb3, usb4)
    }

    fn causes(findings: &[Finding]) -> Vec<(&str, Option<&Cause>)> {
        findings
            .iter()
            .map(|f| (f.path.as_str(), f.cause.as_ref()))
            .collect()
    }

    #[test]
    fn a_superspeed_device_on_a_usb2_port_with_an_empty_superspeed_side() {
        let t = Tree::new();
        paired_roots(&t, 2);
        t.device("3-1", "480", Some("2.10"), Some(SSP));
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![(
                "3-1",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "usb4-port1".into()
                })
            )]
        );
        let f = &findings[0];
        assert_eq!((f.bus, f.port.as_deref()), (3, Some("usb3-port1")));
        assert_eq!(f.capability.speed, UsbSpeed::from_mbps(10000.0));
        assert_eq!(f.capability.source, CapabilitySource::Bos);
        assert_eq!(
            f.message(),
            "linked at 480M, supports 10G: the SuperSpeed side of this connector (usb4-port1) is empty, so the link came up at USB 2 speed; check the cable or the port"
        );
    }

    #[test]
    fn a_hub_whose_superspeed_half_is_up_is_not_a_finding_nor_its_paired_child() {
        let t = Tree::new();
        paired_roots(&t, 1);
        let hub3 = t.device("3-1", "480", Some("2.10"), Some(SS));
        let hub4 = t.device("4-1", "5000", Some("3.00"), Some(SS));
        for n in 1..=2 {
            t.pair(&t.port(&hub3, "3-1", n), &t.port(&hub4, "4-1", n));
        }
        // A camera whose SuperSpeed side is up: only its USB 3 device enumerates.
        t.device("4-1.1", "5000", Some("3.00"), Some(SS));
        // A camera stuck at High Speed on hub port 2: its SuperSpeed side is empty.
        t.device("3-1.2", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![(
                "3-1.2",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "4-1-port2".into()
                })
            )]
        );
    }

    #[test]
    fn a_usb2_only_hub_holds_its_child_and_is_named() {
        let t = Tree::new();
        let (usb3, usb4) = paired_roots(&t, 1);
        let hub3 = t.device("3-1", "480", Some("2.10"), Some(SS));
        let hub4 = t.device("4-1", "5000", Some("3.00"), Some(SS));
        for n in 1..=4 {
            t.pair(&t.port(&hub3, "3-1", n), &t.port(&hub4, "4-1", n));
        }
        // A USB 2 only hub (bcdUSB 2.00, no BOS) on hub port 4, its own ports unpaired.
        let terminus = t.device("3-1.4", "480", Some("2.00"), None);
        t.port(&terminus, "3-1.4", 5);
        t.device("3-1.4.5", "480", Some("2.10"), Some(SS));
        let _ = (usb3, usb4);
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![(
                "3-1.4.5",
                Some(&Cause::UpstreamHubLink {
                    hub: "3-1.4".into(),
                    hub_link: UsbSpeed::from_mbps(480.0)
                })
            )]
        );
        assert_eq!(
            findings[0].message(),
            "linked at 480M, supports 5G: the hub above it (3-1.4) is linked at 480M; move it to a USB 3 port"
        );
    }

    /// The dock's shape: the inner hub's halves sit on different port
    /// numbers, the kernel pairs by number, so its halves are matched by
    /// elimination and neither half nor the billboard is a finding; the
    /// adapter on the outer hub's port 2 is.
    #[test]
    fn hub_halves_on_different_port_numbers_are_matched_by_elimination() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
        let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let inner2 = t.device("5-1.1", "480", Some("2.10"), Some(SS));
        let inner3 = t.device("6-1.4", "10000", Some("3.20"), Some(SSP));
        for n in 1..=8 {
            t.port(&inner2, "5-1.1", n);
        }
        for n in 1..=4 {
            t.port(&inner3, "6-1.4", n);
        }
        t.device(
            "5-1.1.8",
            "12",
            Some("2.01"),
            Some(&[
                0x05, 0x0f, 0x0c, 0x00, 0x01, 0x07, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00,
            ]),
        );
        t.device("5-1.2", "480", Some("2.10"), Some(SSP));
        // A 10 Gb/s SSD stuck at High Speed on the inner hub's port 2: its
        // SuperSpeed port is the matched half's port 2.
        t.device("5-1.1.2", "480", Some("2.10"), Some(SSP));
        // A device on the inner hub's port 8, which has no SuperSpeed twin.
        t.device("5-1.1.7", "480", Some("2.10"), Some(SS));
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![
                (
                    "5-1.2",
                    Some(&Cause::SuperSpeedSideEmpty {
                        peer_port: "6-1-port2".into()
                    })
                ),
                (
                    "5-1.1.2",
                    Some(&Cause::SuperSpeedSideEmpty {
                        peer_port: "6-1.4-port2".into()
                    })
                ),
                (
                    "5-1.1.7",
                    Some(&Cause::UpstreamHubLink {
                        hub: "5-1.1".into(),
                        hub_link: UsbSpeed::from_mbps(480.0)
                    })
                ),
            ]
        );
    }

    #[test]
    fn two_unclaimed_hubs_a_side_are_ambiguous_and_say_nothing_about_the_hubs() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
        let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let a = t.device("5-1.1", "480", Some("2.10"), Some(SS));
        let b = t.device("5-1.2", "480", Some("2.10"), Some(SS));
        let c = t.device("6-1.3", "5000", Some("3.00"), Some(SS));
        let d = t.device("6-1.4", "5000", Some("3.00"), Some(SS));
        t.port(&a, "5-1.1", 1);
        t.port(&b, "5-1.2", 1);
        // A real SuperSpeed hub owns port objects; without them rule H
        // would not see these two as candidates at all.
        t.port(&c, "6-1.3", 1);
        t.port(&d, "6-1.4", 1);
        t.device("5-1.1.1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![(
                "5-1.1.1",
                Some(&Cause::UpstreamHubLink {
                    hub: "5-1.1".into(),
                    hub_link: UsbSpeed::from_mbps(480.0)
                })
            )],
            "the hubs are ambiguous; the child's hub link is still a true statement"
        );
    }

    /// The other side of the ambiguous case: nothing unclaimed on the
    /// SuperSpeed side for this hub's half to be, so rule H proves the half
    /// never enumerated and the hub is named by its own empty twin port.
    #[test]
    fn a_hub_whose_superspeed_half_never_enumerated_is_named_and_so_is_its_child() {
        let t = Tree::new();
        let (_, usb4) = paired_roots(&t, 1);
        let _ = usb4;
        // A SuperSpeed hub stuck at High Speed; `usb4-port1` is empty and
        // bus 4 holds no hub the elimination could mistake for its half.
        let hub = t.device("3-1", "480", Some("2.10"), Some(SS));
        t.port(&hub, "3-1", 1);
        t.device("3-1.1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![
                (
                    "3-1",
                    Some(&Cause::SuperSpeedSideEmpty {
                        peer_port: "usb4-port1".into()
                    })
                ),
                (
                    "3-1.1",
                    Some(&Cause::UpstreamHubLink {
                        hub: "3-1".into(),
                        hub_link: UsbSpeed::from_mbps(480.0)
                    })
                ),
            ]
        );
    }

    #[test]
    fn a_faster_hub_on_a_slower_root_port_and_a_faster_device_under_a_slower_hub() {
        let t = Tree::new();
        let (_, usb4) = paired_roots(&t, 1);
        let _ = usb4;
        // A 10 Gb/s hub on the 5 Gb/s root port.
        let hub = t.device("4-1", "5000", Some("3.20"), Some(SSP));
        t.port(&hub, "4-1", 1);
        t.port(&hub, "4-1", 2);
        // A 10 Gb/s device under it, and a 5 Gb/s device that is fine.
        t.device("4-1.1", "5000", Some("3.20"), Some(SSP));
        t.device("4-1.2", "5000", Some("3.00"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![
                (
                    "4-1",
                    Some(&Cause::HostPortMax {
                        max: UsbSpeed::from_mbps(5000.0)
                    })
                ),
                (
                    "4-1.1",
                    Some(&Cause::UpstreamHubLink {
                        hub: "4-1".into(),
                        hub_link: UsbSpeed::from_mbps(5000.0)
                    })
                ),
            ]
        );
    }

    #[test]
    fn a_link_below_capability_under_a_permitting_port_blames_neither_upstream() {
        let t = Tree::new();
        let usb3 = t.root_hub(3, "480");
        let usb4 = t.root_hub(4, "10000");
        t.pair(&t.port(&usb3, "usb3", 1), &t.port(&usb4, "usb4", 1));
        t.device("4-1", "5000", Some("3.20"), Some(SSP));
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![("4-1", Some(&Cause::UpstreamPermits))]
        );
        assert_eq!(
            findings[0].message(),
            "linked at 5G, supports 10G: the port above it allows 10G; check the cable or the device"
        );
    }

    #[test]
    fn a_superspeed_hub_on_a_usb2_only_root_port_and_its_child() {
        let t = Tree::new();
        let usb1 = t.root_hub(1, "480");
        t.port(&usb1, "usb1", 1);
        let hub = t.device("1-1", "480", Some("2.10"), Some(SS));
        t.port(&hub, "1-1", 1);
        t.device("1-1.1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![
                ("1-1", Some(&Cause::Usb2OnlyHostPort)),
                (
                    "1-1.1",
                    Some(&Cause::UpstreamHubLink {
                        hub: "1-1".into(),
                        hub_link: UsbSpeed::from_mbps(480.0)
                    })
                ),
            ]
        );
    }

    #[test]
    fn a_usb2_root_port_without_a_superspeed_twin_is_usb2_only() {
        let t = Tree::new();
        let (usb3, _) = paired_roots(&t, 1);
        t.port(&usb3, "usb3", 5);
        t.device("3-5", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![("3-5", Some(&Cause::Usb2OnlyHostPort))]
        );
    }

    #[test]
    fn the_bcd_usb_fallback_is_labelled_and_never_claims_more_than_five_gbps() {
        let t = Tree::new();
        let (usb3, usb4) = paired_roots(&t, 2);
        let _ = (usb3, usb4);
        t.device("3-1", "480", Some("3.20"), None);
        t.device("4-2", "5000", Some("3.20"), None);
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![(
                "3-1",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "usb4-port1".into()
                })
            )],
            "the 5 Gb/s device with bcdUSB 3.20 and no BOS is not called out"
        );
        assert_eq!(findings[0].capability.source, CapabilitySource::BcdUsb);
        assert!(findings[0]
            .message()
            .starts_with("linked at 480M, supports 5G (from bcdUSB): "));
    }

    #[test]
    fn a_disconnected_device_is_ignored_and_a_present_twin_counts() {
        let t = Tree::new();
        paired_roots(&t, 1);
        t.device("3-1", "480", Some("2.10"), Some(SS));
        let mut manager = DeviceManager::with_sysfs_base(t.base());
        manager.enumerate_present_devices();
        manager.update_bus_speeds();
        let index = PortIndex::scan_devices(
            manager
                .buses
                .values()
                .flat_map(|bus| bus.devices.values())
                .filter_map(|device| device.sysfs_path.as_deref()),
        );
        assert_eq!(analyze(&manager, &index).len(), 1);
        manager
            .buses
            .get_mut(&3)
            .unwrap()
            .devices
            .get_mut(&2)
            .unwrap()
            .is_disconnected = true;
        assert!(analyze(&manager, &index).is_empty());
    }

    #[test]
    fn a_device_whose_port_is_unknown_has_no_cause() {
        let t = Tree::new();
        t.root_hub(3, "480");
        // No port objects at all (an old kernel or a bare fixture).
        t.device("3-1", "480", Some("3.00"), None);
        let findings = t.analyze();
        assert_eq!(causes(&findings), vec![("3-1", None)]);
        assert_eq!(findings[0].port, None);
        assert!(findings[0]
            .message()
            .ends_with(": its port is unknown to the connector index"));
    }

    #[test]
    fn findings_are_ordered_by_bus_then_address() {
        let t = Tree::new();
        let (usb3, _) = paired_roots(&t, 3);
        let _ = usb3;
        t.device("3-3", "480", Some("2.10"), Some(SS));
        t.device("3-1", "480", Some("2.10"), Some(SS));
        t.device("3-2", "480", Some("2.10"), Some(SS));
        let order: Vec<u8> = t.analyze().iter().map(|f| f.address).collect();
        assert_eq!(order, vec![2, 3, 4]);
    }

    #[test]
    fn short_speed_is_compact() {
        assert_eq!(short_speed(&UsbSpeed::from_mbps(480.0)), "480M");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(12.0)), "12M");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(1.5)), "1.5M");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(5000.0)), "5G");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(10000.0)), "10G");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(20000.0)), "20G");
        assert_eq!(short_speed(&UsbSpeed::UNKNOWN), "?");
    }
}

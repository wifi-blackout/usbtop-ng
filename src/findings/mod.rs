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
//!
//! A port number is never carried from one half of a connector to the
//! other on its own: each side of a controller numbers its root ports
//! independently (the corpus's `tgl-x360` bundle pairs `usb3-port1` with
//! `usb4-port2`). The SuperSpeed receptacle of a port is the kernel's own
//! reciprocal `peer`, and only under a pair matched by elimination — where
//! no `peer` exists by construction — is the number the best available.

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
    /// The hub above has a working SuperSpeed half, but this port of it has
    /// no twin there: the receptacle itself is USB 2 only.
    Usb2OnlyPort { hub: String, number: u32 },
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
            Cause::Usb2OnlyPort { .. } => "usb2_only_port",
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
            Cause::Usb2OnlyPort { hub, number } => format!(
                "port {number} of the hub above it ({hub}) is USB 2 only; move it to a USB 3 port"
            ),
            Cause::UpstreamHubLink { hub, hub_link } => {
                let mut text = format!(
                    "the hub above it ({hub}) is linked at {}",
                    short_speed(hub_link)
                );
                // Advice only on a known USB 2 link: a hub the manager
                // still lists as disconnected has no rate to argue from.
                if hub_link.to_mbps() > 0.0 && hub_link.to_mbps() <= HIGH_SPEED_MBPS {
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
            None => "why is not attributable from the connector topology".to_string(),
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
    vendor_id: Option<u16>,
}

impl Row<'_> {
    fn capable_above_high(&self) -> bool {
        self.capability
            .is_some_and(|c| c.speed.to_mbps() > HIGH_SPEED_MBPS)
    }

    /// Whether `idVendor` allows these two rows to be halves of one hub:
    /// the two halves of a hub are two functions of one silicon and share a
    /// vendor (the dock's inner hub is 2188:0031 and 2188:0032). Both ids
    /// must be known and equal; sysfs states `idVendor` for every device it
    /// enumerates, so an unknown one is a failed read, and a failed read is
    /// not evidence. Two *different* hubs of the same vendor, each failing
    /// on the opposite half, would still pair; that residue is accepted,
    /// since the test is only ever applied to a match already unique by
    /// elimination.
    fn vendor_agrees_with(&self, other: &Row) -> bool {
        matches!(
            (self.vendor_id, other.vendor_id),
            (Some(mine), Some(theirs)) if mine == theirs
        )
    }
}

/// Rule H's verdict about a hub's SuperSpeed half. The distinction that
/// matters is `Missing` against `Ambiguous`: a hub whose half provably never
/// enumerated is a call-out (its SuperSpeed side really is empty), while a
/// hub whose half the topology cannot pin down says nothing at all.
#[derive(Debug, Clone)]
enum Half {
    /// That hub is this hub's SuperSpeed half. `by_elimination` records how
    /// it was found: the kernel's own `peer` link (`false`) proves the two
    /// hubs sit on one receptacle, while an elimination match (`true`) only
    /// proves *which two hubs* are halves. Only the former lets a port
    /// number be carried across to the other side (see `finding_for`).
    Known { name: String, by_elimination: bool },
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

    /// Whether `hub`'s own port has a reciprocal peer holding a present
    /// *hub*. Only a hub can be the other half of a hub, so a plain device
    /// on that peer port — a USB 2 mouse in the receptacle the kernel
    /// happens to pair with a hub half — leaves the half still unclaimed.
    fn peer_holds_a_hub(&self, hub: &str) -> bool {
        self.claimed_half(hub).is_some()
    }

    /// The present hub on the reciprocal kernel peer of `hub`'s own port,
    /// when that pairing can be trusted to mean one receptacle: under a
    /// root hub (root ports pair by the controller's raw port numbers), or
    /// when both ports carry the firmware's ACPI position (a nonzero
    /// `location`, the case `find_and_link_peer` pairs by `match_location`).
    /// A pairing made by port number alone, the kernel's default under a
    /// hub, joins unrelated receptacles on a hub whose halves number their
    /// ports differently (the dock's outer hub does exactly that), and a
    /// vendor match is no evidence either: two hubs of one vendor swap the
    /// same way. Such a peer is a guess, and a guess claims nothing.
    fn claimed_half(&self, hub: &str) -> Option<&'a Row<'a>> {
        let (parent, number) = port_of_device(hub)?;
        let peer = self.peer_port_of(hub)?;
        let dev = self.device_on_port(&peer)?;
        if !self.ports.is_hub(dev.name) {
            return None;
        }
        let under_root = port_of_device(&parent).is_none();
        let located = |port: &str| self.ports.get(port).is_some_and(|p| p.located);
        let by_location = located(&port_name(&parent, number)) && located(&peer);
        (under_root || by_location).then_some(dev)
    }

    /// The present hubs attached to `parent`'s ports, in row order.
    /// Collected rather than returned lazily so the caller may hold the
    /// list across the recursive `ss_half` call.
    ///
    /// "Hub" here is `PortIndex::is_hub`: a device that owns at least one
    /// port object. A hub whose driver has not created its ports yet, and
    /// one the manager lists as disconnected, are both invisible to this
    /// for that tick — the engine is re-run and the whole view rebuilt
    /// every tick, so such a hub simply appears once its ports do.
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
                .map_or(Half::Missing, |name| Half::Known {
                    name,
                    by_elimination: false,
                });
        };
        // Step 2: the kernel's own pairing, where it can be trusted.
        if let Some(dev) = self.claimed_half(hub) {
            return Half::Known {
                name: dev.name.to_string(),
                by_elimination: false,
            };
        }
        // Step 3: match by elimination under the parent pair.
        if !row.capable_above_high() {
            // The hub states no SuperSpeed capability, so it has no half.
            return Half::Missing;
        }
        let parent_ss = match self.ss_half(&parent) {
            Half::Known { name, .. } => name,
            // No SuperSpeed side above at all, so no half of this hub can be
            // enumerated anywhere: that is knowledge, not ignorance.
            Half::Missing => return Half::Missing,
            Half::Ambiguous => return Half::Ambiguous,
        };
        // Set B before any claimed half is struck out. Empty here means
        // there is nothing on the SuperSpeed side at all, so this hub's half
        // never enumerated: knowledge. Emptied only by the strike-out below
        // means some other hub took every candidate through a pairing the
        // kernel made by port number, which under a hub is not proof of a
        // receptacle (see `claimed_half`); that is ambiguity, and ambiguity
        // is silence rather than a conviction.
        let b_pre: Vec<&Row> = self
            .hubs_on(&parent_ss)
            .into_iter()
            .filter(|h| h.link.to_mbps() > HIGH_SPEED_MBPS)
            .collect();
        if b_pre.is_empty() {
            return Half::Missing;
        }
        let unclaimed_usb2: Vec<&Row> = self
            .hubs_on(&parent)
            .into_iter()
            .filter(|h| h.link.to_mbps() <= HIGH_SPEED_MBPS && h.capable_above_high())
            .filter(|h| !self.peer_holds_a_hub(h.name))
            .collect();
        let unclaimed_ss: Vec<&Row> = b_pre
            .into_iter()
            .filter(|h| !self.peer_holds_a_hub(h.name))
            .collect();
        match (unclaimed_usb2.as_slice(), unclaimed_ss.as_slice()) {
            ([one], [half]) if one.name == hub && one.vendor_agrees_with(half) => Half::Known {
                name: half.name.to_string(),
                by_elimination: true,
            },
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
                vendor_id: device.vendor_id,
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
                Half::Known { .. } => return None,
                // Rule H cannot pin the half down, so an empty SuperSpeed
                // port opposite proves nothing -- that half may be
                // enumerated on another port number. Say nothing about the
                // hub; its children still get their own true statement.
                Half::Ambiguous => return None,
                // Rule H proved the half never enumerated: treat the hub
                // like any other device below.
                Half::Missing => {}
            }
        }
        let parent_half = topology.ss_half(&parent);
        // The kernel's own twin of *this* port first. The two sides of one
        // controller number their root ports independently -- the corpus's
        // `tgl-x360` bundle pairs `usb3-port1` with `usb4-port2` -- so the
        // same number on the other half names a different receptacle, and
        // taking it would exonerate a dead connector whenever that other
        // receptacle happens to be occupied. The number is carried across
        // only under a pair rule H matched by elimination, where the kernel
        // left the ports unpaired precisely because the halves sit on
        // different port numbers and nothing better exists.
        let ss_port = topology.peer_port_of(row.name).or_else(|| {
            match &parent_half {
                Half::Known {
                    name,
                    by_elimination: true,
                } => Some(port_name(name, number)),
                _ => None,
            }
            .filter(|name| topology.ports.get(name).is_some())
        });
        let cause = match ss_port {
            Some(ss_port) if topology.device_on_port(&ss_port).is_some() => return None,
            Some(ss_port) => Cause::SuperSpeedSideEmpty { peer_port: ss_port },
            None if root_parent => Cause::Usb2OnlyHostPort,
            None => match parent_half {
                // The hub above has a SuperSpeed half and this port of it
                // has no twin there: the receptacle is USB 2 only. The hub
                // named is the one the device hangs off, not its half.
                Half::Known { .. } => Cause::Usb2OnlyPort {
                    hub: parent,
                    number,
                },
                Half::Missing => Cause::UpstreamHubLink {
                    hub_link: topology
                        .present(&parent)
                        .map_or(UsbSpeed::UNKNOWN, |p| p.link.clone()),
                    hub: parent,
                },
                // Which hub is the parent's half is unknowable, so neither
                // an empty twin nor a USB 2 only port can be claimed.
                Half::Ambiguous => return Some(finding(Some(port), None)),
            },
        };
        return Some(finding(Some(port), Some(cause)));
    }
    let limit = if root_parent {
        bus_speed(row.bus)
    } else {
        topology.present(&parent).map(|p| p.link.clone())
    };
    // An absent, disconnected, or unknown-rate upstream is ignorance, not
    // permission: `UpstreamPermits` blames the cable or the device, which
    // needs an upstream rate that actually allows the capability.
    let Some(max) = limit.filter(|max| max.to_mbps() > 0.0) else {
        return Some(finding(Some(port), None));
    };
    let cause = if max.to_mbps() < capability.speed.to_mbps() {
        if root_parent {
            Cause::HostPortMax { max }
        } else {
            Cause::UpstreamHubLink {
                hub: parent,
                hub_link: max,
            }
        }
    } else {
        Cause::UpstreamPermits
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

        /// A device that also states an `idVendor`, for the vendor
        /// agreement rule inside rule H's elimination match.
        fn device_of_vendor(
            &self,
            name: &str,
            speed: &str,
            version: Option<&str>,
            bos: Option<&[u8]>,
            vendor: u16,
        ) -> PathBuf {
            let dir = self.device(name, speed, version, bos);
            std::fs::write(dir.join("idVendor"), format!("{vendor:04x}\n")).unwrap();
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

        /// The firmware's ACPI position of a port, as sysfs prints it; a
        /// nonzero value marks a pairing the kernel made by location.
        fn locate(&self, port_dir: &Path, location: u32) {
            std::fs::write(port_dir.join("location"), format!("0x{location:08x}\n")).unwrap();
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
        let inner2 = t.device_of_vendor("5-1.1", "480", Some("2.10"), Some(SS), 0x2188);
        let inner3 = t.device_of_vendor("6-1.4", "10000", Some("3.20"), Some(SSP), 0x2188);
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
        // A device on the inner hub's port 7, which has no SuperSpeed twin
        // (the USB 3 half owns four ports): that receptacle is USB 2 only,
        // which the matched half proves.
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
                    Some(&Cause::Usb2OnlyPort {
                        hub: "5-1.1".into(),
                        number: 7
                    })
                ),
            ]
        );
        assert_eq!(
            findings[2].message(),
            "linked at 480M, supports 5G: port 7 of the hub above it (5-1.1) is USB 2 only; move it to a USB 3 port"
        );
    }

    /// Every SuperSpeed hub under the parent is spoken for by a hub on its
    /// kernel peer port. Under a hub that pairing is by port number and can
    /// join unrelated receptacles, so the SuperSpeed-capable USB 2 hub left
    /// over is ambiguous, not proven dead: silence, not a conviction.
    #[test]
    fn a_hub_whose_only_candidates_are_claimed_by_other_hubs_is_silenced() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
        let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
        for n in 1..=2 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        // A healthy hub on port 2, both halves kernel-paired.
        let healthy2 = t.device("5-1.2", "480", Some("2.10"), Some(SS));
        let healthy3 = t.device("6-1.2", "5000", Some("3.00"), Some(SS));
        t.pair(
            &t.port(&healthy2, "5-1.2", 1),
            &t.port(&healthy3, "6-1.2", 1),
        );
        // A SuperSpeed-capable hub on port 1 whose SuperSpeed side is empty.
        let dead = t.device("5-1.1", "480", Some("2.10"), Some(SS));
        t.port(&dead, "5-1.1", 1);
        assert_eq!(causes(&t.analyze()), vec![]);
    }

    /// The dock shape with an unrelated SuperSpeed hub of the same vendor on
    /// the outer hub's SuperSpeed port 1, the port the kernel pairs by
    /// number with the inner hub's USB 2 half, and a healthy nested hub
    /// below the inner one. A by-number pairing under a hub claims
    /// nothing, and two candidates a side are ambiguity: neither hub is
    /// convicted, and the device stuck under the nested hub gets no cause.
    #[test]
    fn a_same_vendor_hub_on_the_wrongly_paired_port_leaves_the_pairing_ambiguous() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device_of_vendor("5-1", "480", Some("2.10"), None, 0x2188);
        let outer3 = t.device_of_vendor("6-1", "10000", Some("3.20"), Some(SSP), 0x8087);
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let inner2 = t.device_of_vendor("5-1.1", "480", Some("2.10"), Some(SSP), 0x2188);
        let inner3 = t.device_of_vendor("6-1.4", "10000", Some("3.20"), Some(SSP), 0x2188);
        for n in 1..=4 {
            t.port(&inner2, "5-1.1", n);
            t.port(&inner3, "6-1.4", n);
        }
        // The unrelated same-vendor SuperSpeed hub, USB 2 half not enumerated.
        let stray = t.device_of_vendor("6-1.1", "5000", Some("3.00"), Some(SS), 0x2188);
        t.port(&stray, "6-1.1", 1);
        // A healthy nested hub on the inner hub's port 2, both halves up,
        // and a SuperSpeed device stuck at High Speed below its USB 2 half.
        let nested2 = t.device_of_vendor("5-1.1.2", "480", Some("2.10"), Some(SS), 0x0bda);
        let nested3 = t.device_of_vendor("6-1.4.2", "5000", Some("3.00"), Some(SS), 0x0bda);
        t.port(&nested2, "5-1.1.2", 1);
        t.port(&nested3, "6-1.4.2", 1);
        t.device("5-1.1.2.1", "480", Some("2.10"), Some(SS));
        assert_eq!(causes(&t.analyze()), vec![("5-1.1.2.1", None)]);
    }

    /// A pairing the kernel made by ACPI location is physical evidence:
    /// two hubs of different vendors on located, paired ports under a hub
    /// are halves of one hub, and a device stuck below them names its own
    /// kernel peer.
    #[test]
    fn a_location_paired_hub_port_is_trusted_under_a_hub() {
        let t = Tree::new();
        let (usb3, usb4) = paired_roots(&t, 1);
        let _ = (usb3, usb4);
        let hub2 = t.device("3-1", "480", Some("2.10"), Some(SS));
        let hub3 = t.device("4-1", "5000", Some("3.00"), Some(SS));
        let p2 = t.port(&hub2, "3-1", 2);
        let p3 = t.port(&hub3, "4-1", 2);
        t.pair(&p2, &p3);
        t.locate(&p2, 0x0000_000a);
        t.locate(&p3, 0x0000_000a);
        let child2 = t.device_of_vendor("3-1.2", "480", Some("2.10"), Some(SS), 0x1111);
        let child3 = t.device_of_vendor("4-1.2", "5000", Some("3.00"), Some(SS), 0x2222);
        t.pair(&t.port(&child2, "3-1.2", 1), &t.port(&child3, "4-1.2", 1));
        t.device("3-1.2.1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![(
                "3-1.2.1",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "4-1.2-port1".into()
                })
            )]
        );
    }

    /// The dock shape with a USB 2 only hub of another vendor on the outer
    /// hub's port 4, the port the kernel pairs by number with the inner
    /// hub's SuperSpeed half. That pairing carries no location, so it
    /// claims nothing: the inner hub stays matched and nothing is convicted
    /// wrongly; the USB 2 only hub's own child is held by it, and says so.
    #[test]
    fn a_usb2_only_hub_on_the_wrongly_paired_port_claims_nothing() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device_of_vendor("5-1", "480", Some("2.10"), None, 0x2188);
        let outer3 = t.device_of_vendor("6-1", "10000", Some("3.20"), Some(SSP), 0x8087);
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let inner2 = t.device_of_vendor("5-1.1", "480", Some("2.10"), Some(SSP), 0x2188);
        let inner3 = t.device_of_vendor("6-1.4", "10000", Some("3.20"), Some(SSP), 0x2188);
        for n in 1..=4 {
            t.port(&inner2, "5-1.1", n);
            t.port(&inner3, "6-1.4", n);
        }
        // A USB 2 only hub (no BOS, bcdUSB 2.00) of another vendor on 5-1.4.
        let terminus = t.device_of_vendor("5-1.4", "480", Some("2.00"), None, 0x1a40);
        t.port(&terminus, "5-1.4", 1);
        t.device("5-1.4.1", "480", Some("2.10"), Some(SS));
        // A 10 Gb/s device stuck at High Speed on the inner hub's port 2.
        t.device("5-1.1.2", "480", Some("2.10"), Some(SSP));
        assert_eq!(
            causes(&t.analyze()),
            vec![
                (
                    "5-1.4.1",
                    Some(&Cause::UpstreamHubLink {
                        hub: "5-1.4".into(),
                        hub_link: UsbSpeed::from_mbps(480.0)
                    })
                ),
                (
                    "5-1.1.2",
                    Some(&Cause::SuperSpeedSideEmpty {
                        peer_port: "6-1.4-port2".into()
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
            vec![("5-1.1.1", None)],
            "the hubs are ambiguous, so nothing above the child is attributable either"
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

    /// A hub the manager still lists as disconnected is not a row, so a
    /// child still present under it is held by a hub of unknown rate: the
    /// statement stays true and the USB 3 advice is withheld.
    #[test]
    fn a_child_of_a_disconnected_hub_gets_no_advice_it_cannot_earn() {
        let t = Tree::new();
        paired_roots(&t, 1);
        let hub = t.device("3-1", "480", Some("2.10"), Some(SS));
        t.port(&hub, "3-1", 1);
        t.device("3-1.1", "480", Some("2.10"), Some(SS));
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
        manager
            .buses
            .get_mut(&3)
            .unwrap()
            .devices
            .get_mut(&2)
            .unwrap()
            .is_disconnected = true;
        let findings = analyze(&manager, &index);
        assert_eq!(
            causes(&findings),
            vec![(
                "3-1.1",
                Some(&Cause::UpstreamHubLink {
                    hub: "3-1".into(),
                    hub_link: UsbSpeed::UNKNOWN
                })
            )]
        );
        assert_eq!(
            findings[0].message(),
            "linked at 480M, supports 5G: the hub above it (3-1) is linked at ?"
        );
    }

    #[test]
    fn a_device_that_went_disconnected_is_ignored() {
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
            .ends_with(": why is not attributable from the connector topology"));
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

    /// Root ports on the two halves of one controller are numbered
    /// independently: the committed `tgl-x360` bundle pairs `usb3-port1`
    /// with `usb4-port2`. The kernel's own `peer` is the only thing that
    /// names the SuperSpeed receptacle, and taking the same number instead
    /// would here exonerate `3-1` because the unrelated `usb4-port1` is
    /// occupied.
    fn cross_numbered_roots(t: &Tree) -> (PathBuf, PathBuf) {
        let usb3 = t.root_hub(3, "480");
        let usb4 = t.root_hub(4, "5000");
        t.pair(&t.port(&usb3, "usb3", 1), &t.port(&usb4, "usb4", 2));
        t.pair(&t.port(&usb3, "usb3", 3), &t.port(&usb4, "usb4", 1));
        t.port(&usb3, "usb3", 2);
        (usb3, usb4)
    }

    #[test]
    fn a_cross_numbered_root_pair_names_the_kernels_twin_not_the_same_number() {
        let occupied = Tree::new();
        cross_numbered_roots(&occupied);
        occupied.device("3-1", "480", Some("2.10"), Some(SS));
        // A healthy SuperSpeed device on the OTHER receptacle, which shares
        // only the port number: it must not exonerate `3-1`.
        occupied.device("4-1", "5000", Some("3.00"), Some(SS));
        assert_eq!(
            causes(&occupied.analyze()),
            vec![(
                "3-1",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "usb4-port2".into()
                })
            )]
        );

        let alone = Tree::new();
        cross_numbered_roots(&alone);
        alone.device("3-1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&alone.analyze()),
            vec![(
                "3-1",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "usb4-port2".into()
                })
            )],
            "the twin is named from the peer link, not from what occupies it"
        );
    }

    #[test]
    fn a_root_port_without_a_peer_never_falls_back_to_the_same_number() {
        let t = Tree::new();
        cross_numbered_roots(&t);
        // `usb3-port2` has no peer, and `usb4-port2` exists but belongs to
        // `usb3-port1`: the number must not be carried across under a root.
        t.device("3-2", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![("3-2", Some(&Cause::Usb2OnlyHostPort))]
        );
    }

    /// The dock shape with a USB 2 mouse in the receptacle the kernel pairs
    /// with the inner hub's SuperSpeed half. Only a hub can be a hub's
    /// half, so the mouse must not make `6-1.4` look claimed and convict
    /// the perfectly healthy `5-1.1`.
    #[test]
    fn a_plain_device_on_a_paired_port_does_not_claim_a_hub_half() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
        let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let inner2 = t.device_of_vendor("5-1.1", "480", Some("2.10"), Some(SSP), 0x2188);
        let inner3 = t.device_of_vendor("6-1.4", "10000", Some("3.20"), Some(SSP), 0x2188);
        for n in 1..=8 {
            t.port(&inner2, "5-1.1", n);
        }
        for n in 1..=4 {
            t.port(&inner3, "6-1.4", n);
        }
        // The mouse sits on `5-1-port4`, whose kernel peer `6-1-port4` is
        // the inner hub's SuperSpeed half's own port.
        t.device("5-1.4", "12", Some("2.00"), None);
        t.device("5-1.1.2", "480", Some("2.10"), Some(SSP));
        assert_eq!(
            causes(&t.analyze()),
            vec![(
                "5-1.1.2",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "6-1.4-port2".into()
                })
            )],
            "the inner hub is still matched and the mouse is not a finding"
        );
    }

    /// Two unrelated hubs, each dead on the opposite half, are a unique
    /// match by elimination alone; `idVendor` is what tells them apart.
    /// An unknown `idVendor` (a failed read: sysfs states one for every
    /// device) is not agreement. Two unrelated half-failed hubs, one of them
    /// vendor-less, stay ambiguous instead of fusing into one.
    #[test]
    fn an_unknown_vendor_never_completes_an_elimination_match() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device_of_vendor("5-1", "480", Some("2.10"), None, 0x2188);
        let outer3 = t.device_of_vendor("6-1", "10000", Some("3.20"), Some(SSP), 0x8087);
        for n in 1..=2 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        // X: SuperSpeed side dead, vendor unknown. Y: USB 2 side dead.
        let x2 = t.device("5-1.1", "480", Some("2.10"), Some(SS));
        t.port(&x2, "5-1.1", 1);
        let y3 = t.device_of_vendor("6-1.2", "5000", Some("3.00"), Some(SS), 0x2222);
        t.port(&y3, "6-1.2", 1);
        t.device("5-1.1.1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![("5-1.1.1", None)],
            "neither hub is named; the device under X gets no cause"
        );
    }

    #[test]
    fn the_elimination_match_needs_the_two_halves_to_share_a_vendor() {
        let build = |vendor_y: u16| {
            let t = Tree::new();
            let usb5 = t.root_hub(5, "480");
            let usb6 = t.root_hub(6, "10000");
            t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
            let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
            let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
            for n in 1..=4 {
                t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
            }
            // Hub X: its USB 2 half is up, its SuperSpeed half never came.
            let x2 = t.device_of_vendor("5-1.1", "480", Some("2.10"), Some(SS), 0x1111);
            t.port(&x2, "5-1.1", 1);
            // Hub Y: its SuperSpeed half is up, its USB 2 half never came.
            let y3 = t.device_of_vendor("6-1.2", "5000", Some("3.00"), Some(SS), vendor_y);
            t.port(&y3, "6-1.2", 1);
            t.device("5-1.1.1", "480", Some("2.10"), Some(SS));
            t
        };
        assert_eq!(
            causes(&build(0x2222).analyze()),
            vec![("5-1.1.1", None)],
            "different vendors cannot be two halves of one hub, so nothing is attributable"
        );
        // Same vendor: the elimination match stands, X is exonerated and the
        // stuck device is placed against Y's port 1. Two *different* hubs of
        // one vendor failing on opposite halves would pair here too; that
        // residue is accepted.
        assert_eq!(
            causes(&build(0x1111).analyze()),
            vec![(
                "5-1.1.1",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "6-1.2-port1".into()
                })
            )]
        );
    }

    /// Elimination nests: a second-level hub whose halves again sit on
    /// different port numbers is matched under the pair its own parents
    /// were matched into, and a device stuck below it is placed against
    /// that second-level half's port.
    #[test]
    fn an_elimination_match_under_an_elimination_match_places_the_device() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
        let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let inner2 = t.device_of_vendor("5-1.1", "480", Some("2.10"), Some(SSP), 0x2188);
        let inner3 = t.device_of_vendor("6-1.4", "10000", Some("3.20"), Some(SSP), 0x2188);
        for n in 1..=8 {
            t.port(&inner2, "5-1.1", n);
        }
        for n in 1..=4 {
            t.port(&inner3, "6-1.4", n);
        }
        // The second level, mis-paired again: port 1 of the USB 2 half,
        // port 3 of the SuperSpeed half.
        let deep2 = t.device_of_vendor("5-1.1.1", "480", Some("2.10"), Some(SS), 0x0bda);
        let deep3 = t.device_of_vendor("6-1.4.3", "5000", Some("3.00"), Some(SS), 0x0bda);
        for n in 1..=4 {
            t.port(&deep2, "5-1.1.1", n);
            t.port(&deep3, "6-1.4.3", n);
        }
        t.device("5-1.1.1.2", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![(
                "5-1.1.1.2",
                Some(&Cause::SuperSpeedSideEmpty {
                    peer_port: "6-1.4.3-port2".into()
                })
            )]
        );
    }

    #[test]
    fn a_device_under_a_disconnected_hub_is_not_told_the_upstream_permits_it() {
        let t = Tree::new();
        paired_roots(&t, 1);
        let hub = t.device("4-1", "5000", Some("3.20"), Some(SSP));
        t.port(&hub, "4-1", 1);
        t.device("4-1.1", "5000", Some("3.20"), Some(SSP));
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
        manager
            .buses
            .get_mut(&4)
            .unwrap()
            .devices
            .get_mut(&2)
            .unwrap()
            .is_disconnected = true;
        let findings = analyze(&manager, &index);
        assert_eq!(causes(&findings), vec![("4-1.1", None)]);
        assert_eq!(
            findings[0].port.as_deref(),
            Some("4-1-port1"),
            "the port is known; it is the upstream's rate that is not"
        );
    }

    #[test]
    fn a_usb2_only_port_on_a_healthy_hub_names_the_port_not_the_hubs_link() {
        let t = Tree::new();
        paired_roots(&t, 1);
        let hub3 = t.device("3-1", "480", Some("2.10"), Some(SS));
        let hub4 = t.device("4-1", "5000", Some("3.00"), Some(SS));
        for n in 1..=2 {
            t.pair(&t.port(&hub3, "3-1", n), &t.port(&hub4, "4-1", n));
        }
        // Port 3 exists on the USB 2 half alone: a USB 2 only receptacle.
        t.port(&hub3, "3-1", 3);
        t.device("3-1.3", "480", Some("2.10"), Some(SS));
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![(
                "3-1.3",
                Some(&Cause::Usb2OnlyPort {
                    hub: "3-1".into(),
                    number: 3
                })
            )]
        );
        assert_eq!(
            findings[0].message(),
            "linked at 480M, supports 5G: port 3 of the hub above it (3-1) is USB 2 only; move it to a USB 3 port"
        );
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

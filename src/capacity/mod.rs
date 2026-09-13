//! Choke points: the theoretical load every hub's link would carry if
//! every device below it pushed what it can, against that link's own
//! capacity. No measured traffic, no I/O: the tree comes from the
//! manager's rows (parents from sysfs names), rates from the rows. See
//! `docs/superpowers/specs/2026-09-13-chokepoints-design.md`.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::connector::{port_name, port_of_device, PortIndex};
use crate::device::manager::DeviceManager;
use crate::usbmon::parser::{short_mbps, UsbSpeed};

/// The breathing room: a hub is listed only when its subtree asks at least
/// this many times its link's practical capacity. Below it, a 480M hub
/// carrying a flash drive and a mouse (1.00x) stays quiet.
pub const CHOKE_FLOOR: f64 = 1.25;

const HIGH_SPEED_MBPS: f64 = 480.0;

/// How far below [`CHOKE_FLOOR`] still counts as reaching it (see `walk`).
const FLOOR_TOLERANCE: f64 = 1e-9;

/// Which rate every device is assumed to push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Basis {
    /// The rate each device has now, on both sides.
    #[default]
    Link,
    /// The topology as it could link: a leaf's capability and a SuperSpeed
    /// hub's capability as its capacity, each bounded by the hubs above it.
    Capability,
}

impl Basis {
    /// The JSON value and the `--demand` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::Link => "link",
            Basis::Capability => "capability",
        }
    }

    /// The other basis.
    pub fn toggled(self) -> Basis {
        match self {
            Basis::Link => Basis::Capability,
            Basis::Capability => Basis::Link,
        }
    }
}

/// One device below a choked hub and what it asks, practical Mb/s.
#[derive(Debug, Clone, PartialEq)]
pub struct Contributor {
    pub path: String,
    pub demand_mbps: f64,
}

/// A hub whose subtree asks more of its link than the link can carry.
#[derive(Debug, Clone, PartialEq)]
pub struct Chokepoint {
    pub bus: u8,
    pub address: u8,
    /// The hub's sysfs name, `3-1`.
    pub path: String,
    /// The kernel port name of the hub's link, `usb3-port1`: the same key
    /// the findings carry, so a script can join the two lists.
    pub port: Option<String>,
    pub capacity_mbps: f64,
    pub demand_mbps: f64,
    pub ratio: f64,
    /// Every device below the hub, hubs included.
    pub devices: usize,
    /// The largest contributors, at most three, demand descending then path.
    pub top: Vec<Contributor>,
}

impl Chokepoint {
    /// `384M carries 9 devices asking 1.17G: 3.05x`.
    pub fn message(&self) -> String {
        format!(
            "{} carries {} devices asking {}: {:.2}x",
            short_mbps(self.capacity_mbps),
            self.devices,
            short_mbps(self.demand_mbps),
            self.ratio
        )
    }
}

struct Node<'a> {
    name: &'a str,
    bus: u8,
    address: u8,
    link: f64,
    capability: Option<f64>,
    children: Vec<usize>,
}

/// A rate in Mb/s to what the class's efficiency leaves of it; zero and
/// unknown rates give zero.
fn practical(rate_mbps: f64) -> f64 {
    let speed = UsbSpeed::from_mbps(rate_mbps);
    rate_mbps * speed.class().efficiency()
}

/// The rate a hub's link is taken to carry, in Mb/s, before efficiency: at
/// the capability basis never more than `bound`, the tightest capacity rate
/// of the hubs above it, because a hub's link cannot come up faster than
/// the port it is plugged into (a 10G hub under a 5G port links at 5G,
/// whatever its BOS says). An unknown link is zero at both bases.
fn capacity_rate(node: &Node, basis: Basis, bound: f64) -> f64 {
    match basis {
        Basis::Link => node.link,
        // A USB 2 half is 480 whatever its BOS says (the SuperSpeed
        // capability it advertises belongs to its other half, a different
        // sysfs hub); a SuperSpeed half linked below its capability counts
        // at that capability.
        Basis::Capability if node.link <= HIGH_SPEED_MBPS => node.link.min(bound),
        Basis::Capability => node
            .capability
            .map_or(node.link, |c| c.max(node.link))
            .min(bound),
    }
}

/// The rate a leaf is taken to push, before efficiency: at the capability
/// basis the larger of its capability and its link, bounded by `bound`, the
/// tightest capacity rate of the hubs above it. The `max` mirrors the hub
/// rule: a stated capability is a floor, never a ceiling below the rate the
/// device has actually reached, so a bcdUSB 3.x floor of 5 Gb/s cannot make
/// a device linked at 10 Gb/s ask for less than it already asks.
fn leaf_rate(node: &Node, basis: Basis, bound: f64) -> f64 {
    match basis {
        Basis::Link => node.link,
        // An unknown link asks nothing at either basis: nothing has been
        // negotiated for the device to push, whatever its BOS advertises.
        Basis::Capability if node.link <= 0.0 => 0.0,
        Basis::Capability => node
            .capability
            .unwrap_or(node.link)
            .max(node.link)
            .min(bound),
    }
}

/// Walk `at`'s subtree: returns (demand in practical Mb/s, devices below,
/// every leaf's contribution), and pushes a `Chokepoint` for `at` when it
/// is a hub at or above the floor.
fn walk(
    nodes: &[Node],
    at: usize,
    ports: &PortIndex,
    basis: Basis,
    bound: f64,
    out: &mut Vec<Chokepoint>,
) -> (f64, usize, Vec<Contributor>) {
    let node = &nodes[at];
    // A hub is a device that owns port objects or has children. A hub with
    // nothing below it is not a traffic source: an empty 4-port hub asks
    // nothing of its uplink, where treating it as a leaf would have it ask
    // its whole link rate.
    if node.children.is_empty() && !ports.is_hub(node.name) {
        let demand = practical(leaf_rate(node, basis, bound));
        let contributors = if demand > 0.0 {
            vec![Contributor {
                path: node.name.to_string(),
                demand_mbps: demand,
            }]
        } else {
            Vec::new()
        };
        return (demand, 0, contributors);
    }
    let rate = capacity_rate(node, basis, bound);
    // What the hubs and leaves below are bounded by: at the capability
    // basis this hub's own rate, itself bounded from above. A hub of
    // unknown rate bounds nothing, so the hubs above it still see the
    // demand that crosses them.
    let inner_bound = match basis {
        Basis::Link => bound,
        Basis::Capability if rate > 0.0 => rate,
        Basis::Capability => bound,
    };
    let mut demand = 0.0;
    let mut devices = 0;
    let mut contributors = Vec::new();
    for &child in &node.children {
        let (d, n, c) = walk(nodes, child, ports, basis, inner_bound, out);
        demand += d;
        devices += n + 1;
        contributors.extend(c);
    }
    // A root hub is a bound for its subtree but never a stage of its own.
    if let Some((parent, number)) = port_of_device(node.name) {
        let capacity = practical(rate);
        if capacity > 0.0 {
            let ratio = demand / capacity;
            // Tolerant on purpose: the practical figures are products of
            // 0.7, 0.8 and 0.85, so a subtree meant to sit exactly on the
            // floor can land an ulp below it and would otherwise vanish.
            if ratio + FLOOR_TOLERANCE >= CHOKE_FLOOR {
                let mut top = contributors.clone();
                top.sort_by(|a, b| {
                    b.demand_mbps
                        .partial_cmp(&a.demand_mbps)
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| a.path.cmp(&b.path))
                });
                top.truncate(3);
                out.push(Chokepoint {
                    bus: node.bus,
                    address: node.address,
                    path: node.name.to_string(),
                    port: Some(port_name(&parent, number)),
                    capacity_mbps: capacity,
                    demand_mbps: demand,
                    ratio,
                    devices,
                    top,
                });
            }
        }
    }
    (demand, devices, contributors)
}

/// Every hub whose subtree asks at least `CHOKE_FLOOR` times its link's
/// practical capacity, worst first, ties by path. `ports` says which
/// devices own port objects, so a hub with nothing plugged into it is
/// recognised as a hub rather than counted as a device asking its link
/// rate.
pub fn analyze(manager: &DeviceManager, ports: &PortIndex, basis: Basis) -> Vec<Chokepoint> {
    let mut nodes: Vec<Node> = manager
        .buses
        .values()
        .flat_map(|bus| bus.devices.values())
        .filter(|device| !device.is_disconnected)
        .filter_map(|device| {
            let name = device.sysfs_path.as_deref()?.file_name()?.to_str()?;
            Some(Node {
                name,
                bus: device.bus_id,
                address: device.device_id,
                link: device.speed.to_mbps(),
                capability: device.capability.as_ref().map(|c| c.speed.to_mbps()),
                children: Vec::new(),
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(b.name));
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.name, i)).collect();
    let parents: Vec<Option<usize>> = nodes
        .iter()
        .map(|node| port_of_device(node.name).and_then(|(hub, _)| index.get(hub.as_str()).copied()))
        .collect();
    for (i, &parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            nodes[parent].children.push(i);
        }
    }
    let mut out = Vec::new();
    for (i, parent) in parents.iter().enumerate() {
        if parent.is_none() {
            walk(&nodes, i, ports, basis, f64::INFINITY, &mut out);
        }
    }
    out.sort_by(|a, b| {
        b.ratio
            .partial_cmp(&a.ratio)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_tree::{Tree, SS, SSP};
    use std::path::PathBuf;

    /// One root hub `usbN` at `speed` with `ports` ports; returns its dir.
    fn root(t: &Tree, bus: u8, speed: &str, ports: u32) -> PathBuf {
        let dir = t.root_hub(bus, speed);
        for n in 1..=ports {
            t.port(&dir, &format!("usb{bus}"), n);
        }
        dir
    }

    /// A hub `name` at `speed` with `ports` ports; returns its dir.
    fn hub(t: &Tree, name: &str, speed: &str, bos: Option<&[u8]>, ports: u32) -> PathBuf {
        let dir = t.device(
            name,
            speed,
            if bos.is_some() {
                Some("2.10")
            } else {
                Some("2.00")
            },
            bos,
        );
        for n in 1..=ports {
            t.port(&dir, name, n);
        }
        dir
    }

    /// `analyze` over the tree as the live callers build it: the manager and
    /// the connector index scanned from the same device directories.
    fn choke(t: &Tree, basis: Basis) -> Vec<Chokepoint> {
        let m = t.manager();
        analyze(&m, &Tree::port_index(&m), basis)
    }

    fn summary(points: &[Chokepoint]) -> Vec<(&str, f64, f64, f64, usize)> {
        points
            .iter()
            .map(|c| {
                (
                    c.path.as_str(),
                    c.capacity_mbps,
                    c.demand_mbps,
                    (c.ratio * 100.0).round() / 100.0,
                    c.devices,
                )
            })
            .collect()
    }

    #[test]
    fn two_high_speed_devices_under_a_high_speed_hub_are_listed_at_two_x() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 2);
        t.device("1-1.1", "480", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        let points = choke(&t, Basis::Link);
        assert_eq!(summary(&points), vec![("1-1", 384.0, 768.0, 2.0, 2)]);
        let c = &points[0];
        assert_eq!(
            (c.bus, c.address, c.port.as_deref()),
            (1, 2, Some("usb1-port1"))
        );
        assert_eq!(
            c.top,
            vec![
                Contributor {
                    path: "1-1.1".into(),
                    demand_mbps: 384.0
                },
                Contributor {
                    path: "1-1.2".into(),
                    demand_mbps: 384.0
                },
            ]
        );
        assert_eq!(c.message(), "384M carries 2 devices asking 768M: 2.00x");
    }

    #[test]
    fn a_flash_drive_and_a_mouse_stay_inside_the_breathing_room() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 2);
        t.device("1-1.1", "480", Some("2.00"), None);
        t.device("1-1.2", "1.5", Some("2.00"), None);
        assert!(
            choke(&t, Basis::Link).is_empty(),
            "1.003x is not a choke point"
        );
    }

    #[test]
    fn the_floor_is_inclusive_at_one_and_a_quarter() {
        // 384 + ten full-speed devices at 9.6 = 480 practical: exactly 1.25x.
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 12);
        t.device("1-1.1", "480", Some("2.00"), None);
        for n in 2..=11 {
            t.device(&format!("1-1.{n}"), "12", Some("2.00"), None);
        }
        let points = choke(&t, Basis::Link);
        assert_eq!(points.len(), 1, "exactly 1.25x is listed");
        assert!((points[0].ratio - 1.25).abs() < 1e-9);
        // Nine of them: 470.4, below the floor.
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 12);
        t.device("1-1.1", "480", Some("2.00"), None);
        for n in 2..=10 {
            t.device(&format!("1-1.{n}"), "12", Some("2.00"), None);
        }
        assert!(choke(&t, Basis::Link).is_empty());
    }

    #[test]
    fn nested_hubs_sum_the_whole_subtree() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 2);
        hub(&t, "1-1.1", "480", None, 2);
        t.device("1-1.1.1", "480", Some("2.00"), None);
        t.device("1-1.1.2", "480", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        let points = choke(&t, Basis::Link);
        assert_eq!(
            summary(&points),
            vec![
                ("1-1", 384.0, 1152.0, 3.0, 4),
                ("1-1.1", 384.0, 768.0, 2.0, 2)
            ],
            "the parent counts the child hub and every device below it"
        );
    }

    #[test]
    fn an_empty_hub_asks_nothing() {
        // A hub owns port objects even with nothing plugged in, so it is a
        // hub and not a leaf asking its own link rate.
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 2);
        t.device("1-1.1", "480", Some("2.00"), None);
        hub(&t, "1-1.2", "480", None, 2);
        assert!(
            choke(&t, Basis::Link).is_empty(),
            "one flash drive asks 384 of 384; the empty hub asks nothing"
        );
        // With a second flash drive the hub is listed, and the empty hub is
        // a device below it but never a contributor.
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 3);
        t.device("1-1.1", "480", Some("2.00"), None);
        hub(&t, "1-1.2", "480", None, 2);
        t.device("1-1.3", "480", Some("2.00"), None);
        let points = choke(&t, Basis::Link);
        assert_eq!(summary(&points), vec![("1-1", 384.0, 768.0, 2.0, 3)]);
        let top: Vec<&str> = points[0].top.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(top, ["1-1.1", "1-1.3"]);
    }

    #[test]
    fn the_two_halves_of_a_hub_are_summed_separately() {
        let t = Tree::new();
        let usb3 = root(&t, 3, "480", 1);
        let usb4 = root(&t, 4, "5000", 1);
        t.pair(
            &usb3.join("usb3:1.0/usb3-port1"),
            &usb4.join("usb4:1.0/usb4-port1"),
        );
        hub(&t, "3-1", "480", Some(SS), 2);
        hub(&t, "4-1", "5000", Some(SS), 2);
        t.device("3-1.1", "480", Some("2.00"), None);
        t.device("3-1.2", "480", Some("2.00"), None);
        t.device("4-1.1", "5000", Some("3.00"), Some(SS));
        assert_eq!(
            summary(&choke(&t, Basis::Link)),
            vec![("3-1", 384.0, 768.0, 2.0, 2)],
            "the USB 3 half carries one 5G device, 1.0x, and is not listed"
        );
    }

    #[test]
    fn unknown_rates_disconnected_devices_and_internal_devices() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 3);
        t.device("1-1.1", "480", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        t.device("1-1.3", "0", Some("2.00"), None); // rate unknown
        let mut manager = t.manager();
        // Scanned once, before the rows are mutated: the index reads sysfs,
        // which the mutations below do not touch.
        let ports = Tree::port_index(&manager);
        let points = analyze(&manager, &ports, Basis::Link);
        assert_eq!(
            summary(&points),
            vec![("1-1", 384.0, 768.0, 2.0, 3)],
            "the unknown one counts as a device but asks nothing"
        );
        assert_eq!(
            points[0].top.len(),
            2,
            "a zero contributor is not in the top list"
        );
        // Internal devices are traffic sources too.
        for device in manager.buses.get_mut(&1).unwrap().devices.values_mut() {
            device.is_internal = true;
        }
        assert_eq!(analyze(&manager, &ports, Basis::Link).len(), 1);
        // A device the manager lists as disconnected is out of the sum.
        manager
            .buses
            .get_mut(&1)
            .unwrap()
            .devices
            .get_mut(&3)
            .unwrap()
            .is_disconnected = true;
        assert!(
            analyze(&manager, &ports, Basis::Link).is_empty(),
            "one 480 device left: 1.0x"
        );
    }

    #[test]
    fn the_capability_basis_moves_both_sides() {
        // A 10G hub linked at 10G with one 10G device linked at 5G: link
        // 0.5x, capability 1.0x, neither listed.
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "10000", Some(SSP), 4);
        t.device("2-1.1", "5000", Some("3.20"), Some(SSP));
        assert!(choke(&t, Basis::Link).is_empty());
        assert!(choke(&t, Basis::Capability).is_empty());
        // Two of them: link 1.0x (not listed), capability 2.0x (listed).
        t.device("2-1.2", "5000", Some("3.20"), Some(SSP));
        assert!(choke(&t, Basis::Link).is_empty());
        assert_eq!(
            summary(&choke(&t, Basis::Capability)),
            vec![("2-1", 8500.0, 17000.0, 2.0, 2)]
        );
    }

    #[test]
    fn a_leaf_is_bounded_by_every_hub_above_it_at_the_capability_basis() {
        // The adapter shape: a 10G device on a USB 2 half stays bounded to
        // 480 by that half, and the half never gains SuperSpeed capacity.
        let t = Tree::new();
        root(&t, 5, "480", 1);
        hub(&t, "5-1", "480", None, 2);
        t.device("5-1.1", "480", Some("2.10"), Some(SSP));
        t.device("5-1.2", "12", Some("2.01"), None);
        assert!(choke(&t, Basis::Link).is_empty(), "1.03x at link");
        assert!(
            choke(&t, Basis::Capability).is_empty(),
            "still 1.03x at capability: bounded"
        );
        // A USB 2 half that advertises SuperSpeed (its other half's) keeps 480.
        let t = Tree::new();
        root(&t, 3, "480", 1);
        hub(&t, "3-1", "480", Some(SS), 2);
        t.device("3-1.1", "480", Some("2.10"), Some(SS));
        t.device("3-1.2", "480", Some("2.10"), Some(SS));
        assert_eq!(
            summary(&choke(&t, Basis::Capability)),
            vec![("3-1", 384.0, 768.0, 2.0, 2)]
        );
    }

    #[test]
    fn a_ten_gig_hub_linked_at_five_counts_as_ten_at_the_capability_basis() {
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "5000", Some(SSP), 4);
        t.device("2-1.1", "5000", Some("3.20"), Some(SSP));
        t.device("2-1.2", "5000", Some("3.20"), Some(SSP));
        assert_eq!(
            summary(&choke(&t, Basis::Link)),
            vec![("2-1", 4250.0, 8500.0, 2.0, 2)]
        );
        assert_eq!(
            summary(&choke(&t, Basis::Capability)),
            vec![("2-1", 8500.0, 17000.0, 2.0, 2)]
        );
    }

    #[test]
    fn a_hub_never_gains_more_capacity_than_the_port_above_it() {
        // The same 10G hub linked at 5G, now under a 5G root port: its link
        // can never come up at 10G there, so the capability basis keeps it
        // at 5G and the two 5G devices below still choke it at 2.0x.
        let t = Tree::new();
        root(&t, 2, "5000", 1);
        hub(&t, "2-1", "5000", Some(SSP), 4);
        t.device("2-1.1", "5000", Some("3.20"), Some(SSP));
        t.device("2-1.2", "5000", Some("3.20"), Some(SSP));
        let expected = vec![("2-1", 4250.0, 8500.0, 2.0, 2)];
        assert_eq!(summary(&choke(&t, Basis::Link)), expected);
        assert_eq!(summary(&choke(&t, Basis::Capability)), expected);
        // The bound passes through a hub in between: a 5G hub on a 10G root
        // port caps the 10G hub below it the way the root port did above.
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "5000", Some(SS), 4);
        hub(&t, "2-1.1", "5000", Some(SSP), 4);
        t.device("2-1.1.1", "5000", Some("3.20"), Some(SSP));
        t.device("2-1.1.2", "5000", Some("3.20"), Some(SSP));
        assert_eq!(
            summary(&choke(&t, Basis::Capability)),
            vec![
                ("2-1", 4250.0, 8500.0, 2.0, 3),
                ("2-1.1", 4250.0, 8500.0, 2.0, 2)
            ]
        );
    }

    #[test]
    fn an_unknown_link_asks_nothing_and_bounds_nothing_at_the_capability_basis() {
        // A leaf whose link is unknown but whose BOS is readable is not a
        // traffic source: nothing has been negotiated for it to push.
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "5000", Some(SS), 4);
        t.device("2-1.1", "5000", Some("3.00"), Some(SS));
        t.device("2-1.2", "0", Some("3.00"), Some(SS)); // rate unknown
        assert!(
            choke(&t, Basis::Capability).is_empty(),
            "one 5G device: 1.0x"
        );
        // A hub whose link is unknown has no capacity of its own to be a
        // stage, and it bounds nothing: the hubs above it still weigh the
        // demand that crosses them.
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "5000", Some(SS), 4);
        hub(&t, "2-1.1", "0", Some(SS), 4); // rate unknown
        t.device("2-1.1.1", "5000", Some("3.00"), Some(SS));
        t.device("2-1.1.2", "5000", Some("3.00"), Some(SS));
        let expected = vec![("2-1", 4250.0, 8500.0, 2.0, 3)];
        assert_eq!(summary(&choke(&t, Basis::Link)), expected);
        assert_eq!(summary(&choke(&t, Basis::Capability)), expected);
    }

    #[test]
    fn a_bcd_usb_floor_leaf_linked_above_it_keeps_its_link_at_the_capability_basis() {
        // No BOS, so the capability is the bcdUSB 3.x floor of 5 Gb/s --
        // below the 10 Gb/s these two are already linked at. The capability
        // basis must not read that floor as a ceiling.
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "10000", Some(SSP), 4);
        t.device("2-1.1", "10000", Some("3.10"), None);
        t.device("2-1.2", "10000", Some("3.10"), None);
        assert_eq!(
            summary(&choke(&t, Basis::Link)),
            vec![("2-1", 8500.0, 17000.0, 2.0, 2)]
        );
        assert_eq!(
            summary(&choke(&t, Basis::Capability)),
            vec![("2-1", 8500.0, 17000.0, 2.0, 2)],
            "the 5G floor never lowers a device already linked at 10G"
        );
    }

    #[test]
    fn top_three_of_five_contributors_by_demand_then_path() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 5);
        t.device("1-1.1", "12", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        t.device("1-1.3", "1.5", Some("2.00"), None);
        t.device("1-1.4", "12", Some("2.00"), None);
        t.device("1-1.5", "480", Some("2.00"), None);
        let points = choke(&t, Basis::Link);
        let top: Vec<&str> = points[0].top.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(top, ["1-1.2", "1-1.5", "1-1.1"]);
    }

    #[test]
    fn worst_first_with_the_path_as_the_tie_break() {
        let t = Tree::new();
        root(&t, 1, "480", 3);
        for (name, kids) in [("1-2", 2), ("1-1", 2), ("1-3", 3)] {
            hub(&t, name, "480", None, kids);
            for n in 1..=kids {
                t.device(&format!("{name}.{n}"), "480", Some("2.00"), None);
            }
        }
        let points = choke(&t, Basis::Link);
        let order: Vec<&str> = points.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(order, ["1-3", "1-1", "1-2"]);
    }

    #[test]
    fn basis_names_and_toggle() {
        assert_eq!(Basis::Link.as_str(), "link");
        assert_eq!(Basis::Capability.as_str(), "capability");
        assert_eq!(Basis::Link.toggled(), Basis::Capability);
        assert_eq!(Basis::Capability.toggled(), Basis::Link);
        assert_eq!(Basis::default(), Basis::Link);
    }
}

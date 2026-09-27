//! The PCIe side of every Thunderbolt or USB4 tunnel, read from sysfs for
//! the device table and the reports: which removable PCI functions sit
//! under which root port, what each is (class, driver, IDs, interface) and
//! how it is linked, and, when the pairing cannot be wrong, the router the
//! tunnel runs through. The rules and their kernel citations are in
//! `docs/superpowers/specs/2026-09-15-tunneled-pcie-rows-design.md`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::pci;

/// `/sys/bus/pci/devices`; `diag::support::Roots::live` uses the same.
pub const PCI_DEVICES: &str = "/sys/bus/pci/devices";
/// `/sys/bus/thunderbolt/devices`; likewise.
pub const THUNDERBOLT_DEVICES: &str = "/sys/bus/thunderbolt/devices";

/// The heading's label for a tunnel whose router is not named.
pub const NO_ROUTER_LABEL: &str = "external PCIe port";

/// A depth-one router on the Thunderbolt bus, as sysfs describes it
/// (Documentation/ABI/testing/sysfs-bus-thunderbolt).
#[derive(Debug, Clone, PartialEq)]
pub struct Router {
    /// `0-3`: domain, then the route in hex.
    pub name: String,
    pub vendor_name: Option<String>,
    pub device_name: Option<String>,
    pub generation: Option<u32>,
    /// `rx_speed`, `20.0 Gb/s` read as 20.0: the rate per lane.
    pub rx_gbps: Option<f64>,
    pub tx_gbps: Option<f64>,
    /// 1, 2 or 3 (3 being the wider side of an asymmetric link).
    pub rx_lanes: Option<u32>,
    pub tx_lanes: Option<u32>,
    /// The router's `authorized`: 0 means no PCIe devices reach the host.
    pub authorized: Option<u32>,
    /// Its domain's `security`: `dponly`, `usbonly` and `nopcie` create no
    /// PCIe tunnel.
    pub security: Option<String>,
}

impl Router {
    /// Whether a PCIe tunnel can run through this router: `security` is
    /// `none`, `user` or `secure` and `authorized` is 1 or 2.
    pub fn can_tunnel_pcie(&self) -> bool {
        matches!(self.security.as_deref(), Some("none" | "user" | "secure"))
            && matches!(self.authorized, Some(1 | 2))
    }
}

/// A function's negotiated link, read only while it was awake.
#[derive(Debug, Clone, PartialEq)]
pub struct PciLink {
    /// `current_link_speed`, `8.0 GT/s PCIe` read as 8.0.
    pub gts: f64,
    /// `current_link_width`; never 0 (a link of no lanes is down, so it
    /// is left unread).
    pub width: u32,
}

/// One removable, non-bridge PCI function under a tunnel's root port.
#[derive(Debug, Clone, PartialEq)]
pub struct PciFunction {
    pub address: String,
    /// The 24-bit class code, `0x020000`.
    pub class: u32,
    /// The class table's name, or `class 0x......` when it has none.
    pub class_name: String,
    pub vendor_id: u16,
    pub device_id: u16,
    /// The basename of the `driver` link, when bound.
    pub driver: Option<String>,
    /// The one entry of `net/`, when there is exactly one; read whatever
    /// the power state.
    pub interface: Option<String>,
    /// `power/runtime_status` as read; `None` when absent.
    pub runtime_status: Option<String>,
    /// `runtime_status` is `active`: the link attributes were opened.
    pub awake: bool,
    /// Only when awake and both attributes parsed.
    pub link: Option<PciLink>,
    /// `max_link_speed`, only when awake (see `pci::runtime_status`).
    pub max_gts: Option<f64>,
}

impl PciFunction {
    /// `class >> 8 == 0x0c03`, except the USB4 host interface `0x0c0340`
    /// and the USB device controller `0x0c03fe`, which host no bus.
    pub fn is_usb_controller(&self) -> bool {
        self.class >> 8 == 0x0c03 && !matches!(self.class & 0xff, 0x40 | 0xfe)
    }
}

/// One tunnel: a root port with tunneled functions under it, and its
/// router when the join names one.
#[derive(Debug, Clone, PartialEq)]
pub struct Tunnel {
    pub root_port: String,
    pub router: Option<Router>,
    /// Every removable non-bridge function under the root port that is not
    /// a discrete controller's own, USB controllers included, by address.
    /// Never empty.
    pub functions: Vec<PciFunction>,
}

/// A rate without decimals when it is integral and with one otherwise:
/// `8`, `2.5`, `20`.
pub fn format_rate(rate: f64) -> String {
    if rate.fract() == 0.0 {
        format!("{rate:.0}")
    } else {
        format!("{rate:.1}")
    }
}

/// The heading's label for a joined router: the name, then the device name
/// when the DROM has one, then `{lanes}×{gbps} Gb/s` when both are known or
/// `{gbps} Gb/s` when only the rate is. `Thunderbolt 0-3 Element Hub ·
/// 2×20 Gb/s`.
pub fn router_label(
    name: &str,
    device_name: Option<&str>,
    rx_lanes: Option<u32>,
    rx_gbps: Option<f64>,
) -> String {
    let mut label = format!("Thunderbolt {name}");
    if let Some(device_name) = device_name {
        label.push(' ');
        label.push_str(device_name);
    }
    match (rx_lanes, rx_gbps) {
        (Some(lanes), Some(gbps)) => {
            label.push_str(&format!(" · {lanes}×{} Gb/s", format_rate(gbps)))
        }
        (None, Some(gbps)) => label.push_str(&format!(" · {} Gb/s", format_rate(gbps))),
        _ => {}
    }
    label
}

/// The link as a row prints it: `8 GT/s ×1` when it was read, `asleep`
/// when the function is `suspended`, `link unread` otherwise (awake but
/// unparsable, or any other status, `unsupported` included).
pub fn link_text(gts: Option<f64>, width: Option<u32>, runtime_status: Option<&str>) -> String {
    match (gts, width, runtime_status) {
        (Some(gts), Some(width), _) => format!("{} GT/s ×{width}", format_rate(gts)),
        (_, _, Some("suspended")) => "asleep".to_string(),
        _ => "link unread".to_string(),
    }
}

/// The class table: the whole 24-bit code first, then the top two bytes,
/// then the top byte. Short forms of pci.ids' class names; the codes match
/// include/linux/pci_ids.h (see the spec's table).
fn class_name(class: u32) -> Option<&'static str> {
    let exact = match class {
        0x010802 => Some("NVMe"),
        0x0c0340 => Some("USB4 host interface"),
        _ => None,
    };
    let two_bytes = || match class >> 8 {
        0x0106 => Some("SATA controller"),
        0x0108 => Some("non-volatile memory"),
        0x0200 => Some("Ethernet"),
        0x0403 => Some("audio"),
        0x0c00 => Some("FireWire"),
        0x0c03 => Some("USB controller"),
        _ => None,
    };
    let top_byte = || match class >> 16 {
        0x01 => Some("mass storage"),
        0x02 => Some("network"),
        0x03 => Some("display"),
        0x04 => Some("multimedia"),
        0x08 => Some("system peripheral"),
        0x0c => Some("serial bus"),
        0x0d => Some("wireless"),
        0x12 => Some("accelerator"),
        _ => None,
    };
    exact.or_else(two_bytes).or_else(top_byte)
}

/// A sysfs attribute, trimmed; `None` when absent or unreadable.
fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// `0x1d6a` -> 0x1d6a.
fn parse_hex(value: &str) -> Option<u32> {
    let digits = value.trim().strip_prefix("0x")?;
    u32::from_str_radix(digits, 16).ok()
}

/// `8.0 GT/s PCIe` -> 8.0, `20.0 Gb/s` -> 20.0; `Unknown` -> `None`.
fn leading_float(value: &str) -> Option<f64> {
    value.split_whitespace().next()?.parse().ok()
}

/// The basename of the symlink at `path`, when it resolves.
fn link_basename(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

/// Sorted entry names under `dir`; empty when it cannot be listed.
fn sorted_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `0-3` -> the domain when the route (hex) is a depth-one route, 1..=255
/// (`tb_route_length` in drivers/thunderbolt/tb.h: bits above each 8-bit
/// hop); `0-0` is the host router and `0-301` is depth two.
fn depth_one_domain(name: &str) -> Option<u32> {
    let (domain, route) = name.split_once('-')?;
    let domain: u32 = domain.parse().ok()?;
    if route.is_empty() || !route.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let route = u64::from_str_radix(route, 16).ok()?;
    (1..=255).contains(&route).then_some(domain)
}

/// The depth-one routers on the bus, by name with their domain: entries of
/// the `%u-%llx` shape whose `uevent` carries `DEVTYPE=thunderbolt_device`
/// (a host-to-host peer has the same name shape and
/// `DEVTYPE=thunderbolt_xdomain`; retimers, services and `domainN` do not
/// match the shape).
fn router_names(thunderbolt: &Path) -> Vec<(String, u32)> {
    sorted_entries(thunderbolt)
        .into_iter()
        .filter_map(|name| {
            let domain = depth_one_domain(&name)?;
            let uevent = std::fs::read_to_string(thunderbolt.join(&name).join("uevent")).ok()?;
            uevent
                .lines()
                .any(|line| line == "DEVTYPE=thunderbolt_device")
                .then_some((name, domain))
        })
        .collect()
}

/// The router's attributes in the formats sysfs prints them; each `None`
/// when absent or unparsable, and an empty `device_name` or `vendor_name`
/// (a DROM without one) `None` too.
fn read_router(thunderbolt: &Path, name: &str, domain: u32) -> Router {
    let dir = thunderbolt.join(name);
    let text = |attr: &str| read_trimmed(&dir.join(attr)).filter(|s| !s.is_empty());
    let gbps = |attr: &str| text(attr).and_then(|s| leading_float(&s));
    let number = |attr: &str| text(attr).and_then(|s| s.parse::<u32>().ok());
    Router {
        name: name.to_string(),
        vendor_name: text("vendor_name"),
        device_name: text("device_name"),
        generation: number("generation"),
        rx_gbps: gbps("rx_speed"),
        tx_gbps: gbps("tx_speed"),
        rx_lanes: number("rx_lanes"),
        tx_lanes: number("tx_lanes"),
        authorized: number("authorized"),
        security: read_trimmed(&thunderbolt.join(format!("domain{domain}")).join("security"))
            .filter(|s| !s.is_empty()),
    }
}

/// The root ports with a discrete Thunderbolt controller below them: for
/// each `domainN` on the bus, the resolved directory's parent is the host
/// interface's PCI device, and the top of its chain, when it has one, is
/// such a port. An integrated host interface sits on the root bus and has
/// no chain.
fn discrete_ports(thunderbolt: &Path) -> BTreeSet<String> {
    sorted_entries(thunderbolt)
        .into_iter()
        .filter(|name| name.starts_with("domain"))
        .filter_map(|name| std::fs::canonicalize(thunderbolt.join(name)).ok())
        .filter_map(|real| real.parent().map(Path::to_path_buf))
        .filter_map(|nhi| pci::chain(&nhi).into_iter().next())
        .collect()
}

/// Every removable, non-bridge function under `pci` with its root port,
/// in address order, and whether the walk was complete (no function
/// skipped for an unparsable identity). Under a discrete controller's
/// root port, a function whose chain is exactly root port, upstream port,
/// downstream port is the controller's own and is skipped.
fn read_functions(
    pci_root: &Path,
    discrete: &BTreeSet<String>,
) -> (Vec<(String, PciFunction)>, bool) {
    let mut functions = Vec::new();
    let mut complete = true;
    for name in sorted_entries(pci_root) {
        if !pci::is_address(&name) {
            continue;
        }
        let Ok(real) = std::fs::canonicalize(pci_root.join(&name)) else {
            continue;
        };
        if read_trimmed(&real.join("removable")).as_deref() != Some("removable") {
            continue;
        }
        let Some(class) = read_trimmed(&real.join("class")).and_then(|s| parse_hex(&s)) else {
            complete = false;
            continue;
        };
        if class >> 16 == 0x06 {
            continue;
        }
        let chain = pci::chain(&real);
        let Some(root_port) = chain.first().cloned() else {
            continue;
        };
        if discrete.contains(&root_port) && chain.len() == 3 {
            continue;
        }
        let id = |attr: &str| {
            read_trimmed(&real.join(attr))
                .and_then(|s| parse_hex(&s))
                .and_then(|v| u16::try_from(v).ok())
        };
        let (Some(vendor_id), Some(device_id)) = (id("vendor"), id("device")) else {
            complete = false;
            continue;
        };
        let runtime_status = pci::runtime_status(&real);
        let awake = pci::is_awake(runtime_status.as_deref());
        let (link, max_gts) = if awake {
            let gts =
                read_trimmed(&real.join("current_link_speed")).and_then(|s| leading_float(&s));
            let width = read_trimmed(&real.join("current_link_width"))
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|w| *w > 0);
            let link = match (gts, width) {
                (Some(gts), Some(width)) => Some(PciLink { gts, width }),
                _ => None,
            };
            let max_gts =
                read_trimmed(&real.join("max_link_speed")).and_then(|s| leading_float(&s));
            (link, max_gts)
        } else {
            (None, None)
        };
        let mut net = sorted_entries(&real.join("net"));
        let interface = if net.len() == 1 { net.pop() } else { None };
        functions.push((
            root_port,
            PciFunction {
                address: name,
                class,
                class_name: class_name(class)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("class 0x{class:06x}")),
                vendor_id,
                device_id,
                driver: link_basename(&real.join("driver")),
                interface,
                runtime_status,
                awake,
                link,
                max_gts,
            },
        ));
    }
    (functions, complete)
}

/// The join, pure: the one router of `routers` names the one tunnel only
/// when `names_after` (the bus listed again after the PCI walk) holds the
/// same names, there is exactly one router and it can carry PCIe, there is
/// exactly one tunnel, and the walk was complete.
fn join(routers: &[Router], names_after: &[String], tunnel_count: usize, complete: bool) -> bool {
    let unchanged = routers.len() == names_after.len()
        && routers
            .iter()
            .zip(names_after)
            .all(|(router, name)| router.name == *name);
    complete
        && unchanged
        && tunnel_count == 1
        && matches!(routers, [router] if router.can_tunnel_pcie())
}

/// The tunnels under `pci_root` (`/sys/bus/pci/devices`) with the routers
/// under `thunderbolt` (`/sys/bus/thunderbolt/devices`), joined by the rule
/// in the spec's Decisions. Sorted by root port, functions by address. An
/// unreadable `pci_root` yields nothing; an unreadable `thunderbolt` yields
/// tunnels without routers. Nothing here resumes a sleeping device (see
/// `pci::runtime_status`).
pub fn read_tunnels(pci_root: &Path, thunderbolt: &Path) -> Vec<Tunnel> {
    let first = router_names(thunderbolt);
    let routers: Vec<Router> = first
        .iter()
        .map(|(name, domain)| read_router(thunderbolt, name, *domain))
        .collect();
    let discrete = discrete_ports(thunderbolt);
    let (functions, complete) = read_functions(pci_root, &discrete);
    let second: Vec<String> = router_names(thunderbolt)
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    let mut tunnels: Vec<Tunnel> = Vec::new();
    for (root_port, function) in functions {
        match tunnels.iter_mut().find(|t| t.root_port == root_port) {
            Some(tunnel) => tunnel.functions.push(function),
            None => tunnels.push(Tunnel {
                root_port,
                router: None,
                functions: vec![function],
            }),
        }
    }
    tunnels.sort_by(|a, b| a.root_port.cmp(&b.root_port));
    for tunnel in &mut tunnels {
        tunnel.functions.sort_by(|a, b| a.address.cmp(&b.address));
    }
    if join(&routers, &second, tunnels.len(), complete) {
        tunnels[0].router = routers.into_iter().next();
    }
    tunnels
}

/// The bundle's copy of the tunnels: an `interface` of the form `enx` or
/// `wlx` followed by twelve hex digits embeds a MAC (systemd's
/// `NamePolicy=mac`), so it is masked as the bundle masks every other MAC,
/// keeping the three-letter prefix.
pub fn mask_mac_interfaces(tunnels: &mut [Tunnel]) {
    for function in tunnels.iter_mut().flat_map(|t| t.functions.iter_mut()) {
        if let Some(name) = &function.interface {
            if embeds_mac(name) {
                function.interface = Some(format!("{}<redacted>", &name[..3]));
            }
        }
    }
}

fn embeds_mac(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("enx")
        .or_else(|| name.strip_prefix("wlx"))
    else {
        return false;
    };
    rest.len() == 12 && rest.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_tree::PciTree;

    /// The laptop with the dock: root port 07.1, two bridges, the xHCI
    /// awake at 2.5 GT/s x4; router 0-3 authorized on a `none` domain
    /// whose NHI sits on the root bus.
    fn dock(t: &PciTree) {
        t.device(
            &["0000:00:07.1", "0000:2c:00.0"],
            &[
                ("removable", "removable"),
                ("class", "0x060400"),
                ("vendor", "0x8086"),
                ("device", "0x0b26"),
            ],
        );
        t.device(
            &["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0"],
            &[
                ("removable", "removable"),
                ("class", "0x060400"),
                ("vendor", "0x8086"),
                ("device", "0x0b26"),
            ],
        );
        let xhci = t.device(
            &[
                "0000:00:07.1",
                "0000:2c:00.0",
                "0000:2d:00.0",
                "0000:2e:00.0",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x0c0330"),
                ("vendor", "0x8086"),
                ("device", "0x0b27"),
                ("power/runtime_status", "active"),
                ("current_link_speed", "2.5 GT/s PCIe"),
                ("current_link_width", "4"),
                ("max_link_speed", "2.5 GT/s PCIe"),
            ],
        );
        t.driver(&xhci, "xhci_hcd");
        let nhi = t.device(&["0000:00:0d.2"], &[("class", "0x0c0340")]);
        t.domain(0, &nhi, "none");
        t.router(
            "0-3",
            "thunderbolt_device",
            &[
                ("generation", "4"),
                ("rx_speed", "20.0 Gb/s"),
                ("tx_speed", "20.0 Gb/s"),
                ("rx_lanes", "2"),
                ("tx_lanes", "2"),
                ("device_name", "Element Hub"),
                ("vendor_name", "CalDigit, Inc."),
                ("authorized", "1"),
            ],
        );
    }

    /// A NIC at 2d:00.1 under the dock's second bridge, `runtime_status` as
    /// given, with link attributes present whatever the status says.
    fn nic(t: &PciTree, status: &str) -> std::path::PathBuf {
        let nic = t.device(
            &[
                "0000:00:07.1",
                "0000:2c:00.0",
                "0000:2d:00.0",
                "0000:2d:00.1",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x020000"),
                ("vendor", "0x1d6a"),
                ("device", "0x14c0"),
                ("power/runtime_status", status),
                ("current_link_speed", "8.0 GT/s PCIe"),
                ("current_link_width", "1"),
                ("max_link_speed", "16.0 GT/s PCIe"),
            ],
        );
        t.driver(&nic, "atlantic");
        t.net(&nic, "enp45s0");
        nic
    }

    #[test]
    fn functions_group_under_their_root_port_bridges_excluded_and_the_router_joins() {
        let t = PciTree::new();
        dock(&t);
        nic(&t, "active");
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        let tunnel = &tunnels[0];
        assert_eq!(tunnel.root_port, "0000:00:07.1");
        let addresses: Vec<&str> = tunnel
            .functions
            .iter()
            .map(|f| f.address.as_str())
            .collect();
        assert_eq!(addresses, ["0000:2d:00.1", "0000:2e:00.0"]);
        let nic = &tunnel.functions[0];
        assert_eq!(nic.class, 0x020000);
        assert_eq!(nic.class_name, "Ethernet");
        assert_eq!((nic.vendor_id, nic.device_id), (0x1d6a, 0x14c0));
        assert_eq!(nic.driver.as_deref(), Some("atlantic"));
        assert_eq!(nic.interface.as_deref(), Some("enp45s0"));
        assert!(nic.awake);
        assert_eq!(nic.link, Some(PciLink { gts: 8.0, width: 1 }));
        assert_eq!(nic.max_gts, Some(16.0));
        assert!(!nic.is_usb_controller());
        let xhci = &tunnel.functions[1];
        assert!(xhci.is_usb_controller());
        assert_eq!(xhci.link, Some(PciLink { gts: 2.5, width: 4 }));
        let router = tunnel
            .router
            .as_ref()
            .expect("one router, one tunnel: joined");
        assert_eq!(router.name, "0-3");
        assert_eq!(router.device_name.as_deref(), Some("Element Hub"));
        assert_eq!(router.vendor_name.as_deref(), Some("CalDigit, Inc."));
        assert_eq!(
            (router.generation, router.rx_gbps, router.rx_lanes),
            (Some(4), Some(20.0), Some(2))
        );
        assert_eq!(
            (router.authorized, router.security.as_deref()),
            (Some(1), Some("none"))
        );
    }

    #[test]
    fn the_wake_gate_leaves_a_sleeping_functions_link_unread() {
        let t = PciTree::new();
        dock(&t);
        nic(&t, "suspended");
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let nic = &tunnels[0].functions[0];
        assert!(!nic.awake);
        assert_eq!(nic.runtime_status.as_deref(), Some("suspended"));
        // The attributes hold valid values: a reader that opened them could
        // not produce this.
        assert_eq!(nic.link, None);
        assert_eq!(nic.max_gts, None);
        assert_eq!(
            nic.interface.as_deref(),
            Some("enp45s0"),
            "read whatever the power state"
        );
        assert_eq!(link_text(None, None, Some("suspended")), "asleep");
        assert_eq!(link_text(None, None, Some("unsupported")), "link unread");
        assert_eq!(link_text(None, None, None), "link unread");
        assert_eq!(link_text(Some(8.0), Some(1), Some("active")), "8 GT/s ×1");
        assert_eq!(link_text(Some(2.5), Some(4), Some("active")), "2.5 GT/s ×4");
    }

    #[test]
    fn a_zero_width_link_is_unread() {
        let t = PciTree::new();
        dock(&t);
        let nic = nic(&t, "active");
        std::fs::write(nic.join("current_link_width"), "0\n").unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let nic = &tunnels[0].functions[0];
        assert!(nic.awake);
        assert_eq!(nic.link, None);
        assert_eq!(nic.max_gts, Some(16.0));
        assert_eq!(link_text(None, None, Some("active")), "link unread");
    }

    #[test]
    fn the_interface_is_the_single_net_entry_or_none() {
        let t = PciTree::new();
        dock(&t);
        let nic = nic(&t, "active");
        t.net(&nic, "enp45s0v1");
        assert_eq!(
            read_tunnels(&t.pci(), &t.thunderbolt())[0].functions[0].interface,
            None
        );
        std::fs::remove_dir_all(nic.join("net")).unwrap();
        std::fs::write(nic.join("net"), "").unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels[0].functions[0].interface, None);
        assert_eq!(tunnels[0].functions.len(), 2, "still a row");
    }

    #[test]
    fn class_names_come_from_three_tiers_or_fall_back_to_the_code() {
        assert_eq!(class_name(0x010802), Some("NVMe"));
        assert_eq!(class_name(0x010801), Some("non-volatile memory"));
        assert_eq!(class_name(0x010601), Some("SATA controller"));
        assert_eq!(class_name(0x018000), Some("mass storage"));
        assert_eq!(class_name(0x020000), Some("Ethernet"));
        assert_eq!(class_name(0x028000), Some("network"));
        assert_eq!(class_name(0x030000), Some("display"));
        assert_eq!(class_name(0x040300), Some("audio"));
        assert_eq!(class_name(0x048000), Some("multimedia"));
        assert_eq!(class_name(0x088000), Some("system peripheral"));
        assert_eq!(class_name(0x0c0010), Some("FireWire"));
        assert_eq!(class_name(0x0c0340), Some("USB4 host interface"));
        assert_eq!(class_name(0x0c0330), Some("USB controller"));
        assert_eq!(class_name(0x0c0500), Some("serial bus"));
        assert_eq!(class_name(0x0d1100), Some("wireless"));
        assert_eq!(class_name(0x120000), Some("accelerator"));
        assert_eq!(class_name(0xff0000), None);
        let t = PciTree::new();
        dock(&t);
        t.device(
            &[
                "0000:00:07.1",
                "0000:2c:00.0",
                "0000:2d:00.0",
                "0000:2d:00.2",
            ],
            &[
                ("removable", "removable"),
                ("class", "0xff0000"),
                ("vendor", "0x1234"),
                ("device", "0x5678"),
            ],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let odd = tunnels[0]
            .functions
            .iter()
            .find(|f| f.address == "0000:2d:00.2")
            .unwrap();
        assert_eq!(odd.class_name, "class 0xff0000");
        assert_eq!(odd.driver, None);
        assert_eq!(odd.runtime_status, None);
        assert_eq!(
            link_text(None, None, odd.runtime_status.as_deref()),
            "link unread"
        );
    }

    #[test]
    fn usb_controllers_are_the_0c03_class_less_the_host_interface_and_the_gadget() {
        let f = |class: u32| PciFunction {
            address: "0000:2e:00.0".into(),
            class,
            class_name: String::new(),
            vendor_id: 0,
            device_id: 0,
            driver: None,
            interface: None,
            runtime_status: None,
            awake: false,
            link: None,
            max_gts: None,
        };
        assert!(f(0x0c0330).is_usb_controller());
        assert!(f(0x0c0320).is_usb_controller());
        assert!(!f(0x0c0340).is_usb_controller(), "the USB4 host interface");
        assert!(
            !f(0x0c03fe).is_usb_controller(),
            "the USB device controller"
        );
        assert!(!f(0x020000).is_usb_controller());
    }

    #[test]
    fn a_discrete_controllers_own_functions_are_not_tunneled() {
        // Root port 1c.4, the chip's switch 05:00.0 / 06:0x.0, its NHI at
        // 07:00.0 and its xHCI at 08:00.0 directly under the switch, a
        // dock's NIC behind the dock's own switch under 06:02.0.
        let t = PciTree::new();
        let switch = &["0000:00:1c.4", "0000:05:00.0"];
        t.device(
            switch,
            &[
                ("removable", "removable"),
                ("class", "0x060400"),
                ("vendor", "0x8086"),
                ("device", "0x15da"),
            ],
        );
        for down in ["0000:06:00.0", "0000:06:01.0", "0000:06:02.0"] {
            t.device(
                &["0000:00:1c.4", "0000:05:00.0", down],
                &[
                    ("removable", "removable"),
                    ("class", "0x060400"),
                    ("vendor", "0x8086"),
                    ("device", "0x15da"),
                ],
            );
        }
        let nhi = t.device(
            &[
                "0000:00:1c.4",
                "0000:05:00.0",
                "0000:06:00.0",
                "0000:07:00.0",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x0c0340"),
                ("vendor", "0x8086"),
                ("device", "0x15d9"),
            ],
        );
        t.device(
            &[
                "0000:00:1c.4",
                "0000:05:00.0",
                "0000:06:01.0",
                "0000:08:00.0",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x0c0330"),
                ("vendor", "0x8086"),
                ("device", "0x15db"),
            ],
        );
        t.device(
            &[
                "0000:00:1c.4",
                "0000:05:00.0",
                "0000:06:02.0",
                "0000:09:00.0",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x060400"),
                ("vendor", "0x8086"),
                ("device", "0x0b26"),
            ],
        );
        t.device(
            &[
                "0000:00:1c.4",
                "0000:05:00.0",
                "0000:06:02.0",
                "0000:09:00.0",
                "0000:0a:01.0",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x060400"),
                ("vendor", "0x8086"),
                ("device", "0x0b26"),
            ],
        );
        t.device(
            &[
                "0000:00:1c.4",
                "0000:05:00.0",
                "0000:06:02.0",
                "0000:09:00.0",
                "0000:0a:01.0",
                "0000:0b:00.0",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x020000"),
                ("vendor", "0x1d6a"),
                ("device", "0x14c0"),
            ],
        );
        t.domain(0, &nhi, "user");
        t.router(
            "0-1",
            "thunderbolt_device",
            &[("authorized", "1"), ("generation", "3")],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].root_port, "0000:00:1c.4");
        let addresses: Vec<&str> = tunnels[0]
            .functions
            .iter()
            .map(|f| f.address.as_str())
            .collect();
        assert_eq!(
            addresses,
            ["0000:0b:00.0"],
            "the NHI and the chip's xHCI are the host's own"
        );
        assert!(
            tunnels[0].router.is_some(),
            "still one router and one tunnel"
        );
    }

    #[test]
    fn the_join_stays_silent_unless_everything_lines_up() {
        // Two routers.
        let t = PciTree::new();
        dock(&t);
        t.router("0-1", "thunderbolt_device", &[("authorized", "1")]);
        assert!(read_tunnels(&t.pci(), &t.thunderbolt())[0].router.is_none());

        // One router, two tunnels.
        let t = PciTree::new();
        dock(&t);
        t.device(
            &[
                "0000:00:07.0",
                "0000:01:00.0",
                "0000:02:00.0",
                "0000:03:00.0",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x0c0330"),
                ("vendor", "0x8086"),
                ("device", "0x0b27"),
            ],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 2);
        assert!(tunnels.iter().all(|t| t.router.is_none()));
        assert_eq!(tunnels[0].root_port, "0000:00:07.0", "sorted by root port");

        // A router that cannot carry PCIe: unauthorized, or a domain with
        // no PCIe tunneling.
        for (security, authorized) in [
            ("none", "0"),
            ("nopcie", "1"),
            ("usbonly", "1"),
            ("dponly", "1"),
        ] {
            let t = PciTree::new();
            dock(&t);
            std::fs::write(
                t.thunderbolt().join("0-3/authorized"),
                format!("{authorized}\n"),
            )
            .unwrap();
            std::fs::write(
                t.thunderbolt().join("domain0/security"),
                format!("{security}\n"),
            )
            .unwrap();
            assert!(
                read_tunnels(&t.pci(), &t.thunderbolt())[0].router.is_none(),
                "security {security}, authorized {authorized}"
            );
        }

        // A host-to-host peer is not a router, however it is named.
        let t = PciTree::new();
        dock(&t);
        std::fs::remove_dir_all(t.thunderbolt().join("0-3")).unwrap();
        t.router(
            "0-3",
            "thunderbolt_xdomain",
            &[("device_name", "peer-hostname"), ("authorized", "1")],
        );
        assert!(read_tunnels(&t.pci(), &t.thunderbolt())[0].router.is_none());

        // An incomplete walk: a function whose class does not parse.
        let t = PciTree::new();
        dock(&t);
        t.device(
            &[
                "0000:00:07.1",
                "0000:2c:00.0",
                "0000:2d:00.0",
                "0000:2d:00.3",
            ],
            &[
                ("removable", "removable"),
                ("class", "0x"),
                ("vendor", "0x1234"),
                ("device", "0x5678"),
            ],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(
            tunnels[0].functions.len(),
            1,
            "the unreadable function is no row"
        );
        assert!(
            tunnels[0].router.is_none(),
            "and a half-read tree decides no join"
        );

        // Router lists that differ between the two passes, in `join` itself.
        let router = Router {
            name: "0-3".into(),
            vendor_name: None,
            device_name: None,
            generation: None,
            rx_gbps: None,
            tx_gbps: None,
            rx_lanes: None,
            tx_lanes: None,
            authorized: Some(1),
            security: Some("none".into()),
        };
        let one = std::slice::from_ref(&router);
        assert!(join(one, &["0-3".to_string()], 1, true));
        assert!(!join(one, &["0-1".to_string()], 1, true));
        assert!(!join(one, &[], 1, true));
        assert!(!join(one, &["0-3".to_string()], 2, true));
        assert!(!join(one, &["0-3".to_string()], 1, false));
        assert!(!join(&[], &[], 1, true));
    }

    #[test]
    fn only_depth_one_routers_count_and_only_bridges_make_no_tunnel() {
        let t = PciTree::new();
        dock(&t);
        // Ignored by name shape or depth: the host router, a depth-two
        // router, a retimer, a service, the domain itself.
        t.router("0-0", "thunderbolt_device", &[("authorized", "1")]);
        t.router("0-301", "thunderbolt_device", &[("authorized", "1")]);
        t.router("0-0:1.1", "thunderbolt_device", &[]);
        t.router("0-3.1", "thunderbolt_device", &[]);
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        assert_eq!(
            tunnels[0].router.as_ref().map(|r| r.name.as_str()),
            Some("0-3")
        );

        // A second root port with only removable bridges below it is no
        // tunnel and does not block the join.
        t.device(
            &["0000:00:07.0", "0000:01:00.0"],
            &[
                ("removable", "removable"),
                ("class", "0x060400"),
                ("vendor", "0x8086"),
                ("device", "0x0b26"),
            ],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        assert!(tunnels[0].router.is_some());
    }

    #[test]
    fn parse_failures_leave_fields_empty_and_names_empty_are_none() {
        let t = PciTree::new();
        dock(&t);
        let nic = nic(&t, "active");
        std::fs::write(nic.join("current_link_speed"), "Unknown\n").unwrap();
        std::fs::write(nic.join("max_link_speed"), "Unknown\n").unwrap();
        std::fs::write(t.thunderbolt().join("0-3/device_name"), "\n").unwrap();
        std::fs::write(t.thunderbolt().join("0-3/rx_lanes"), "many\n").unwrap();
        std::fs::remove_file(t.thunderbolt().join("0-3/tx_speed")).unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let nic = &tunnels[0].functions[0];
        assert!(nic.awake);
        assert_eq!(nic.link, None);
        assert_eq!(nic.max_gts, None);
        let router = tunnels[0].router.as_ref().unwrap();
        assert_eq!(router.device_name, None);
        assert_eq!(router.rx_lanes, None);
        assert_eq!(router.tx_gbps, None);
        assert_eq!(router.rx_gbps, Some(20.0));
    }

    #[test]
    fn an_unreadable_removable_is_skipped() {
        let t = PciTree::new();
        dock(&t);
        let odd = t.device(
            &[
                "0000:00:07.0",
                "0000:01:00.0",
                "0000:02:00.0",
                "0000:03:00.0",
            ],
            &[
                ("class", "0x0c0330"),
                ("vendor", "0x8086"),
                ("device", "0x0b27"),
            ],
        );
        std::fs::create_dir_all(odd.join("removable")).unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(
            tunnels.len(),
            1,
            "a `removable` that cannot be read counts for nothing"
        );
        assert!(tunnels[0].router.is_some());
    }

    #[test]
    fn unreadable_roots_give_nothing_or_no_routers() {
        let t = PciTree::new();
        dock(&t);
        assert!(read_tunnels(Path::new("/nonexistent/pci"), &t.thunderbolt()).is_empty());
        let tunnels = read_tunnels(&t.pci(), Path::new("/nonexistent/thunderbolt"));
        assert_eq!(tunnels.len(), 1);
        assert!(tunnels[0].router.is_none());
    }

    #[test]
    fn mask_mac_interfaces_masks_only_names_that_embed_a_mac() {
        let function = |interface: Option<&str>| PciFunction {
            address: "0000:2d:00.1".into(),
            class: 0x020000,
            class_name: "Ethernet".into(),
            vendor_id: 0x1d6a,
            device_id: 0x14c0,
            driver: None,
            interface: interface.map(str::to_string),
            runtime_status: None,
            awake: false,
            link: None,
            max_gts: None,
        };
        let mut tunnels = vec![Tunnel {
            root_port: "0000:00:07.1".into(),
            router: None,
            functions: vec![
                function(Some("enx001122334455")),
                function(Some("wlxAABBCCDDEEFF")),
                function(Some("enp45s0")),
                function(Some("enx00112233445")),
                function(None),
            ],
        }];
        mask_mac_interfaces(&mut tunnels);
        let names: Vec<Option<&str>> = tunnels[0]
            .functions
            .iter()
            .map(|f| f.interface.as_deref())
            .collect();
        assert_eq!(
            names,
            [
                Some("enx<redacted>"),
                Some("wlx<redacted>"),
                Some("enp45s0"),
                Some("enx00112233445"),
                None
            ]
        );
    }

    #[test]
    fn the_label_and_the_rates_print_as_the_spec_says() {
        assert_eq!(format_rate(8.0), "8");
        assert_eq!(format_rate(2.5), "2.5");
        assert_eq!(format_rate(20.0), "20");
        assert_eq!(
            router_label("0-3", Some("Element Hub"), Some(2), Some(20.0)),
            "Thunderbolt 0-3 Element Hub · 2×20 Gb/s"
        );
        assert_eq!(
            router_label("0-3", None, None, Some(10.0)),
            "Thunderbolt 0-3 · 10 Gb/s"
        );
        assert_eq!(
            router_label("0-1", Some("Dock"), Some(3), None),
            "Thunderbolt 0-1 Dock"
        );
        assert_eq!(NO_ROUTER_LABEL, "external PCIe port");
    }
}

//! Collector C: the full self-description of every USB controller, hub,
//! dock, and device, read from sysfs and stored verbatim (serial strings
//! included: device identity is what a maintainer needs to tell a clone from
//! the real thing, and the reporter reviews the file list before attaching).
//! Also the raw descriptor blobs and generic attribute dumps of the
//! Thunderbolt and Type-C trees. This is the foundation the device
//! disclosure audit on the roadmap will consume.
//!
//! Kernel semantics, verified against v7.0:
//! - `drivers/usb/core/sysfs.c:855-893` `descriptors_read`: the file is the
//!   device descriptor followed by each configuration's raw descriptors up
//!   to its `wTotalLength`; a read past that returns 0 bytes, so
//!   `std::fs::read` (to EOF) yields the real length even though the
//!   attribute declares `18 + 65535`.
//! - `sysfs.c:895-916` `bos_descriptors_read` returns the BOS block to its
//!   `wTotalLength`; `sysfs.c:927-944` hides the file when the device has
//!   no BOS, so its absence is normal.
//! - `sysfs.c:1105-1121`, `1262-1284`: `iad_*` interface attributes exist
//!   only on an interface inside an interface association.
//! - `drivers/usb/core/port.c:166-227`: `location` (`0x%08x`),
//!   `connect_type` (`hotplug`/`hardwired`/`not used`/`unknown`), `state`,
//!   `over_current_count`, `quirks` (`%08x`); `port.c:484-516` `link_peers`
//!   creates the `peer` symlink between the USB 2 and USB 3 ports of one
//!   connector.
//! - `drivers/usb/core/endpoint.c:47-116`: `bEndpointAddress`,
//!   `bmAttributes`, `bInterval` (`%02x`), `wMaxPacketSize` (`%04x`),
//!   `type` (`Control`/`Isoc`/`Bulk`/`Interrupt`), `direction`
//!   (`both`/`in`/`out`); line 168 names each directory `ep_%02x`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::collect::read_trimmed;
use super::redact::Redactor;
use super::{note, Note};
use crate::usbids::{self, UsbIds};

/// Largest attribute value `dump_attrs` records; longer values are cut.
pub const ATTR_VALUE_CAP: usize = 4096;

#[derive(Debug, Default, Serialize)]
pub struct UsbidsInfo {
    /// The active usb.ids source, home paths rewritten.
    pub source: Option<String>,
    /// Its `# Date:` header as `YYYY-MM-DD`.
    pub date: Option<String>,
}

/// Which usb.ids file name resolution would use, and how old it is.
pub fn usbids_info(chain: &[&Path], redactor: &mut Redactor) -> UsbidsInfo {
    let Some(active) = usbids::active_source(chain) else {
        return UsbidsInfo::default();
    };
    let date = std::fs::read_to_string(active)
        .ok()
        .and_then(|text| usbids::parse_header_date(&text))
        .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"));
    UsbidsInfo {
        source: Some(redactor.path(active)),
        date,
    }
}

#[derive(Debug, Serialize)]
pub struct ControllerInfo {
    /// The controller's sysfs name (`0000:06:00.3`, or a platform id).
    pub name: String,
    pub buses: Vec<u8>,
    pub pci_vendor: Option<String>,
    pub pci_device: Option<String>,
    pub pci_revision: Option<String>,
    pub driver: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EndpointInfo {
    pub name: String,
    pub address: Option<String>,
    pub attributes: Option<String>,
    pub max_packet_size: Option<String>,
    pub interval: Option<String>,
    pub direction: Option<String>,
    /// The kernel's `type` attribute (`kind` because `type` is reserved).
    pub kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IadInfo {
    pub first_interface: Option<String>,
    pub interface_count: Option<String>,
    pub function_class: Option<String>,
    pub function_subclass: Option<String>,
    pub function_protocol: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InterfaceInfo {
    pub name: String,
    pub number: Option<String>,
    pub alt_setting: Option<String>,
    pub class: Option<String>,
    pub subclass: Option<String>,
    pub protocol: Option<String>,
    pub num_endpoints: Option<String>,
    /// The `interface` string attribute.
    pub description: Option<String>,
    pub driver: Option<String>,
    pub iad: Option<IadInfo>,
    pub endpoints: Vec<EndpointInfo>,
}

#[derive(Debug, Serialize)]
pub struct HubPortInfo {
    pub name: String,
    pub connect_type: Option<String>,
    /// The paired port's name (the `peer` link target's last component).
    pub peer: Option<String>,
    /// The Type-C connector's name when the port has a `connector` link.
    pub connector: Option<String>,
    pub location: Option<String>,
    pub state: Option<String>,
    pub over_current_count: Option<String>,
    pub quirks: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UsbDeviceInfo {
    pub port_chain: String,
    pub bus: Option<u8>,
    pub devnum: Option<u8>,
    pub id_vendor: Option<String>,
    pub id_product: Option<String>,
    pub bcd_device: Option<String>,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub vendor_name: Option<String>,
    pub product_name: Option<String>,
    pub device_class: Option<String>,
    pub device_subclass: Option<String>,
    pub device_protocol: Option<String>,
    /// sysfs `version` (bcdUSB).
    pub bcd_usb: Option<String>,
    pub speed: Option<String>,
    pub max_packet_size0: Option<String>,
    pub num_configurations: Option<String>,
    pub configuration_value: Option<String>,
    pub num_interfaces: Option<String>,
    pub bm_attributes: Option<String>,
    pub max_power: Option<String>,
    pub quirks: Option<String>,
    pub avoid_reset_quirk: Option<String>,
    pub ltm_capable: Option<String>,
    pub rx_lanes: Option<String>,
    pub tx_lanes: Option<String>,
    pub maxchild: Option<String>,
    pub urbnum: Option<String>,
    pub authorized: Option<String>,
    pub removable: Option<String>,
    pub physical_location: BTreeMap<String, String>,
    /// `power/control`, `power/autosuspend`, `power/runtime_status`.
    pub power: BTreeMap<String, String>,
    pub interfaces: Vec<InterfaceInfo>,
    pub ports: Vec<HubPortInfo>,
}

#[derive(Debug, Serialize)]
pub struct UsbInventory {
    pub usbids: UsbidsInfo,
    pub controllers: Vec<ControllerInfo>,
    pub devices: Vec<UsbDeviceInfo>,
}

fn attr(dir: &Path, name: &str) -> Option<String> {
    read_trimmed(&dir.join(name))
}

fn link_name(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

fn is_root_hub(name: &str) -> bool {
    name.strip_prefix("usb")
        .is_some_and(|rest| rest.parse::<u8>().is_ok())
}

/// Sorted directory entry names under `dir`; empty when unreadable.
fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn read_endpoints(interface_dir: &Path) -> Vec<EndpointInfo> {
    entry_names(interface_dir)
        .into_iter()
        .filter(|n| n.starts_with("ep_"))
        .map(|name| {
            let dir = interface_dir.join(&name);
            EndpointInfo {
                address: attr(&dir, "bEndpointAddress"),
                attributes: attr(&dir, "bmAttributes"),
                max_packet_size: attr(&dir, "wMaxPacketSize"),
                interval: attr(&dir, "bInterval"),
                direction: attr(&dir, "direction"),
                kind: attr(&dir, "type"),
                name,
            }
        })
        .collect()
}

/// Interface directories are the only entries of a device directory whose
/// name carries a `:` (`3-1:1.0`; a root hub's is `3-0:1.0` under `usb3`).
fn interface_names(device_dir: &Path) -> Vec<String> {
    entry_names(device_dir)
        .into_iter()
        .filter(|n| n.contains(':'))
        .collect()
}

fn read_interfaces(device_dir: &Path) -> Vec<InterfaceInfo> {
    interface_names(device_dir)
        .into_iter()
        .map(|name| {
            let dir = device_dir.join(&name);
            let iad = attr(&dir, "iad_bFirstInterface").map(|first| IadInfo {
                first_interface: Some(first),
                interface_count: attr(&dir, "iad_bInterfaceCount"),
                function_class: attr(&dir, "iad_bFunctionClass"),
                function_subclass: attr(&dir, "iad_bFunctionSubClass"),
                function_protocol: attr(&dir, "iad_bFunctionProtocol"),
            });
            InterfaceInfo {
                number: attr(&dir, "bInterfaceNumber"),
                alt_setting: attr(&dir, "bAlternateSetting"),
                class: attr(&dir, "bInterfaceClass"),
                subclass: attr(&dir, "bInterfaceSubClass"),
                protocol: attr(&dir, "bInterfaceProtocol"),
                num_endpoints: attr(&dir, "bNumEndpoints"),
                description: attr(&dir, "interface"),
                driver: link_name(&dir.join("driver")),
                iad,
                endpoints: read_endpoints(&dir),
                name,
            }
        })
        .collect()
}

/// A hub's ports live under its interface 0 directory as
/// `<device>-port<N>` (`3-1:1.0/3-1-port1`, `usb3/3-0:1.0/usb3-port1`).
fn read_ports(device_dir: &Path, device_name: &str) -> Vec<HubPortInfo> {
    let mut ports = Vec::new();
    let port_prefix = format!("{device_name}-port");
    for interface in interface_names(device_dir) {
        let interface_dir = device_dir.join(&interface);
        for name in entry_names(&interface_dir)
            .into_iter()
            .filter(|n| n.starts_with(&port_prefix))
        {
            let dir = interface_dir.join(&name);
            ports.push(HubPortInfo {
                connect_type: attr(&dir, "connect_type"),
                peer: link_name(&dir.join("peer")),
                connector: link_name(&dir.join("connector")),
                location: attr(&dir, "location"),
                state: attr(&dir, "state"),
                over_current_count: attr(&dir, "over_current_count"),
                quirks: attr(&dir, "quirks"),
                name,
            });
        }
    }
    ports.sort_by_key(|a| port_number(&a.name));
    ports
}

fn port_number(name: &str) -> u32 {
    name.rsplit("port")
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX)
}

fn read_map(dir: &Path) -> BTreeMap<String, String> {
    entry_names(dir)
        .into_iter()
        .filter_map(|name| attr(dir, &name).map(|v| (name, v)))
        .collect()
}

fn read_device(device_dir: &Path, name: &str, usbids: Option<&UsbIds>) -> UsbDeviceInfo {
    let id_vendor = attr(device_dir, "idVendor");
    let id_product = attr(device_dir, "idProduct");
    let vid = id_vendor
        .as_deref()
        .and_then(|v| u16::from_str_radix(v, 16).ok());
    let pid = id_product
        .as_deref()
        .and_then(|p| u16::from_str_radix(p, 16).ok());
    let vendor_name = usbids
        .zip(vid)
        .and_then(|(db, vid)| db.vendor_name(vid).map(str::to_string));
    let product_name = usbids
        .zip(vid)
        .zip(pid)
        .and_then(|((db, vid), pid)| db.product_name(vid, pid).map(str::to_string));
    let power_dir = device_dir.join("power");
    let power = ["control", "autosuspend", "runtime_status"]
        .into_iter()
        .filter_map(|k| attr(&power_dir, k).map(|v| (k.to_string(), v)))
        .collect();
    UsbDeviceInfo {
        port_chain: name.to_string(),
        bus: attr(device_dir, "busnum").and_then(|s| s.parse().ok()),
        devnum: attr(device_dir, "devnum").and_then(|s| s.parse().ok()),
        id_vendor,
        id_product,
        bcd_device: attr(device_dir, "bcdDevice"),
        serial: attr(device_dir, "serial"),
        manufacturer: attr(device_dir, "manufacturer"),
        product: attr(device_dir, "product"),
        vendor_name,
        product_name,
        device_class: attr(device_dir, "bDeviceClass"),
        device_subclass: attr(device_dir, "bDeviceSubClass"),
        device_protocol: attr(device_dir, "bDeviceProtocol"),
        bcd_usb: attr(device_dir, "version"),
        speed: attr(device_dir, "speed"),
        max_packet_size0: attr(device_dir, "bMaxPacketSize0"),
        num_configurations: attr(device_dir, "bNumConfigurations"),
        configuration_value: attr(device_dir, "bConfigurationValue"),
        num_interfaces: attr(device_dir, "bNumInterfaces"),
        bm_attributes: attr(device_dir, "bmAttributes"),
        max_power: attr(device_dir, "bMaxPower"),
        quirks: attr(device_dir, "quirks"),
        avoid_reset_quirk: attr(device_dir, "avoid_reset_quirk"),
        ltm_capable: attr(device_dir, "ltm_capable"),
        rx_lanes: attr(device_dir, "rx_lanes"),
        tx_lanes: attr(device_dir, "tx_lanes"),
        maxchild: attr(device_dir, "maxchild"),
        urbnum: attr(device_dir, "urbnum"),
        authorized: attr(device_dir, "authorized"),
        removable: attr(device_dir, "removable"),
        physical_location: read_map(&device_dir.join("physical_location")),
        power,
        interfaces: read_interfaces(device_dir),
        ports: read_ports(device_dir, name),
    }
}

/// The controller behind a root hub entry: canonicalize `usbN` (through the
/// host's symlink) and take its parent directory.
fn read_controllers(sysfs_devices: &Path, names: &[String]) -> Vec<ControllerInfo> {
    let mut by_name: BTreeMap<String, ControllerInfo> = BTreeMap::new();
    for name in names.iter().filter(|n| is_root_hub(n)) {
        let Ok(real) = std::fs::canonicalize(sysfs_devices.join(name)) else {
            continue;
        };
        let Some(parent) = real.parent() else {
            continue;
        };
        let ctrl_name = parent
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bus = name.strip_prefix("usb").and_then(|n| n.parse::<u8>().ok());
        let entry = by_name
            .entry(ctrl_name.clone())
            .or_insert_with(|| ControllerInfo {
                name: ctrl_name,
                buses: Vec::new(),
                pci_vendor: attr(parent, "vendor"),
                pci_device: attr(parent, "device"),
                pci_revision: attr(parent, "revision"),
                driver: link_name(&parent.join("driver")),
            });
        entry.buses.extend(bus);
        entry.buses.sort_unstable();
    }
    by_name.into_values().collect()
}

/// Every device directory under `sysfs_devices` (interface directories,
/// which carry a `:`, are skipped at the top level and read per device).
pub fn collect_usb_inventory(
    sysfs_devices: &Path,
    usbids: Option<&UsbIds>,
    usbids_info: UsbidsInfo,
) -> (UsbInventory, Vec<Note>) {
    let mut notes = Vec::new();
    if let Err(e) = std::fs::read_dir(sysfs_devices) {
        notes.push(note(
            "sysfs usb devices",
            format!("could not read {}: {e}", sysfs_devices.display()),
        ));
        return (
            UsbInventory {
                usbids: usbids_info,
                controllers: Vec::new(),
                devices: Vec::new(),
            },
            notes,
        );
    }
    let names: Vec<String> = entry_names(sysfs_devices)
        .into_iter()
        .filter(|n| !n.contains(':'))
        .collect();
    let controllers = read_controllers(sysfs_devices, &names);
    let devices = names
        .iter()
        .map(|name| read_device(&sysfs_devices.join(name), name, usbids))
        .collect();
    (
        UsbInventory {
            usbids: usbids_info,
            controllers,
            devices,
        },
        notes,
    )
}

/// The raw `descriptors` and `bos_descriptors` blobs of one device.
#[derive(Debug)]
pub struct DescriptorBlob {
    pub port_chain: String,
    pub descriptors: Vec<u8>,
    pub bos: Option<Vec<u8>>,
}

/// Read every device's descriptor blobs to their real length (see the
/// module doc for why `std::fs::read`, not the declared size). A device
/// without a readable `descriptors` file is noted; a missing
/// `bos_descriptors` is normal (no BOS) and is not.
pub fn read_descriptor_blobs(sysfs_devices: &Path) -> (Vec<DescriptorBlob>, Vec<Note>) {
    let mut blobs = Vec::new();
    let mut notes = Vec::new();
    for name in entry_names(sysfs_devices)
        .into_iter()
        .filter(|n| !n.contains(':'))
    {
        let dir = sysfs_devices.join(&name);
        match std::fs::read(dir.join("descriptors")) {
            Ok(descriptors) => {
                let bos_path = dir.join("bos_descriptors");
                let bos = if bos_path.exists() {
                    match std::fs::read(&bos_path) {
                        Ok(bytes) => Some(bytes),
                        Err(e) => {
                            notes.push(note(
                                &format!("{name}/bos_descriptors"),
                                format!("could not read: {e}"),
                            ));
                            None
                        }
                    }
                } else {
                    None
                };
                blobs.push(DescriptorBlob {
                    port_chain: name,
                    descriptors,
                    bos,
                });
            }
            Err(e) => notes.push(note(
                &format!("{name}/descriptors"),
                format!("could not read: {e}"),
            )),
        }
    }
    (blobs, notes)
}

/// One top-level entry of a class or bus directory and every readable
/// attribute under it, keyed by relative path.
#[derive(Debug, Serialize)]
pub struct AttrDump {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
}

/// Names never read: `nvmem` is a device's firmware image, `key` is a
/// Thunderbolt device's stored authentication secret, `power/` is
/// runtime-PM noise recorded elsewhere for USB devices, and the two links
/// below point out of the tree.
const SKIPPED_DIRS: [&str; 3] = ["power", "subsystem", "firmware_node"];
const SKIPPED_FILES: [&str; 2] = ["nvmem", "key"];

fn walk_attrs(
    base: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    attrs: &mut BTreeMap<String, String>,
    notes: &mut Vec<Note>,
) {
    if depth > max_depth {
        return;
    }
    for name in entry_names(dir) {
        let path = dir.join(&name);
        let rel = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| name.clone());
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            // Sparse by design: almost every device is missing almost every
            // attribute, and noting each absence would flood a non-root run
            // and bury the real signal.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                notes.push(note(
                    &path.display().to_string(),
                    format!("could not read: {e}"),
                ));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            // Inner links (`driver`, `connector`, `device`) lead out of the
            // entry; record where they point, never follow them.
            if let Some(target) = link_name(&path) {
                attrs.insert(rel, format!("-> {target}"));
            }
        } else if meta.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                walk_attrs(base, &path, depth + 1, max_depth, attrs, notes);
            }
        } else if !SKIPPED_FILES.contains(&name.as_str()) {
            // Attribute reads are bounded to the cap; a file that vanished
            // (or was never really there despite the directory listing) is
            // simply not an attribute here, and not worth a note.
            let bytes = match read_capped(&path, ATTR_VALUE_CAP) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    notes.push(note(
                        &path.display().to_string(),
                        format!("could not read: {e}"),
                    ));
                    continue;
                }
            };
            match String::from_utf8(bytes) {
                Ok(text) => {
                    attrs.insert(rel, text.trim().to_string());
                }
                Err(_) => notes.push(note(&path.display().to_string(), "not UTF-8, skipped")),
            }
        }
    }
}

fn read_capped(path: &Path, cap: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(cap.min(4096));
    std::fs::File::open(path)?
        .take(cap as u64)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// Every entry under `root` (a class or bus `devices` directory, whose
/// entries are symlinks into the device tree: those are followed) with its
/// attributes to `max_depth` levels below the entry. Values are trimmed and
/// capped at [`ATTR_VALUE_CAP`] bytes.
pub fn dump_attrs(root: &Path, max_depth: usize) -> (Vec<AttrDump>, Vec<Note>) {
    let mut notes = Vec::new();
    if let Err(e) = std::fs::read_dir(root) {
        notes.push(note(
            &root.display().to_string(),
            format!("could not read: {e}"),
        ));
        return (Vec::new(), notes);
    }
    let mut dumps = Vec::new();
    for name in entry_names(root) {
        let entry = root.join(&name);
        // The entry itself is followed (canonicalized) so `strip_prefix`
        // works on the real tree below it.
        let real = match std::fs::canonicalize(&entry) {
            Ok(real) => real,
            Err(e) => {
                notes.push(note(
                    &entry.display().to_string(),
                    format!("could not resolve: {e}"),
                ));
                continue;
            }
        };
        let mut attrs = BTreeMap::new();
        // Depth counts directories below the entry: the entry itself is 0,
        // so with `max_depth = 3` a file in `a/b/c/` is recorded and one in
        // `a/b/c/d/` is not.
        walk_attrs(&real, &real, 0, max_depth, &mut attrs, &mut notes);
        dumps.push(AttrDump { name, attrs });
    }
    (dumps, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Raw bytes (descriptor blobs); never spelled as string escapes, which
    /// cannot hold bytes above 0x7F and tempt tool JSON into emitting NULs.
    fn write_bytes(dir: &Path, rel: &str, bytes: &[u8]) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// A sysfs tree shaped like the real one: one xHCI controller with a
    /// symlinked root hub `usb3`, a hub `3-1` with two ports (one carrying a
    /// `peer` link), a leaf `3-1.2` with two interfaces (one in an IAD) and
    /// endpoints, and an 18-byte device descriptor blob.
    fn build_tree(root: &Path) -> std::path::PathBuf {
        let devices = root.join("devices");
        let ctrl = root.join("pci/0000:06:00.3");
        write(&ctrl, "vendor", "0x1022\n");
        write(&ctrl, "device", "0x1639\n");
        write(&ctrl, "revision", "0x00\n");
        std::fs::create_dir_all(root.join("drivers/xhci_hcd")).unwrap();
        std::os::unix::fs::symlink(root.join("drivers/xhci_hcd"), ctrl.join("driver")).unwrap();
        let usb3 = ctrl.join("usb3");
        write(&usb3, "busnum", "3\n");
        write(&usb3, "devnum", "1\n");
        write(&usb3, "idVendor", "1d6b\n");
        write(&usb3, "idProduct", "0002\n");
        write(&usb3, "speed", "480\n");
        write(&usb3, "maxchild", "4\n");
        write_bytes(&usb3, "descriptors", &[]);
        std::fs::create_dir_all(&devices).unwrap();
        std::os::unix::fs::symlink(&usb3, devices.join("usb3")).unwrap();

        let hub = devices.join("3-1");
        write(&hub, "busnum", "3\n");
        write(&hub, "devnum", "2\n");
        write(&hub, "idVendor", "05e3\n");
        write(&hub, "idProduct", "0610\n");
        write(&hub, "bcdDevice", "0663\n");
        write(&hub, "speed", "480\n");
        write(&hub, "maxchild", "4\n");
        write(&hub, "bDeviceClass", "09\n");
        write(&hub, "version", " 2.10\n");
        write_bytes(
            &hub,
            "descriptors",
            &[
                0x12, 0x01, 0x10, 0x02, 0x09, 0x00, 0x01, 0x40, 0xe3, 0x05, 0x10, 0x06, 0x63, 0x06,
                0x00, 0x01, 0x00, 0x01,
            ],
        );
        write(&hub, "power/control", "auto\n");
        write(&hub, "power/runtime_status", "active\n");
        let hub_if = hub.join("3-1:1.0");
        write(&hub_if, "bInterfaceNumber", "00\n");
        write(&hub_if, "bInterfaceClass", "09\n");
        write(&hub_if, "bNumEndpoints", "01\n");
        std::fs::create_dir_all(root.join("drivers/hub")).unwrap();
        std::os::unix::fs::symlink(root.join("drivers/hub"), hub_if.join("driver")).unwrap();
        let port1 = hub_if.join("3-1-port1");
        write(&port1, "connect_type", "hotplug\n");
        write(&port1, "location", "0x00000001\n");
        write(&port1, "over_current_count", "0\n");
        write(&port1, "quirks", "00000000\n");
        write(&port1, "state", "configured\n");
        let peer_target = root.join("elsewhere/4-1:1.0/4-1-port1");
        std::fs::create_dir_all(&peer_target).unwrap();
        std::os::unix::fs::symlink(&peer_target, port1.join("peer")).unwrap();
        let port2 = hub_if.join("3-1-port2");
        write(&port2, "connect_type", "not used\n");
        write(&port2, "over_current_count", "0\n");

        let leaf = devices.join("3-1.2");
        write(&leaf, "busnum", "3\n");
        write(&leaf, "devnum", "5\n");
        write(&leaf, "idVendor", "04f2\n");
        write(&leaf, "idProduct", "b71a\n");
        write(&leaf, "serial", "SN0001\n");
        write(&leaf, "manufacturer", "SunplusIT Inc\n");
        write(&leaf, "product", "HD Webcam\n");
        write(&leaf, "speed", "480\n");
        write(&leaf, "bMaxPower", "500mA\n");
        write(&leaf, "bNumInterfaces", " 2\n");
        write(&leaf, "bDeviceClass", "ef\n");
        write(&leaf, "bDeviceSubClass", "02\n");
        write(&leaf, "bDeviceProtocol", "01\n");
        write(&leaf, "bMaxPacketSize0", "64\n");
        write(&leaf, "bNumConfigurations", "1\n");
        write(&leaf, "bConfigurationValue", "1\n");
        write(&leaf, "bmAttributes", "80\n");
        write(&leaf, "quirks", "0x0\n");
        write(&leaf, "avoid_reset_quirk", "0\n");
        write(&leaf, "ltm_capable", "no\n");
        write(&leaf, "rx_lanes", "1\n");
        write(&leaf, "tx_lanes", "1\n");
        write(&leaf, "urbnum", "11268\n");
        write(&leaf, "authorized", "1\n");
        write(&leaf, "removable", "fixed\n");
        write(&leaf, "maxchild", "0\n");
        write_bytes(
            &leaf,
            "descriptors",
            &[
                0x12, 0x01, 0x00, 0x02, 0xef, 0x02, 0x01, 0x40, 0xf2, 0x04, 0x1a, 0xb7, 0x03, 0x00,
                0x01, 0x02, 0x03, 0x01,
            ],
        );
        write_bytes(&leaf, "bos_descriptors", &[0x05, 0x0f, 0x05, 0x00, 0x00]);
        write(&leaf, "physical_location/panel", "front\n");
        write(&leaf, "physical_location/lid", "no\n");
        let if0 = leaf.join("3-1.2:1.0");
        write(&if0, "bInterfaceNumber", "00\n");
        write(&if0, "bAlternateSetting", " 0\n");
        write(&if0, "bInterfaceClass", "0e\n");
        write(&if0, "bInterfaceSubClass", "01\n");
        write(&if0, "bInterfaceProtocol", "01\n");
        write(&if0, "bNumEndpoints", "01\n");
        write(&if0, "interface", "HD Webcam\n");
        write(&if0, "iad_bFirstInterface", "00\n");
        write(&if0, "iad_bInterfaceCount", "02\n");
        write(&if0, "iad_bFunctionClass", "0e\n");
        write(&if0, "iad_bFunctionSubClass", "03\n");
        write(&if0, "iad_bFunctionProtocol", "00\n");
        std::fs::create_dir_all(root.join("drivers/uvcvideo")).unwrap();
        std::os::unix::fs::symlink(root.join("drivers/uvcvideo"), if0.join("driver")).unwrap();
        let ep = if0.join("ep_87");
        write(&ep, "bEndpointAddress", "87\n");
        write(&ep, "bmAttributes", "03\n");
        write(&ep, "wMaxPacketSize", "0010\n");
        write(&ep, "bInterval", "08\n");
        write(&ep, "direction", "in\n");
        write(&ep, "type", "Interrupt\n");
        let if1 = leaf.join("3-1.2:1.1");
        write(&if1, "bInterfaceNumber", "01\n");
        write(&if1, "bInterfaceClass", "0e\n");
        write(&if1, "bInterfaceSubClass", "02\n");
        write(&if1, "bNumEndpoints", "01\n");
        let ep81 = if1.join("ep_81");
        write(&ep81, "bEndpointAddress", "81\n");
        write(&ep81, "bmAttributes", "05\n");
        write(&ep81, "wMaxPacketSize", "0c00\n");
        write(&ep81, "direction", "in\n");
        write(&ep81, "type", "Isoc\n");
        devices
    }

    #[test]
    fn inventory_reads_devices_interfaces_endpoints_ports_and_controllers() {
        let temp = tempfile::tempdir().unwrap();
        let devices = build_tree(temp.path());
        let db = UsbIds::parse("04f2  Chicony Electronics Co., Ltd\n\tb71a  HD WebCam\n05e3  Genesys Logic, Inc.\n\t0610  Hub\n");
        let (inv, notes) = collect_usb_inventory(&devices, Some(&db), UsbidsInfo::default());
        assert!(notes.is_empty(), "{notes:?}");

        assert_eq!(inv.controllers.len(), 1);
        let ctrl = &inv.controllers[0];
        assert_eq!(ctrl.name, "0000:06:00.3");
        assert_eq!(ctrl.buses, vec![3]);
        assert_eq!(ctrl.pci_vendor.as_deref(), Some("0x1022"));
        assert_eq!(ctrl.pci_device.as_deref(), Some("0x1639"));
        assert_eq!(ctrl.pci_revision.as_deref(), Some("0x00"));
        assert_eq!(ctrl.driver.as_deref(), Some("xhci_hcd"));

        let chains: Vec<&str> = inv.devices.iter().map(|d| d.port_chain.as_str()).collect();
        assert_eq!(
            chains,
            vec!["3-1", "3-1.2", "usb3"],
            "sorted by name; no interface dirs"
        );

        let hub = &inv.devices[0];
        assert_eq!(hub.bus, Some(3));
        assert_eq!(hub.devnum, Some(2));
        assert_eq!(hub.bcd_device.as_deref(), Some("0663"));
        assert_eq!(hub.bcd_usb.as_deref(), Some("2.10"));
        assert_eq!(hub.vendor_name.as_deref(), Some("Genesys Logic, Inc."));
        assert_eq!(hub.product_name.as_deref(), Some("Hub"));
        assert_eq!(hub.power.get("control").map(String::as_str), Some("auto"));
        assert_eq!(hub.interfaces.len(), 1);
        assert_eq!(hub.interfaces[0].driver.as_deref(), Some("hub"));
        assert_eq!(hub.ports.len(), 2);
        assert_eq!(hub.ports[0].name, "3-1-port1");
        assert_eq!(hub.ports[0].connect_type.as_deref(), Some("hotplug"));
        assert_eq!(hub.ports[0].peer.as_deref(), Some("4-1-port1"));
        assert_eq!(hub.ports[0].location.as_deref(), Some("0x00000001"));
        assert_eq!(hub.ports[0].state.as_deref(), Some("configured"));
        assert_eq!(hub.ports[1].connect_type.as_deref(), Some("not used"));
        assert_eq!(hub.ports[1].peer, None);
        assert_eq!(hub.maxchild.as_deref(), Some("4"));
        assert_eq!(hub.device_class.as_deref(), Some("09"));
        assert_eq!(hub.id_product.as_deref(), Some("0610"));
        assert_eq!(hub.speed.as_deref(), Some("480"));

        let leaf = &inv.devices[1];
        assert_eq!(
            leaf.serial.as_deref(),
            Some("SN0001"),
            "device identity is kept verbatim"
        );
        assert_eq!(leaf.manufacturer.as_deref(), Some("SunplusIT Inc"));
        assert_eq!(
            leaf.vendor_name.as_deref(),
            Some("Chicony Electronics Co., Ltd")
        );
        assert_eq!(leaf.num_interfaces.as_deref(), Some("2"));
        assert_eq!(leaf.max_power.as_deref(), Some("500mA"));
        assert_eq!(leaf.id_product.as_deref(), Some("b71a"));
        assert_eq!(leaf.product.as_deref(), Some("HD Webcam"));
        assert_eq!(leaf.speed.as_deref(), Some("480"));
        assert_eq!(leaf.device_class.as_deref(), Some("ef"));
        assert_eq!(leaf.device_subclass.as_deref(), Some("02"));
        assert_eq!(leaf.device_protocol.as_deref(), Some("01"));
        assert_eq!(leaf.max_packet_size0.as_deref(), Some("64"));
        assert_eq!(leaf.num_configurations.as_deref(), Some("1"));
        assert_eq!(leaf.configuration_value.as_deref(), Some("1"));
        assert_eq!(leaf.bm_attributes.as_deref(), Some("80"));
        assert_eq!(leaf.quirks.as_deref(), Some("0x0"));
        assert_eq!(leaf.avoid_reset_quirk.as_deref(), Some("0"));
        assert_eq!(leaf.ltm_capable.as_deref(), Some("no"));
        assert_eq!(leaf.rx_lanes.as_deref(), Some("1"));
        assert_eq!(leaf.tx_lanes.as_deref(), Some("1"));
        assert_eq!(leaf.urbnum.as_deref(), Some("11268"));
        assert_eq!(leaf.authorized.as_deref(), Some("1"));
        assert_eq!(leaf.removable.as_deref(), Some("fixed"));
        assert_eq!(leaf.maxchild.as_deref(), Some("0"));
        assert_eq!(
            leaf.physical_location.get("panel").map(String::as_str),
            Some("front")
        );
        assert_eq!(leaf.interfaces.len(), 2);
        let if0 = &leaf.interfaces[0];
        assert_eq!(if0.name, "3-1.2:1.0");
        assert_eq!(if0.class.as_deref(), Some("0e"));
        assert_eq!(if0.description.as_deref(), Some("HD Webcam"));
        assert_eq!(if0.driver.as_deref(), Some("uvcvideo"));
        let iad = if0.iad.as_ref().expect("interface 0 belongs to an IAD");
        assert_eq!(iad.interface_count.as_deref(), Some("02"));
        assert_eq!(iad.function_class.as_deref(), Some("0e"));
        assert_eq!(if0.endpoints.len(), 1);
        assert_eq!(if0.endpoints[0].name, "ep_87");
        assert_eq!(if0.endpoints[0].max_packet_size.as_deref(), Some("0010"));
        assert_eq!(if0.endpoints[0].kind.as_deref(), Some("Interrupt"));
        assert_eq!(if0.endpoints[0].direction.as_deref(), Some("in"));
        let if1 = &leaf.interfaces[1];
        assert!(if1.iad.is_none(), "no iad_* files, no IAD");
        assert_eq!(if1.endpoints[0].kind.as_deref(), Some("Isoc"));

        let root_hub = &inv.devices[2];
        assert_eq!(root_hub.port_chain, "usb3");
        assert_eq!(root_hub.id_vendor.as_deref(), Some("1d6b"));
        assert!(root_hub.serial.is_none());

        let text = toml::to_string(&inv).unwrap();
        assert!(text.contains("[[devices]]"), "{text}");
        assert!(text.contains("[[devices.interfaces]]"), "{text}");
        assert!(text.contains("[[devices.ports]]"), "{text}");
    }

    #[test]
    fn inventory_without_a_usbids_database_leaves_resolved_names_absent() {
        let temp = tempfile::tempdir().unwrap();
        let devices = build_tree(temp.path());
        let (inv, _) = collect_usb_inventory(&devices, None, UsbidsInfo::default());
        assert!(inv
            .devices
            .iter()
            .all(|d| d.vendor_name.is_none() && d.product_name.is_none()));
        assert_eq!(inv.usbids.source, None);
    }

    #[test]
    fn inventory_notes_an_unreadable_root() {
        let temp = tempfile::tempdir().unwrap();
        let (inv, notes) =
            collect_usb_inventory(&temp.path().join("absent"), None, UsbidsInfo::default());
        assert!(inv.devices.is_empty());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].item, "sysfs usb devices");
    }

    #[test]
    fn descriptor_blobs_are_read_to_their_real_length() {
        let temp = tempfile::tempdir().unwrap();
        let devices = build_tree(temp.path());
        let (blobs, notes) = read_descriptor_blobs(&devices);
        assert!(notes.is_empty(), "{notes:?}");
        let chains: Vec<&str> = blobs.iter().map(|b| b.port_chain.as_str()).collect();
        assert_eq!(chains, vec!["3-1", "3-1.2", "usb3"]);
        assert_eq!(blobs[0].descriptors.len(), 18);
        assert_eq!(blobs[0].descriptors[0], 0x12);
        assert!(
            blobs[0].bos.is_none(),
            "no bos_descriptors file: no BOS, not a note"
        );
        assert_eq!(blobs[1].bos.as_deref().map(<[u8]>::len), Some(5));
        assert!(
            blobs[2].descriptors.is_empty(),
            "an empty file is an empty blob"
        );
    }

    #[test]
    fn descriptor_blobs_note_a_device_without_a_descriptors_file() {
        let temp = tempfile::tempdir().unwrap();
        let devices = temp.path().join("devices");
        write(&devices.join("1-1"), "busnum", "1\n");
        let (blobs, notes) = read_descriptor_blobs(&devices);
        assert!(blobs.is_empty());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].item, "1-1/descriptors");
    }

    #[test]
    fn attr_dump_follows_top_level_links_caps_depth_and_records_inner_links_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real/port0");
        write(&real, "data_role", "[host] device\n");
        write(&real, "power_role", "[source] sink\n");
        write(&real, "port0-partner/accessory_mode", "none\n");
        write(&real, "port0-partner/port0-partner.0/mode1/active", "yes\n");
        write(
            &real,
            "port0-partner/port0-partner.0/mode1/deep/too-deep",
            "x\n",
        );
        write(&real, "nvm_active0/nvmem", "binary\n");
        write(&real, "key", "device-auth-secret\n");
        write(&real, "power/control", "auto\n");
        write(&real, "big", &"x".repeat(ATTR_VALUE_CAP + 10));
        std::fs::write(real.join("bytes"), [0xff, 0xfe, 0x00]).unwrap();
        std::fs::create_dir_all(temp.path().join("drivers/typec")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("drivers/typec"), real.join("driver")).unwrap();
        let class = temp.path().join("class/typec");
        std::fs::create_dir_all(&class).unwrap();
        std::os::unix::fs::symlink(&real, class.join("port0")).unwrap();

        let (dumps, notes) = dump_attrs(&class, 3);
        assert_eq!(dumps.len(), 1);
        let d = &dumps[0];
        assert_eq!(d.name, "port0");
        assert_eq!(
            d.attrs.get("data_role").map(String::as_str),
            Some("[host] device")
        );
        assert_eq!(
            d.attrs
                .get("port0-partner/accessory_mode")
                .map(String::as_str),
            Some("none")
        );
        assert_eq!(
            d.attrs
                .get("port0-partner/port0-partner.0/mode1/active")
                .map(String::as_str),
            Some("yes")
        );
        assert!(
            !d.attrs
                .contains_key("port0-partner/port0-partner.0/mode1/deep/too-deep"),
            "depth 4 is past the cap"
        );
        assert!(
            !d.attrs.contains_key("nvm_active0/nvmem"),
            "nvmem is never read"
        );
        assert!(
            !d.attrs.contains_key("key"),
            "key (a stored device-authentication secret) is never read"
        );
        assert!(!d.attrs.contains_key("power/control"), "power/ is skipped");
        assert_eq!(
            d.attrs.get("driver").map(String::as_str),
            Some("-> typec"),
            "inner links are recorded, not followed"
        );
        assert_eq!(d.attrs.get("big").map(String::len), Some(ATTR_VALUE_CAP));
        assert!(
            !d.attrs.contains_key("bytes"),
            "non-UTF-8 values are skipped"
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].item.ends_with("port0/bytes"));
    }

    #[test]
    fn attr_dump_of_a_missing_root_is_empty_with_one_note() {
        let temp = tempfile::tempdir().unwrap();
        let (dumps, notes) = dump_attrs(&temp.path().join("thunderbolt"), 2);
        assert!(dumps.is_empty());
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn attr_dump_notes_an_entry_it_cannot_resolve() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real/port0");
        write(&real, "data_role", "[host] device\n");
        std::fs::create_dir_all(temp.path().join("drivers/typec")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("drivers/typec"), real.join("driver")).unwrap();
        let class = temp.path().join("class/typec");
        std::fs::create_dir_all(&class).unwrap();
        // Valid entry that should be dumped
        std::os::unix::fs::symlink(&real, class.join("port0")).unwrap();
        // Dangling symlink that cannot be resolved
        std::os::unix::fs::symlink(temp.path().join("gone"), class.join("port9")).unwrap();

        let (dumps, notes) = dump_attrs(&class, 2);
        assert_eq!(dumps.len(), 1, "valid entry is dumped");
        assert_eq!(dumps[0].name, "port0");
        assert_eq!(
            dumps[0].attrs.get("data_role").map(String::as_str),
            Some("[host] device")
        );
        assert_eq!(notes.len(), 1, "one note for the dangling symlink");
        assert!(notes[0].item.ends_with("port9"));
        assert!(notes[0].reason.starts_with("could not resolve: "));
    }

    /// Attributes are sparse by design: a device tree only ever has a small
    /// subset of the attribute files another device's tree might have. An
    /// attribute that simply never existed on disk must never generate a
    /// note; only the walk never visits it, since `entry_names` lists real
    /// directory entries and nothing else.
    #[test]
    fn attr_dump_of_a_sparse_tree_notes_nothing_for_the_missing_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real/port0");
        write(&real, "data_role", "[host] device\n");
        // `power_role`, `preferred_role`, and `usb_power_delivery` are all
        // real Type-C attributes this port simply does not have; none of
        // them exist as files, so none of them can be read at all.
        let class = temp.path().join("class/typec");
        std::fs::create_dir_all(&class).unwrap();
        std::os::unix::fs::symlink(&real, class.join("port0")).unwrap();

        let (dumps, notes) = dump_attrs(&class, 2);
        assert_eq!(dumps.len(), 1);
        assert_eq!(
            dumps[0].attrs.get("data_role").map(String::as_str),
            Some("[host] device")
        );
        assert!(!dumps[0].attrs.contains_key("power_role"));
        assert!(!dumps[0].attrs.contains_key("preferred_role"));
        assert!(!dumps[0].attrs.contains_key("usb_power_delivery"));
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// A present-but-unreadable attribute (permission denied) is a real
    /// diagnostic event, unlike a merely absent one: it must surface as a
    /// note naming the path, and the attribute stays out of the dump. Root
    /// bypasses file permissions and can read a 0000 file, so this check
    /// cannot be made to fail as root; skip it there.
    #[test]
    fn attr_dump_notes_a_permission_denied_attribute_and_leaves_it_out_of_the_dump() {
        // SAFETY: geteuid() takes no arguments, touches no memory, and
        // cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real/port0");
        write(&real, "data_role", "[host] device\n");
        let locked = real.join("power_role");
        std::fs::write(&locked, "[source] sink\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let class = temp.path().join("class/typec");
        std::fs::create_dir_all(&class).unwrap();
        std::os::unix::fs::symlink(&real, class.join("port0")).unwrap();

        let (dumps, notes) = dump_attrs(&class, 2);
        assert_eq!(dumps.len(), 1);
        assert_eq!(
            dumps[0].attrs.get("data_role").map(String::as_str),
            Some("[host] device"),
            "the readable sibling attribute is unaffected"
        );
        assert!(
            !dumps[0].attrs.contains_key("power_role"),
            "an unreadable attribute is never recorded as a value"
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].item.ends_with("port0/power_role"));
        assert!(notes[0].reason.starts_with("could not read: "));
    }

    #[test]
    fn usbids_info_names_the_active_source_and_its_date() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home/alice");
        let ids = home.join(".usbtop-ng/usb.ids");
        write(
            &home,
            ".usbtop-ng/usb.ids",
            "# Date:\t2026-08-30 20:34:02\n1d6b  Linux Foundation\n",
        );
        let missing = temp.path().join("missing.ids");
        let mut r = Redactor::new(Some(home.as_path()));
        let info = usbids_info(&[missing.as_path(), ids.as_path()], &mut r);
        assert_eq!(info.source.as_deref(), Some("~/.usbtop-ng/usb.ids"));
        assert_eq!(info.date.as_deref(), Some("2026-08-30"));
        let none = usbids_info(&[missing.as_path()], &mut r);
        assert_eq!(none.source, None);
        assert_eq!(none.date, None);
    }
}

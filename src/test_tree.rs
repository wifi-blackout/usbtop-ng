//! A fake `/sys/bus/usb/devices` for the engine tests: root hubs as
//! symlinks into a controller directory, devices as directories with
//! `busnum`, `devnum`, `speed`, optional `version`, `bos_descriptors` and
//! `idVendor`, hub ports under `<hub>/<hub>:1.0/<hub>-port<N>/` with `peer`
//! links and an optional `location`. Shared by the findings and capacity
//! tests; read back through the real `DeviceManager` and `PortIndex`.

use std::path::{Path, PathBuf};

use crate::connector::{parse_device_name, port_name, PortIndex};
use crate::device::manager::DeviceManager;

/// SuperSpeed only: 5 Gb/s (the camera's shape).
pub(crate) const SS: &[u8] = &[
    0x05, 0x0f, 0x16, 0x00, 0x02, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10, 0x03, 0x00,
    0x0c, 0x00, 0x03, 0x0a, 0xff, 0x07,
];
/// SuperSpeed plus SuperSpeedPlus at 10 Gb/s (the adapter's shape).
pub(crate) const SSP: &[u8] = &[
    0x05, 0x0f, 0x2a, 0x00, 0x03, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10, 0x03, 0x00,
    0x0e, 0x00, 0x03, 0x0a, 0xff, 0x07, 0x14, 0x10, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x11,
    0x00, 0x00, 0x30, 0x40, 0x0a, 0x00, 0xb0, 0x40, 0x0a, 0x00,
];

pub(crate) struct Tree {
    root: tempfile::TempDir,
    next_devnum: std::cell::Cell<u8>,
}

impl Tree {
    pub(crate) fn new() -> Tree {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("devices")).unwrap();
        Tree {
            root,
            next_devnum: std::cell::Cell::new(2),
        }
    }

    pub(crate) fn base(&self) -> PathBuf {
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
    pub(crate) fn root_hub(&self, bus: u8, speed: &str) -> PathBuf {
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
    pub(crate) fn device(
        &self,
        name: &str,
        speed: &str,
        version: Option<&str>,
        bos: Option<&[u8]>,
    ) -> PathBuf {
        let (bus, _) = parse_device_name(name).unwrap();
        let devnum = self.next_devnum.get();
        self.next_devnum.set(devnum + 1);
        let dir = self.base().join(name);
        Self::write(&dir, bus, devnum, speed, version, bos);
        dir
    }

    /// A device that also states an `idVendor`, for the vendor
    /// agreement rule inside rule H's elimination match.
    pub(crate) fn device_of_vendor(
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

    pub(crate) fn port(&self, hub_dir: &Path, hub: &str, number: u32) -> PathBuf {
        let dir = hub_dir
            .join(format!("{hub}:1.0"))
            .join(port_name(hub, number));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn pair(&self, a: &Path, b: &Path) {
        std::os::unix::fs::symlink(b, a.join("peer")).unwrap();
        std::os::unix::fs::symlink(a, b.join("peer")).unwrap();
    }

    /// The firmware's ACPI position of a port, as sysfs prints it; a
    /// nonzero value marks a pairing the kernel made by location.
    pub(crate) fn locate(&self, port_dir: &Path, location: u32) {
        std::fs::write(port_dir.join("location"), format!("0x{location:08x}\n")).unwrap();
    }

    /// The manager as the live tool builds it: every device enumerated,
    /// bus speeds resolved.
    pub(crate) fn manager(&self) -> DeviceManager {
        let mut manager = DeviceManager::with_sysfs_base(self.base());
        manager.enumerate_present_devices();
        manager.update_bus_speeds();
        manager
    }

    /// The connector index over the manager's devices, as `ui::sync_from`
    /// and `headless::build_report` build it.
    pub(crate) fn port_index(manager: &DeviceManager) -> PortIndex {
        PortIndex::scan_devices(
            manager
                .buses
                .values()
                .flat_map(|bus| bus.devices.values())
                .filter_map(|device| device.sysfs_path.as_deref()),
        )
    }
}

/// A fake `/sys/bus/pci/devices` and `/sys/bus/thunderbolt/devices` for the
/// tunnel tests: device directories nested under `sys/devices/pci0000:00`
/// in their parent chain and symlinked from the flat `pci/` directory, so
/// `canonicalize` and `pci::chain` read them like real sysfs; routers and
/// domains as directories under `thunderbolt/`.
pub(crate) struct PciTree {
    root: tempfile::TempDir,
}

impl PciTree {
    pub(crate) fn new() -> PciTree {
        let root = tempfile::tempdir().unwrap();
        for dir in ["pci", "thunderbolt", "sys/devices/pci0000:00"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        PciTree { root }
    }

    /// The flat devices directory the reader lists.
    pub(crate) fn pci(&self) -> PathBuf {
        self.root.path().join("pci")
    }

    /// The Thunderbolt bus directory the reader lists.
    pub(crate) fn thunderbolt(&self) -> PathBuf {
        self.root.path().join("thunderbolt")
    }

    /// A PCI device: `chain` is root-first and ends with the device's own
    /// address. Its real directory nests under the addresses above it, the
    /// flat `pci/` directory gets a symlink to it, and each of `attrs` is
    /// written as a file with a trailing newline (a name with a `/` makes
    /// its directory, so `power/runtime_status` works). Returns the real
    /// directory.
    pub(crate) fn device(&self, chain: &[&str], attrs: &[(&str, &str)]) -> PathBuf {
        let mut real = self.root.path().join("sys/devices/pci0000:00");
        for address in chain {
            real = real.join(address);
        }
        std::fs::create_dir_all(&real).unwrap();
        let own = chain.last().expect("a chain ends with the device");
        let link = self.pci().join(own);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(&real, &link).unwrap();
        }
        for (name, value) in attrs {
            let path = real.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, format!("{value}\n")).unwrap();
        }
        real
    }

    /// The device's `net/<name>` directory: its network interface.
    pub(crate) fn net(&self, real: &Path, name: &str) {
        std::fs::create_dir_all(real.join("net").join(name)).unwrap();
    }

    /// A `driver` symlink in `real` to a directory named `name`.
    pub(crate) fn driver(&self, real: &Path, name: &str) {
        let target = self.root.path().join("drivers").join(name);
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, real.join("driver")).unwrap();
    }

    /// An entry `name` on the Thunderbolt bus whose `uevent` names
    /// `devtype` (`thunderbolt_device` for a router, `thunderbolt_xdomain`
    /// for a host-to-host peer), plus attrs. Returns the directory.
    pub(crate) fn router(&self, name: &str, devtype: &str, attrs: &[(&str, &str)]) -> PathBuf {
        let dir = self.thunderbolt().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("uevent"), format!("DEVTYPE={devtype}\n")).unwrap();
        for (name, value) in attrs {
            std::fs::write(dir.join(name), format!("{value}\n")).unwrap();
        }
        dir
    }

    /// `domain<n>` on the Thunderbolt bus: a symlink to `<nhi>/domain<n>`,
    /// the way the kernel hangs a domain off its host interface's PCI
    /// device directory, holding `security`.
    pub(crate) fn domain(&self, n: u32, nhi: &Path, security: &str) {
        let real = nhi.join(format!("domain{n}"));
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("security"), format!("{security}\n")).unwrap();
        std::os::unix::fs::symlink(&real, self.thunderbolt().join(format!("domain{n}"))).unwrap();
    }
}

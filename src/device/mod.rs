use std::time::Instant;

use crate::stats::BandwidthStats;
use crate::usbmon::parser::UsbSpeed;

pub mod manager;

#[derive(Debug, Clone)]
pub struct UsbDevice {
    pub bus_id: u8,
    pub device_id: u8,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub speed: UsbSpeed,
    pub bandwidth_stats: BandwidthStats,
    pub is_disconnected: bool,
    pub disconnect_time: Option<Instant>,
    pub last_seen: Instant,
    pub sysfs_path: Option<std::path::PathBuf>,
}

impl UsbDevice {
    pub fn new(bus_id: u8, device_id: u8) -> Self {
        Self {
            bus_id,
            device_id,
            vendor_id: None,
            product_id: None,
            vendor: None,
            product: None,
            serial: None,
            speed: UsbSpeed::Unknown,
            bandwidth_stats: BandwidthStats::new(),
            is_disconnected: false,
            disconnect_time: None,
            last_seen: Instant::now(),
            sysfs_path: None,
        }
    }

    /// Physical port chain from the resolved sysfs name: "3-1.4.2" -> [1,4,2];
    /// a root hub ("usbN") -> empty chain (sorts first); unresolved -> None.
    pub fn port_chain(&self) -> Option<Vec<u32>> {
        let name = self.sysfs_path.as_ref()?.file_name()?.to_str()?;
        if let Some(rest) = name.strip_prefix("usb") {
            return rest.parse::<u8>().ok().map(|_| Vec::new());
        }
        let (_bus, ports) = name.split_once('-')?;
        ports.split('.').map(|p| p.parse::<u32>().ok()).collect()
    }

    /// Populate metadata from sysfs; `base` overrides /sys/bus/usb/devices for tests.
    pub fn populate_from_sysfs(&mut self, base: Option<&std::path::Path>) {
        #[cfg(target_os = "linux")]
        {
            let default = std::path::Path::new("/sys/bus/usb/devices");
            let _ = self.update_linux_device_info_from_base(base.unwrap_or(default));
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = base;
            let _ = self.update_from_sysfs();
        }
    }

    /// Non-Linux fallback dispatcher: on Linux, `populate_from_sysfs` calls
    /// `update_linux_device_info_from_base` directly, so this is unreachable
    /// (and therefore not compiled) on that platform.
    #[cfg(not(target_os = "linux"))]
    pub fn update_from_sysfs(&mut self) -> Result<(), std::io::Error> {
        #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
        {
            self.update_bsd_device_info()
        }

        #[cfg(target_os = "macos")]
        {
            self.update_macos_device_info()
        }
    }

    #[cfg(target_os = "linux")]
    fn update_linux_device_info_from_base(
        &mut self,
        base: &std::path::Path,
    ) -> Result<(), std::io::Error> {
        let Some(sysfs_path) = self.find_linux_sysfs_path(base) else {
            return Ok(());
        };
        self.sysfs_path = Some(sysfs_path.clone());

        // Read device attributes
        if let Ok(speed_str) = std::fs::read_to_string(sysfs_path.join("speed")) {
            self.speed = UsbSpeed::from_speed_str(speed_str.trim());
        }

        if let Ok(vendor_str) = std::fs::read_to_string(sysfs_path.join("idVendor")) {
            if let Ok(vendor_id) = u16::from_str_radix(vendor_str.trim(), 16) {
                self.vendor_id = Some(vendor_id);
            }
        }

        if let Ok(product_str) = std::fs::read_to_string(sysfs_path.join("idProduct")) {
            if let Ok(product_id) = u16::from_str_radix(product_str.trim(), 16) {
                self.product_id = Some(product_id);
            }
        }

        if let Ok(manufacturer) = std::fs::read_to_string(sysfs_path.join("manufacturer")) {
            self.vendor = Some(manufacturer.trim().to_string());
        }

        if let Ok(product) = std::fs::read_to_string(sysfs_path.join("product")) {
            self.product = Some(product.trim().to_string());
        }

        if let Ok(serial) = std::fs::read_to_string(sysfs_path.join("serial")) {
            self.serial = Some(serial.trim().to_string());
        }

        Ok(())
    }

    /// Scan `base` for the sysfs entry whose `busnum`/`devnum` files match
    /// this device. Real sysfs USB device directories are named by port
    /// topology (e.g. `3-1.4`), not by bus/device number, so a name guess
    /// doesn't work; we have to read the attribute files instead.
    #[cfg(target_os = "linux")]
    fn find_linux_sysfs_path(&self, base: &std::path::Path) -> Option<std::path::PathBuf> {
        let entries = std::fs::read_dir(base).ok()?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let is_interface = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.contains(':'));
            if is_interface {
                continue;
            }

            let Ok(busnum_str) = std::fs::read_to_string(path.join("busnum")) else {
                continue;
            };
            let Ok(busnum) = busnum_str.trim().parse::<u8>() else {
                continue;
            };
            if busnum != self.bus_id {
                continue;
            }

            let Ok(devnum_str) = std::fs::read_to_string(path.join("devnum")) else {
                continue;
            };
            let Ok(devnum) = devnum_str.trim().parse::<u8>() else {
                continue;
            };
            if devnum != self.device_id {
                continue;
            }

            return Some(path);
        }
        None
    }

    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    fn update_bsd_device_info(&mut self) -> Result<(), std::io::Error> {
        // For BSD systems, we might use usbconfig or similar utilities
        // This is a placeholder implementation
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn update_macos_device_info(&mut self) -> Result<(), std::io::Error> {
        // For macOS, we might use ioreg or system_profiler
        // This is a placeholder implementation
        Ok(())
    }

    pub fn mark_disconnected(&mut self) {
        if !self.is_disconnected {
            self.is_disconnected = true;
            self.disconnect_time = Some(Instant::now());
        }
    }

    pub fn should_remove(&self) -> bool {
        if let Some(disconnect_time) = self.disconnect_time {
            // Remove after 5 seconds of being disconnected
            disconnect_time.elapsed().as_secs() > 5
        } else {
            false
        }
    }

    pub fn update_activity(&mut self) {
        self.last_seen = Instant::now();
        if self.is_disconnected {
            self.is_disconnected = false;
            self.disconnect_time = None;
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn write_device(dir: &std::path::Path, busnum: u8, devnum: u8, extra: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("busnum"), format!("{busnum}\n")).unwrap();
        std::fs::write(dir.join("devnum"), format!("{devnum}\n")).unwrap();
        for (name, value) in extra {
            std::fs::write(dir.join(name), format!("{value}\n")).unwrap();
        }
    }

    #[test]
    fn resolves_device_by_busnum_devnum_topology() {
        let temp = tempfile::tempdir().unwrap();
        // Root hub: usb1 (busnum 1, devnum 1); device at port path 1-2.4
        write_device(&temp.path().join("usb1"), 1, 1, &[("speed", "480")]);
        write_device(
            &temp.path().join("1-2.4"),
            1,
            5,
            &[
                ("speed", "480"),
                ("idVendor", "1d6b"),
                ("idProduct", "0002"),
                ("manufacturer", "Linux Foundation"),
                ("product", "Root Hub"),
                ("serial", "test-serial"),
            ],
        );
        // Interface directory must be skipped, not matched
        std::fs::create_dir_all(temp.path().join("1-2.4:1.0")).unwrap();

        let mut device = UsbDevice::new(1, 5);
        device.populate_from_sysfs(Some(temp.path()));

        assert_eq!(device.speed, UsbSpeed::High);
        assert_eq!(device.vendor_id, Some(0x1d6b));
        assert_eq!(device.product_id, Some(0x0002));
        assert_eq!(device.vendor.as_deref(), Some("Linux Foundation"));
        assert_eq!(device.product.as_deref(), Some("Root Hub"));
        assert_eq!(device.serial.as_deref(), Some("test-serial"));
        assert_eq!(device.sysfs_path, Some(temp.path().join("1-2.4")));
    }

    #[test]
    fn port_chain_parses_topology_names() {
        let temp = tempfile::tempdir().unwrap();
        let mk = |name: &str| {
            let mut d = UsbDevice::new(3, 2);
            d.sysfs_path = Some(temp.path().join(name));
            d
        };
        assert_eq!(mk("3-1.4.2").port_chain(), Some(vec![1, 4, 2]));
        assert_eq!(mk("3-2").port_chain(), Some(vec![2]));
        assert_eq!(mk("usb3").port_chain(), Some(vec![]));
        assert_eq!(mk("garbage").port_chain(), None);
        assert_eq!(UsbDevice::new(3, 9).port_chain(), None); // no sysfs_path
    }

    #[test]
    fn unmatched_device_resolves_nothing() {
        let temp = tempfile::tempdir().unwrap();
        write_device(&temp.path().join("usb1"), 1, 1, &[("speed", "480")]);

        let mut device = UsbDevice::new(1, 9);
        device.populate_from_sysfs(Some(temp.path()));

        assert_eq!(device.sysfs_path, None);
        assert_eq!(device.vendor, None);
    }
}

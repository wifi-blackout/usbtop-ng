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
    /// Highest speed this device is electrically capable of, independent of
    /// how fast it's actually linked (see `check_speed_mismatch`). Cached at
    /// sysfs read time so mismatch checks never touch the filesystem.
    pub max_capability: Option<UsbSpeed>,
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
            max_capability: None,
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

        self.max_capability = read_max_capability(&sysfs_path);

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

    /// Percentage of this device's practical bandwidth currently in use.
    /// Uses practical bandwidth (accounting for protocol overhead), not the
    /// raw theoretical maximum.
    pub fn get_busy_percentage(&self) -> f64 {
        let max_bandwidth = self.speed.to_practical_bytes_per_second();
        self.bandwidth_stats
            .get_utilization_percentage(max_bandwidth)
    }

    /// `Some(capability)` when this device's cached max capability
    /// (`max_capability`) is faster than both the bus it's plugged into and
    /// its current link speed — i.e. it could run faster on a better bus.
    /// Reads only the cached field; no live sysfs access.
    pub fn check_speed_mismatch(&self, bus_speed: &UsbSpeed) -> Option<UsbSpeed> {
        let capability = self.max_capability.clone()?;
        if capability.to_mbps() > bus_speed.to_mbps() && capability.to_mbps() > self.speed.to_mbps()
        {
            Some(capability)
        } else {
            None
        }
    }

    /// Visual indicator for speed-capability issues. `LimitedByBus` takes
    /// precedence over `HighUtilization` when both apply.
    pub fn get_speed_indicator(&self, bus_speed: &UsbSpeed) -> SpeedIndicator {
        if let Some(capable_speed) = self.check_speed_mismatch(bus_speed) {
            SpeedIndicator::LimitedByBus(capable_speed)
        } else if self.speed.to_mbps() > 0.0 && self.get_busy_percentage() > 80.0 {
            SpeedIndicator::HighUtilization
        } else {
            SpeedIndicator::Normal
        }
    }
}

/// Heuristic max speed capability from USB descriptors: a `bcdDevice`
/// (device release number) of 0x0300 or higher signals a SuperSpeed-era
/// device; otherwise `bMaxPacketSize0` (the control endpoint's max packet
/// size) narrows it to High/Full/Low. Neither is authoritative, but it's
/// what sysfs exposes without a live USB descriptor read.
#[cfg(target_os = "linux")]
fn read_max_capability(dir: &std::path::Path) -> Option<UsbSpeed> {
    if let Ok(bcd) = std::fs::read_to_string(dir.join("bcdDevice")) {
        if u16::from_str_radix(bcd.trim(), 16).is_ok_and(|v| v >= 0x0300) {
            return Some(UsbSpeed::SuperSpeed);
        }
    }
    let raw = std::fs::read_to_string(dir.join("bMaxPacketSize0")).ok()?;
    match raw.trim().parse::<u16>().ok()? {
        n if n >= 64 => Some(UsbSpeed::High),
        8 => Some(UsbSpeed::Low),
        _ => Some(UsbSpeed::Full),
    }
}

/// Visual indicator for a device's speed-capability status, surfaced as the
/// `!` column in the device list.
#[derive(Debug, Clone, PartialEq)]
pub enum SpeedIndicator {
    Normal,
    HighUtilization,
    LimitedByBus(UsbSpeed), // Contains the speed the device is capable of
}

impl SpeedIndicator {
    /// Visual symbol for the device list's `!` column.
    pub fn get_symbol(&self) -> &'static str {
        match self {
            SpeedIndicator::Normal => "",
            SpeedIndicator::HighUtilization => "⚡",
            SpeedIndicator::LimitedByBus(_) => "🔺",
        }
    }

    /// Reference color for the indicator symbol.
    pub fn get_color(&self) -> (u8, u8, u8) {
        match self {
            SpeedIndicator::Normal => (128, 128, 128),        // Gray
            SpeedIndicator::HighUtilization => (255, 165, 0), // Orange
            SpeedIndicator::LimitedByBus(_) => (255, 255, 0), // Yellow
        }
    }

    /// Human-readable description of the indicator.
    ///
    /// `cfg(test)`-only for now: the device list wires `get_symbol`/`get_color`
    /// only, so nothing in production code reads this yet; verified here and
    /// ready for that wiring (e.g. a status line or tooltip for the selected
    /// device).
    #[cfg(test)]
    pub fn get_description(&self) -> String {
        match self {
            SpeedIndicator::Normal => "Normal operation".to_string(),
            SpeedIndicator::HighUtilization => "High bandwidth utilization".to_string(),
            SpeedIndicator::LimitedByBus(capable_speed) => {
                format!(
                    "Device capable of {} but limited by bus speed",
                    format_speed(capable_speed)
                )
            }
        }
    }
}

/// Format USB speed for display.
///
/// `cfg(test)`-only for now; see [`SpeedIndicator::get_description`].
#[cfg(test)]
fn format_speed(speed: &UsbSpeed) -> String {
    match speed {
        UsbSpeed::Low => "1.5 Mbps (Low Speed)".to_string(),
        UsbSpeed::Full => "12 Mbps (Full Speed)".to_string(),
        UsbSpeed::High => "480 Mbps (High Speed)".to_string(),
        UsbSpeed::SuperSpeed => "5 Gbps (SuperSpeed)".to_string(),
        UsbSpeed::SuperSpeedPlus => "10+ Gbps (SuperSpeed+)".to_string(),
        UsbSpeed::Unknown => "Unknown".to_string(),
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

    #[test]
    fn max_capability_reads_bcd_and_packet_size() {
        let temp = tempfile::tempdir().unwrap();
        // device with bcdDevice >= 0x0300 -> SuperSpeed capability
        write_device(
            &temp.path().join("1-2"),
            1,
            5,
            &[("speed", "480"), ("bcdDevice", "0310")],
        );
        let mut d = UsbDevice::new(1, 5);
        d.populate_from_sysfs(Some(temp.path()));
        assert_eq!(d.max_capability, Some(UsbSpeed::SuperSpeed));
        // 🔺: capable of SuperSpeed, linked High on a High bus
        assert_eq!(
            d.check_speed_mismatch(&UsbSpeed::High),
            Some(UsbSpeed::SuperSpeed)
        );
        assert_eq!(
            d.get_speed_indicator(&UsbSpeed::High),
            SpeedIndicator::LimitedByBus(UsbSpeed::SuperSpeed)
        );
    }

    #[test]
    fn high_utilization_indicator_above_80_percent() {
        let mut d = UsbDevice::new(1, 3);
        d.speed = UsbSpeed::Full; // practical 1.2 MB/s
        d.bandwidth_stats.current_bps = 1_100_000.0;
        assert!(d.get_busy_percentage() > 80.0);
        assert_eq!(
            d.get_speed_indicator(&UsbSpeed::Full),
            SpeedIndicator::HighUtilization
        );
    }

    #[test]
    fn limited_by_bus_takes_precedence_over_high_utilization() {
        let temp = tempfile::tempdir().unwrap();
        write_device(
            &temp.path().join("1-2"),
            1,
            7,
            &[("speed", "480"), ("bcdDevice", "0310")],
        );
        let mut d = UsbDevice::new(1, 7);
        d.populate_from_sysfs(Some(temp.path()));
        // Also pin utilization above the 80% threshold, so both conditions
        // are true at once; LimitedByBus must still win.
        d.bandwidth_stats.current_bps = 1_000_000_000.0;
        assert!(d.get_busy_percentage() > 80.0);
        assert_eq!(
            d.get_speed_indicator(&UsbSpeed::High),
            SpeedIndicator::LimitedByBus(UsbSpeed::SuperSpeed)
        );
    }

    #[test]
    fn normal_indicator_when_no_mismatch_and_low_utilization() {
        let mut d = UsbDevice::new(1, 9);
        d.speed = UsbSpeed::High;
        assert_eq!(
            d.get_speed_indicator(&UsbSpeed::High),
            SpeedIndicator::Normal
        );
    }

    #[test]
    fn read_max_capability_falls_back_to_max_packet_size() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("bMaxPacketSize0"), "64\n").unwrap();
        assert_eq!(read_max_capability(temp.path()), Some(UsbSpeed::High));

        std::fs::write(temp.path().join("bMaxPacketSize0"), "8\n").unwrap();
        assert_eq!(read_max_capability(temp.path()), Some(UsbSpeed::Low));

        std::fs::write(temp.path().join("bMaxPacketSize0"), "32\n").unwrap();
        assert_eq!(read_max_capability(temp.path()), Some(UsbSpeed::Full));
    }

    #[test]
    fn read_max_capability_none_when_no_attributes() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(read_max_capability(temp.path()), None);
    }

    #[test]
    fn speed_indicator_symbols_and_colors() {
        assert_eq!(SpeedIndicator::Normal.get_symbol(), "");
        assert_eq!(SpeedIndicator::HighUtilization.get_symbol(), "⚡");
        assert_eq!(
            SpeedIndicator::LimitedByBus(UsbSpeed::SuperSpeed).get_symbol(),
            "🔺"
        );
        assert_eq!(SpeedIndicator::Normal.get_color(), (128, 128, 128));
        assert_eq!(SpeedIndicator::HighUtilization.get_color(), (255, 165, 0));
        assert_eq!(
            SpeedIndicator::LimitedByBus(UsbSpeed::SuperSpeed).get_color(),
            (255, 255, 0)
        );
    }

    #[test]
    fn speed_indicator_description_mentions_capability() {
        let indicator = SpeedIndicator::LimitedByBus(UsbSpeed::SuperSpeed);
        assert!(indicator.get_description().contains("5 Gbps"));
        assert_eq!(SpeedIndicator::Normal.get_description(), "Normal operation");
    }
}

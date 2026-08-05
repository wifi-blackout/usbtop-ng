use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::device::UsbDevice;
use crate::usbmon::parser::{UrbType, UsbPacket, UsbSpeed};

#[derive(Debug, Clone)]
pub struct UsbBus {
    pub bus_id: u8,
    pub speed: UsbSpeed,
    pub devices: HashMap<u8, UsbDevice>,
    /// Host controller this bus hangs off, e.g. `0000:00:14.0`; `None` until
    /// the root hub's sysfs parent can be resolved.
    pub controller: Option<String>,
}

impl UsbBus {
    pub fn new(bus_id: u8) -> Self {
        Self {
            bus_id,
            speed: UsbSpeed::Unknown,
            devices: HashMap::new(),
            controller: None,
        }
    }

    /// Update bus speed by detecting the root hub speed, and resolve the host
    /// controller once. `base` overrides `/sys/bus/usb/devices` for tests.
    pub fn update_bus_speed(&mut self, base: Option<&Path>) -> Result<(), std::io::Error> {
        let default_base = Path::new("/sys/bus/usb/devices");
        let base_path = base.unwrap_or(default_base);
        if self.controller.is_none() {
            // The flat devices directory symlinks each root hub into its
            // controller's directory, so the canonical parent names the controller.
            self.controller = fs::canonicalize(base_path.join(format!("usb{}", self.bus_id)))
                .ok()
                .and_then(|real| Some(real.parent()?.file_name()?.to_string_lossy().into_owned()));
        }

        #[cfg(target_os = "linux")]
        {
            // Try to read the root hub speed (usually device 1 on the bus)
            let root_hub_path = base_path.join(format!("usb{}", self.bus_id)).join("speed");
            if root_hub_path.exists() {
                if let Ok(speed_str) = fs::read_to_string(&root_hub_path) {
                    self.speed = UsbSpeed::from_speed_str(speed_str.trim());
                    return Ok(());
                }
            }

            // Fallback: find the highest speed device on the bus as bus speed
            let highest_speed = self
                .devices
                .values()
                .map(|device| &device.speed)
                .max_by_key(|speed| speed.to_mbps() as u64)
                .cloned()
                .unwrap_or(UsbSpeed::Unknown);

            self.speed = highest_speed;
        }

        #[cfg(not(target_os = "linux"))]
        {
            // For non-Linux systems, estimate bus speed from devices
            let highest_speed = self
                .devices
                .values()
                .map(|device| &device.speed)
                .max_by_key(|speed| speed.to_mbps() as u64)
                .cloned()
                .unwrap_or(UsbSpeed::Unknown);

            self.speed = highest_speed;
        }

        Ok(())
    }

    /// Remove a device from this bus
    pub fn remove_device(&mut self, device_id: u8) {
        self.devices.remove(&device_id);
    }

    /// Aggregate %busy across every device on this bus, against the bus's
    /// practical maximum bandwidth. `None` when the bus speed is unknown (no
    /// meaningful denominator) rather than a misleading `0.0`.
    pub fn busy_percentage(&self) -> Option<f64> {
        let max_bandwidth = self.speed.to_practical_bytes_per_second();
        if max_bandwidth <= 0.0 {
            return None;
        }
        let total_usage: f64 = self
            .devices
            .values()
            .map(|device| device.bandwidth_stats.current_bps)
            .sum();
        Some((total_usage / max_bandwidth * 100.0).min(100.0))
    }
}

#[derive(Debug)]
pub struct DeviceManager {
    pub buses: HashMap<u8, UsbBus>,
    sysfs_base: Option<PathBuf>,
}

impl DeviceManager {
    pub fn new() -> Self {
        Self {
            buses: HashMap::new(),
            sysfs_base: None,
        }
    }

    /// Test seam: point sysfs lookups (device metadata, bus speed) at a
    /// fixture directory instead of the real `/sys/bus/usb/devices`.
    #[cfg(test)]
    pub fn with_sysfs_base(base: PathBuf) -> Self {
        Self {
            buses: HashMap::new(),
            sysfs_base: Some(base),
        }
    }

    /// Get or create a USB bus
    pub fn get_or_create_bus(&mut self, bus_id: u8) -> &mut UsbBus {
        self.buses
            .entry(bus_id)
            .or_insert_with(|| UsbBus::new(bus_id))
    }

    /// Update all bus speeds
    pub fn update_bus_speeds(&mut self) {
        let sysfs_base = self.sysfs_base.clone();
        for bus in self.buses.values_mut() {
            let _ = bus.update_bus_speed(sysfs_base.as_deref()); // Ignore errors for now
        }
    }

    /// Route one parsed usbmon event into per-device stats.
    /// Only callbacks carry the actual transferred length; submissions would
    /// double-count every URB.
    pub fn apply_packet(&mut self, packet: &UsbPacket) {
        let sysfs_base = self.sysfs_base.clone();
        let bus = self.get_or_create_bus(packet.bus_id);
        let device = bus.devices.entry(packet.device_id).or_insert_with(|| {
            let mut d = UsbDevice::new(packet.bus_id, packet.device_id);
            d.populate_from_sysfs(sysfs_base.as_deref());
            d
        });
        device.update_activity();
        if packet.urb_type == UrbType::Callback && packet.data_length > 0 {
            if packet.direction {
                device
                    .bandwidth_stats
                    .update_rx(u64::from(packet.data_length));
            } else {
                device
                    .bandwidth_stats
                    .update_tx(u64::from(packet.data_length));
            }
        }
    }

    /// Once-per-tick maintenance: decay rates, drop devices disconnected
    /// long enough, refresh bus speeds. Returns removed (bus_id, device_id).
    pub fn refresh(&mut self) -> Vec<(u8, u8)> {
        let sysfs_base = self.sysfs_base.clone();
        let mut removed = Vec::new();
        for bus in self.buses.values_mut() {
            for device in bus.devices.values_mut() {
                device.bandwidth_stats.refresh();
                if let Some(path) = &device.sysfs_path {
                    if !path.exists() {
                        device.mark_disconnected();
                    }
                } else if !device.is_disconnected {
                    // metadata may become readable later (e.g. permissions, race at first sight)
                    device.populate_from_sysfs(sysfs_base.as_deref());
                }
            }
            let stale: Vec<u8> = bus
                .devices
                .values()
                .filter(|d| d.should_remove())
                .map(|d| d.device_id)
                .collect();
            for device_id in stale {
                bus.remove_device(device_id);
                removed.push((bus.bus_id, device_id));
            }
        }
        self.buses.retain(|_, bus| !bus.devices.is_empty());
        self.update_bus_speeds();
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::parser::parse_usbmon_text_line;
    use std::time::Duration;

    fn manager_with_empty_sysfs() -> (tempfile::TempDir, DeviceManager) {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        (temp, mgr)
    }

    #[test]
    fn apply_packet_counts_only_callback_data() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let submission =
            parse_usbmon_text_line("ffff0000aaaa0001 100 S Bi:1:003:1 -115 512 <").unwrap();
        let callback =
            parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:003:1 0 512 = 00").unwrap();

        mgr.apply_packet(&submission);
        let dev = &mgr.buses[&1].devices[&3];
        assert_eq!(
            dev.bandwidth_stats.total_rx_bytes, 0,
            "submissions must not count"
        );

        mgr.apply_packet(&callback);
        let dev = &mgr.buses[&1].devices[&3];
        assert_eq!(dev.bandwidth_stats.total_rx_bytes, 512);
        assert_eq!(dev.bandwidth_stats.total_tx_bytes, 0);
    }

    #[test]
    fn apply_packet_routes_out_transfers_to_tx() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let callback = parse_usbmon_text_line("ffff0000bbbb0001 300 C Bo:2:004:2 0 128 >").unwrap();
        mgr.apply_packet(&callback);
        let dev = &mgr.buses[&2].devices[&4];
        assert_eq!(dev.bandwidth_stats.total_tx_bytes, 128);
        assert_eq!(dev.bandwidth_stats.total_rx_bytes, 0);
    }

    #[test]
    fn refresh_reports_removed_devices() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let callback = parse_usbmon_text_line("ffff0000cccc0001 400 C Bi:1:005:1 0 64 <").unwrap();
        mgr.apply_packet(&callback);
        // Force the disconnect path directly (sysfs-based detection arrives in Task 6).
        {
            let dev = mgr.buses.get_mut(&1).unwrap().devices.get_mut(&5).unwrap();
            dev.mark_disconnected();
            dev.disconnect_time = Some(std::time::Instant::now() - Duration::from_secs(10));
        }
        let removed = mgr.refresh();
        assert_eq!(removed, vec![(1, 5)]);
        assert!(!mgr.buses.contains_key(&1), "empty buses are dropped");
    }

    #[test]
    fn busy_percentage_none_for_unknown_bus_speed() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let bus = mgr.get_or_create_bus(1);
        assert_eq!(bus.speed, UsbSpeed::Unknown);
        assert_eq!(bus.busy_percentage(), None);
    }

    #[test]
    fn busy_percentage_sums_devices_against_bus_practical_max() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let bus = mgr.get_or_create_bus(1);
        bus.speed = UsbSpeed::Full; // practical max = 1_200_000 bytes/s
        let mut d1 = UsbDevice::new(1, 3);
        d1.bandwidth_stats.current_bps = 600_000.0;
        let mut d2 = UsbDevice::new(1, 4);
        d2.bandwidth_stats.current_bps = 300_000.0;
        bus.devices.insert(3, d1);
        bus.devices.insert(4, d2);

        assert_eq!(bus.busy_percentage(), Some(75.0));
    }

    #[test]
    fn busy_percentage_clamps_at_100() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let bus = mgr.get_or_create_bus(1);
        bus.speed = UsbSpeed::Full;
        let mut d1 = UsbDevice::new(1, 3);
        d1.bandwidth_stats.current_bps = 10_000_000.0;
        bus.devices.insert(3, d1);

        assert_eq!(bus.busy_percentage(), Some(100.0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refresh_marks_devices_disconnected_when_sysfs_path_vanishes() {
        let temp = tempfile::tempdir().unwrap();
        let dev_dir = temp.path().join("1-2");
        std::fs::create_dir_all(&dev_dir).unwrap();
        std::fs::write(dev_dir.join("busnum"), "1\n").unwrap();
        std::fs::write(dev_dir.join("devnum"), "3\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let callback = parse_usbmon_text_line("ffff0000ffff0001 100 C Bi:1:003:1 0 64 <").unwrap();
        mgr.apply_packet(&callback);
        assert!(!mgr.buses[&1].devices[&3].is_disconnected);

        std::fs::remove_dir_all(&dev_dir).unwrap();
        mgr.refresh();
        assert!(mgr.buses[&1].devices[&3].is_disconnected);
    }
}

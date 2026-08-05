use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::device::UsbDevice;
use crate::stats::BandwidthStats;
use crate::usbmon::parser::{UrbType, UsbPacket, UsbSpeed};

#[derive(Debug, Clone)]
pub struct UsbBus {
    pub bus_id: u8,
    pub speed: UsbSpeed,
    pub bandwidth_stats: BandwidthStats,
    pub devices: HashMap<u8, UsbDevice>,
}

impl UsbBus {
    pub fn new(bus_id: u8) -> Self {
        Self {
            bus_id,
            speed: UsbSpeed::Unknown,
            bandwidth_stats: BandwidthStats::new(),
            devices: HashMap::new(),
        }
    }

    /// Update bus speed by detecting the root hub speed.
    /// `base` overrides `/sys/bus/usb/devices` for tests.
    pub fn update_bus_speed(&mut self, base: Option<&Path>) -> Result<(), std::io::Error> {
        #[cfg(target_os = "linux")]
        {
            // Try to read the root hub speed (usually device 1 on the bus)
            let root_hub_path = base
                .unwrap_or(Path::new("/sys/bus/usb/devices"))
                .join(format!("usb{}", self.bus_id))
                .join("speed");
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
            let _ = base;
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

    /// Calculate the percentage of bus bandwidth being utilized
    /// Aggregates bandwidth usage from all devices on the bus
    pub fn get_busy_percentage(&self) -> f64 {
        let max_bandwidth = self.speed.to_practical_bytes_per_second();

        // Sum up bandwidth usage from all devices on this bus
        let total_usage = self
            .devices
            .values()
            .map(|device| device.bandwidth_stats.current_bps)
            .sum::<f64>();

        if max_bandwidth > 0.0 {
            (total_usage / max_bandwidth * 100.0).min(100.0)
        } else {
            0.0
        }
    }

    /// Calculate the percentage of bus bandwidth being utilized (theoretical)
    pub fn get_busy_percentage_theoretical(&self) -> f64 {
        let max_bandwidth = self.speed.to_bytes_per_second();

        let total_usage = self
            .devices
            .values()
            .map(|device| device.bandwidth_stats.current_bps)
            .sum::<f64>();

        if max_bandwidth > 0.0 {
            (total_usage / max_bandwidth * 100.0).min(100.0)
        } else {
            0.0
        }
    }

    /// Add or update a device on this bus
    pub fn add_or_update_device(&mut self, device: UsbDevice) {
        self.devices.insert(device.device_id, device);
    }

    /// Remove a device from this bus
    pub fn remove_device(&mut self, device_id: u8) {
        self.devices.remove(&device_id);
    }

    /// Get total bytes per second for all devices on this bus
    pub fn get_total_bps(&self) -> f64 {
        self.devices
            .values()
            .map(|device| device.bandwidth_stats.current_bps)
            .sum()
    }

    /// Check for devices that might be limited by bus speed
    pub fn get_speed_limited_devices(&self) -> Vec<(u8, crate::device::SpeedIndicator)> {
        self.devices
            .values()
            .map(|device| (device.device_id, device.get_speed_indicator(&self.speed)))
            .filter(|(_, indicator)| !matches!(indicator, crate::device::SpeedIndicator::Normal))
            .collect()
    }

    /// Get count of devices that could benefit from a faster bus
    pub fn get_limited_device_count(&self) -> usize {
        self.devices
            .values()
            .filter(|device| device.check_speed_mismatch(&self.speed).is_some())
            .count()
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
        let mut removed = Vec::new();
        for bus in self.buses.values_mut() {
            for device in bus.devices.values_mut() {
                device.bandwidth_stats.refresh();
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

    /// Add or update a device
    pub fn add_or_update_device(&mut self, device: UsbDevice) {
        let bus = self.get_or_create_bus(device.bus_id);
        bus.add_or_update_device(device);
    }

    /// Remove old/disconnected devices
    pub fn cleanup_old_devices(&mut self) {
        for bus in self.buses.values_mut() {
            let devices_to_remove: Vec<u8> = bus
                .devices
                .values()
                .filter(|device| device.should_remove())
                .map(|device| device.device_id)
                .collect();

            for device_id in devices_to_remove {
                bus.remove_device(device_id);
            }
        }

        // Remove empty buses
        self.buses.retain(|_, bus| !bus.devices.is_empty());
    }

    /// Get device count across all buses
    pub fn get_total_device_count(&self) -> usize {
        self.buses.values().map(|bus| bus.devices.len()).sum()
    }

    /// Get total bandwidth usage across all buses
    pub fn get_total_bandwidth(&self) -> f64 {
        self.buses.values().map(|bus| bus.get_total_bps()).sum()
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
}

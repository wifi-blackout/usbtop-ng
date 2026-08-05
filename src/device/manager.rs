use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::device::UsbDevice;
use crate::stats::BandwidthStats;
use crate::usbmon::parser::UsbSpeed;

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

    /// Update bus speed by detecting the root hub speed
    pub fn update_bus_speed(&mut self) -> Result<(), std::io::Error> {
        #[cfg(target_os = "linux")]
        {
            // Try to read the root hub speed (usually device 1 on the bus)
            let root_hub_path = format!("/sys/bus/usb/devices/usb{}/speed", self.bus_id);
            if Path::new(&root_hub_path).exists() {
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
}

impl DeviceManager {
    pub fn new() -> Self {
        Self {
            buses: HashMap::new(),
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
        for bus in self.buses.values_mut() {
            let _ = bus.update_bus_speed(); // Ignore errors for now
        }
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

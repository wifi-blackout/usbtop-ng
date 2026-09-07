use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::device::UsbDevice;
use crate::filter::FilterSet;
use crate::snapshot::Snapshot;
use crate::usbids::UsbIds;
use crate::usbmon::parser::{TransferType, UrbType, UsbPacket, UsbSpeed};

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
            speed: UsbSpeed::UNKNOWN,
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
            .unwrap_or(UsbSpeed::UNKNOWN);

        self.speed = highest_speed;

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

/// One backend-neutral traffic event: `bytes` moved on `device_id`'s
/// `endpoint`, in the direction `dir_in` says, over `transfer_type` (or
/// `None` when the source could not identify the type — usbmon's edge
/// case; a future eBPF source always supplies `Some`). `apply_packet`
/// builds one of these per callback carrying data; a future eBPF source
/// will build one per per-key cumulative delta since the last poll and
/// feed it to `apply_delta` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrafficDelta {
    pub bus_id: u8,
    pub device_id: u8,
    pub endpoint: u8,
    /// true = IN (device to host, rx); false = OUT (host to device, tx).
    pub dir_in: bool,
    pub transfer_type: Option<TransferType>,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct DeviceManager {
    pub buses: HashMap<u8, UsbBus>,
    sysfs_base: Option<PathBuf>,
    filter: FilterSet,
    usbids: Option<Arc<UsbIds>>,
    internal_snapshot: Option<Arc<Snapshot>>,
}

impl DeviceManager {
    pub fn new() -> Self {
        Self {
            buses: HashMap::new(),
            sysfs_base: None,
            filter: FilterSet::default(),
            usbids: None,
            internal_snapshot: None,
        }
    }

    /// Test/capture seam: point sysfs lookups (device metadata, bus speed) at a
    /// fixture directory instead of the real `/sys/bus/usb/devices`.
    pub fn with_sysfs_base(base: PathBuf) -> Self {
        Self {
            buses: HashMap::new(),
            sysfs_base: Some(base),
            filter: FilterSet::default(),
            usbids: None,
            internal_snapshot: None,
        }
    }

    /// Replace the active `--filter` set. Packets that don't match any
    /// expression in it stop counting toward device/endpoint bandwidth (see
    /// `apply_packet`), though their device row still appears and its
    /// `update_activity` timer still resets.
    pub fn set_filter(&mut self, filter: FilterSet) {
        self.filter = filter;
    }

    /// Install (or clear) the usb.ids database every newly populated device
    /// gets overlaid with (see `UsbDevice::apply_usbids`). `None` (the
    /// default) leaves device/product names exactly as sysfs reported them.
    pub fn set_usbids(&mut self, db: Option<Arc<UsbIds>>) {
        self.usbids = db;
    }

    /// Install (or clear) the internal-device snapshot every device gets
    /// stamped against (see `stamp_internal`). Unlike `set_usbids`, this
    /// also immediately restamps every device already known, so a snapshot
    /// taken mid-session takes effect on the very next tick instead of
    /// waiting for each device to be re-populated on its own schedule. A
    /// `None` here would clear every mark -- today's callers always pass
    /// `Some`.
    pub fn set_internal_snapshot(&mut self, snapshot: Option<Arc<Snapshot>>) {
        self.internal_snapshot = snapshot;
        for bus in self.buses.values_mut() {
            for device in bus.devices.values_mut() {
                stamp_internal(device, &self.internal_snapshot);
            }
        }
    }

    /// Whether an internal-device snapshot is currently installed. Lets a
    /// reader (e.g. `headless::build_report`) tell "no snapshot, so origin
    /// is unknown" apart from "a snapshot says this device is external"
    /// without needing its own copy of the flag.
    pub fn has_internal_snapshot(&self) -> bool {
        self.internal_snapshot.is_some()
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

    /// Ensure `bus_id`/`device_id` has a row (create + populate sysfs
    /// metadata + usb.ids overlay + internal-snapshot stamp on first sight)
    /// and mark it seen. Shared by every path that observes a device,
    /// whether or not it goes on to account any bytes: `apply_delta`'s
    /// touch always precedes its filter check, so a filtered-out or
    /// zero-byte delta still marks the device seen.
    pub fn touch_device(&mut self, bus_id: u8, device_id: u8) -> &mut UsbDevice {
        let sysfs_base = self.sysfs_base.clone();
        let usbids = self.usbids.clone();
        let internal_snapshot = self.internal_snapshot.clone();
        let bus = self.get_or_create_bus(bus_id);
        let device = bus.devices.entry(device_id).or_insert_with(|| {
            let mut d = UsbDevice::new(bus_id, device_id);
            d.populate_from_sysfs(sysfs_base.as_deref());
            if let Some(db) = &usbids {
                d.apply_usbids(db);
            }
            stamp_internal(&mut d, &internal_snapshot);
            d
        });
        device.update_activity();
        device
    }

    /// Backend-neutral accounting entry point. Touches the device (see
    /// `touch_device`), then, when the active filter matches this key and
    /// `delta.bytes > 0`, adds `delta.bytes` to its rx (`dir_in`) or tx
    /// stats and, when `delta.transfer_type` is `Some`, records it against
    /// the endpoint. `apply_packet` is usbmon's adapter onto this; a future
    /// eBPF source will call this directly with per-key cumulative deltas.
    pub fn apply_delta(&mut self, delta: &TrafficDelta) {
        // `matches_traffic` needs `&self.filter` while `device` below is
        // borrowed from `self.buses`, so the handle is cloned up front —
        // same pattern `touch_device` uses for its own cloned handles.
        let filter = self.filter.clone();
        let device = self.touch_device(delta.bus_id, delta.device_id);
        let counts =
            filter.matches_traffic(device, delta.endpoint, delta.dir_in, delta.transfer_type);
        if counts && delta.bytes > 0 {
            if delta.dir_in {
                device.bandwidth_stats.update_rx(delta.bytes);
            } else {
                device.bandwidth_stats.update_tx(delta.bytes);
            }
            if let Some(transfer_type) = delta.transfer_type {
                device.record_endpoint(delta.endpoint, delta.dir_in, transfer_type, delta.bytes);
            }
        }
    }

    /// Route one parsed usbmon event into per-device stats. Only callbacks
    /// carry the actual transferred length; submissions would double-count
    /// every URB, so anything else just touches the device (marks it seen)
    /// with no accounting.
    pub fn apply_packet(&mut self, packet: &UsbPacket) {
        if packet.urb_type == UrbType::Callback && packet.data_length > 0 {
            self.apply_delta(&TrafficDelta {
                bus_id: packet.bus_id,
                device_id: packet.device_id,
                endpoint: packet.endpoint,
                dir_in: packet.direction,
                transfer_type: packet.transfer_type,
                bytes: u64::from(packet.data_length),
            });
        } else {
            self.touch_device(packet.bus_id, packet.device_id);
        }
    }

    /// Once-per-tick maintenance: decay rates, drop devices disconnected
    /// long enough, refresh bus speeds. Returns removed (bus_id, device_id).
    pub fn refresh(&mut self) -> Vec<(u8, u8)> {
        let sysfs_base = self.sysfs_base.clone();
        let usbids = self.usbids.clone();
        let internal_snapshot = self.internal_snapshot.clone();
        let mut removed = Vec::new();
        for bus in self.buses.values_mut() {
            for device in bus.devices.values_mut() {
                device.bandwidth_stats.refresh();
                device.refresh_endpoints();
                if let Some(path) = &device.sysfs_path {
                    if !path.exists() {
                        device.mark_disconnected();
                    }
                } else if !device.is_disconnected {
                    // metadata may become readable later (e.g. permissions, race at first sight)
                    device.populate_from_sysfs(sysfs_base.as_deref());
                    if let Some(db) = &usbids {
                        device.apply_usbids(db);
                    }
                    stamp_internal(device, &internal_snapshot);
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

    /// Add a row for every USB device currently in sysfs, so idle devices show
    /// before they transfer. Metadata is read once, when a device is first
    /// seen, from the directory this scan already found; `busnum`/`devnum`
    /// are still re-read every tick for every present entry, to detect newly
    /// connected devices. A device already known keeps its row and its
    /// bandwidth untouched. Removal stays in `refresh`, through the existing
    /// disconnect path.
    pub fn enumerate_present_devices(&mut self) {
        let base = self
            .sysfs_base
            .clone()
            .unwrap_or_else(|| PathBuf::from("/sys/bus/usb/devices"));
        let usbids = self.usbids.clone();
        let internal_snapshot = self.internal_snapshot.clone();
        let Ok(entries) = std::fs::read_dir(&base) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains(':') {
                continue; // an interface, not a device
            }
            let dir = entry.path();
            let (Some(bus), Some(dev)) = (
                read_sysfs_u8(&dir.join("busnum")),
                read_sysfs_u8(&dir.join("devnum")),
            ) else {
                continue;
            };
            self.buses
                .entry(bus)
                .or_insert_with(|| UsbBus::new(bus))
                .devices
                .entry(dev)
                .or_insert_with(|| {
                    let mut device = UsbDevice::new(bus, dev);
                    device.populate_from_dir(&dir);
                    if let Some(db) = &usbids {
                        device.apply_usbids(db);
                    }
                    stamp_internal(&mut device, &internal_snapshot);
                    device
                });
        }
    }
}

fn read_sysfs_u8(path: &Path) -> Option<u8> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Stamp one device's `is_internal` against `snapshot`: matches when the
/// device's sysfs directory name and IDs are in the snapshot (see
/// `Snapshot::is_internal`). No snapshot, or no resolved `sysfs_path`,
/// always stamps `false`.
fn stamp_internal(device: &mut UsbDevice, snapshot: &Option<Arc<Snapshot>>) {
    device.is_internal = match (snapshot, &device.sysfs_path) {
        (Some(snap), Some(path)) => path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| snap.is_internal(name, device.vendor_id, device.product_id)),
        _ => false,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::FilterSet;
    use crate::usbmon::parser::{parse_usbmon_text_line, TransferType, UsbSpeed};
    use std::time::Duration;

    fn manager_with_empty_sysfs() -> (tempfile::TempDir, DeviceManager) {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        (temp, mgr)
    }

    fn write_sysfs_device(base: &std::path::Path, name: &str, bus: u8, dev: u8, speed: &str) {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("busnum"), format!("{bus}\n")).unwrap();
        std::fs::write(dir.join("devnum"), format!("{dev}\n")).unwrap();
        std::fs::write(dir.join("speed"), format!("{speed}\n")).unwrap();
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
        assert_eq!(bus.speed, UsbSpeed::UNKNOWN);
        assert_eq!(bus.busy_percentage(), None);
    }

    #[test]
    fn busy_percentage_sums_devices_against_bus_practical_max() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let bus = mgr.get_or_create_bus(1);
        bus.speed = UsbSpeed::from_mbps(12.0); // practical max = 1_200_000 bytes/s
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
        bus.speed = UsbSpeed::from_mbps(12.0);
        let mut d1 = UsbDevice::new(1, 3);
        d1.bandwidth_stats.current_bps = 10_000_000.0;
        bus.devices.insert(3, d1);

        assert_eq!(bus.busy_percentage(), Some(100.0));
    }

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

    #[test]
    fn enumeration_adds_a_row_per_present_device_at_zero_bandwidth() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "usb1", 1, 1, "480"); // root hub
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480"); // a device
        std::fs::create_dir_all(temp.path().join("1-4:1.0")).unwrap(); // interface, skipped

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.enumerate_present_devices();

        let bus = mgr.buses.get(&1).expect("bus 1 present");
        assert!(bus.devices.contains_key(&1), "root hub enumerated");
        assert!(bus.devices.contains_key(&4), "device enumerated");
        assert_eq!(bus.devices.len(), 2, "the interface dir is not a device");
        assert_eq!(bus.devices[&4].bandwidth_stats.current_bps, 0.0);
        assert_eq!(bus.devices[&4].speed, UsbSpeed::from_mbps(480.0));
    }

    #[test]
    fn enumeration_sets_sysfs_path_to_the_dir_it_found() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.enumerate_present_devices();

        assert_eq!(
            mgr.buses[&1].devices[&4].sysfs_path,
            Some(temp.path().join("1-4"))
        );
    }

    #[test]
    fn enumeration_does_not_disturb_a_device_that_has_traffic() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());

        let callback = parse_usbmon_text_line("ffff0000aaaa0001 100 C Bi:1:004:1 0 512 <").unwrap();
        mgr.apply_packet(&callback);
        mgr.enumerate_present_devices();

        assert_eq!(
            mgr.buses[&1].devices[&4].bandwidth_stats.total_rx_bytes,
            512
        );
        assert_eq!(mgr.buses[&1].devices.len(), 1, "same row, not a duplicate");
    }

    #[test]
    fn apply_packet_tracks_per_endpoint_stats() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let iso = parse_usbmon_text_line("ffff0000aaaa0001 200 C Zi:1:004:1 0:1:6672:0 32 27000 =")
            .unwrap();
        let bulk_out = parse_usbmon_text_line("ffff0000aaaa0002 300 C Bo:1:004:2 0 512 >").unwrap();
        mgr.apply_packet(&iso);
        mgr.apply_packet(&bulk_out);

        let dev = &mgr.buses[&1].devices[&4];
        let iso_ep = &dev.endpoints[&(1, true)];
        assert_eq!(iso_ep.transfer_type, TransferType::Isochronous);
        assert_eq!(iso_ep.total_bytes, 27_000);
        assert!(iso_ep.counter.bps() > 0.0);
        let bulk_ep = &dev.endpoints[&(2, false)];
        assert_eq!(bulk_ep.transfer_type, TransferType::Bulk);
        assert_eq!(bulk_ep.total_bytes, 512);
        assert!(dev.has_iso_traffic());
    }

    #[test]
    fn submissions_do_not_touch_endpoint_stats() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        let s = parse_usbmon_text_line("ffff0000aaaa0001 100 S Bi:1:003:1 -115 512 <").unwrap();
        mgr.apply_packet(&s);
        assert!(mgr.buses[&1].devices[&3].endpoints.is_empty());
    }

    #[test]
    fn filtered_out_packets_do_not_count() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        mgr.set_filter(FilterSet::parse(&["type=iso".into()]).unwrap());
        let bulk = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:003:1 0 512 = 00").unwrap();
        mgr.apply_packet(&bulk);
        let dev = &mgr.buses[&1].devices[&3];
        assert_eq!(
            dev.bandwidth_stats.total_rx_bytes, 0,
            "bulk bytes must not count under type=iso"
        );
        assert!(dev.endpoints.is_empty());
    }

    #[test]
    fn apply_delta_accounts_rx_tx_and_endpoint_for_a_matching_some_type_delta() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        mgr.apply_delta(&TrafficDelta {
            bus_id: 1,
            device_id: 4,
            endpoint: 2,
            dir_in: true,
            transfer_type: Some(TransferType::Bulk),
            bytes: 256,
        });
        mgr.apply_delta(&TrafficDelta {
            bus_id: 1,
            device_id: 4,
            endpoint: 3,
            dir_in: false,
            transfer_type: Some(TransferType::Interrupt),
            bytes: 64,
        });

        let dev = &mgr.buses[&1].devices[&4];
        assert_eq!(dev.bandwidth_stats.total_rx_bytes, 256);
        assert_eq!(dev.bandwidth_stats.total_tx_bytes, 64);
        let rx_ep = &dev.endpoints[&(2, true)];
        assert_eq!(rx_ep.transfer_type, TransferType::Bulk);
        assert_eq!(rx_ep.total_bytes, 256);
        let tx_ep = &dev.endpoints[&(3, false)];
        assert_eq!(tx_ep.transfer_type, TransferType::Interrupt);
        assert_eq!(tx_ep.total_bytes, 64);
    }

    #[test]
    fn apply_delta_touches_the_device_but_accounts_nothing_when_filtered_out() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        mgr.set_filter(FilterSet::parse(&["type=iso".into()]).unwrap());
        mgr.apply_delta(&TrafficDelta {
            bus_id: 1,
            device_id: 4,
            endpoint: 2,
            dir_in: true,
            transfer_type: Some(TransferType::Bulk),
            bytes: 256,
        });

        assert!(
            mgr.buses[&1].devices.contains_key(&4),
            "the device row exists (touched) even though the delta was filtered out"
        );
        let dev = &mgr.buses[&1].devices[&4];
        assert_eq!(dev.bandwidth_stats.total_rx_bytes, 0);
        assert_eq!(dev.bandwidth_stats.total_tx_bytes, 0);
        assert!(dev.endpoints.is_empty());
    }

    #[test]
    fn apply_delta_with_no_transfer_type_accounts_bytes_but_records_no_endpoint() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        mgr.apply_delta(&TrafficDelta {
            bus_id: 1,
            device_id: 4,
            endpoint: 2,
            dir_in: true,
            transfer_type: None,
            bytes: 256,
        });

        let dev = &mgr.buses[&1].devices[&4];
        assert_eq!(dev.bandwidth_stats.total_rx_bytes, 256);
        assert!(
            dev.endpoints.is_empty(),
            "an unrecognized transfer type still accounts rx/tx but records no endpoint"
        );
    }

    #[test]
    fn refresh_marks_an_unplugged_enumerated_device_disconnected() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.enumerate_present_devices();
        assert!(!mgr.buses[&1].devices[&4].is_disconnected);

        std::fs::remove_dir_all(temp.path().join("1-4")).unwrap();
        mgr.refresh();
        assert!(mgr.buses[&1].devices[&4].is_disconnected);
    }

    #[test]
    fn usbids_names_win_over_device_strings_per_field() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let dir = temp.path().join("1-4");
        std::fs::write(dir.join("idVendor"), "0430\n").unwrap();
        std::fs::write(dir.join("idProduct"), "0100\n").unwrap();
        std::fs::write(dir.join("manufacturer"), "StringVendor\n").unwrap();
        std::fs::write(dir.join("product"), "StringProduct\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.set_usbids(Some(std::sync::Arc::new(crate::usbids::UsbIds::parse(
            "0430  Fujitsu Component Limited\n\t0100  3-button Mouse\n",
        ))));
        mgr.enumerate_present_devices();

        let dev = &mgr.buses[&1].devices[&4];
        assert_eq!(dev.vendor.as_deref(), Some("Fujitsu Component Limited"));
        assert_eq!(dev.product.as_deref(), Some("3-button Mouse"));
    }

    #[test]
    fn device_strings_fill_the_gaps_the_database_leaves() {
        // db has the vendor but not this product: vendor from db, product
        // from the device string. And with no db entry at all, both strings
        // survive.
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let dir = temp.path().join("1-4");
        std::fs::write(dir.join("idVendor"), "0430\n").unwrap();
        std::fs::write(dir.join("idProduct"), "0a99\n").unwrap();
        std::fs::write(dir.join("manufacturer"), "StringVendor\n").unwrap();
        std::fs::write(dir.join("product"), "StringProduct\n").unwrap();

        write_sysfs_device(temp.path(), "1-5", 1, 5, "480");
        let dir2 = temp.path().join("1-5");
        std::fs::write(dir2.join("idVendor"), "9999\n").unwrap();
        std::fs::write(dir2.join("idProduct"), "0001\n").unwrap();
        std::fs::write(dir2.join("manufacturer"), "OtherVendor\n").unwrap();
        std::fs::write(dir2.join("product"), "OtherProduct\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.set_usbids(Some(std::sync::Arc::new(crate::usbids::UsbIds::parse(
            "0430  Fujitsu Component Limited\n",
        ))));
        mgr.enumerate_present_devices();

        let dev = &mgr.buses[&1].devices[&4];
        assert_eq!(
            dev.vendor.as_deref(),
            Some("Fujitsu Component Limited"),
            "vendor known to the database wins"
        );
        assert_eq!(
            dev.product.as_deref(),
            Some("StringProduct"),
            "product unknown to the database keeps the device string"
        );

        let dev2 = &mgr.buses[&1].devices[&5];
        assert_eq!(
            dev2.vendor.as_deref(),
            Some("OtherVendor"),
            "no db entry at all: both strings survive"
        );
        assert_eq!(dev2.product.as_deref(), Some("OtherProduct"));
    }

    #[test]
    fn no_database_keeps_todays_names() {
        // set_usbids(None) is the default; pinned so the overlay cannot
        // regress the no-db path.
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let dir = temp.path().join("1-4");
        std::fs::write(dir.join("idVendor"), "0430\n").unwrap();
        std::fs::write(dir.join("idProduct"), "0100\n").unwrap();
        std::fs::write(dir.join("manufacturer"), "StringVendor\n").unwrap();
        std::fs::write(dir.join("product"), "StringProduct\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.enumerate_present_devices();

        let dev = &mgr.buses[&1].devices[&4];
        assert_eq!(dev.vendor.as_deref(), Some("StringVendor"));
        assert_eq!(dev.product.as_deref(), Some("StringProduct"));
    }

    #[test]
    fn apply_packet_overlays_usbids_names_for_a_newly_seen_device() {
        // The traffic path (apply_packet's or_insert), not enumeration.
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("1-4");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("busnum"), "1\n").unwrap();
        std::fs::write(dir.join("devnum"), "4\n").unwrap();
        std::fs::write(dir.join("idVendor"), "0430\n").unwrap();
        std::fs::write(dir.join("idProduct"), "0100\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.set_usbids(Some(std::sync::Arc::new(crate::usbids::UsbIds::parse(
            "0430  Fujitsu Component Limited\n\t0100  3-button Mouse\n",
        ))));
        let callback = parse_usbmon_text_line("ffff0000aaaa0001 100 C Bi:1:004:1 0 64 <").unwrap();
        mgr.apply_packet(&callback);

        let dev = &mgr.buses[&1].devices[&4];
        assert_eq!(dev.vendor.as_deref(), Some("Fujitsu Component Limited"));
        assert_eq!(dev.product.as_deref(), Some("3-button Mouse"));
    }

    fn matching_snapshot() -> crate::snapshot::Snapshot {
        crate::snapshot::Snapshot {
            captured_unix: 0,
            devices: vec![crate::snapshot::SnapshotDevice {
                port_path: "1-4".into(),
                vendor_id: Some("04f2".into()),
                product_id: Some("b71a".into()),
            }],
        }
    }

    #[test]
    fn enumeration_marks_a_device_matching_the_snapshot_internal() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let dir = temp.path().join("1-4");
        std::fs::write(dir.join("idVendor"), "04f2\n").unwrap();
        std::fs::write(dir.join("idProduct"), "b71a\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.set_internal_snapshot(Some(Arc::new(matching_snapshot())));
        mgr.enumerate_present_devices();

        assert!(mgr.buses[&1].devices[&4].is_internal);
    }

    #[test]
    fn apply_packet_marks_a_newly_seen_device_internal_when_it_matches_the_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("1-4");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("busnum"), "1\n").unwrap();
        std::fs::write(dir.join("devnum"), "4\n").unwrap();
        std::fs::write(dir.join("idVendor"), "04f2\n").unwrap();
        std::fs::write(dir.join("idProduct"), "b71a\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.set_internal_snapshot(Some(Arc::new(matching_snapshot())));
        let callback = parse_usbmon_text_line("ffff0000aaaa0001 100 C Bi:1:004:1 0 64 <").unwrap();
        mgr.apply_packet(&callback);

        assert!(mgr.buses[&1].devices[&4].is_internal);
    }

    #[test]
    fn a_different_device_on_a_snapshotted_port_stays_external() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let dir = temp.path().join("1-4");
        std::fs::write(dir.join("idVendor"), "9999\n").unwrap();
        std::fs::write(dir.join("idProduct"), "0001\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.set_internal_snapshot(Some(Arc::new(matching_snapshot())));
        mgr.enumerate_present_devices();

        assert!(
            !mgr.buses[&1].devices[&4].is_internal,
            "same port, different device: external"
        );
    }

    #[test]
    fn a_device_with_no_sysfs_path_stays_external() {
        let (_t, mut mgr) = manager_with_empty_sysfs();
        mgr.set_internal_snapshot(Some(Arc::new(matching_snapshot())));
        let callback = parse_usbmon_text_line("ffff0000aaaa0001 100 C Bi:1:003:1 0 64 <").unwrap();
        mgr.apply_packet(&callback);

        assert!(
            !mgr.buses[&1].devices[&3].is_internal,
            "no sysfs_path resolved, so nothing to match against"
        );
    }

    #[test]
    fn set_internal_snapshot_restamps_existing_devices_in_both_directions() {
        let temp = tempfile::tempdir().unwrap();
        write_sysfs_device(temp.path(), "1-4", 1, 4, "480");
        let dir = temp.path().join("1-4");
        std::fs::write(dir.join("idVendor"), "04f2\n").unwrap();
        std::fs::write(dir.join("idProduct"), "b71a\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        mgr.enumerate_present_devices();
        assert!(
            !mgr.buses[&1].devices[&4].is_internal,
            "no snapshot loaded yet"
        );

        mgr.set_internal_snapshot(Some(Arc::new(matching_snapshot())));
        assert!(
            mgr.buses[&1].devices[&4].is_internal,
            "a snapshot arriving mid-session must restamp existing devices: false -> true"
        );

        mgr.set_internal_snapshot(None);
        assert!(
            !mgr.buses[&1].devices[&4].is_internal,
            "clearing the snapshot must restamp existing devices back: true -> false"
        );
    }
}

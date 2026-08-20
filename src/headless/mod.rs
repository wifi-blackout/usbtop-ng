//! Non-TUI reports: `--once` samples one window and prints, `--batch`
//! prints every window until interrupted. Never prompts.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::Serialize;

use crate::device::manager::DeviceManager;
use crate::filter::FilterSet;
use crate::ui::drain_packets;
use crate::usbmon::parser::UsbPacket;

pub struct HeadlessOptions {
    pub json: bool,
    pub batch: bool,
    pub window: Duration,
}

#[derive(Serialize)]
pub struct Report {
    pub version: u32,
    pub timestamp: f64,
    pub window_seconds: f64,
    pub source: &'static str,
    pub dropped_packets: u64,
    pub total_rx_bps: f64,
    pub total_tx_bps: f64,
    pub buses: Vec<BusReport>,
}

#[derive(Serialize)]
pub struct BusReport {
    pub bus: u8,
    pub speed_mbps: f64,
    pub controller: Option<String>,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub devices: Vec<DeviceReport>,
}

#[derive(Serialize)]
pub struct DeviceReport {
    pub bus: u8,
    pub address: u8,
    pub port: Option<String>,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub speed_mbps: f64,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
    pub estimated: bool,
    pub endpoints: Vec<EndpointReport>,
}

#[derive(Serialize)]
pub struct EndpointReport {
    pub endpoint: u8,
    pub direction: &'static str,
    pub transfer_type: &'static str,
    pub bps: f64,
    pub total_bytes: u64,
}

/// Cumulative totals at window start, so report rates are exact
/// bytes-in-window over window seconds — not the manager's own 10s
/// sliding-window rates, which would misreport any other window length.
pub struct Baseline {
    device_totals: HashMap<(u8, u8), (u64, u64)>,
    endpoint_totals: HashMap<(u8, u8, u8, bool), u64>,
}

impl Baseline {
    /// Snapshot every device's and endpoint's cumulative byte totals as they
    /// stand right now, to be diffed against a later snapshot by
    /// [`build_report`].
    pub fn capture(manager: &DeviceManager) -> Baseline {
        let mut device_totals = HashMap::new();
        let mut endpoint_totals = HashMap::new();
        for bus in manager.buses.values() {
            for device in bus.devices.values() {
                device_totals.insert(
                    (bus.bus_id, device.device_id),
                    (
                        device.bandwidth_stats.total_rx_bytes,
                        device.bandwidth_stats.total_tx_bytes,
                    ),
                );
                for (&(endpoint, dir_in), stats) in &device.endpoints {
                    endpoint_totals.insert(
                        (bus.bus_id, device.device_id, endpoint, dir_in),
                        stats.total_bytes,
                    );
                }
            }
        }
        Baseline {
            device_totals,
            endpoint_totals,
        }
    }
}

/// Rate over `window_secs`, given a cumulative total at window start and now.
/// A missing baseline (device/endpoint first seen mid-window) counts as a
/// zero start rather than skipping the row, so new arrivals still report a
/// rate instead of vanishing from the window's numbers.
fn windowed_rate(baseline_total: Option<u64>, now_total: u64, window_secs: f64) -> f64 {
    let start = baseline_total.unwrap_or(0);
    let delta = now_total.saturating_sub(start);
    delta as f64 / window_secs
}

/// Build one report from the manager's current state and a `baseline` taken
/// at the start of the window. Pure and fully testable: no clock reads other
/// than the `timestamp` field, no I/O.
pub fn build_report(
    manager: &DeviceManager,
    baseline: &Baseline,
    window: Duration,
    source: &'static str,
    dropped: u64,
    text_active: bool,
    filter: &FilterSet,
) -> Report {
    let window_secs = window.as_secs_f64();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);

    let mut buses: Vec<_> = manager.buses.values().collect();
    buses.sort_by_key(|bus| bus.bus_id);

    let bus_reports: Vec<BusReport> = buses
        .into_iter()
        .map(|bus| {
            let mut devices: Vec<_> = bus
                .devices
                .values()
                .filter(|device| filter.matches_device(device))
                .collect();
            // Same ordering rule as `ui::bus_view`: port_chain missing sorts
            // last, then numeric port chain, then device_id.
            devices.sort_by_key(|device| {
                let port_chain = device.port_chain();
                (
                    port_chain.is_none(),
                    port_chain.unwrap_or_default(),
                    device.device_id,
                )
            });

            let device_reports: Vec<DeviceReport> = devices
                .into_iter()
                .map(|device| {
                    let key = (bus.bus_id, device.device_id);
                    let (baseline_rx, baseline_tx) = baseline
                        .device_totals
                        .get(&key)
                        .copied()
                        .map_or((None, None), |(rx, tx)| (Some(rx), Some(tx)));
                    let rx_bps = windowed_rate(
                        baseline_rx,
                        device.bandwidth_stats.total_rx_bytes,
                        window_secs,
                    );
                    let tx_bps = windowed_rate(
                        baseline_tx,
                        device.bandwidth_stats.total_tx_bytes,
                        window_secs,
                    );

                    let endpoints: Vec<EndpointReport> = device
                        .endpoints
                        .iter()
                        .map(|(&(endpoint, dir_in), stats)| {
                            let ep_key = (bus.bus_id, device.device_id, endpoint, dir_in);
                            let baseline_total = baseline.endpoint_totals.get(&ep_key).copied();
                            let bps = windowed_rate(baseline_total, stats.total_bytes, window_secs);
                            EndpointReport {
                                endpoint,
                                direction: if dir_in { "in" } else { "out" },
                                transfer_type: stats.transfer_type.label(),
                                bps,
                                total_bytes: stats.total_bytes,
                            }
                        })
                        .collect();

                    DeviceReport {
                        bus: bus.bus_id,
                        address: device.device_id,
                        port: device.port_chain().map(|chain| {
                            chain
                                .iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join(".")
                        }),
                        vendor_id: device.vendor_id.map(|id| format!("{id:04x}")),
                        product_id: device.product_id.map(|id| format!("{id:04x}")),
                        vendor: device.vendor.clone(),
                        product: device.product.clone(),
                        speed_mbps: device.speed.to_mbps(),
                        rx_bps,
                        tx_bps,
                        total_rx_bytes: device.bandwidth_stats.total_rx_bytes,
                        total_tx_bytes: device.bandwidth_stats.total_tx_bytes,
                        estimated: text_active && device.has_iso_traffic(),
                        endpoints,
                    }
                })
                .collect();

            let rx_bps = device_reports.iter().map(|d| d.rx_bps).sum();
            let tx_bps = device_reports.iter().map(|d| d.tx_bps).sum();

            BusReport {
                bus: bus.bus_id,
                speed_mbps: bus.speed.to_mbps(),
                controller: bus.controller.clone(),
                rx_bps,
                tx_bps,
                devices: device_reports,
            }
        })
        // A bus every device on it was filtered out of is not a bus worth
        // reporting: mirrors `ui::retain_filtered_devices` pruning empty
        // buses rather than leaving a header with no rows under it.
        .filter(|bus| !bus.devices.is_empty())
        .collect();

    let total_rx_bps = bus_reports.iter().map(|b| b.rx_bps).sum();
    let total_tx_bps = bus_reports.iter().map(|b| b.tx_bps).sum();

    Report {
        version: 1,
        timestamp,
        window_seconds: window_secs,
        source,
        dropped_packets: dropped,
        total_rx_bps,
        total_tx_bps,
        buses: bus_reports,
    }
}

/// Bytes per second as MB/s, floored at zero (mirrors `ui::to_mbps`, kept
/// separate since the two rendering paths don't share a module).
fn to_mbps(bytes_per_second: f64) -> f64 {
    let mbps = bytes_per_second / 1_000_000.0;
    if mbps <= 0.0 {
        0.0
    } else {
        mbps
    }
}

/// Render a report as plain text: a `ts=` line, one header per bus, and one
/// indented row per device. `~rx`/`~tx` marks a device whose rate is
/// `estimated` (see [`DeviceReport::estimated`]).
pub fn render_text(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "ts={:.3} window={:.2}s source={} dropped={}\n",
        report.timestamp, report.window_seconds, report.source, report.dropped_packets
    ));
    for bus in &report.buses {
        out.push_str(&format!(
            "bus {} ({:.0} Mbps) rx {:.2} MB/s tx {:.2} MB/s\n",
            bus.bus,
            bus.speed_mbps,
            to_mbps(bus.rx_bps),
            to_mbps(bus.tx_bps)
        ));
        for device in &bus.devices {
            let id = match (&device.vendor_id, &device.product_id) {
                (Some(v), Some(p)) => format!("{v}:{p}"),
                _ => "----:----".to_string(),
            };
            let name = match (&device.vendor, &device.product) {
                (Some(v), Some(p)) => format!("{v} {p}"),
                (Some(v), None) => v.clone(),
                (None, Some(p)) => p.clone(),
                (None, None) => "Unknown".to_string(),
            };
            let rx_prefix = if device.estimated { "~rx" } else { "rx" };
            let tx_prefix = if device.estimated { "~tx" } else { "tx" };
            out.push_str(&format!(
                "  {}:{}  {}  {:.0} Mbps  {} {:.2} MB/s  {} {:.2} MB/s  {}\n",
                device.bus,
                device.address,
                id,
                device.speed_mbps,
                rx_prefix,
                to_mbps(device.rx_bps),
                tx_prefix,
                to_mbps(device.tx_bps),
                name,
            ));
        }
    }
    out.push('\n');
    out
}

/// Sample the manager's state on `opts.window`-second windows, printing a
/// report at the end of each. `--once` (`opts.batch == false`) prints one and
/// returns; `--batch` repeats until SIGINT/SIGTERM or the reader stops.
pub fn run(
    mut manager: DeviceManager,
    packets: Receiver<UsbPacket>,
    dropped: Arc<AtomicU64>,
    text_active: Arc<AtomicBool>,
    filter: FilterSet,
    opts: HeadlessOptions,
) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop))?;

    loop {
        manager.enumerate_present_devices();
        let baseline = Baseline::capture(&manager);
        let deadline = Instant::now() + opts.window;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            drain_packets(&mut manager, &packets, crate::ui::DRAIN_BATCH);
            std::thread::sleep(Duration::from_millis(50).min(opts.window));
        }
        drain_packets(&mut manager, &packets, crate::ui::DRAIN_BATCH);
        manager.enumerate_present_devices();
        manager.refresh();

        let source = if text_active.load(Ordering::Relaxed) {
            "text"
        } else {
            "binary"
        };
        let report = build_report(
            &manager,
            &baseline,
            opts.window,
            source,
            dropped.load(Ordering::Relaxed),
            text_active.load(Ordering::Relaxed),
            &filter,
        );
        if emit(&report, opts.json).is_err() {
            return Ok(()); // broken pipe: the reader left, that is not our error
        }
        if !opts.batch || stop.load(Ordering::Relaxed) {
            return Ok(());
        }
    }
}

/// Write one report. A `BrokenPipe` comes back as Err for the caller to
/// treat as a clean end; other write errors do too (stdout is gone).
fn emit(report: &Report, json: bool) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        let line = serde_json::to_string(report).expect("report serializes");
        writeln!(out, "{line}")?;
    } else {
        write!(out, "{}", render_text(report))?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::manager::DeviceManager;
    use crate::usbmon::parser::parse_usbmon_text_line;

    #[test]
    fn report_rates_come_from_window_deltas() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        let report = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(2),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let dev = &report.buses[0].devices[0];
        assert_eq!(dev.rx_bps, 500.0, "1000 bytes over a 2s window");
        assert_eq!(dev.total_rx_bytes, 1000);
        assert_eq!(report.total_rx_bps, 500.0);
        assert_eq!(report.buses[0].rx_bps, 500.0);
        assert_eq!(dev.endpoints[0].endpoint, 1);
        assert_eq!(dev.endpoints[0].direction, "in");
        assert_eq!(dev.endpoints[0].transfer_type, "bulk");
        assert_eq!(dev.endpoints[0].bps, 500.0);
    }

    #[test]
    fn estimated_marks_iso_devices_only_when_text_is_active() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let iso = parse_usbmon_text_line("ffff0000aaaa0001 200 C Zi:1:004:1 0:1:6672:0 32 27000 =")
            .unwrap();
        mgr.apply_packet(&iso);

        let not_estimated = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "text",
            0,
            false,
            &FilterSet::default(),
        );
        assert!(
            !not_estimated.buses[0].devices[0].estimated,
            "text_active=false must never mark a device estimated"
        );

        let estimated = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "text",
            0,
            true,
            &FilterSet::default(),
        );
        assert!(
            estimated.buses[0].devices[0].estimated,
            "an iso device under an active text source must be marked estimated"
        );
    }

    #[test]
    fn report_respects_device_filters() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let filter = FilterSet::parse(&["bus=2".into()]).unwrap();
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        let report = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &filter,
        );
        assert!(
            report.buses.is_empty(),
            "the only device is on bus 1, which does not match bus=2"
        );
    }

    #[test]
    fn json_report_serializes_with_version_1() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        let report = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["version"], 1);
        assert!(
            v["buses"][0]["devices"][0]["vendor_id"].is_null(),
            "an unread vendor id serializes as JSON null, not a placeholder string"
        );
    }

    #[test]
    fn render_text_lists_buses_and_devices() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        let report = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let text = render_text(&report);
        assert!(text.contains("bus 1"), "{text}");
        assert!(text.contains("1:4"), "{text}");
        assert!(text.contains("rx"), "{text}");
    }
}

//! Non-TUI reports: `--once` samples one window and prints, `--batch`
//! prints every window until interrupted. Never prompts.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::device::manager::DeviceManager;
use crate::filter::FilterSet;
use crate::usbmon::parser::{format_mbps, UsbPacket};

pub struct HeadlessOptions {
    pub json: bool,
    pub batch: bool,
    pub window: Duration,
    /// Whether reader threads were spawned for this run. When they were, a
    /// disconnected packet channel means capture failed, and the run must
    /// fail rather than report zeros forever. `--force` with no detected
    /// buses spawns no readers, so its empty reports stay legitimate.
    pub expect_capture: bool,
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
    /// `true`/`false` when an internal-device snapshot was loaded and this
    /// device did/didn't match it; `null` (`None`) when no snapshot exists,
    /// so a script can tell "external" apart from "unknown".
    pub internal: Option<bool>,
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
///
/// `elapsed` is the *measured* time since `baseline` was captured, not the
/// nominal `--window` value: a SIGINT/SIGTERM can end a window early, and
/// dividing by the requested length rather than the actual one would both
/// understate every rate and put a false `window_seconds` in the report.
/// Floored at 1ms so a pathological zero-elapsed window (e.g. a signal
/// landing in the same instant as `Baseline::capture`) cannot divide by zero.
pub fn build_report(
    manager: &DeviceManager,
    baseline: &Baseline,
    elapsed: Duration,
    source: &'static str,
    dropped: u64,
    text_active: bool,
    filter: &FilterSet,
) -> Report {
    // Read, not threaded as a parameter: `manager` is already an argument,
    // and adding an 8th argument alongside it would just duplicate state the
    // manager already carries (see `set_internal_snapshot`).
    let snapshot_loaded = manager.has_internal_snapshot();
    let window_secs = elapsed.as_secs_f64().max(0.001);
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
                        internal: snapshot_loaded.then_some(device.is_internal),
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
            "bus {} ({}) rx {:.2} MB/s tx {:.2} MB/s\n",
            bus.bus,
            format_mbps(bus.speed_mbps),
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
            let marker = if device.internal == Some(true) {
                "i"
            } else {
                " "
            };
            out.push_str(&format!(
                "  {}:{}  {}  {}  {}  {} {:.2} MB/s  {} {:.2} MB/s  {}\n",
                device.bus,
                device.address,
                marker,
                id,
                format_mbps(device.speed_mbps),
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

/// One bounded drain pass. `Disconnected` means the queue is empty AND every
/// reader thread has exited (the monitor's senders are all dropped), so no
/// packet can ever arrive again. Queued packets are always consumed before
/// that state surfaces, so nothing a dying reader captured is lost.
#[derive(Debug, PartialEq)]
enum DrainStatus {
    Alive,
    Disconnected,
}

fn drain(manager: &mut DeviceManager, packets: &Receiver<UsbPacket>) -> DrainStatus {
    for _ in 0..crate::ui::DRAIN_BATCH {
        match packets.try_recv() {
            Ok(packet) => manager.apply_packet(&packet),
            Err(TryRecvError::Empty) => return DrainStatus::Alive,
            Err(TryRecvError::Disconnected) => return DrainStatus::Disconnected,
        }
    }
    DrainStatus::Alive
}

/// Sample the manager's state on `opts.window`-second windows, printing a
/// report at the end of each. `--once` (`opts.batch == false`) prints one and
/// returns; `--batch` repeats until SIGINT/SIGTERM. A signal that lands
/// mid-window ends the wait early; the report that follows carries the true
/// measured elapsed time (see `build_report`), not the nominal `opts.window`.
///
/// When `opts.expect_capture` is set and every reader has stopped, the run
/// fails with an error instead of printing a report: a zero report after a
/// capture failure would read as a quiet bus, and automation would believe
/// it.
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
        let window_start = Instant::now();
        let deadline = window_start + opts.window;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            if drain(&mut manager, &packets) == DrainStatus::Disconnected && opts.expect_capture {
                return Err(anyhow!(
                    "every usbmon reader stopped; no capture source remains"
                ));
            }
            std::thread::sleep(Duration::from_millis(50).min(opts.window));
        }
        if drain(&mut manager, &packets) == DrainStatus::Disconnected && opts.expect_capture {
            return Err(anyhow!(
                "every usbmon reader stopped; no capture source remains"
            ));
        }
        manager.enumerate_present_devices();
        manager.refresh();

        // The true elapsed time, not `opts.window`: a SIGINT/SIGTERM can
        // break the wait loop above before the deadline (see `build_report`).
        let elapsed = window_start.elapsed();

        let source = if text_active.load(Ordering::Relaxed) {
            "text"
        } else {
            "binary"
        };
        let report = build_report(
            &manager,
            &baseline,
            elapsed,
            source,
            dropped.load(Ordering::Relaxed),
            text_active.load(Ordering::Relaxed),
            &filter,
        );
        if let Err(e) = emit(&report, opts.json) {
            if is_expected_write_failure(&e) {
                return Ok(()); // broken pipe: the reader left, that is not our error
            }
            return Err(e.into());
        }
        if !opts.batch || stop.load(Ordering::Relaxed) {
            return Ok(());
        }
    }
}

/// Write one report. A `BrokenPipe` comes back as Err for the caller to
/// treat as a clean end (see [`is_expected_write_failure`]); other write
/// errors come back as Err too, but are propagated as a real failure instead.
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

/// Whether a stdout write error is expected and should end the run quietly
/// (`Ok(())`, exit 0) rather than propagate. Only `BrokenPipe` is routine —
/// the reader left, e.g. `usbtop-ng --batch --json | head -n 1`. Anything
/// else (ENOSPC, a closed fd that is not a pipe, ...) means the report was
/// not actually written, which the caller should see as a nonzero exit
/// rather than silence.
fn is_expected_write_failure(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::BrokenPipe
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::manager::DeviceManager;
    use crate::usbmon::parser::parse_usbmon_text_line;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn drain_consumes_queued_packets_before_reporting_disconnected() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let (tx, rx) = sync_channel(4);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        tx.send(cb).unwrap();
        drop(tx);

        assert_eq!(drain(&mut mgr, &rx), DrainStatus::Disconnected);
        assert_eq!(
            mgr.buses[&1].devices[&4].bandwidth_stats.total_rx_bytes, 1000,
            "a dying reader's queued packets must land before the disconnect surfaces"
        );
    }

    #[test]
    fn drain_reports_alive_while_a_sender_exists() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let (tx, rx) = sync_channel::<crate::usbmon::parser::UsbPacket>(4);

        assert_eq!(drain(&mut mgr, &rx), DrainStatus::Alive);
        drop(tx);
    }

    #[test]
    fn run_fails_when_capture_was_expected_and_every_reader_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let (tx, rx) = sync_channel::<crate::usbmon::parser::UsbPacket>(1);
        drop(tx);

        let err = run(
            mgr,
            rx,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
            FilterSet::default(),
            HeadlessOptions {
                json: true,
                batch: false,
                window: Duration::from_millis(300),
                expect_capture: true,
            },
        )
        .expect_err("a dead capture channel must fail the run, not report zeros");
        assert!(err.to_string().contains("usbmon reader"));
    }

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
    fn report_window_seconds_reflects_the_measured_elapsed_time_not_a_nominal_value() {
        // A window cut short by SIGINT/SIGTERM passes its true measured
        // duration here, not the `--window` the user asked for: the report's
        // `window_seconds` must say so too, not the nominal length.
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        let report = build_report(
            &mgr,
            &baseline,
            Duration::from_millis(1500),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        assert_eq!(report.window_seconds, 1.5);
        assert_eq!(
            report.buses[0].devices[0].rx_bps,
            1000.0 / 1.5,
            "the rate divides by the same measured elapsed time as window_seconds reports"
        );
    }

    #[test]
    fn build_report_floors_a_zero_elapsed_window_instead_of_dividing_by_zero() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        let report = build_report(
            &mgr,
            &baseline,
            Duration::ZERO,
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        assert!(
            report.window_seconds > 0.0,
            "a zero-elapsed window must still report a positive window_seconds"
        );
        assert!(
            report.total_rx_bps.is_finite(),
            "a zero-elapsed window must not divide by zero into an infinite rate"
        );
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
    fn internal_field_is_null_without_a_snapshot_and_true_or_false_with_one() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);

        let no_snapshot = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        assert_eq!(
            no_snapshot.buses[0].devices[0].internal, None,
            "no snapshot loaded: internal is unknown, not false"
        );
        let v = serde_json::to_value(&no_snapshot).unwrap();
        assert!(v["buses"][0]["devices"][0]["internal"].is_null());

        // A snapshot IS now loaded (its contents don't matter here: the
        // device has no sysfs_path in this fixture, so `stamp_internal`
        // always stamps it false; only `has_internal_snapshot()` matters
        // for whether `internal` serializes as `null` or `Some(_)`).
        mgr.set_internal_snapshot(Some(std::sync::Arc::new(crate::snapshot::Snapshot {
            captured_unix: 0,
            devices: vec![],
        })));
        let snapshot_loaded_external = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        assert_eq!(
            snapshot_loaded_external.buses[0].devices[0].internal,
            Some(false)
        );

        mgr.buses
            .get_mut(&1)
            .unwrap()
            .devices
            .get_mut(&4)
            .unwrap()
            .is_internal = true;
        let snapshot_loaded_internal = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        assert_eq!(
            snapshot_loaded_internal.buses[0].devices[0].internal,
            Some(true)
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

    #[test]
    fn render_text_marks_internal_devices_with_an_i_cell_and_pads_external_ones() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        mgr.set_internal_snapshot(Some(std::sync::Arc::new(crate::snapshot::Snapshot {
            captured_unix: 0,
            devices: vec![],
        })));

        let external_report = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let external_text = render_text(&external_report);
        let external_row = external_text.lines().find(|l| l.contains("1:4")).unwrap();
        assert!(
            external_row.contains("1:4     ----:----"),
            "an external row's marker cell is a space: {external_row}"
        );

        mgr.buses
            .get_mut(&1)
            .unwrap()
            .devices
            .get_mut(&4)
            .unwrap()
            .is_internal = true;
        let internal_report = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let internal_text = render_text(&internal_report);
        let internal_row = internal_text.lines().find(|l| l.contains("1:4")).unwrap();
        assert!(
            internal_row.contains("1:4  i  ----:----"),
            "an internal row carries the i marker: {internal_row}"
        );
    }

    #[test]
    fn render_text_uses_the_shared_integral_bare_speed_format() {
        // format_mbps itself (integral bare, one-decimal fractional) is
        // covered in usbmon::parser; this pins that render_text actually
        // calls the shared function rather than a local reimplementation.
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        mgr.buses.get_mut(&1).unwrap().speed = crate::usbmon::parser::UsbSpeed::from_mbps(480.0);

        let baseline = Baseline::capture(&mgr);
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
        let bus_row = text.lines().find(|l| l.starts_with("bus 1")).unwrap();
        assert!(
            bus_row.contains("480 Mbps"),
            "bus row must use the shared bare-integral format: {bus_row}"
        );
    }

    #[test]
    fn render_text_keeps_one_decimal_for_a_fractional_device_speed() {
        // 1.5 Mbps (Low Speed) is the case a bare `{:.0}` would round away
        // to "2 Mbps"; render_text must keep the fractional digit.
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let cb = parse_usbmon_text_line("ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <").unwrap();
        mgr.apply_packet(&cb);
        mgr.buses
            .get_mut(&1)
            .unwrap()
            .devices
            .get_mut(&4)
            .unwrap()
            .speed = crate::usbmon::parser::UsbSpeed::from_mbps(1.5);

        let baseline = Baseline::capture(&mgr);
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
        let device_row = text.lines().find(|l| l.contains("1:4")).unwrap();
        assert!(
            device_row.contains("1.5 Mbps"),
            "device row must keep the fractional digit: {device_row}"
        );
    }

    #[test]
    fn broken_pipe_is_the_only_expected_write_failure() {
        assert!(is_expected_write_failure(&std::io::Error::from(
            std::io::ErrorKind::BrokenPipe
        )));
        assert!(!is_expected_write_failure(&std::io::Error::from(
            std::io::ErrorKind::WriteZero
        )));
        assert!(!is_expected_write_failure(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        assert!(!is_expected_write_failure(&std::io::Error::from(
            std::io::ErrorKind::Other
        )));
    }
}

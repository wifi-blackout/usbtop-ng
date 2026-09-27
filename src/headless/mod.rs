//! Non-TUI reports: `--once` samples one window and prints, `--batch`
//! prints every window until interrupted. Never prompts.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::capacity::{Basis, Chokepoint, CHOKE_FLOOR};
use crate::connector::PortIndex;
use crate::device::manager::DeviceManager;
use crate::filter::FilterSet;
use crate::findings::{Cause, Finding};
use crate::tunnel::{self, Tunnel};
use crate::usbmon::monitor::{CaptureStream, SourceFlags};
use crate::usbmon::parser::format_mbps;

pub mod export;

pub struct HeadlessOptions {
    pub json: bool,
    pub batch: bool,
    pub window: Duration,
    /// Whether reader threads were spawned for this run. When they were, a
    /// disconnected packet channel means capture failed, and the run must
    /// fail rather than report zeros forever. `--force` with no detected
    /// buses spawns no readers, so its empty reports stay legitimate.
    pub expect_capture: bool,
    /// `--output PATH`; `None` prints to stdout.
    pub output: Option<std::path::PathBuf>,
    /// Leads a file export; never printed to stdout.
    pub run_record: export::RunRecord,
    /// `--demand`: which rate the choke-point model assumes every device
    /// pushes (see `capacity::Basis`).
    pub demand: Basis,
}

#[derive(Serialize)]
pub struct Report {
    pub version: u32,
    pub timestamp: f64,
    pub window_seconds: f64,
    pub source: &'static str,
    pub dropped_packets: u64,
    /// Kernel-side drops the mmap ring readers' `MON_IOCG_STATS` reported
    /// (see `usbmon::mmap_ring::MmapReader` and
    /// `usbmon::monitor::MonitorHandle::kernel_dropped`) — distinct from
    /// [`Self::dropped_packets`] (a full channel, after the kernel already
    /// delivered the packet). Always 0 on a session not using the mmap
    /// interface; [`build_report`] leaves it at 0, and [`run`] fills it in
    /// from the live counter afterward, since it is not part of the
    /// manager/baseline state `build_report` otherwise diffs.
    pub kernel_dropped_packets: u64,
    pub total_rx_bps: f64,
    pub total_tx_bps: f64,
    pub buses: Vec<BusReport>,
    /// The PCIe side of every Thunderbolt or USB4 tunnel: one entry per
    /// root port with tunneled functions under it (see
    /// `tunnel::read_tunnels`). Empty when there is none; always empty in
    /// a fixture replay, which carries no PCI tree.
    pub tunnels: Vec<TunnelReport>,
    /// Devices linked below the speed they support, with the cause the
    /// topology proves (see `findings::analyze`); only devices the report
    /// lists. Empty when there is nothing to call out.
    pub findings: Vec<FindingReport>,
    /// The basis the choke points were computed at, `"link"` or
    /// `"capability"` (see `capacity::Basis`).
    pub demand_basis: &'static str,
    /// The breathing room applied: a hub is listed only when its subtree
    /// asks at least this many times its link's capacity.
    pub choke_floor: f64,
    /// Hubs whose links are asked more than they can carry, worst first
    /// (see `capacity::analyze`); only hubs the report lists.
    pub chokepoints: Vec<ChokepointReport>,
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
    /// The highest link rate the device says it supports, in Mbps, and
    /// where that came from (`"bos"` or `"bcd_usb"`); `null` when unknown.
    pub capability_mbps: Option<f64>,
    pub capability_source: Option<&'static str>,
    pub endpoints: Vec<EndpointReport>,
}

/// One finding as the JSON report carries it: the device, the numbers,
/// the cause as a `snake_case` tag, and the sentence the text report and
/// the TUI show. Fields a cause does not use are `null`.
#[derive(Serialize)]
pub struct FindingReport {
    pub bus: u8,
    pub address: u8,
    pub path: String,
    pub port: Option<String>,
    pub link_mbps: f64,
    pub capability_mbps: f64,
    pub capability_source: &'static str,
    pub cause: Option<&'static str>,
    pub peer_port: Option<String>,
    pub upstream: Option<String>,
    pub limit_mbps: Option<f64>,
    pub message: String,
}

impl From<&Finding> for FindingReport {
    fn from(finding: &Finding) -> Self {
        let (peer_port, upstream, limit_mbps) = match &finding.cause {
            Some(Cause::SuperSpeedSideEmpty { peer_port }) => (Some(peer_port.clone()), None, None),
            Some(Cause::UpstreamHubLink { hub, hub_link }) => {
                (None, Some(hub.clone()), Some(hub_link.to_mbps()))
            }
            Some(Cause::Usb2OnlyPort { hub, .. }) => (None, Some(hub.clone()), None),
            Some(Cause::HostPortMax { max }) => (None, None, Some(max.to_mbps())),
            Some(Cause::Usb2OnlyHostPort) | Some(Cause::UpstreamPermits) | None => {
                (None, None, None)
            }
        };
        FindingReport {
            bus: finding.bus,
            address: finding.address,
            path: finding.path.clone(),
            port: finding.port.clone(),
            link_mbps: finding.link.to_mbps(),
            capability_mbps: finding.capability.speed.to_mbps(),
            capability_source: finding.capability.source.as_str(),
            cause: finding.cause.as_ref().map(Cause::kind),
            peer_port,
            upstream,
            limit_mbps,
            message: finding.message(),
        }
    }
}

/// One tunnel as the JSON report carries it (see `tunnel::Tunnel`).
#[derive(Serialize)]
pub struct TunnelReport {
    pub root_port: String,
    pub router: Option<RouterReport>,
    /// Addresses among `functions` that are USB controllers; join with
    /// `buses[].controller`.
    pub controllers: Vec<String>,
    pub functions: Vec<PciFunctionReport>,
}

/// The joined router (see `tunnel::Router`); `null` when the join named none.
#[derive(Serialize)]
pub struct RouterReport {
    pub name: String,
    pub vendor_name: Option<String>,
    pub device_name: Option<String>,
    pub generation: Option<u32>,
    pub rx_gbps: Option<f64>,
    pub tx_gbps: Option<f64>,
    pub rx_lanes: Option<u32>,
    pub tx_lanes: Option<u32>,
    pub authorized: Option<u32>,
    pub security: Option<String>,
}

/// One tunneled function (see `tunnel::PciFunction`).
#[derive(Serialize)]
pub struct PciFunctionReport {
    pub address: String,
    /// The 24-bit class code as six hex digits, `020000`, no prefix, as
    /// the IDs.
    pub class: String,
    pub class_name: String,
    pub vendor_id: String,
    pub device_id: String,
    pub driver: Option<String>,
    pub interface: Option<String>,
    pub runtime_status: Option<String>,
    pub awake: bool,
    pub link_gts: Option<f64>,
    pub link_width: Option<u32>,
    pub max_link_gts: Option<f64>,
}

impl From<&Tunnel> for TunnelReport {
    fn from(t: &Tunnel) -> Self {
        TunnelReport {
            root_port: t.root_port.clone(),
            router: t.router.as_ref().map(|r| RouterReport {
                name: r.name.clone(),
                vendor_name: r.vendor_name.clone(),
                device_name: r.device_name.clone(),
                generation: r.generation,
                rx_gbps: r.rx_gbps,
                tx_gbps: r.tx_gbps,
                rx_lanes: r.rx_lanes,
                tx_lanes: r.tx_lanes,
                authorized: r.authorized,
                security: r.security.clone(),
            }),
            controllers: t
                .functions
                .iter()
                .filter(|f| f.is_usb_controller())
                .map(|f| f.address.clone())
                .collect(),
            functions: t
                .functions
                .iter()
                .map(|f| PciFunctionReport {
                    address: f.address.clone(),
                    class: format!("{:06x}", f.class),
                    class_name: f.class_name.clone(),
                    vendor_id: format!("{:04x}", f.vendor_id),
                    device_id: format!("{:04x}", f.device_id),
                    driver: f.driver.clone(),
                    interface: f.interface.clone(),
                    runtime_status: f.runtime_status.clone(),
                    awake: f.awake,
                    link_gts: f.link.as_ref().map(|l| l.gts),
                    link_width: f.link.as_ref().map(|l| l.width),
                    max_link_gts: f.max_gts,
                })
                .collect(),
        }
    }
}

/// The label the text report prints for a tunnel: the TUI's.
fn tunnel_label(t: &TunnelReport) -> String {
    match &t.router {
        Some(r) => tunnel::router_label(&r.name, r.device_name.as_deref(), r.rx_lanes, r.rx_gbps),
        None => tunnel::NO_ROUTER_LABEL.to_string(),
    }
}

/// One device below a choke point and what it asks, as the JSON report
/// carries it (see `capacity::Contributor`).
#[derive(Serialize)]
pub struct ContributorReport {
    pub path: String,
    pub demand_mbps: f64,
}

/// One choke point as the JSON report carries it (see `capacity::Chokepoint`).
#[derive(Serialize)]
pub struct ChokepointReport {
    pub bus: u8,
    pub address: u8,
    pub path: String,
    pub port: Option<String>,
    pub capacity_mbps: f64,
    pub demand_mbps: f64,
    pub ratio: f64,
    pub devices: usize,
    pub top: Vec<ContributorReport>,
    pub message: String,
}

impl From<&Chokepoint> for ChokepointReport {
    fn from(c: &Chokepoint) -> Self {
        ChokepointReport {
            bus: c.bus,
            address: c.address,
            path: c.path.clone(),
            port: c.port.clone(),
            capacity_mbps: c.capacity_mbps,
            demand_mbps: c.demand_mbps,
            ratio: c.ratio,
            devices: c.devices,
            top: c
                .top
                .iter()
                .map(|t| ContributorReport {
                    path: t.path.clone(),
                    demand_mbps: t.demand_mbps,
                })
                .collect(),
            message: c.message(),
        }
    }
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

/// The facts of one sample window that a report carries beside the
/// manager's state: which usbmon interface fed it, how many packets the
/// channel dropped, and whether the text interface was active (its
/// isochronous figures are estimates).
#[derive(Clone, Copy)]
pub struct WindowFacts {
    pub source: &'static str,
    pub dropped: u64,
    pub text_active: bool,
}

/// Build one report from the manager's current state and a `baseline` taken
/// at the start of the window. Pure over the manager's state except for one
/// read-only scan of the manager's own device directories for their port
/// objects (the connector index the findings need), no clock reads other
/// than the `timestamp` field. That scan runs once per report — so once per
/// window under `--batch` — and reads nothing outside the device
/// directories the manager already knows, which under `--replay` are the
/// bundle's own `sysfs/`.
///
/// `elapsed` is the *measured* time since `baseline` was captured, not the
/// nominal `--window` value: a SIGINT/SIGTERM can end a window early, and
/// dividing by the requested length rather than the actual one would both
/// understate every rate and put a false `window_seconds` in the report.
/// Floored at 1ms so a pathological zero-elapsed window (e.g. a signal
/// landing in the same instant as `Baseline::capture`) cannot divide by zero.
///
/// The choke points are computed at `basis`. The tunnels are the caller's
/// read of the PCI side (see `tunnel::read_tunnels`); a replay passes none.
pub fn build_report_at(
    basis: Basis,
    manager: &DeviceManager,
    baseline: &Baseline,
    elapsed: Duration,
    facts: WindowFacts,
    filter: &FilterSet,
    tunnels: &[Tunnel],
) -> Report {
    // Read, not threaded as a parameter: `manager` is already an argument,
    // and another argument alongside it would just duplicate state the
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
                        estimated: facts.text_active && device.has_iso_traffic(),
                        internal: snapshot_loaded.then_some(device.is_internal),
                        capability_mbps: device.capability.as_ref().map(|c| c.speed.to_mbps()),
                        capability_source: device.capability.as_ref().map(|c| c.source.as_str()),
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

    // The same scan `ui::sync_from` does: bounded to the manager's own
    // device directories, under the bundle's `sysfs/` in replay.
    let index = PortIndex::scan_devices(
        manager
            .buses
            .values()
            .flat_map(|bus| bus.devices.values())
            .filter_map(|device| device.sysfs_path.as_deref()),
    );
    let listed: HashSet<(u8, u8)> = bus_reports
        .iter()
        .flat_map(|bus| bus.devices.iter().map(|d| (d.bus, d.address)))
        .collect();
    let findings = crate::findings::analyze(manager, &index)
        .iter()
        .filter(|f| listed.contains(&(f.bus, f.address)))
        .map(FindingReport::from)
        .collect();
    let chokepoints = crate::capacity::analyze(manager, &index, basis)
        .iter()
        .filter(|c| listed.contains(&(c.bus, c.address)))
        .map(ChokepointReport::from)
        .collect();

    Report {
        version: 1,
        timestamp,
        window_seconds: window_secs,
        source: facts.source,
        dropped_packets: facts.dropped,
        // Filled in by `run` after this call, from the live kernel-drop
        // counter -- see the field's own doc comment.
        kernel_dropped_packets: 0,
        total_rx_bps,
        total_tx_bps,
        buses: bus_reports,
        tunnels: tunnels.iter().map(TunnelReport::from).collect(),
        findings,
        demand_basis: basis.as_str(),
        choke_floor: CHOKE_FLOOR,
        chokepoints,
    }
}

/// [`build_report_at`] at the link basis, which is the view the committed
/// goldens hold. Only the tests call it: the replay path and `run` both
/// choose a basis explicitly, so the shipped binary reaches
/// [`build_report_at`] directly.
#[cfg(test)]
pub fn build_report(
    manager: &DeviceManager,
    baseline: &Baseline,
    elapsed: Duration,
    source: &'static str,
    dropped: u64,
    text_active: bool,
    filter: &FilterSet,
) -> Report {
    build_report_at(
        Basis::Link,
        manager,
        baseline,
        elapsed,
        WindowFacts {
            source,
            dropped,
            text_active,
        },
        filter,
        &[],
    )
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

/// `vid:pid` of the listed device, or `----:----`.
fn device_id_cell(report: &Report, bus: u8, address: u8) -> String {
    report
        .buses
        .iter()
        .flat_map(|b| b.devices.iter())
        .find(|d| d.bus == bus && d.address == address)
        .map_or_else(
            || "----:----".to_string(),
            |d| match (&d.vendor_id, &d.product_id) {
                (Some(v), Some(p)) => format!("{v}:{p}"),
                _ => "----:----".to_string(),
            },
        )
}

/// Render a report as plain text: a `ts=` line, one header per bus, one
/// indented row per device, then `tunnels: none` or `tunnels: N` with one
/// line per tunnel (root port, label, its USB controllers) and one
/// indented line per function, then the `findings:` section — `none`, or a
/// count and one indented line per call-out (see [`Report::findings`]) —
/// and a closing `chokepoints:` section, `none` or a count and one
/// indented line per choked hub link (see [`Report::chokepoints`]).
/// `~rx`/`~tx` marks a device whose rate is `estimated` (see
/// [`DeviceReport::estimated`]).
pub fn render_text(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "ts={:.3} window={:.2}s source={} dropped={} kdropped={}\n",
        report.timestamp,
        report.window_seconds,
        report.source,
        report.dropped_packets,
        report.kernel_dropped_packets
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
    if report.tunnels.is_empty() {
        out.push_str("tunnels: none\n");
    } else {
        out.push_str(&format!("tunnels: {}\n", report.tunnels.len()));
        for tunnel in &report.tunnels {
            let controllers = if tunnel.controllers.is_empty() {
                "none".to_string()
            } else {
                tunnel.controllers.join(", ")
            };
            out.push_str(&format!(
                "  {}  {}  controllers {}\n",
                tunnel.root_port,
                tunnel_label(tunnel),
                controllers
            ));
            for f in &tunnel.functions {
                out.push_str(&format!(
                    "    {}  {}  {}  {}  {}  {}:{}\n",
                    f.address,
                    f.class_name,
                    tunnel::link_text(f.link_gts, f.link_width, f.runtime_status.as_deref()),
                    f.driver.as_deref().unwrap_or("no driver"),
                    f.interface.as_deref().unwrap_or("-"),
                    f.vendor_id,
                    f.device_id
                ));
            }
        }
    }
    if report.findings.is_empty() {
        out.push_str("findings: none\n");
    } else {
        out.push_str(&format!("findings: {}\n", report.findings.len()));
        for finding in &report.findings {
            let id = device_id_cell(report, finding.bus, finding.address);
            out.push_str(&format!(
                "  {}:{}  {}  {}  {}\n",
                finding.bus, finding.address, finding.path, id, finding.message
            ));
        }
    }
    if report.chokepoints.is_empty() {
        out.push_str("chokepoints: none\n");
    } else {
        out.push_str(&format!("chokepoints: {}\n", report.chokepoints.len()));
        for point in &report.chokepoints {
            let id = device_id_cell(report, point.bus, point.address);
            out.push_str(&format!(
                "  hub {} ({}:{}, {}) {}\n",
                point.path, point.bus, point.address, id, point.message
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

/// [`drain`]'s per-channel body, generic over which [`CaptureStream`]
/// variant it is draining: `apply` is `DeviceManager::apply_packet` for the
/// `Packets` arm, `DeviceManager::apply_delta` for the `Deltas` arm.
fn drain_channel<T>(
    manager: &mut DeviceManager,
    rx: &Receiver<T>,
    apply: fn(&mut DeviceManager, &T),
) -> DrainStatus {
    for _ in 0..crate::ui::DRAIN_BATCH {
        match rx.try_recv() {
            Ok(item) => apply(manager, &item),
            Err(TryRecvError::Empty) => return DrainStatus::Alive,
            Err(TryRecvError::Disconnected) => return DrainStatus::Disconnected,
        }
    }
    DrainStatus::Alive
}

/// Dispatch on which capture backend `capture` is: usbmon's `Packets` via
/// `apply_packet`, the eBPF backend's `Deltas` via `apply_delta`. The
/// `Packets` arm is exactly what this function did before `CaptureStream`
/// existed -- this only adds the dispatch in front of it.
fn drain(manager: &mut DeviceManager, capture: &CaptureStream) -> DrainStatus {
    match capture {
        CaptureStream::Packets(packets) => {
            drain_channel(manager, packets, DeviceManager::apply_packet)
        }
        CaptureStream::Deltas(deltas) => drain_channel(manager, deltas, DeviceManager::apply_delta),
    }
}

/// The `Report::source` label for `capture`: `"ebpf"` for the eBPF backend's
/// exact byte counts, or usbmon's own `"text"`/`"mmap"`/`"binary"` split for a
/// `Packets` session. `text_active` and `mmap_active` come from
/// `monitor::run_source_chain`, which sets exactly one to true for the
/// interface actually running; text wins if both somehow read true (it is the
/// terminal fallback), then mmap, else the `read()`-based binary interface.
/// Both are always `false` for the life of a `Deltas` session, which spawns no
/// usbmon reader, so the eBPF arm never consults them.
fn capture_source_label(
    capture: &CaptureStream,
    text_active: bool,
    mmap_active: bool,
) -> &'static str {
    match capture {
        CaptureStream::Deltas(_) => "ebpf",
        CaptureStream::Packets(_) if text_active => "text",
        CaptureStream::Packets(_) if mmap_active => "mmap",
        CaptureStream::Packets(_) => "binary",
    }
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
    capture: CaptureStream,
    dropped: Arc<AtomicU64>,
    kernel_dropped: Arc<AtomicU64>,
    flags: SourceFlags,
    filter: FilterSet,
    opts: HeadlessOptions,
) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop))?;

    let mut sink = export::ReportSink::open(opts.output.as_deref(), &opts.run_record, opts.json)?;

    loop {
        manager.enumerate_present_devices();
        let baseline = Baseline::capture(&manager);
        let window_start = Instant::now();
        let deadline = window_start + opts.window;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            if drain(&mut manager, &capture) == DrainStatus::Disconnected && opts.expect_capture {
                return Err(anyhow!(
                    "every usbmon reader stopped; no capture source remains"
                ));
            }
            std::thread::sleep(Duration::from_millis(50).min(opts.window));
        }
        if drain(&mut manager, &capture) == DrainStatus::Disconnected && opts.expect_capture {
            return Err(anyhow!(
                "every usbmon reader stopped; no capture source remains"
            ));
        }
        manager.enumerate_present_devices();
        manager.refresh();

        // The true elapsed time, not `opts.window`: a SIGINT/SIGTERM can
        // break the wait loop above before the deadline (see `build_report`).
        let elapsed = window_start.elapsed();

        let source = capture_source_label(
            &capture,
            flags.text_active.load(Ordering::Relaxed),
            flags.mmap_active.load(Ordering::Relaxed),
        );
        // The PCI side of every tunnel, read fresh per window: the report
        // is a sample of the host, and a dock can come and go between two.
        let tunnels = tunnel::read_tunnels(
            Path::new(tunnel::PCI_DEVICES),
            Path::new(tunnel::THUNDERBOLT_DEVICES),
        );
        let mut report = build_report_at(
            opts.demand,
            &manager,
            &baseline,
            elapsed,
            WindowFacts {
                source,
                dropped: dropped.load(Ordering::Relaxed),
                text_active: flags.text_active.load(Ordering::Relaxed),
            },
            &filter,
            &tunnels,
        );
        // Not a `build_report` parameter (see the field's doc comment): the
        // kernel-drop count is read straight from the live counter here,
        // same source `dropped`/`text_active` load from just above.
        report.kernel_dropped_packets = kernel_dropped.load(Ordering::Relaxed);
        if let Err(e) = sink.write(&report, opts.json) {
            if opts.output.is_none() && is_expected_write_failure(&e) {
                break; // broken pipe on stdout: the reader left, that is not our error
            }
            return Err(anyhow!("could not write the report: {e}"));
        }
        if !opts.batch || stop.load(Ordering::Relaxed) {
            break;
        }
    }
    if let Some((n, path)) = sink.finish() {
        eprintln!("wrote {n} report(s) to {}", path.display());
    }
    Ok(())
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
    use crate::tunnel::{PciFunction, PciLink, Router};
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

        let capture = CaptureStream::Packets(rx);
        assert_eq!(drain(&mut mgr, &capture), DrainStatus::Disconnected);
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

        let capture = CaptureStream::Packets(rx);
        assert_eq!(drain(&mut mgr, &capture), DrainStatus::Alive);
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
            CaptureStream::Packets(rx),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            SourceFlags {
                text_active: Arc::new(AtomicBool::new(false)),
                mmap_active: Arc::new(AtomicBool::new(false)),
            },
            FilterSet::default(),
            HeadlessOptions {
                json: true,
                batch: false,
                window: Duration::from_millis(300),
                expect_capture: true,
                output: None,
                run_record: export::RunRecord {
                    record: "run",
                    usbtop_ng: String::new(),
                    features: vec![],
                    started_unix: 0,
                    window_seconds: 1.0,
                    batch: false,
                    filters: vec![],
                    command: vec![],
                    backend: "binary".into(),
                    kernel: String::new(),
                    os: String::new(),
                    arch: "x86_64",
                    buses: vec![],
                },
                demand: Basis::Link,
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

    /// `build_report` itself never learns about kernel-side drops -- that
    /// wiring lives in `run`, which reads the live counter and fills the
    /// field in afterward (see the field's doc comment) -- but `render_text`
    /// must still surface whatever value the field ends up holding.
    #[test]
    fn render_text_includes_kernel_dropped_packets() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let mut report = build_report(
            &mgr,
            &baseline,
            Duration::from_secs(1),
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        assert_eq!(
            report.kernel_dropped_packets, 0,
            "build_report has no kernel-drop input of its own to report from"
        );

        report.kernel_dropped_packets = 9;
        let text = render_text(&report);
        assert!(text.contains("kdropped=9"), "{text}");
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

    /// A USB 3 device (bcdUSB 3.20, no BOS) at 480 on a paired root port
    /// whose SuperSpeed twin is empty, and a plain device: the report
    /// carries one finding and per-device capability fields.
    fn tree_with_a_finding() -> (tempfile::TempDir, DeviceManager) {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("devices");
        let ctrl = temp.path().join("0000:00:14.0");
        std::fs::create_dir_all(&base).unwrap();
        let write = |dir: &std::path::Path, attrs: &[(&str, &str)]| {
            std::fs::create_dir_all(dir).unwrap();
            for (k, v) in attrs {
                std::fs::write(dir.join(k), format!("{v}\n")).unwrap();
            }
        };
        let usb3 = ctrl.join("usb3");
        let usb4 = ctrl.join("usb4");
        write(&usb3, &[("busnum", "3"), ("devnum", "1"), ("speed", "480")]);
        write(
            &usb4,
            &[("busnum", "4"), ("devnum", "1"), ("speed", "5000")],
        );
        symlink(&usb3, base.join("usb3")).unwrap();
        symlink(&usb4, base.join("usb4")).unwrap();
        let p3 = usb3.join("usb3:1.0").join("usb3-port1");
        let p4 = usb4.join("usb4:1.0").join("usb4-port1");
        std::fs::create_dir_all(&p3).unwrap();
        std::fs::create_dir_all(&p4).unwrap();
        symlink(&p4, p3.join("peer")).unwrap();
        symlink(&p3, p4.join("peer")).unwrap();
        write(
            &base.join("3-1"),
            &[
                ("busnum", "3"),
                ("devnum", "2"),
                ("speed", "480"),
                ("version", "3.20"),
                ("idVendor", "0bda"),
                ("idProduct", "9210"),
            ],
        );
        let mut mgr = DeviceManager::with_sysfs_base(base);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        (temp, mgr)
    }

    #[test]
    fn json_report_carries_findings_and_per_device_capability() {
        let (_temp, mgr) = tree_with_a_finding();
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
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["version"], 1, "additive fields do not bump the schema");
        let findings = v["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f["bus"], 3);
        assert_eq!(f["address"], 2);
        assert_eq!(f["path"], "3-1");
        assert_eq!(f["port"], "usb3-port1");
        assert_eq!(f["link_mbps"], 480.0);
        assert_eq!(f["capability_mbps"], 5000.0);
        assert_eq!(f["capability_source"], "bcd_usb");
        assert_eq!(f["cause"], "superspeed_side_empty");
        assert_eq!(f["peer_port"], "usb4-port1");
        assert!(f["upstream"].is_null());
        assert!(f["limit_mbps"].is_null());
        assert!(f["message"]
            .as_str()
            .unwrap()
            .starts_with("linked at 480M, supports 5G (from bcdUSB): "));
        let devices = v["buses"][0]["devices"].as_array().unwrap();
        let root = devices.iter().find(|d| d["address"] == 1).unwrap();
        assert!(root["capability_mbps"].is_null());
        assert!(root["capability_source"].is_null());
        let dev = devices.iter().find(|d| d["address"] == 2).unwrap();
        assert_eq!(dev["capability_mbps"], 5000.0);
        assert_eq!(dev["capability_source"], "bcd_usb");
    }

    /// A 480 hub on a root port with two 480 devices: one choke point.
    fn tree_with_a_choke() -> (tempfile::TempDir, DeviceManager) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("devices");
        let ctrl = temp.path().join("0000:00:14.0");
        std::fs::create_dir_all(&base).unwrap();
        let write = |dir: &std::path::Path, attrs: &[(&str, &str)]| {
            std::fs::create_dir_all(dir).unwrap();
            for (k, v) in attrs {
                std::fs::write(dir.join(k), format!("{v}\n")).unwrap();
            }
        };
        let usb1 = ctrl.join("usb1");
        write(&usb1, &[("busnum", "1"), ("devnum", "1"), ("speed", "480")]);
        std::os::unix::fs::symlink(&usb1, base.join("usb1")).unwrap();
        std::fs::create_dir_all(usb1.join("usb1:1.0").join("usb1-port1")).unwrap();
        let hub = base.join("1-1");
        write(
            &hub,
            &[
                ("busnum", "1"),
                ("devnum", "2"),
                ("speed", "480"),
                ("idVendor", "1a40"),
                ("idProduct", "0201"),
            ],
        );
        for n in 1..=2 {
            std::fs::create_dir_all(hub.join("1-1:1.0").join(format!("1-1-port{n}"))).unwrap();
            write(
                &base.join(format!("1-1.{n}")),
                &[
                    ("busnum", "1"),
                    ("devnum", &(n + 2).to_string()),
                    ("speed", "480"),
                ],
            );
        }
        let mut mgr = DeviceManager::with_sysfs_base(base);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        (temp, mgr)
    }

    #[test]
    fn json_report_carries_the_choke_points_and_the_basis() {
        let (_temp, mgr) = tree_with_a_choke();
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
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["demand_basis"], "link");
        assert_eq!(v["choke_floor"], 1.25);
        let points = v["chokepoints"].as_array().unwrap();
        assert_eq!(points.len(), 1);
        let c = &points[0];
        assert_eq!(
            (c["bus"].as_u64(), c["address"].as_u64()),
            (Some(1), Some(2))
        );
        assert_eq!(c["path"], "1-1");
        assert_eq!(c["port"], "usb1-port1");
        assert_eq!(c["capacity_mbps"], 384.0);
        assert_eq!(c["demand_mbps"], 768.0);
        assert_eq!(c["ratio"], 2.0);
        assert_eq!(c["devices"], 2);
        assert_eq!(c["top"].as_array().unwrap().len(), 2);
        assert_eq!(c["top"][0]["path"], "1-1.1");
        assert_eq!(c["top"][0]["demand_mbps"], 384.0);
        assert_eq!(c["message"], "384M carries 2 devices asking 768M: 2.00x");

        let report = build_report_at(
            crate::capacity::Basis::Capability,
            &mgr,
            &baseline,
            Duration::from_secs(1),
            WindowFacts {
                source: "binary",
                dropped: 0,
                text_active: false,
            },
            &FilterSet::default(),
            &[],
        );
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["demand_basis"], "capability");
    }

    #[test]
    fn choke_points_follow_the_filter() {
        let (_temp, mgr) = tree_with_a_choke();
        let baseline = Baseline::capture(&mgr);
        let filter = FilterSet::parse(&["bus=2".to_string()]).unwrap();
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
            report.chokepoints.is_empty(),
            "the hub is on bus 1, which the filter excludes"
        );
    }

    #[test]
    fn render_text_lists_the_choke_points_after_the_findings() {
        let (_temp, mgr) = tree_with_a_choke();
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
        assert!(
            text.contains("findings: none\nchokepoints: 1\n  hub 1-1 (1:2, 1a40:0201) 384M carries 2 devices asking 768M: 2.00x\n\n"),
            "{text}"
        );
    }

    #[test]
    fn render_text_says_chokepoints_none_when_there_are_none() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
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
        assert!(render_text(&report).ends_with("findings: none\nchokepoints: none\n\n"));
    }

    #[test]
    fn findings_follow_the_filter() {
        let (_temp, mgr) = tree_with_a_finding();
        let baseline = Baseline::capture(&mgr);
        let filter = FilterSet::parse(&["bus=4".to_string()]).unwrap();
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
            report.findings.is_empty(),
            "the flagged device is on bus 3, which the filter excludes"
        );
    }

    #[test]
    fn render_text_ends_with_the_findings_and_chokepoints_sections() {
        let (_temp, mgr) = tree_with_a_finding();
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
        assert!(text.contains("\nfindings: 1\n  3:2  3-1  0bda:9210  linked at 480M, supports 5G (from bcdUSB): the SuperSpeed side of this connector (usb4-port1) is empty, so the link came up at USB 2 speed; check the cable or the port\nchokepoints: none\n\n"), "{text}");
        assert!(
            text.ends_with("\n\n"),
            "the blank terminator still ends the report"
        );
    }

    #[test]
    fn render_text_says_findings_none_when_there_are_none() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
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
        assert!(
            text.contains("\nfindings: none\nchokepoints:"),
            "the findings section precedes the choke section: {text}"
        );
    }

    fn sample_tunnel() -> Tunnel {
        Tunnel {
            root_port: "0000:00:07.1".into(),
            router: Some(Router {
                name: "0-3".into(),
                vendor_name: Some("CalDigit, Inc.".into()),
                device_name: Some("Element Hub".into()),
                generation: Some(4),
                rx_gbps: Some(20.0),
                tx_gbps: Some(20.0),
                rx_lanes: Some(2),
                tx_lanes: Some(2),
                authorized: Some(1),
                security: Some("none".into()),
            }),
            functions: vec![
                PciFunction {
                    address: "0000:2d:00.1".into(),
                    class: 0x020000,
                    class_name: "Ethernet".into(),
                    vendor_id: 0x1d6a,
                    device_id: 0x14c0,
                    driver: Some("atlantic".into()),
                    interface: Some("enp45s0".into()),
                    runtime_status: Some("active".into()),
                    awake: true,
                    link: Some(PciLink { gts: 8.0, width: 1 }),
                    max_gts: Some(16.0),
                },
                PciFunction {
                    address: "0000:2e:00.0".into(),
                    class: 0x0c0330,
                    class_name: "USB controller".into(),
                    vendor_id: 0x8086,
                    device_id: 0x0b27,
                    driver: Some("xhci_hcd".into()),
                    interface: None,
                    runtime_status: Some("suspended".into()),
                    awake: false,
                    link: None,
                    max_gts: None,
                },
            ],
        }
    }

    fn report_with(tunnels: &[Tunnel], filter: &FilterSet) -> Report {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        build_report_at(
            Basis::Link,
            &mgr,
            &baseline,
            Duration::from_secs(1),
            WindowFacts {
                source: "none",
                dropped: 0,
                text_active: false,
            },
            filter,
            tunnels,
        )
    }

    #[test]
    fn json_report_carries_a_tunnel_with_its_router_and_functions() {
        let report = report_with(&[sample_tunnel()], &FilterSet::default());
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["version"], 1, "additive fields do not bump the schema");
        let tunnels = v["tunnels"].as_array().unwrap();
        assert_eq!(tunnels.len(), 1);
        let t = &tunnels[0];
        assert_eq!(t["root_port"], "0000:00:07.1");
        assert_eq!(t["router"]["name"], "0-3");
        assert_eq!(t["router"]["device_name"], "Element Hub");
        assert_eq!(t["router"]["rx_gbps"], 20.0);
        assert_eq!(t["router"]["rx_lanes"], 2);
        assert_eq!(t["router"]["authorized"], 1);
        assert_eq!(t["router"]["security"], "none");
        assert_eq!(t["controllers"], serde_json::json!(["0000:2e:00.0"]));
        let nic = &t["functions"][0];
        assert_eq!(nic["address"], "0000:2d:00.1");
        assert_eq!(nic["class"], "020000");
        assert_eq!(nic["class_name"], "Ethernet");
        assert_eq!(nic["vendor_id"], "1d6a");
        assert_eq!(nic["device_id"], "14c0");
        assert_eq!(nic["driver"], "atlantic");
        assert_eq!(nic["interface"], "enp45s0");
        assert_eq!(nic["runtime_status"], "active");
        assert_eq!(nic["awake"], true);
        assert_eq!(nic["link_gts"], 8.0);
        assert_eq!(nic["link_width"], 1);
        assert_eq!(nic["max_link_gts"], 16.0);
        let xhci = &t["functions"][1];
        assert_eq!(xhci["awake"], false);
        assert!(xhci["link_gts"].is_null());
        assert!(xhci["interface"].is_null());
    }

    #[test]
    fn tunnels_ignore_the_filter() {
        let filter = FilterSet::parse(&["bus=9".into()]).unwrap();
        let report = report_with(&[sample_tunnel()], &filter);
        assert_eq!(
            report.tunnels.len(),
            1,
            "a USB filter narrows buses, never the tunnels"
        );
    }

    #[test]
    fn render_text_prints_the_tunnels_section_after_the_buses_and_before_findings() {
        let text = render_text(&report_with(&[sample_tunnel()], &FilterSet::default()));
        assert!(
            text.contains(
                "tunnels: 1\n  0000:00:07.1  Thunderbolt 0-3 Element Hub · 2×20 Gb/s  controllers 0000:2e:00.0\n    0000:2d:00.1  Ethernet  8 GT/s ×1  atlantic  enp45s0  1d6a:14c0\n    0000:2e:00.0  USB controller  asleep  xhci_hcd  -  8086:0b27\nfindings: none\n"
            ),
            "{text}"
        );
        let mut bare = sample_tunnel();
        bare.router = None;
        bare.functions.truncate(1);
        let text = render_text(&report_with(&[bare], &FilterSet::default()));
        assert!(
            text.contains("tunnels: 1\n  0000:00:07.1  external PCIe port  controllers none\n"),
            "{text}"
        );
    }

    #[test]
    fn render_text_says_tunnels_none_when_there_are_none() {
        let text = render_text(&report_with(&[], &FilterSet::default()));
        assert!(text.contains("\ntunnels: none\nfindings: none\n"), "{text}");
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

/// [`drain`]'s `Deltas` arm and its interaction with `expect_capture`,
/// exercised with a fixture `TrafficDelta` channel. Gated on the `ebpf`
/// feature for the same reason as `ui::capture_dispatch_tests`: it keeps the
/// default and `--features integration` test counts unchanged, since those
/// configs already cover the `Packets` arm in `tests` above.
#[cfg(all(test, feature = "ebpf"))]
mod capture_dispatch_tests {
    use super::*;
    use crate::usbmon::parser::TransferType;
    use std::sync::mpsc::sync_channel;

    fn delta(device_id: u8, bytes: u64) -> crate::device::manager::TrafficDelta {
        crate::device::manager::TrafficDelta {
            bus_id: 1,
            device_id,
            endpoint: 1,
            dir_in: true,
            transfer_type: Some(TransferType::Bulk),
            bytes,
        }
    }

    #[test]
    fn drain_dispatches_a_deltas_stream_to_apply_delta() {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let (tx, rx) = sync_channel(4);
        tx.send(delta(4, 1000)).unwrap();
        drop(tx);

        let capture = CaptureStream::Deltas(rx);
        assert_eq!(drain(&mut mgr, &capture), DrainStatus::Disconnected);
        assert_eq!(
            mgr.buses[&1].devices[&4].bandwidth_stats.total_rx_bytes, 1000,
            "a dying eBPF poller's queued deltas must land before the disconnect surfaces"
        );
    }

    #[test]
    fn capture_source_label_reports_ebpf_for_a_deltas_stream_regardless_of_text_active() {
        let (_tx, rx) = sync_channel::<crate::device::manager::TrafficDelta>(1);
        let capture = CaptureStream::Deltas(rx);
        assert_eq!(capture_source_label(&capture, false, false), "ebpf");
        assert_eq!(
            capture_source_label(&capture, true, true),
            "ebpf",
            "a Deltas stream is the eBPF backend regardless of the usbmon flags"
        );
    }

    #[test]
    fn capture_source_label_splits_mmap_binary_and_text_for_a_packets_stream() {
        let (_tx, rx) = sync_channel::<crate::usbmon::parser::UsbPacket>(1);
        let capture = CaptureStream::Packets(rx);
        assert_eq!(
            capture_source_label(&capture, false, false),
            "binary",
            "the read()-based binary interface: neither text nor mmap"
        );
        assert_eq!(
            capture_source_label(&capture, false, true),
            "mmap",
            "the mmap ring reader must not be mislabeled as binary"
        );
        assert_eq!(
            capture_source_label(&capture, true, false),
            "text",
            "text is the terminal fallback"
        );
        assert_eq!(
            capture_source_label(&capture, true, true),
            "text",
            "text wins if both flags somehow read true"
        );
    }
}

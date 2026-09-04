//! The `--support` orchestrator: runs every collector, embeds a fixture (a
//! live usbmon capture as root, a static one otherwise), replays it into
//! `report.json`, writes the manifest, archives the directory, and returns
//! the summary the CLI prints with the filing guidance. Nothing here changes
//! the system: no modprobe, no prompts, no network. Every filesystem root is
//! injectable through [`Roots`] and every live probe result arrives through
//! [`Environment`], so the whole run is testable against a fake tree.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context};
use log::info;
use serde::Serialize;

use super::bundle::{self, utc_stamp, BundleWriter};
use super::collect::{self, BackendInfo, BuildInfo, HostInfo, TerminalInfo, UsbmonInfo};
use super::inventory::{self, AttrDump, UsbInventory};
use super::redact::Redactor;
use super::{note, Note};
use crate::capture::{self, BaselineSource, CaptureFixtureOpts};
use crate::config;
use crate::fixture_replay::{replay_fixture_with_elapsed, FixtureSource};
use crate::headless::export::{enabled_features, ReportSink, RunRecord};
use crate::tui::sync::{probe_decision, probe_sync_mode, ProbeDecision, SyncMode};
use crate::usbids::{self, UsbIds};
use crate::usbmon::{self, UsbmonStatus};

pub struct SupportOpts {
    pub window: Duration,
    pub no_capture: bool,
    /// The command line as run (redacted when written).
    pub command: Vec<String>,
}

/// The bundle directory (created) and the archive path (not yet written).
#[derive(Debug)]
pub struct Prepared {
    pub dir: PathBuf,
    pub archive: PathBuf,
}

/// Resolve `--support`'s target: a directory (existing or not) holds the
/// bundle directory and the archive; a name ending in `.tar.gz` names the
/// archive and the bundle directory goes beside it. The bundle directory is
/// created here so the logger can tee into it before anything else runs.
pub fn prepare_dir(target: &Path, now_unix: u64) -> anyhow::Result<Prepared> {
    let stamp = utc_stamp(now_unix);
    let name = format!("usbtop-ng-support-{stamp}");
    let is_archive = target.to_string_lossy().ends_with(".tar.gz");
    let (parent, archive_name) = if is_archive {
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file = target
            .file_name()
            .ok_or_else(|| anyhow!("{} has no file name", target.display()))?;
        (parent.to_path_buf(), file.to_os_string())
    } else {
        (target.to_path_buf(), format!("{name}.tar.gz").into())
    };
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    let parent = std::fs::canonicalize(&parent)
        .with_context(|| format!("could not resolve {}", parent.display()))?;
    let dir = parent.join(&name);
    if dir.exists() {
        return Err(anyhow!("{} already exists", dir.display()));
    }
    std::fs::create_dir(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(Prepared {
        dir,
        archive: parent.join(archive_name),
    })
}

/// Every filesystem root the collectors read.
pub struct Roots {
    pub sysfs_devices: PathBuf,
    pub proc: PathBuf,
    pub sys: PathBuf,
    pub etc: PathBuf,
    pub dev: PathBuf,
    pub debugfs_usbmon: PathBuf,
    pub dmi: PathBuf,
    pub device_tree: PathBuf,
    pub btf: PathBuf,
    pub thunderbolt: PathBuf,
    pub typec: PathBuf,
    pub power_delivery: PathBuf,
    pub home: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub preferences_file: Option<PathBuf>,
    pub usbids_chain: Vec<PathBuf>,
}

impl Roots {
    /// The real roots, with the config directory and usb.ids chain resolved
    /// the way the monitoring path resolves them (sudo invoker's home,
    /// `--config`, the preferences' `usbids_path`, the home copy, the distro
    /// files). The preferences file is read if present and never created.
    pub fn live(cli_config: Option<&Path>, cli_usbids: Option<&Path>) -> Roots {
        let home = config::config_home().ok();
        let config_dir = home.as_ref().map(|h| h.join(config::CONFIG_DIR_NAME));
        let preferences_file = match cli_config {
            Some(path) => Some(path.to_path_buf()),
            None => config::preferences_path().ok(),
        };
        let preferences: Option<config::Preferences> = preferences_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str(&text).ok());
        let pref_usbids = preferences
            .as_ref()
            .and_then(|p| p.usbids_path.clone())
            .map(PathBuf::from);
        let home_copy = config_dir.as_ref().map(|d| d.join("usb.ids"));
        let usbids_chain =
            usbids::source_chain(cli_usbids, pref_usbids.as_deref(), home_copy.as_deref());
        Roots {
            sysfs_devices: PathBuf::from("/sys/bus/usb/devices"),
            proc: PathBuf::from("/proc"),
            sys: PathBuf::from("/sys"),
            etc: PathBuf::from("/etc"),
            dev: PathBuf::from("/dev"),
            debugfs_usbmon: PathBuf::from("/sys/kernel/debug/usb/usbmon"),
            dmi: PathBuf::from("/sys/devices/virtual/dmi/id"),
            device_tree: PathBuf::from("/proc/device-tree"),
            btf: PathBuf::from("/sys/kernel/btf/vmlinux"),
            thunderbolt: PathBuf::from("/sys/bus/thunderbolt/devices"),
            typec: PathBuf::from("/sys/class/typec"),
            power_delivery: PathBuf::from("/sys/class/usb_power_delivery"),
            home,
            config_dir,
            preferences_file,
            usbids_chain,
        }
    }
}

/// Everything that comes from a live probe rather than a file under a root,
/// gathered once by `main` so `run_support` itself is pure over its inputs.
pub struct Environment {
    pub usbmon: Result<UsbmonStatus, String>,
    pub terminal: TerminalInfo,
    pub effective_uid: u32,
    pub under_sudo: bool,
    pub rust_log: Option<String>,
    pub virtualization: Option<String>,
    pub dmesg: Result<String, String>,
    pub usbids: Option<UsbIds>,
}

impl Environment {
    pub fn live(roots: &Roots) -> Environment {
        let chain: Vec<&Path> = roots.usbids_chain.iter().map(PathBuf::as_path).collect();
        Environment {
            usbmon: usbmon::check_usbmon_status().map_err(|e| e.to_string()),
            terminal: live_terminal(),
            // SAFETY: geteuid() takes no arguments, touches no memory, and
            // cannot fail.
            effective_uid: unsafe { libc::geteuid() },
            under_sudo: config::sudo_invoker().is_some(),
            rust_log: std::env::var("RUST_LOG").ok(),
            virtualization: collect::detect_virtualization(),
            dmesg: collect::run_dmesg(),
            usbids: usbids::resolve_from_chain(&chain),
        }
    }

    /// Whether a live capture can run, or the reason it is skipped (the
    /// note's text and the summary's `capture:` line).
    pub fn capture_decision(&self, no_capture: bool) -> Result<(), String> {
        if no_capture {
            return Err("skipped: --no-capture".to_string());
        }
        if self.effective_uid != 0 {
            return Err(
                "skipped: not running as root; run with sudo to include a usbmon capture"
                    .to_string(),
            );
        }
        if !self.usbmon.as_ref().is_ok_and(|s| s.usbmon_available) {
            return Err(
                "skipped: no usbmon interface is available; run 'sudo modprobe usbmon' first"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// The live terminal facts for `terminal.toml`. The mode-2026 handshake
/// runs only when both stdin and stdout are terminals and the session is
/// local (the same policy the TUI applies), inside a raw-mode bracket that
/// is undone even if the probe panics.
pub fn live_terminal() -> TerminalInfo {
    // SAFETY: isatty() reads a descriptor's type; no memory, cannot fail.
    let stdin_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    let stdout_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1;
    let env = |name: &str| std::env::var(name).ok();
    let sync_mode = if !(stdin_tty && stdout_tty) {
        "not probed: stdin or stdout is not a terminal".to_string()
    } else {
        match probe_decision(
            env("SSH_TTY").as_deref(),
            env("SSH_CONNECTION").as_deref(),
            env("SSH_CLIENT").as_deref(),
            env("TERM").as_deref(),
        ) {
            ProbeDecision::AssumeUnsupported => {
                "not probed: remote session, assumed unsupported".to_string()
            }
            ProbeDecision::Probe => match probe_in_raw_mode() {
                SyncMode::Supported => "supported".to_string(),
                SyncMode::Unsupported => "unsupported".to_string(),
            },
        }
    };
    collect::collect_terminal(
        &env,
        crossterm::terminal::size().ok(),
        stdout_tty,
        stdin_tty,
        &sync_mode,
    )
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn probe_in_raw_mode() -> SyncMode {
    if crossterm::terminal::enable_raw_mode().is_err() {
        return SyncMode::Unsupported;
    }
    let _guard = RawModeGuard;
    probe_sync_mode()
}

/// What happened to the capture.
pub enum CaptureState {
    Captured {
        window: Duration,
        sources: Vec<FixtureSource>,
        events: u64,
        kernel_dropped: Option<u64>,
    },
    Skipped(String),
    Failed(String),
}

/// Where the archive stands: not yet attempted (the copy of the summary
/// inside the bundle), written, or not producible. `Written`'s `String` is
/// the display form [`display_archive`] computes, never the raw absolute
/// path: the summary is meant to be pasted into a bug report, so it must
/// never carry the reporter's home directory or user name.
pub enum ArchiveState {
    Pending,
    Written(String, u64),
    Missing(String),
}

pub struct Summary {
    pub dir_name: String,
    pub archive: ArchiveState,
    pub file_count: usize,
    pub version: String,
    pub host: String,
    pub usbmon: String,
    pub backend: String,
    pub capture: String,
    pub devices: String,
    pub notes: Vec<Note>,
    pub redacted: String,
}

#[derive(Serialize)]
struct UsbmonFile<'a> {
    usbmon: &'a UsbmonInfo,
    backend: &'a BackendInfo,
}

#[derive(Serialize)]
struct ThunderboltFile {
    devices: Vec<AttrDump>,
}

#[derive(Serialize)]
struct TypecFile {
    typec: Vec<AttrDump>,
    power_delivery: Vec<AttrDump>,
}

/// Append the "static fixture written instead" clause to a `Failed` state's
/// reason, once the static fallback assembly has actually succeeded -- never
/// claimed before that, since the fallback can itself fail. A `Skipped`
/// state (no capture was attempted) is left untouched.
fn note_static_fixture_written(state: &mut CaptureState) {
    if let CaptureState::Failed(reason) = state {
        reason.push_str("; static fixture written instead");
    }
}

/// Embed the fixture: a live capture when [`Environment::capture_decision`]
/// allows it, else (or after a capture failure) a static bundle from the
/// sysfs tree alone. Returns what happened; failures become notes.
fn write_fixture(
    opts: &SupportOpts,
    roots: &Roots,
    env: &Environment,
    fixture_dir: &Path,
    notes: &mut Vec<Note>,
) -> CaptureState {
    let mut state = match env.capture_decision(opts.no_capture) {
        Ok(()) => {
            info!(
                "capturing the usbmon aggregate bus for {:.1} s",
                opts.window.as_secs_f64()
            );
            match capture::run_capture_fixture(CaptureFixtureOpts {
                outdir: fixture_dir.to_path_buf(),
                window: opts.window,
                bus: None,
                baseline: None,
            }) {
                Ok(outcome) => {
                    return CaptureState::Captured {
                        window: opts.window,
                        sources: outcome.sources,
                        events: outcome.events,
                        kernel_dropped: outcome.binary_kernel_dropped,
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(fixture_dir);
                    CaptureState::Failed(format!("failed: {e:#}"))
                }
            }
        }
        Err(reason) => CaptureState::Skipped(reason),
    };
    // The "static fixture written instead" clause is appended only once the
    // static assembly below actually succeeds -- claiming it before trying
    // would be false whenever this assembly itself then fails.
    match capture::assemble_bundle(
        &roots.sysfs_devices,
        fixture_dir,
        &[],
        &BaselineSource::CaptureFrom(roots.sysfs_devices.clone()),
        None,
    ) {
        Ok(()) => note_static_fixture_written(&mut state),
        Err(e) => {
            let _ = std::fs::remove_dir_all(fixture_dir);
            notes.push(note(
                "fixture",
                format!("could not write the static fixture: {e:#}"),
            ));
        }
    }
    match &state {
        CaptureState::Skipped(reason) | CaptureState::Failed(reason) => {
            notes.push(note("capture", reason));
        }
        CaptureState::Captured { .. } => {}
    }
    state
}

/// Run the whole collection into `prepared.dir`. Fails only when a file in
/// the bundle cannot be written or the embedded fixture violates SEC-1 or
/// SEC-2; everything else becomes a note.
pub fn run_support(
    opts: &SupportOpts,
    roots: &Roots,
    env: &Environment,
    prepared: &Prepared,
    now_unix: u64,
) -> anyhow::Result<Summary> {
    let dir = &prepared.dir;
    let mut notes: Vec<Note> = Vec::new();
    let mut writer = BundleWriter::create(dir, Redactor::new(roots.home.as_deref()))
        .with_context(|| format!("could not create {}", dir.display()))?;

    info!("collecting build and host information");
    let build = collect::collect_build(
        &opts.command,
        env.rust_log.clone(),
        env.effective_uid,
        env.under_sudo,
        writer.redactor(),
    );
    writer.write_toml("build.toml", &build)?;

    let (host, host_notes) = collect::collect_host(
        &roots.proc,
        &roots.sys,
        &roots.etc,
        &roots.dmi,
        &roots.device_tree,
        env.virtualization.clone(),
        writer.redactor(),
    );
    notes.extend(host_notes);
    writer.write_toml("host.toml", &host)?;

    let (usbmon_info, usbmon_notes) =
        collect::collect_usbmon(&env.usbmon, &roots.dev, &roots.debugfs_usbmon);
    notes.extend(usbmon_notes);
    let backend = collect::probe_backend(
        &usbmon_info.available_buses,
        &roots.dev,
        &roots.debugfs_usbmon,
        &roots.btf,
    );
    writer.write_toml(
        "usbmon.toml",
        &UsbmonFile {
            usbmon: &usbmon_info,
            backend: &backend,
        },
    )?;
    match &env.dmesg {
        Ok(text) => {
            let masked = writer.redactor().mac_addresses(text);
            writer.write_text("dmesg-usb.txt", &masked)?;
        }
        Err(reason) => notes.push(note("dmesg", reason)),
    }

    info!("reading the USB device inventory");
    let chain: Vec<&Path> = roots.usbids_chain.iter().map(PathBuf::as_path).collect();
    let usbids_info = inventory::usbids_info(&chain, writer.redactor());
    let (inv, inv_notes) =
        inventory::collect_usb_inventory(&roots.sysfs_devices, env.usbids.as_ref(), usbids_info);
    notes.extend(inv_notes);
    writer.write_toml("inventory/usb.toml", &inv)?;
    let (blobs, blob_notes) = inventory::read_descriptor_blobs(&roots.sysfs_devices);
    notes.extend(blob_notes);
    for blob in &blobs {
        writer.write_bytes(
            &format!("inventory/descriptors/{}.bin", blob.port_chain),
            &blob.descriptors,
        )?;
        if let Some(bos) = &blob.bos {
            writer.write_bytes(
                &format!("inventory/descriptors/{}.bos.bin", blob.port_chain),
                bos,
            )?;
        }
    }
    let (thunderbolt, tb_notes) = inventory::dump_attrs(&roots.thunderbolt, 3);
    notes.extend(tb_notes);
    writer.write_toml(
        "inventory/thunderbolt.toml",
        &ThunderboltFile {
            devices: thunderbolt,
        },
    )?;
    let (typec, typec_notes) = inventory::dump_attrs(&roots.typec, 3);
    notes.extend(typec_notes);
    let (power_delivery, pd_notes) = inventory::dump_attrs(&roots.power_delivery, 3);
    notes.extend(pd_notes);
    writer.write_toml(
        "inventory/typec.toml",
        &TypecFile {
            typec,
            power_delivery,
        },
    )?;

    let (config_info, config_notes) = collect::collect_config(
        roots.config_dir.as_deref(),
        roots.preferences_file.as_deref(),
        env.under_sudo,
        writer.redactor(),
    );
    notes.extend(config_notes);
    writer.write_toml("config/config.toml", &config_info)?;
    if let Some(text) = &config_info.preferences {
        writer.write_text("config/preferences.toml", text)?;
    }
    if let Some(text) = &config_info.internal_devices {
        writer.write_text("config/internal-devices.toml", text)?;
    }

    writer.write_toml("terminal.toml", &env.terminal)?;

    let fixture_dir = dir.join("fixture");
    let capture_state = write_fixture(opts, roots, env, &fixture_dir, &mut notes);
    if fixture_dir.join("meta.toml").exists() {
        bundle::assert_fixture_invariants(&fixture_dir)?;
        writer.record_dir("fixture")?;
        let source = match &capture_state {
            CaptureState::Captured { sources, .. } => sources
                .iter()
                .copied()
                .find(|s| *s == FixtureSource::Binary)
                .or_else(|| sources.first().copied()),
            _ => None,
        };
        match replay_fixture_with_elapsed(&fixture_dir, source, opts.window) {
            Ok(report) => {
                let run = RunRecord {
                    record: "run",
                    usbtop_ng: build.version.clone(),
                    features: enabled_features(),
                    started_unix: now_unix,
                    window_seconds: opts.window.as_secs_f64(),
                    batch: false,
                    filters: Vec::new(),
                    command: build.command.clone(),
                    backend: backend.would_select.to_string(),
                    kernel: host.kernel.clone(),
                    os: host.os.clone(),
                    arch: std::env::consts::ARCH,
                    buses: usbmon_info.available_buses.clone(),
                };
                let mut sink = ReportSink::open(Some(&dir.join("report.json")), &run, true)?;
                sink.write(&report, true)?;
                sink.finish();
                writer.redact_file("report.json")?;
            }
            Err(e) => notes.push(note("report.json", format!("replay failed: {e:#}"))),
        }
    }

    let log_present = dir.join("usbtop-ng.log").exists();
    // Every note that reaches the summary or the manifest passes through the
    // redactor first: a collector's note can carry a path (a "could not
    // read: …" reason or item naming an unreadable file under the home
    // directory), and the manifest and SUMMARY.txt are the only text this
    // pipeline writes without already going through `BundleWriter::write_text`
    // or `write_toml`, both of which redact on the way out.
    let notes: Vec<Note> = notes
        .iter()
        .map(|n| Note {
            item: writer.redactor().text(&n.item),
            reason: writer.redactor().text(&n.reason),
        })
        .collect();
    let mut summary = Summary {
        dir_name: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        archive: ArchiveState::Pending,
        // Recorded so far, plus SUMMARY.txt, the manifest, and the log.
        file_count: writer.files().len() + 2 + usize::from(log_present),
        version: version_line(&build),
        host: host_line(&host),
        usbmon: usbmon_line(&usbmon_info, build.running_as_root),
        backend: backend_line(&backend),
        capture: capture_line(&capture_state),
        devices: devices_line(&inv),
        notes: notes.clone(),
        redacted: redacted_line(&writer.redactor().summary()),
    };
    writer.write_text("SUMMARY.txt", &render_summary(&summary))?;

    info!("bundle assembled; writing the manifest");
    // Nothing logs past this line: the log is adopted with its final size,
    // and the archive must match the manifest.
    if log_present {
        writer.adopt_file("usbtop-ng.log")?;
    }
    writer.write_manifest(now_unix, &notes)?;
    summary.archive = match writer.archive(&prepared.archive) {
        Ok(bytes) => {
            let cwd = std::env::current_dir()
                .ok()
                .and_then(|d| d.canonicalize().ok());
            ArchiveState::Written(
                display_archive(&prepared.archive, cwd.as_deref(), roots.home.as_deref()),
                bytes,
            )
        }
        Err(n) => {
            let n = Note {
                item: writer.redactor().text(&n.item),
                reason: writer.redactor().text(&n.reason),
            };
            let reason = n.reason.clone();
            summary.notes.push(n);
            ArchiveState::Missing(reason)
        }
    };
    bundle::own_tree(dir);
    Ok(summary)
}

// --- summary lines --------------------------------------------------------

fn with_commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_size(bytes: u64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", (bytes + 500) / 1000)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

fn version_line(build: &BuildInfo) -> String {
    let features = if build.features.is_empty() {
        "none".to_string()
    } else {
        build.features.join(" ")
    };
    format!(
        "usbtop-ng {} (features: {features}) {}",
        build.version, build.arch
    )
}

fn host_line(host: &HostInfo) -> String {
    let mut parts = Vec::new();
    if !host.kernel.is_empty() {
        parts.push(format!("Linux {}", host.kernel));
    }
    for s in [&host.os, &host.board] {
        if !s.is_empty() {
            parts.push(s.clone());
        }
    }
    parts.join(", ")
}

fn usbmon_line(u: &UsbmonInfo, running_as_root: bool) -> String {
    let mut parts = vec![
        if u.module_loaded {
            "module loaded".to_string()
        } else {
            "module not loaded".to_string()
        },
        format!("{} buses", u.available_buses.len()),
    ];
    match u.nodes.first() {
        Some(node) => {
            let owner = |id: u32| {
                if id == 0 {
                    "root".to_string()
                } else {
                    id.to_string()
                }
            };
            parts.push(format!(
                "/dev/usbmon* {}:{} {}",
                owner(node.owner_uid),
                owner(node.group_gid),
                node.mode_octal
            ));
        }
        None => parts.push("no /dev/usbmon* nodes".to_string()),
    }
    if u.permission_denied {
        parts.push("permission denied".to_string());
    }
    parts.push(if running_as_root {
        "running as root".to_string()
    } else {
        "not running as root".to_string()
    });
    parts.join(", ")
}

fn backend_line(b: &BackendInfo) -> String {
    let chosen = match b.would_select {
        "mmap" => format!(
            "mmap ring ({}) would be selected",
            b.ring_bytes
                .map(|n| format!("{} MiB", n / (1024 * 1024)))
                .unwrap_or_else(|| "size unknown".to_string())
        ),
        "binary" => "read()-based binary interface would be selected".to_string(),
        "text" => "debugfs text interface would be selected".to_string(),
        _ => "no usbmon interface would be selected".to_string(),
    };
    format!(
        "{chosen}; eBPF: BTF {}, {}",
        if b.btf_present { "present" } else { "absent" },
        if b.ebpf_built_in {
            "built in"
        } else {
            "not built in"
        }
    )
}

fn capture_line(state: &CaptureState) -> String {
    match state {
        CaptureState::Captured {
            window,
            sources,
            events,
            kernel_dropped,
        } => format!(
            "{:.1} s aggregate, {} events, kernel drops {}, sources {}",
            window.as_secs_f64(),
            with_commas(*events),
            kernel_dropped.map_or("unknown".to_string(), |n| n.to_string()),
            sources
                .iter()
                .map(|s| s.tag())
                .collect::<Vec<_>>()
                .join("+")
        ),
        CaptureState::Skipped(reason) | CaptureState::Failed(reason) => reason.clone(),
    }
}

fn devices_line(inv: &UsbInventory) -> String {
    let devices = inv
        .devices
        .iter()
        .filter(|d| !d.port_chain.starts_with("usb"))
        .count();
    let buses: usize = inv.controllers.iter().map(|c| c.buses.len()).sum();
    // Speeds carry a fraction (1.5 Mbps), so they are kept as the strings
    // sysfs printed and sorted numerically.
    let mut speeds: Vec<String> = inv.devices.iter().filter_map(|d| d.speed.clone()).collect();
    speeds.sort_by(|a, b| {
        a.parse::<f64>()
            .unwrap_or(0.0)
            .partial_cmp(&b.parse::<f64>().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    speeds.dedup();
    if speeds.is_empty() {
        format!("{devices} across {buses} buses")
    } else {
        format!("{devices} across {buses} buses ({} Mbps)", speeds.join("/"))
    }
}

fn redacted_line(redaction: &[(String, usize)]) -> String {
    let label = |rule: &str, n: usize| -> String {
        let (one, many) = match rule {
            "home_path" => ("home path", "home paths"),
            "user_name" => ("user name", "user names"),
            "mac_address" => ("MAC address", "MAC addresses"),
            "fs_uuid" => ("filesystem UUID", "filesystem UUIDs"),
            "build_stamp" => ("build stamp", "build stamps"),
            other => (other, other),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    };
    let rewritten = if redaction.is_empty() {
        "nothing rewritten".to_string()
    } else {
        redaction
            .iter()
            .map(|(rule, n)| label(rule, *n))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{rewritten}; host identity never collected; device serials included")
}

/// How the summary names the archive: relative to the current directory
/// when it lives under it (`./usbtop-ng-support-….tar.gz`, as the spec
/// shows), else with the home rule applied (`~/bugs/x.tar.gz`), so the
/// pasted summary never carries the user's home path.
fn display_archive(archive: &Path, cwd: Option<&Path>, home: Option<&Path>) -> String {
    if let Some(rel) = cwd.and_then(|cwd| archive.strip_prefix(cwd).ok()) {
        return format!("./{}", rel.display());
    }
    Redactor::new(home).text(&archive.display().to_string())
}

fn bundle_line(s: &Summary) -> String {
    match &s.archive {
        ArchiveState::Pending => format!("{}/ ({} files)", s.dir_name, s.file_count),
        ArchiveState::Written(shown, bytes) => {
            format!("{shown} ({}, {} files)", format_size(*bytes), s.file_count)
        }
        ArchiveState::Missing(reason) => format!(
            "{}/ ({} files; not archived: {reason}. Archive it by hand from its parent directory: tar -czf {}.tar.gz {})",
            s.dir_name, s.file_count, s.dir_name, s.dir_name
        ),
    }
}

/// The ten-line block from the spec.
pub fn render_summary(s: &Summary) -> String {
    let mut out = String::from("usbtop-ng support bundle\n");
    out.push_str(&format!("  bundle:   {}\n", bundle_line(s)));
    out.push_str(&format!("  version:  {}\n", s.version));
    out.push_str(&format!("  host:     {}\n", s.host));
    out.push_str(&format!("  usbmon:   {}\n", s.usbmon));
    out.push_str(&format!("  backend:  {}\n", s.backend));
    out.push_str(&format!("  capture:  {}\n", s.capture));
    out.push_str(&format!("  devices:  {}\n", s.devices));
    if s.notes.is_empty() {
        out.push_str("  notes:    none\n");
    } else {
        for (i, n) in s.notes.iter().enumerate() {
            let label = if i == 0 {
                "  notes:    "
            } else {
                "            "
            };
            out.push_str(&format!("{label}{}: {}\n", n.item, n.reason));
        }
    }
    out.push_str(&format!("  redacted: {}\n", s.redacted));
    out
}

/// Printed after the summary. Sentence case, one action per line.
pub const GUIDANCE: &str = "\nTo report a bug:\n  \
1. Review the bundle before attaching it: `tar tzf <archive>` lists every file.\n     \
It carries your devices' full details, including their serial numbers, and\n     \
nothing about the host itself; you decide what to attach.\n  \
2. Open https://github.com/wifi-blackout/usbtop-ng/issues/new?template=bug_report.yml\n  \
3. Paste the summary above into \"Support summary\" and attach the .tar.gz.\n  \
4. Describe what you expected, what happened, and the exact command you ran.\n     \
For a display problem, name the terminal and say whether it was over SSH.\n";

// --- the log tee ----------------------------------------------------------

/// Writes every log record to stderr as before and, with home paths
/// rewritten, to `usbtop-ng.log` inside the bundle.
pub struct TeeWriter {
    file: File,
    redactor: Redactor,
}

impl TeeWriter {
    pub fn create(path: &Path, home: Option<&Path>) -> io::Result<TeeWriter> {
        Ok(TeeWriter {
            file: File::create(path)?,
            redactor: Redactor::new(home),
        })
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // stderr is best-effort, as it is for every other log line.
        let _ = io::stderr().write_all(buf);
        let text = String::from_utf8_lossy(buf);
        self.file.write_all(self.redactor.text(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The process logger: the same default-env builder and levels as before;
/// with a tee, records go through it (styles off, so the file has no escape
/// codes).
pub fn init_logger(verbose: bool, tee: Option<TeeWriter>) {
    let mut builder = env_logger::Builder::from_default_env();
    builder.filter_level(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
    if let Some(tee) = tee {
        builder.target(env_logger::Target::Pipe(Box::new(tee)));
        builder.write_style(env_logger::WriteStyle::Never);
    }
    builder.init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::collect::collect_terminal;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Raw bytes (descriptor blobs); never spelled as string escapes.
    fn write_bytes(dir: &Path, rel: &str, bytes: &[u8]) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn prepare_dir_places_the_bundle_inside_a_directory_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("out");
        let p = prepare_dir(&target, 1_788_000_000).unwrap();
        assert_eq!(
            p.dir,
            target
                .canonicalize()
                .unwrap()
                .join("usbtop-ng-support-20260829T104000Z")
        );
        assert_eq!(
            p.archive,
            target
                .canonicalize()
                .unwrap()
                .join("usbtop-ng-support-20260829T104000Z.tar.gz")
        );
        assert!(p.dir.is_dir(), "the directory is created up front");
        assert!(!p.archive.exists());
    }

    #[test]
    fn prepare_dir_treats_a_tar_gz_target_as_the_archive_name() {
        let temp = tempfile::tempdir().unwrap();
        let p = prepare_dir(&temp.path().join("bug-42.tar.gz"), 0).unwrap();
        let parent = temp.path().canonicalize().unwrap();
        assert_eq!(p.dir, parent.join("usbtop-ng-support-19700101T000000Z"));
        assert_eq!(p.archive, parent.join("bug-42.tar.gz"));
    }

    #[test]
    fn prepare_dir_refuses_an_existing_bundle_directory() {
        let temp = tempfile::tempdir().unwrap();
        prepare_dir(temp.path(), 0).unwrap();
        let err = prepare_dir(temp.path(), 0).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    fn status(available: bool) -> UsbmonStatus {
        UsbmonStatus {
            module_loaded: available,
            debugfs_mounted: available,
            usbmon_available: available,
            binary_available: available,
            text_available: available,
            permission_denied: false,
            available_buses: if available { vec![0, 1] } else { Vec::new() },
        }
    }

    fn environment(uid: u32, usbmon: Result<UsbmonStatus, String>) -> Environment {
        Environment {
            usbmon,
            terminal: collect_terminal(&|_| None, None, false, false, "not probed: test"),
            effective_uid: uid,
            under_sudo: false,
            rust_log: None,
            virtualization: Some("none".to_string()),
            dmesg: Err("permission denied".to_string()),
            usbids: None,
        }
    }

    #[test]
    fn capture_decision_explains_each_skip() {
        let root_ok = environment(0, Ok(status(true)));
        assert_eq!(root_ok.capture_decision(false), Ok(()));
        assert_eq!(
            root_ok.capture_decision(true).unwrap_err(),
            "skipped: --no-capture"
        );
        let user = environment(1000, Ok(status(true)));
        assert!(user
            .capture_decision(false)
            .unwrap_err()
            .contains("not running as root"));
        let no_usbmon = environment(0, Ok(status(false)));
        assert!(no_usbmon
            .capture_decision(false)
            .unwrap_err()
            .contains("no usbmon interface"));
        let broken = environment(0, Err("boom".to_string()));
        assert!(broken
            .capture_decision(false)
            .unwrap_err()
            .contains("no usbmon interface"));
    }

    /// The "static fixture written instead" clause is appended to a `Failed`
    /// reason only when the caller has actually confirmed the fallback
    /// assembly succeeded -- never claimed up front, since that assembly can
    /// itself fail. A `Skipped` reason is left untouched either way.
    #[test]
    fn note_static_fixture_written_only_appends_to_a_failed_state_and_never_touches_skipped() {
        let mut failed = CaptureState::Failed("failed: boom".to_string());
        note_static_fixture_written(&mut failed);
        assert_eq!(
            capture_line(&failed),
            "failed: boom; static fixture written instead"
        );

        let mut skipped = CaptureState::Skipped("skipped: --no-capture".to_string());
        note_static_fixture_written(&mut skipped);
        assert_eq!(capture_line(&skipped), "skipped: --no-capture");
    }

    #[test]
    fn summary_lines_match_the_spec_shapes() {
        let mut r = Redactor::new(None);
        let build = collect::collect_build(&["usbtop-ng".to_string()], None, 0, false, &mut r);
        let version = version_line(&build);
        assert!(
            version.starts_with(&format!(
                "usbtop-ng {} (features: ",
                env!("CARGO_PKG_VERSION")
            )),
            "{version}"
        );
        assert!(version.ends_with(std::env::consts::ARCH), "{version}");

        let host = collect::HostInfo {
            kernel: "7.0.0-30-generic".into(),
            proc_version: String::new(),
            os: "Linux Mint 22.3".into(),
            board: "MG-VCP17A-3080".into(),
            soc: String::new(),
            cpu_model: String::new(),
            cpu_count: 0,
            mem_total_kb: None,
            uptime_s: None,
            virtualization: None,
            cmdline: String::new(),
            lockdown: String::new(),
            usbcore_params: Default::default(),
        };
        assert_eq!(
            host_line(&host),
            "Linux 7.0.0-30-generic, Linux Mint 22.3, MG-VCP17A-3080"
        );

        let (usbmon_info, _) = collect::collect_usbmon(
            &Ok(status(true)),
            Path::new("/nonexistent"),
            Path::new("/nonexistent"),
        );
        assert_eq!(
            usbmon_line(&usbmon_info, true),
            "module loaded, 2 buses, no /dev/usbmon* nodes, running as root"
        );

        let backend = collect::BackendInfo {
            would_select: "mmap",
            reason: String::new(),
            probed_bus: Some(0),
            ring_bytes: Some(64 * 1024 * 1024),
            ebpf_built_in: false,
            btf_present: true,
        };
        assert_eq!(
            backend_line(&backend),
            "mmap ring (64 MiB) would be selected; eBPF: BTF present, not built in"
        );

        let captured = CaptureState::Captured {
            window: Duration::from_secs(5),
            sources: vec![FixtureSource::Binary, FixtureSource::Text],
            events: 1234,
            kernel_dropped: Some(0),
        };
        assert_eq!(
            capture_line(&captured),
            "5.0 s aggregate, 1,234 events, kernel drops 0, sources binary+text"
        );
        assert_eq!(
            capture_line(&CaptureState::Skipped("skipped: --no-capture".into())),
            "skipped: --no-capture"
        );

        assert_eq!(
            redacted_line(&[("home_path".to_string(), 3), ("mac_address".to_string(), 1)]),
            "3 home paths, 1 MAC address; host identity never collected; device serials included"
        );
        assert_eq!(
            redacted_line(&[("user_name".to_string(), 1)]),
            "1 user name; host identity never collected; device serials included"
        );
        assert_eq!(
            redacted_line(&[("home_path".to_string(), 2), ("user_name".to_string(), 2)]),
            "2 home paths, 2 user names; host identity never collected; device serials included"
        );
        assert_eq!(
            redacted_line(&[]),
            "nothing rewritten; host identity never collected; device serials included"
        );
        assert_eq!(
            redacted_line(&[("build_stamp".to_string(), 1)]),
            "1 build stamp; host identity never collected; device serials included"
        );
        assert_eq!(with_commas(1_234_567), "1,234,567");
        assert_eq!(format_size(412_300), "412 KB");
        assert_eq!(format_size(3_400_000), "3.4 MB");
    }

    #[test]
    fn display_archive_never_shows_the_home_path() {
        let home = Path::new("/home/alice");
        assert_eq!(
            display_archive(
                Path::new("/home/alice/bug.tar.gz"),
                Some(Path::new("/home/alice")),
                Some(home)
            ),
            "./bug.tar.gz",
            "under the cwd: shown relative to it"
        );
        assert_eq!(
            display_archive(
                Path::new("/home/alice/bugs/bug.tar.gz"),
                Some(Path::new("/tmp/elsewhere")),
                Some(home)
            ),
            "~/bugs/bug.tar.gz",
            "under home but not the cwd: the home rule applies"
        );
        assert_eq!(
            display_archive(
                Path::new("/tmp/x/bug.tar.gz"),
                Some(Path::new("/var/run")),
                Some(home)
            ),
            "/tmp/x/bug.tar.gz",
            "neither under the cwd nor under home: unchanged"
        );
    }

    #[test]
    fn render_summary_has_the_ten_line_layout() {
        let summary = Summary {
            dir_name: "usbtop-ng-support-20260903T091500Z".into(),
            archive: ArchiveState::Written(
                "./usbtop-ng-support-20260903T091500Z.tar.gz".to_string(),
                412_300,
            ),
            file_count: 14,
            version: "usbtop-ng 1.5.0 (features: none) x86_64".into(),
            host: "Linux 7.0.0-30-generic, Linux Mint 22.3, MG-VCP17A-3080".into(),
            usbmon: "module loaded, 4 buses, /dev/usbmon* root:root 0600, running as root".into(),
            backend: "mmap ring (64 MiB) would be selected; eBPF: BTF present, not built in".into(),
            capture: "5.0 s aggregate, 1,234 events, kernel drops 0, sources binary+text".into(),
            devices: "21 across 4 buses (1.5/12/480/5000/10000 Mbps)".into(),
            notes: vec![note("dmesg", "permission denied")],
            redacted: "3 home paths; host identity never collected; device serials included".into(),
        };
        let text = render_summary(&summary);
        // Built as plain lines (never `\x20` continuations, which the write
        // tooling in this environment would decode) but byte-identical to
        // the spec's ten-line block: two leading spaces before each label,
        // and each label padded so every value starts in the same column.
        let lines = [
            "usbtop-ng support bundle",
            "  bundle:   ./usbtop-ng-support-20260903T091500Z.tar.gz (412 KB, 14 files)",
            "  version:  usbtop-ng 1.5.0 (features: none) x86_64",
            "  host:     Linux 7.0.0-30-generic, Linux Mint 22.3, MG-VCP17A-3080",
            "  usbmon:   module loaded, 4 buses, /dev/usbmon* root:root 0600, running as root",
            "  backend:  mmap ring (64 MiB) would be selected; eBPF: BTF present, not built in",
            "  capture:  5.0 s aggregate, 1,234 events, kernel drops 0, sources binary+text",
            "  devices:  21 across 4 buses (1.5/12/480/5000/10000 Mbps)",
            "  notes:    dmesg: permission denied",
            "  redacted: 3 home paths; host identity never collected; device serials included",
        ];
        let expected = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
        assert_eq!(text, expected);

        let pending = Summary {
            archive: ArchiveState::Pending,
            notes: Vec::new(),
            ..summary
        };
        let text = render_summary(&pending);
        assert!(
            text.contains("  bundle:   usbtop-ng-support-20260903T091500Z/ (14 files)\n"),
            "{text}"
        );
        assert!(text.contains("  notes:    none\n"), "{text}");
        let missing = Summary {
            archive: ArchiveState::Missing("could not run tar: not found".into()),
            ..pending
        };
        let text = render_summary(&missing);
        assert!(
            text.contains("not archived: could not run tar: not found"),
            "{text}"
        );
        assert!(text.contains("tar -czf usbtop-ng-support-20260903T091500Z.tar.gz usbtop-ng-support-20260903T091500Z"), "{text}");
    }

    #[test]
    fn guidance_pins_the_url_and_the_four_steps() {
        assert!(GUIDANCE.contains(
            "https://github.com/wifi-blackout/usbtop-ng/issues/new?template=bug_report.yml"
        ));
        for step in [
            "  1. Review the bundle",
            "  2. Open https://",
            "  3. Paste the summary",
            "  4. Describe what you expected",
        ] {
            assert!(GUIDANCE.contains(step), "{step}");
        }
        assert!(GUIDANCE.contains("tar tzf <archive>"));
        assert!(GUIDANCE.contains("serial numbers"));
    }

    #[test]
    fn tee_writer_redacts_the_file_copy() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbtop-ng.log");
        let mut tee = TeeWriter::create(&path, Some(Path::new("/home/alice"))).unwrap();
        tee.write_all(b"[INFO] usb.ids loaded from /home/alice/.usbtop-ng/usb.ids\n")
            .unwrap();
        tee.flush().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[INFO] usb.ids loaded from ~/.usbtop-ng/usb.ids\n"
        );
    }

    /// A sysfs tree the capturer can materialize: a controller with a
    /// symlinked root hub and one device carrying a descriptor blob.
    fn fake_roots(base: &Path) -> Roots {
        let devices = base.join("sys/bus/usb/devices");
        let ctrl = base.join("sys/devices/pci0000:00/0000:00:14.0");
        let usb1 = ctrl.join("usb1");
        write(&usb1, "busnum", "1\n");
        write(&usb1, "devnum", "1\n");
        write(&usb1, "speed", "480\n");
        write(&usb1, "idVendor", "1d6b\n");
        write(&usb1, "idProduct", "0002\n");
        write_bytes(&usb1, "descriptors", &[]);
        std::fs::create_dir_all(&devices).unwrap();
        std::os::unix::fs::symlink(&usb1, devices.join("usb1")).unwrap();
        let dev = devices.join("1-1");
        write(&dev, "busnum", "1\n");
        write(&dev, "devnum", "3\n");
        write(&dev, "speed", "480\n");
        write(&dev, "idVendor", "0430\n");
        write(&dev, "idProduct", "0100\n");
        write(&dev, "serial", "SN-KEEP-ME\n");
        write_bytes(
            &dev,
            "descriptors",
            &[
                0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x30, 0x04, 0x00, 0x01, 0x00, 0x01,
                0x00, 0x00, 0x00, 0x01,
            ],
        );
        write(base, "proc/sys/kernel/osrelease", "7.0.0-30-generic\n");
        write(base, "proc/version", "Linux version 7.0.0-30-generic\n");
        write(
            base,
            "proc/cpuinfo",
            "processor\t: 0\nmodel name\t: Test CPU\n",
        );
        write(base, "proc/meminfo", "MemTotal: 1024 kB\n");
        write(base, "proc/uptime", "1.5 2.0\n");
        write(base, "proc/cmdline", "root=UUID=aaaa-bbbb ro\n");
        write(base, "sys/module/usbcore/parameters/autosuspend", "2\n");
        write(base, "sys/kernel/security/lockdown", "[none]\n");
        write(base, "etc/os-release", "PRETTY_NAME=\"Test OS\"\n");
        write(base, "dmi/product_name", "Test Board\n");
        write(base, "dmi/sys_vendor", "Test\n");
        std::fs::create_dir_all(base.join("dev")).unwrap();
        let typec_real = base.join("sys/devices/platform/typec/port0");
        write(&typec_real, "data_role", "[host] device\n");
        std::fs::create_dir_all(base.join("sys/class/typec")).unwrap();
        std::os::unix::fs::symlink(&typec_real, base.join("sys/class/typec/port0")).unwrap();
        let home = base.join("home/alice");
        write(
            &home,
            ".usbtop-ng/preferences.toml",
            &format!("usbids_path = \"{}/usb.ids\"\n", home.display()),
        );
        // A file, not a directory, under the fake home: dump_attrs's
        // read_dir fails on it and notes "could not read: …" with this path
        // as the item, the cheapest way to put a home path into a note (see
        // run_support_without_capture_writes_a_consistent_static_bundle).
        write(&home, "not-a-dir", "");
        Roots {
            sysfs_devices: devices,
            proc: base.join("proc"),
            sys: base.join("sys"),
            etc: base.join("etc"),
            dev: base.join("dev"),
            debugfs_usbmon: base.join("sys/kernel/debug/usb/usbmon"),
            dmi: base.join("dmi"),
            device_tree: base.join("proc/device-tree"),
            btf: base.join("sys/kernel/btf/vmlinux"),
            thunderbolt: home.join("not-a-dir"),
            typec: base.join("sys/class/typec"),
            power_delivery: base.join("sys/class/usb_power_delivery"),
            home: Some(home.clone()),
            config_dir: Some(home.join(".usbtop-ng")),
            preferences_file: Some(home.join(".usbtop-ng/preferences.toml")),
            usbids_chain: vec![home.join(".usbtop-ng/usb.ids")],
        }
    }

    /// The hermetic end-to-end: a non-root `--support --no-capture` against
    /// a fake tree writes a bundle whose manifest matches the files on disk,
    /// whose redaction counts match, whose fixture is static and passes the
    /// capturer's invariants, and which carries no home path anywhere.
    #[test]
    fn run_support_without_capture_writes_a_consistent_static_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let roots = fake_roots(temp.path());
        let home = roots.home.clone().unwrap();
        let prepared = prepare_dir(&temp.path().join("out"), 1_788_000_000).unwrap();
        // What main's tee would have written before run_support starts.
        std::fs::write(
            prepared.dir.join("usbtop-ng.log"),
            "[INFO] starting usbtop-ng\n",
        )
        .unwrap();
        let env = environment(1000, Ok(status(false)));
        let opts = SupportOpts {
            window: Duration::from_secs(1),
            no_capture: true,
            command: vec![
                "usbtop-ng".into(),
                "--support".into(),
                "--no-capture".into(),
            ],
        };

        let summary = run_support(&opts, &roots, &env, &prepared, 1_788_000_000).unwrap();
        let dir = &prepared.dir;

        // The manifest lists every file on disk (except itself) with its size.
        let manifest: bundle::Manifest =
            toml::from_str(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
        let mut listed: Vec<String> = manifest.files.iter().map(|f| f.path.clone()).collect();
        listed.sort();
        for entry in &manifest.files {
            let meta = std::fs::symlink_metadata(dir.join(&entry.path)).unwrap();
            if entry.symlink {
                assert!(meta.file_type().is_symlink(), "{}", entry.path);
            } else {
                assert_eq!(meta.len(), entry.bytes, "{}", entry.path);
            }
        }
        let mut on_disk = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let m = std::fs::symlink_metadata(&p).unwrap();
                if m.is_dir() && !m.file_type().is_symlink() {
                    stack.push(p);
                } else {
                    on_disk.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        on_disk.retain(|p| p != "manifest.toml");
        on_disk.sort();
        assert_eq!(listed, on_disk);
        for expected in [
            "build.toml",
            "host.toml",
            "usbmon.toml",
            "inventory/usb.toml",
            "inventory/descriptors/1-1.bin",
            "inventory/thunderbolt.toml",
            "inventory/typec.toml",
            "config/config.toml",
            "config/preferences.toml",
            "terminal.toml",
            "fixture/meta.toml",
            "fixture/internal-devices.toml",
            "fixture/sysfs/usb1",
            "report.json",
            "SUMMARY.txt",
            "usbtop-ng.log",
        ] {
            assert!(
                listed.iter().any(|p| p == expected),
                "missing {expected}: {listed:?}"
            );
        }
        assert_eq!(
            summary.file_count,
            listed.len() + 1,
            "files plus the manifest"
        );

        // The fixture is static, valid, and replayed into report.json.
        let meta = std::fs::read_to_string(dir.join("fixture/meta.toml")).unwrap();
        assert!(meta.contains("sources = []"), "{meta}");
        assert!(
            !dir.join("fixture/sysfs/1-1/serial").exists(),
            "the fixture never carries a serial"
        );
        bundle::assert_fixture_invariants(&dir.join("fixture")).unwrap();
        let report = std::fs::read_to_string(dir.join("report.json")).unwrap();
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines.len(), 2);
        let head: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(head["record"], "run");
        assert_eq!(head["backend"], "none");
        let doc: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(doc["source"], "none");
        assert_eq!(doc["window_seconds"], 1.0);

        // Device identity in, host identity out.
        let usb = std::fs::read_to_string(dir.join("inventory/usb.toml")).unwrap();
        assert!(usb.contains("serial = \"SN-KEEP-ME\""), "{usb}");
        let home_text = home.display().to_string();
        for path in &on_disk {
            if path.ends_with(".toml")
                || path.ends_with(".txt")
                || path.ends_with(".json")
                || path.ends_with(".log")
            {
                let text = std::fs::read_to_string(dir.join(path)).unwrap();
                assert!(
                    !text.contains(&home_text),
                    "{path} leaks the home path: {text}"
                );
            }
        }
        // config dir, preferences path, the preferences body, and the
        // thunderbolt note's item below (every note now passes through the
        // redactor too, per the final-review fix wave, so its "could not
        // read: …" item -- the home-relative `not-a-dir` path -- adds a
        // fourth rewrite): four rewrites.
        assert_eq!(manifest.redaction.get("home_path"), Some(&4));
        assert_eq!(manifest.redaction.get("fs_uuid"), Some(&1));
        // The fake tree's `alice` appears only under the home, which the
        // home rule already rewrites.
        assert_eq!(manifest.redaction.get("user_name"), None);
        // Rules sort by name in the summary: fs_uuid before home_path.
        assert!(
            summary
                .redacted
                .starts_with("1 filesystem UUID, 4 home paths"),
            "{}",
            summary.redacted
        );

        // Notes and summary.
        let items: Vec<&str> = manifest
            .unavailable
            .iter()
            .map(|n| n.item.as_str())
            .collect();
        assert!(items.contains(&"dmesg"), "{items:?}");
        assert!(items.contains(&"capture"), "{items:?}");
        // The thunderbolt root is a file under the fake home, so
        // dump_attrs's note names it; the note passes through the redactor
        // before it reaches the manifest, so the home path never survives
        // and the item reads as a `~/…` path instead.
        let thunderbolt_note = manifest
            .unavailable
            .iter()
            .find(|n| n.item.ends_with("not-a-dir"))
            .expect("the thunderbolt note is present");
        assert_eq!(thunderbolt_note.item, "~/not-a-dir");
        assert!(
            thunderbolt_note.item.starts_with("~/"),
            "{thunderbolt_note:?}"
        );
        for n in &manifest.unavailable {
            assert!(!n.item.contains(&home_text), "{n:?}");
            assert!(!n.reason.contains(&home_text), "{n:?}");
        }
        assert_eq!(summary.capture, "skipped: --no-capture");
        assert_eq!(summary.devices, "1 across 1 buses (480 Mbps)");
        let summary_text = std::fs::read_to_string(dir.join("SUMMARY.txt")).unwrap();
        assert!(summary_text.starts_with("usbtop-ng support bundle\n"));
        assert!(
            summary_text.contains("  bundle:   usbtop-ng-support-20260829T104000Z/ ("),
            "{summary_text}"
        );
        match &summary.archive {
            ArchiveState::Written(shown, bytes) => {
                assert!(
                    shown.ends_with("usbtop-ng-support-20260829T104000Z.tar.gz"),
                    "{shown}"
                );
                assert!(!shown.contains(&home_text), "{shown}");
                assert_eq!(*bytes, std::fs::metadata(&prepared.archive).unwrap().len());
            }
            ArchiveState::Missing(reason) => assert!(reason.contains("tar"), "{reason}"),
            ArchiveState::Pending => panic!("run_support must settle the archive state"),
        }
    }

    /// Live, behind the `integration` feature, following the convention of
    /// the other root-only tests (`config`, `usbmon::mmap_ring`): as root
    /// with a usable usbmon interface, a real `--support` run captures a
    /// fixture whose goldens replay and, on an idle bus, reports zero kernel
    /// drops. Skips with a message otherwise.
    #[cfg(all(test, feature = "integration"))]
    mod live {
        use super::*;
        use crate::fixture_replay::{replay_fixture, to_masked_value};

        #[test]
        fn live_support_as_root_captures_a_replayable_fixture() {
            let roots = Roots::live(None, None);
            let env = Environment::live(&roots);
            if let Err(reason) = env.capture_decision(false) {
                eprintln!("skipping: {reason}");
                return;
            }
            let temp = tempfile::tempdir().unwrap();
            let prepared = prepare_dir(temp.path(), 1_788_000_000).unwrap();
            let opts = SupportOpts {
                window: Duration::from_secs(1),
                no_capture: false,
                command: vec!["usbtop-ng".into(), "--support".into()],
            };
            let summary = run_support(&opts, &roots, &env, &prepared, 1_788_000_000).unwrap();
            assert!(
                summary.capture.starts_with("1.0 s aggregate"),
                "{}",
                summary.capture
            );
            assert!(
                summary.capture.contains("kernel drops 0"),
                "an idle bus loses nothing (run this with no device streaming): {}",
                summary.capture
            );
            let fixture = prepared.dir.join("fixture");
            for source in [FixtureSource::Binary, FixtureSource::Text] {
                if !fixture.join(source.trace_filename()).exists() {
                    continue;
                }
                let report = replay_fixture(&fixture, source).unwrap();
                let got = to_masked_value(&serde_json::to_string(&report).unwrap()).unwrap();
                let golden = to_masked_value(
                    &std::fs::read_to_string(fixture.join(source.golden_filename())).unwrap(),
                )
                .unwrap();
                assert_eq!(got, golden, "golden must equal replay for {source:?}");
            }
            let mut stack = vec![fixture.join("sysfs")];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                    let path = entry.path();
                    assert_ne!(
                        path.file_name().and_then(|n| n.to_str()),
                        Some("serial"),
                        "the fixture never carries a serial: {}",
                        path.display()
                    );
                    if path.is_dir() {
                        stack.push(path);
                    }
                }
            }
        }
    }
}

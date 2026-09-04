//! Collectors A (build), B (host, usbmon, backend, dmesg), D (configuration),
//! and F (terminal) for the support bundle. Every collector reads through
//! roots its caller passes in, so tests inject a fake tree the way
//! `DeviceManager::with_sysfs_base` does, and every collector returns notes
//! instead of errors: a missing file is a fact about the host, not a failure
//! of the bundle. The device inventory (collector C) lives in `inventory.rs`.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use super::redact::Redactor;
use super::{note, Note};
use crate::headless::export::enabled_features;
use crate::usbmon::mmap_ring::MmapReader;
use crate::usbmon::{open_nonblocking, ring, UsbmonStatus};

/// Read a sysfs/procfs/device-tree file, trimmed. Device-tree files
/// (`model`, `compatible`) are NUL-separated string lists, so beyond
/// edge-trimming, interior NULs are flattened to single spaces:
/// `"raspberrypi,5-model-b\0brcm,bcm2712\0"` reads as
/// `"raspberrypi,5-model-b brcm,bcm2712"` rather than carrying a raw NUL
/// into TOML (where it would serialize as a `\u0000` escape). Unreadable,
/// missing, or empty files are `None`.
pub fn read_trimmed(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!trimmed.is_empty()).then(|| trimmed.replace('\0', " "))
}

/// `PRETTY_NAME` from `/etc/os-release` text, quotes stripped.
pub fn os_pretty_name_from(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim().trim_matches('"').to_string())
}

// --- A. build and invocation ---------------------------------------------

#[derive(Debug, Serialize)]
pub struct BuildInfo {
    pub version: String,
    pub features: Vec<&'static str>,
    pub arch: &'static str,
    /// `rustc --version` at build time (see `build.rs`); absent when the
    /// build script could not run the compiler.
    pub rustc: Option<&'static str>,
    /// The command line as run, home paths rewritten.
    pub command: Vec<String>,
    pub effective_uid: u32,
    pub running_as_root: bool,
    pub under_sudo: bool,
    pub rust_log: Option<String>,
}

pub fn collect_build(
    command: &[String],
    rust_log: Option<String>,
    effective_uid: u32,
    under_sudo: bool,
    redactor: &mut Redactor,
) -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        features: enabled_features(),
        arch: std::env::consts::ARCH,
        rustc: option_env!("USBTOP_NG_RUSTC"),
        command: command.iter().map(|arg| redactor.text(arg)).collect(),
        effective_uid,
        running_as_root: effective_uid == 0,
        under_sudo,
        rust_log,
    }
}

// --- B. host --------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct HostInfo {
    pub kernel: String,
    pub proc_version: String,
    pub os: String,
    /// Device-tree `model`, else DMI `sys_vendor product_name`.
    pub board: String,
    /// Device-tree `compatible`; empty on DMI hosts.
    pub soc: String,
    pub cpu_model: String,
    pub cpu_count: usize,
    pub mem_total_kb: Option<u64>,
    pub uptime_s: Option<f64>,
    /// `systemd-detect-virt`'s answer, when the tool exists.
    pub virtualization: Option<String>,
    /// `/proc/cmdline` with filesystem UUIDs masked.
    pub cmdline: String,
    pub lockdown: String,
    /// Every file under `/sys/module/usbcore/parameters/`.
    pub usbcore_params: BTreeMap<String, String>,
}

/// Read `rel` under `root`, noting its absence under the name `label`.
fn read_or_note(root: &Path, rel: &str, label: &str, notes: &mut Vec<Note>) -> Option<String> {
    let value = read_trimmed(&root.join(rel));
    if value.is_none() {
        notes.push(note(label, "not readable"));
    }
    value
}

pub fn collect_host(
    proc_root: &Path,
    sys_root: &Path,
    etc_root: &Path,
    dmi_root: &Path,
    device_tree_root: &Path,
    virtualization: Option<String>,
    redactor: &mut Redactor,
) -> (HostInfo, Vec<Note>) {
    let mut notes = Vec::new();
    let kernel = read_or_note(
        proc_root,
        "sys/kernel/osrelease",
        "proc/sys/kernel/osrelease",
        &mut notes,
    );
    let proc_version = read_or_note(proc_root, "version", "proc/version", &mut notes);
    let os = read_or_note(etc_root, "os-release", "etc/os-release", &mut notes)
        .and_then(|text| os_pretty_name_from(&text));

    let board = match read_trimmed(&device_tree_root.join("model")) {
        Some(model) => model,
        None => {
            let vendor = read_trimmed(&dmi_root.join("sys_vendor")).unwrap_or_default();
            let product = read_trimmed(&dmi_root.join("product_name")).unwrap_or_default();
            let joined = format!("{vendor} {product}");
            let joined = joined.trim().to_string();
            if joined.is_empty() {
                notes.push(note(
                    "board",
                    "neither device-tree model nor DMI product name is readable",
                ));
            }
            joined
        }
    };
    let soc = read_trimmed(&device_tree_root.join("compatible")).unwrap_or_default();

    let cpuinfo = read_or_note(proc_root, "cpuinfo", "proc/cpuinfo", &mut notes);
    let cpu_model = cpuinfo
        .as_deref()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("model name") || l.starts_with("Model"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_default();
    let cpu_count = cpuinfo
        .as_deref()
        .map(|text| text.lines().filter(|l| l.starts_with("processor")).count())
        .unwrap_or(0);

    let mem_total_kb =
        read_or_note(proc_root, "meminfo", "proc/meminfo", &mut notes).and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse().ok())
        });
    let uptime_s = read_or_note(proc_root, "uptime", "proc/uptime", &mut notes)
        .and_then(|text| text.split_whitespace().next().and_then(|n| n.parse().ok()));
    if virtualization.is_none() {
        notes.push(note("systemd-detect-virt", "not available"));
    }
    let cmdline = read_or_note(proc_root, "cmdline", "proc/cmdline", &mut notes)
        .map(|text| redactor.cmdline(&text))
        .unwrap_or_default();
    let lockdown = read_or_note(
        sys_root,
        "kernel/security/lockdown",
        "sys/kernel/security/lockdown",
        &mut notes,
    )
    .unwrap_or_default();

    let mut usbcore_params = BTreeMap::new();
    let params_dir = sys_root.join("module/usbcore/parameters");
    match std::fs::read_dir(&params_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // Parameters may legitimately be empty (`quirks`), so an empty
                // read is a value, not an absence.
                let value = std::fs::read(entry.path())
                    .map(|b| String::from_utf8_lossy(&b).trim().to_string())
                    .unwrap_or_default();
                usbcore_params.insert(name, value);
            }
        }
        Err(e) => notes.push(note("sys/module/usbcore/parameters", e)),
    }

    (
        HostInfo {
            kernel: kernel.unwrap_or_default(),
            proc_version: proc_version.unwrap_or_default(),
            os: os.unwrap_or_default(),
            board,
            soc,
            cpu_model,
            cpu_count,
            mem_total_kb,
            uptime_s,
            virtualization,
            cmdline,
            lockdown,
            usbcore_params,
        },
        notes,
    )
}

/// Live: `systemd-detect-virt`'s first output line (`none` on bare metal).
/// `None` when the tool is missing or fails to run.
pub fn detect_virtualization() -> Option<String> {
    let output = Command::new("systemd-detect-virt").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next()?.trim();
    (!first.is_empty()).then(|| first.to_string())
}

// --- B. usbmon and the backend probe ------------------------------------

#[derive(Debug, Serialize)]
pub struct NodeInfo {
    pub path: String,
    pub owner_uid: u32,
    pub group_gid: u32,
    pub mode_octal: String,
    /// Whether this process could open the node (dropped at once, so the
    /// probe never pins the module).
    pub openable: bool,
}

/// `check_usbmon_status` exactly as startup sees it, plus the `/dev/usbmon*`
/// nodes with ownership and mode, and the debugfs directory listing.
#[derive(Debug, Serialize)]
pub struct UsbmonInfo {
    pub module_loaded: bool,
    pub debugfs_mounted: bool,
    pub usbmon_available: bool,
    pub binary_available: bool,
    pub text_available: bool,
    pub permission_denied: bool,
    pub available_buses: Vec<u8>,
    pub status_error: Option<String>,
    pub debugfs_entries: Vec<String>,
    pub nodes: Vec<NodeInfo>,
}

pub fn collect_usbmon(
    status: &Result<UsbmonStatus, String>,
    dev_root: &Path,
    debugfs_root: &Path,
) -> (UsbmonInfo, Vec<Note>) {
    let mut notes = Vec::new();
    let (status, status_error) = match status {
        Ok(s) => (s.clone(), None),
        Err(e) => {
            notes.push(note("usbmon status probe", e));
            (
                UsbmonStatus {
                    module_loaded: false,
                    debugfs_mounted: false,
                    usbmon_available: false,
                    binary_available: false,
                    text_available: false,
                    permission_denied: false,
                    available_buses: Vec::new(),
                },
                Some(e.clone()),
            )
        }
    };

    let mut nodes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dev_root) {
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.strip_prefix("usbmon")
                        .is_some_and(|rest| rest.parse::<u8>().is_ok())
                })
            })
            .collect();
        paths.sort_by_key(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("usbmon"))
                .and_then(|n| n.parse::<u8>().ok())
                .unwrap_or(u8::MAX)
        });
        for path in paths {
            if let Ok(meta) = std::fs::metadata(&path) {
                nodes.push(NodeInfo {
                    path: path.display().to_string(),
                    owner_uid: meta.uid(),
                    group_gid: meta.gid(),
                    mode_octal: format!("{:04o}", meta.mode() & 0o7777),
                    openable: open_nonblocking(&path).is_ok(),
                });
            }
        }
    }

    let debugfs_entries = match std::fs::read_dir(debugfs_root) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
        Err(e) => {
            notes.push(note("debugfs usbmon directory", e));
            Vec::new()
        }
    };

    (
        UsbmonInfo {
            module_loaded: status.module_loaded,
            debugfs_mounted: status.debugfs_mounted,
            usbmon_available: status.usbmon_available,
            binary_available: status.binary_available,
            text_available: status.text_available,
            permission_denied: status.permission_denied,
            available_buses: status.available_buses,
            status_error,
            debugfs_entries,
            nodes,
        },
        notes,
    )
}

/// Which source `usbmon::monitor::start_monitoring` would pick and why,
/// found with the same probes it uses (`MmapReader::probe`, then a
/// non-blocking open, then the debugfs file) without starting a capture.
#[derive(Debug, Serialize)]
pub struct BackendInfo {
    /// `"mmap"`, `"binary"`, `"text"`, or `"none"`.
    pub would_select: &'static str,
    pub reason: String,
    /// The bus whose node was probed: 0 (the aggregate) when it is listed,
    /// else the first bus, else none.
    pub probed_bus: Option<u8>,
    /// The ring size the kernel granted after the ladder, on a mappable node.
    pub ring_bytes: Option<usize>,
    pub ebpf_built_in: bool,
    pub btf_present: bool,
}

pub fn probe_backend(
    buses: &[u8],
    dev_root: &Path,
    debugfs_root: &Path,
    btf_path: &Path,
) -> BackendInfo {
    let ebpf_built_in = cfg!(feature = "ebpf");
    let btf_present = btf_path.exists();
    let probed_bus = if buses.contains(&0) {
        Some(0)
    } else {
        buses.first().copied()
    };
    let Some(bus) = probed_bus else {
        return BackendInfo {
            would_select: "none",
            reason: "no usbmon bus is available".to_string(),
            probed_bus,
            ring_bytes: None,
            ebpf_built_in,
            btf_present,
        };
    };
    let node = dev_root.join(format!("usbmon{bus}"));
    let text_file = debugfs_root.join(format!("{bus}u"));

    let (would_select, reason, ring_bytes) = if MmapReader::probe(&node) {
        // The ladder resizes only this open descriptor's ring; the kernel
        // frees it with the file, so the host is left as found.
        let ring_bytes = open_nonblocking(&node).ok().and_then(|file| {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            ring::request_ring_ladder(fd, &node);
            ring::ring_size(fd).ok()
        });
        (
            "mmap",
            format!("{} opened and its ring mapped", node.display()),
            ring_bytes,
        )
    } else if open_nonblocking(&node).is_ok() {
        (
            "binary",
            format!("{} opened but its ring could not be mapped", node.display()),
            None,
        )
    } else if text_file.exists() {
        (
            "text",
            format!(
                "{} could not be opened; {} exists",
                node.display(),
                text_file.display()
            ),
            None,
        )
    } else {
        (
            "none",
            format!(
                "neither {} nor {} can be used",
                node.display(),
                text_file.display()
            ),
            None,
        )
    };
    BackendInfo {
        would_select,
        reason,
        probed_bus,
        ring_bytes,
        ebpf_built_in,
        btf_present,
    }
}

// --- B. dmesg -------------------------------------------------------------

const DMESG_KEYWORDS: [&str; 8] = [
    "usb",
    "xhci",
    "ehci",
    "ohci",
    "dwc",
    "thunderbolt",
    "hub",
    "usbmon",
];

/// Keep the lines that mention USB, a host controller, Thunderbolt, a hub,
/// or usbmon (case-insensitive), whole. Host identity never appears on
/// those lines except a USB network adapter's MAC, which the caller masks
/// with `Redactor::mac_addresses`.
pub fn filter_dmesg(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if DMESG_KEYWORDS.iter().any(|k| lower.contains(k)) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Live: run `dmesg` and filter it. `Err` carries the reason (the tool is
/// missing, or the kernel restricts the log to root) for a note.
pub fn run_dmesg() -> Result<String, String> {
    let output = Command::new("dmesg")
        .output()
        .map_err(|e| format!("could not run dmesg: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "dmesg exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(filter_dmesg(&String::from_utf8_lossy(&output.stdout)))
}

// --- D. configuration -----------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ConfigInfo {
    /// The resolved config directory as `~/…`.
    pub dir: Option<String>,
    pub dir_mode_octal: Option<String>,
    pub preferences_path: Option<String>,
    /// `"home resolved to ~ (sudo invoker)"` or `"not under sudo"`.
    pub sudo_resolution: &'static str,
    /// File bodies, redacted, written as their own files by the bundle
    /// writer rather than inlined into `config.toml`.
    #[serde(skip)]
    pub preferences: Option<String>,
    #[serde(skip)]
    pub internal_devices: Option<String>,
}

pub fn collect_config(
    config_dir: Option<&Path>,
    preferences_file: Option<&Path>,
    under_sudo: bool,
    redactor: &mut Redactor,
) -> (ConfigInfo, Vec<Note>) {
    let mut notes = Vec::new();
    let sudo_resolution = if under_sudo {
        "home resolved to ~ (sudo invoker)"
    } else {
        "not under sudo"
    };
    let Some(dir) = config_dir else {
        notes.push(note(
            "config directory",
            "could not be resolved (HOME is not set)",
        ));
        return (
            ConfigInfo {
                dir: None,
                dir_mode_octal: None,
                preferences_path: None,
                sudo_resolution,
                preferences: None,
                internal_devices: None,
            },
            notes,
        );
    };
    let dir_mode_octal = match std::fs::metadata(dir) {
        Ok(meta) => Some(format!("{:04o}", meta.mode() & 0o7777)),
        Err(e) => {
            notes.push(note("config directory", e));
            None
        }
    };
    let preferences_file = preferences_file
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.join("preferences.toml"));
    let preferences = match std::fs::read_to_string(&preferences_file) {
        Ok(text) => Some(redactor.text(&text)),
        Err(e) => {
            notes.push(note("preferences.toml", e));
            None
        }
    };
    let internal_devices = match std::fs::read_to_string(dir.join("internal-devices.toml")) {
        Ok(text) => Some(redactor.text(&text)),
        Err(e) => {
            notes.push(note("internal-devices.toml", e));
            None
        }
    };
    (
        ConfigInfo {
            dir: Some(redactor.path(dir)),
            dir_mode_octal,
            preferences_path: Some(redactor.path(&preferences_file)),
            sudo_resolution,
            preferences,
            internal_devices,
        },
        notes,
    )
}

// --- F. terminal ----------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct TerminalInfo {
    pub term: Option<String>,
    pub colorterm: Option<String>,
    pub lang: Option<String>,
    pub lc_all: Option<String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub stdout_is_tty: bool,
    pub stdin_is_tty: bool,
    /// Whether an ssh marker variable is set; its value is never recorded.
    pub ssh_present: bool,
    /// The synchronized-output decision: `supported`, `unsupported`, or a
    /// `not probed: …` reason.
    pub sync_mode: String,
}

/// Pure: `env` answers each variable lookup, so the live gatherer in
/// `diag::support` and the tests share one function.
pub fn collect_terminal(
    env: &dyn Fn(&str) -> Option<String>,
    size: Option<(u16, u16)>,
    stdout_is_tty: bool,
    stdin_is_tty: bool,
    sync_mode: &str,
) -> TerminalInfo {
    // The allowlist is enforced here, not just documented: a name outside
    // it can never reach the bundle by value.
    let allowed = |name: &str| {
        if Redactor::env_allowlisted(name) {
            env(name)
        } else {
            None
        }
    };
    TerminalInfo {
        term: allowed("TERM"),
        colorterm: allowed("COLORTERM"),
        lang: allowed("LANG"),
        lc_all: allowed("LC_ALL"),
        cols: size.map(|s| s.0),
        rows: size.map(|s| s.1),
        stdout_is_tty,
        stdin_is_tty,
        ssh_present: Redactor::ssh_present(|name| env(name).is_some_and(|v| !v.is_empty())),
        sync_mode: sync_mode.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn read_trimmed_flattens_interior_nuls_from_device_tree_string_lists() {
        let temp = tempfile::tempdir().unwrap();
        let compatible = temp.path().join("compatible");
        std::fs::write(&compatible, b"raspberrypi,5-model-b\0brcm,bcm2712\0\n").unwrap();
        assert_eq!(
            read_trimmed(&compatible).as_deref(),
            Some("raspberrypi,5-model-b brcm,bcm2712")
        );
        assert_eq!(read_trimmed(&temp.path().join("missing")), None);
        std::fs::write(temp.path().join("empty"), b"\0\n").unwrap();
        assert_eq!(read_trimmed(&temp.path().join("empty")), None);
    }

    #[test]
    fn os_pretty_name_strips_the_quotes() {
        assert_eq!(
            os_pretty_name_from("NAME=\"Linux Mint\"\nPRETTY_NAME=\"Linux Mint 22.3\"\n")
                .as_deref(),
            Some("Linux Mint 22.3")
        );
        assert_eq!(os_pretty_name_from("NAME=x\n"), None);
    }

    #[test]
    fn build_info_records_the_invocation_without_naming_the_user() {
        let mut r = Redactor::new(Some(Path::new("/home/alice")));
        let info = collect_build(
            &[
                "/home/alice/bin/usbtop-ng".to_string(),
                "--support".to_string(),
            ],
            Some("debug".to_string()),
            0,
            true,
            &mut r,
        );
        assert_eq!(info.command, vec!["~/bin/usbtop-ng", "--support"]);
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(info.running_as_root);
        assert!(info.under_sudo);
        assert_eq!(info.rust_log.as_deref(), Some("debug"));
        assert_eq!(info.arch, std::env::consts::ARCH);
        let text = toml::to_string(&info).unwrap();
        assert!(!text.contains("alice"), "{text}");
    }

    #[test]
    fn host_info_reads_every_source_through_the_injected_roots() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "proc/sys/kernel/osrelease", "7.0.0-30-generic\n");
        write(
            root,
            "proc/version",
            "Linux version 7.0.0-30-generic (buildd@host) #30\n",
        );
        write(root, "proc/cpuinfo", "processor\t: 0\nmodel name\t: AMD Ryzen 9\nprocessor\t: 1\nmodel name\t: AMD Ryzen 9\n");
        write(
            root,
            "proc/meminfo",
            "MemTotal:       32000000 kB\nMemFree: 1 kB\n",
        );
        write(root, "proc/uptime", "12345.67 99999.00\n");
        write(
            root,
            "proc/cmdline",
            "BOOT_IMAGE=/boot/vmlinuz root=UUID=1234-abcd ro\n",
        );
        write(root, "sys/module/usbcore/parameters/autosuspend", "2\n");
        write(root, "sys/module/usbcore/parameters/quirks", "\n");
        write(
            root,
            "sys/kernel/security/lockdown",
            "[none] integrity confidentiality\n",
        );
        write(root, "etc/os-release", "PRETTY_NAME=\"Linux Mint 22.3\"\n");
        write(root, "dmi/product_name", "MG-VCP17A-3080\n");
        write(root, "dmi/sys_vendor", "Example\n");
        // No device tree: an x86 host.
        let mut r = Redactor::new(None);
        let (host, notes) = collect_host(
            &root.join("proc"),
            &root.join("sys"),
            &root.join("etc"),
            &root.join("dmi"),
            &root.join("device-tree"),
            Some("none".to_string()),
            &mut r,
        );
        assert_eq!(host.kernel, "7.0.0-30-generic");
        assert!(host
            .proc_version
            .starts_with("Linux version 7.0.0-30-generic"));
        assert_eq!(host.os, "Linux Mint 22.3");
        assert_eq!(host.board, "Example MG-VCP17A-3080");
        assert_eq!(host.soc, "");
        assert_eq!(host.cpu_model, "AMD Ryzen 9");
        assert_eq!(host.cpu_count, 2);
        assert_eq!(host.mem_total_kb, Some(32_000_000));
        assert_eq!(host.uptime_s, Some(12345.67));
        assert_eq!(host.virtualization.as_deref(), Some("none"));
        assert_eq!(
            host.cmdline,
            "BOOT_IMAGE=/boot/vmlinuz root=UUID=<redacted> ro"
        );
        assert_eq!(
            host.usbcore_params.get("autosuspend").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            host.usbcore_params.get("quirks").map(String::as_str),
            Some("")
        );
        assert_eq!(host.lockdown, "[none] integrity confidentiality");
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn host_info_notes_what_is_missing_instead_of_failing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(
            root,
            "device-tree/model",
            "Raspberry Pi 5 Model B Rev 1.0\0",
        );
        write(
            root,
            "device-tree/compatible",
            "raspberrypi,5-model-b\0brcm,bcm2712\0",
        );
        let mut r = Redactor::new(None);
        let (host, notes) = collect_host(
            &root.join("proc"),
            &root.join("sys"),
            &root.join("etc"),
            &root.join("dmi"),
            &root.join("device-tree"),
            None,
            &mut r,
        );
        assert_eq!(host.board, "Raspberry Pi 5 Model B Rev 1.0");
        assert_eq!(host.soc, "raspberrypi,5-model-b brcm,bcm2712");
        assert_eq!(host.kernel, "");
        assert_eq!(host.mem_total_kb, None);
        let items: Vec<&str> = notes.iter().map(|n| n.item.as_str()).collect();
        for expected in [
            "proc/sys/kernel/osrelease",
            "proc/version",
            "proc/cpuinfo",
            "proc/meminfo",
            "proc/uptime",
            "proc/cmdline",
            "sys/module/usbcore/parameters",
            "sys/kernel/security/lockdown",
            "etc/os-release",
            "systemd-detect-virt",
        ] {
            assert!(
                items.contains(&expected),
                "missing note for {expected}: {items:?}"
            );
        }
    }

    #[test]
    fn detect_virtualization_never_panics() {
        // Live: `systemd-detect-virt` may or may not exist here; either
        // answer is fine, only a panic would not be.
        let _ = detect_virtualization();
    }

    #[test]
    fn usbmon_info_lists_nodes_with_ownership_and_openability() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("usbmon0"), b"").unwrap();
        std::fs::write(dev.join("usbmon1"), b"").unwrap();
        std::fs::write(dev.join("unrelated"), b"").unwrap();
        let debugfs = temp.path().join("usbmon");
        std::fs::create_dir_all(&debugfs).unwrap();
        std::fs::write(debugfs.join("0u"), b"").unwrap();
        std::fs::write(debugfs.join("1u"), b"").unwrap();
        let status = Ok(UsbmonStatus {
            module_loaded: true,
            debugfs_mounted: true,
            usbmon_available: true,
            binary_available: true,
            text_available: true,
            permission_denied: false,
            available_buses: vec![0, 1],
        });
        let (info, notes) = collect_usbmon(&status, &dev, &debugfs);
        assert!(info.module_loaded);
        assert_eq!(info.available_buses, vec![0, 1]);
        assert_eq!(info.nodes.len(), 2);
        assert_eq!(
            info.nodes[0].path,
            dev.join("usbmon0").display().to_string()
        );
        assert_eq!(info.nodes[0].mode_octal.len(), 4);
        assert!(info.nodes[0].openable);
        assert_eq!(info.debugfs_entries, vec!["0u", "1u"]);
        assert!(info.status_error.is_none());
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn usbmon_info_carries_a_failed_status_probe_as_a_note() {
        let temp = tempfile::tempdir().unwrap();
        let status = Err("could not read /proc/modules".to_string());
        let (info, notes) = collect_usbmon(&status, temp.path(), &temp.path().join("nope"));
        assert_eq!(
            info.status_error.as_deref(),
            Some("could not read /proc/modules")
        );
        assert!(!info.usbmon_available);
        assert!(info.nodes.is_empty());
        assert_eq!(notes.len(), 2, "status and debugfs: {notes:?}");
    }

    #[test]
    fn backend_probe_walks_the_same_chain_as_start_monitoring() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("dev");
        let debugfs = temp.path().join("usbmon");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::create_dir_all(&debugfs).unwrap();
        let btf = temp.path().join("vmlinux");

        // Nothing at all: no interface.
        let none = probe_backend(&[], &dev, &debugfs, &btf);
        assert_eq!(none.would_select, "none");
        assert!(!none.btf_present);
        assert_eq!(none.ebpf_built_in, cfg!(feature = "ebpf"));

        // Only the debugfs text file for the aggregate bus.
        std::fs::write(debugfs.join("0u"), b"").unwrap();
        let text = probe_backend(&[0, 1], &dev, &debugfs, &btf);
        assert_eq!(text.would_select, "text");
        assert_eq!(text.probed_bus, Some(0));

        // A regular file where the binary node would be: it opens (so the
        // read()-based reader would take it) but has no ring, exactly what
        // `MmapReader::probe` and `ring_size` answer for a fixture file.
        std::fs::write(dev.join("usbmon0"), b"").unwrap();
        std::fs::write(&btf, b"").unwrap();
        let binary = probe_backend(&[0, 1], &dev, &debugfs, &btf);
        assert_eq!(binary.would_select, "binary");
        assert_eq!(binary.ring_bytes, None);
        assert!(binary.btf_present);

        // No aggregate node: the first per-bus node is probed instead.
        std::fs::remove_file(dev.join("usbmon0")).unwrap();
        std::fs::remove_file(debugfs.join("0u")).unwrap();
        std::fs::write(dev.join("usbmon2"), b"").unwrap();
        let per_bus = probe_backend(&[2, 3], &dev, &debugfs, &btf);
        assert_eq!(per_bus.probed_bus, Some(2));
        assert_eq!(per_bus.would_select, "binary");
    }

    #[test]
    fn dmesg_filter_keeps_usb_lines_case_insensitively_and_whole() {
        let text = "[    0.1] Linux version 7.0\n\
                    [    1.2] usb 1-4: new high-speed USB device number 3 using xhci_hcd\n\
                    [    1.3] usb 1-4: SerialNumber: 0123ABCD\n\
                    [    2.0] systemd[1]: Set hostname to <box>.\n\
                    [    3.0] thunderbolt 0-1: new device found\n\
                    [    4.0] hub 3-1:1.0: USB hub found\n\
                    [    5.0] DWC2 controller ready\n\
                    [    6.0] usbmon: debugfs is not available\n";
        let kept = filter_dmesg(text);
        assert!(
            kept.contains("SerialNumber: 0123ABCD"),
            "device lines stay whole"
        );
        assert!(kept.contains("thunderbolt 0-1"));
        assert!(kept.contains("USB hub found"));
        assert!(kept.contains("DWC2"));
        assert!(kept.contains("usbmon: debugfs"));
        assert!(!kept.contains("Linux version"));
        assert!(!kept.contains("hostname"));
        assert_eq!(kept.lines().count(), 6);
    }

    #[test]
    fn run_dmesg_returns_text_or_a_reason_and_never_panics() {
        match run_dmesg() {
            Ok(text) => assert!(text.lines().all(|line| {
                let lower = line.to_lowercase();
                DMESG_KEYWORDS.iter().any(|k| lower.contains(k))
            })),
            Err(reason) => assert!(!reason.is_empty()),
        }
    }

    #[test]
    fn config_info_redacts_the_directory_and_copies_both_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home").join("alice");
        let dir = home.join(".usbtop-ng");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("preferences.toml"),
            format!("usbids_path = \"{}/usb.ids\"\n", home.display()),
        )
        .unwrap();
        std::fs::write(dir.join("internal-devices.toml"), "captured_unix = 1\n").unwrap();
        let prefs = dir.join("preferences.toml");
        let mut r = Redactor::new(Some(home.as_path()));
        let (info, notes) =
            collect_config(Some(dir.as_path()), Some(prefs.as_path()), true, &mut r);
        assert_eq!(info.dir.as_deref(), Some("~/.usbtop-ng"));
        assert_eq!(info.dir_mode_octal.as_deref().map(str::len), Some(4));
        assert_eq!(
            info.preferences_path.as_deref(),
            Some("~/.usbtop-ng/preferences.toml")
        );
        assert_eq!(
            info.preferences.as_deref(),
            Some("usbids_path = \"~/usb.ids\"\n")
        );
        assert_eq!(
            info.internal_devices.as_deref(),
            Some("captured_unix = 1\n")
        );
        assert_eq!(info.sudo_resolution, "home resolved to ~ (sudo invoker)");
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn config_info_notes_a_missing_directory_and_missing_files() {
        let temp = tempfile::tempdir().unwrap();
        let mut r = Redactor::new(None);
        let absent = temp.path().join("absent");
        let (info, notes) = collect_config(Some(absent.as_path()), None, false, &mut r);
        assert!(info.dir_mode_octal.is_none());
        assert!(info.preferences.is_none());
        assert_eq!(info.sudo_resolution, "not under sudo");
        assert_eq!(
            notes.len(),
            3,
            "dir, preferences, internal-devices: {notes:?}"
        );
        let (_, none) = collect_config(None, None, false, &mut r);
        assert_eq!(none[0].item, "config directory");
    }

    #[test]
    fn terminal_info_records_allowlisted_values_and_ssh_presence_only() {
        let env = |name: &str| -> Option<String> {
            match name {
                "TERM" => Some("xterm-256color".into()),
                "COLORTERM" => Some("truecolor".into()),
                "LANG" => Some("en_US.UTF-8".into()),
                "SSH_CONNECTION" => Some("10.0.0.2 51234 10.0.0.1 22".into()),
                "HOME" => Some("/home/alice".into()),
                _ => None,
            }
        };
        let info = collect_terminal(&env, Some((120, 40)), true, false, "unsupported");
        assert_eq!(info.term.as_deref(), Some("xterm-256color"));
        assert_eq!(info.colorterm.as_deref(), Some("truecolor"));
        assert_eq!(info.lang.as_deref(), Some("en_US.UTF-8"));
        assert_eq!(info.lc_all, None);
        assert_eq!((info.cols, info.rows), (Some(120), Some(40)));
        assert!(info.stdout_is_tty);
        assert!(!info.stdin_is_tty);
        assert!(info.ssh_present);
        assert_eq!(info.sync_mode, "unsupported");
        let text = toml::to_string(&info).unwrap();
        assert!(
            !text.contains("10.0.0"),
            "ssh values never recorded: {text}"
        );
        assert!(!text.contains("alice"), "{text}");
    }
}

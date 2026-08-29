use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub mod binary;
#[cfg(feature = "ebpf")]
mod ebpf;
pub mod mmap_ring;
pub mod monitor;
pub mod parser;
pub mod reader;

/// How long a reader parks between polls when the interface has nothing to
/// give (EAGAIN or EOF). Also the worst-case latency of a shutdown request.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Open a usbmon interface non-blocking, so an idle bus cannot pin the reader
/// thread inside `read`: without `O_NONBLOCK` a thread parked on a silent `Nu`
/// file or `/dev/usbmonN` device keeps it (and therefore the usbmon module)
/// open indefinitely. Regular files never report `WouldBlock`, so
/// fixture-backed tests behave exactly as they would with a plain open.
///
/// Shared by the text ([`reader`]) and binary ([`binary`]) readers, and used by
/// [`monitor::start_monitoring`] to probe whether the binary interface exists.
pub(crate) fn open_nonblocking(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[derive(Debug, Clone)]
pub struct UsbmonStatus {
    pub module_loaded: bool,
    pub debugfs_mounted: bool,
    /// `binary_available || text_available`: whether *some* usbmon interface
    /// can be read, regardless of which one.
    pub usbmon_available: bool,
    /// Whether some `/dev/usbmon*` node opened (see [`binary_interface_available`]).
    pub binary_available: bool,
    /// Whether the debugfs `usbmon` directory is present and readable.
    pub text_available: bool,
    pub permission_denied: bool,
    /// Sysfs-discovered bus numbers, gated to empty whenever
    /// `usbmon_available` is false (see [`gate_available_buses`]): the
    /// `--force` contract is that a host with no usbmon interface at all
    /// gets an empty bus list -- and therefore empty, legitimate reports --
    /// rather than readers spawned against buses nothing can actually read.
    pub available_buses: Vec<u8>,
}

pub fn check_usbmon_status() -> Result<UsbmonStatus> {
    debug!("Checking usbmon kernel module status");

    let sysfs_root = Path::new("/sys/bus/usb/devices");
    let dev_root = Path::new("/dev");
    let debugfs_root = Path::new("/sys/kernel/debug/usb/usbmon");

    let discovered_buses = discover_buses(sysfs_root, dev_root, debugfs_root);
    let binary_available = binary_interface_available(dev_root, &discovered_buses);
    let debugfs_state = classify_debugfs_path(debugfs_root);
    let text_available = debugfs_state == DebugfsState::Present;
    let usbmon_available = binary_available || text_available;

    let debugfs_mounted = is_debugfs_mounted()?;
    let permission_denied =
        permission_denied_from(debugfs_mounted, debugfs_state, binary_available);
    let module_loaded = is_usbmon_module_loaded(binary_available, text_available)?;

    Ok(UsbmonStatus {
        module_loaded,
        debugfs_mounted,
        usbmon_available,
        binary_available,
        text_available,
        permission_denied,
        available_buses: gate_available_buses(usbmon_available, discovered_buses),
    })
}

/// Restores the `--force` contract: [`discover_buses`] runs unconditionally
/// (its result feeds [`binary_interface_available`], the probe that decides
/// `usbmon_available` in the first place), but that discovery must not leak
/// into `available_buses` when no usbmon interface actually exists. Without
/// this gate, `--force` on a host with real USB buses but no usbmon spawns
/// text readers against debugfs files that were never there, and both the
/// headless report and the TUI's `~`-estimate legend end up lying about
/// having a source.
fn gate_available_buses(usbmon_available: bool, discovered: Vec<u8>) -> Vec<u8> {
    if usbmon_available {
        discovered
    } else {
        Vec::new()
    }
}

/// Bus numbers reachable without debugfs: one per `usbN` root-hub directory
/// under `sysfs_root`, plus the aggregate bus 0 when either usbmon interface
/// for it exists (`dev_root/usbmon0`, or the debugfs `0u` file). Sysfs is the
/// only source of the per-bus list — the debugfs directory is consulted only
/// for the bus-0 special case, never scanned for its own `Nu` files, so a
/// stale debugfs entry with no matching sysfs root hub is never reported.
///
/// Roots are injectable — production passes the real `/sys/bus/usb/devices`,
/// `/dev`, and `/sys/kernel/debug/usb/usbmon` (see [`check_usbmon_status`]) —
/// mirroring the seam idiom in `DeviceManager::with_sysfs_base` /
/// `snapshot::capture`, so tests never touch the real filesystem roots.
fn discover_buses(sysfs_root: &Path, dev_root: &Path, debugfs_root: &Path) -> Vec<u8> {
    let mut buses: Vec<u8> = fs::read_dir(sysfs_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_prefix("usb")?
                .parse::<u8>()
                .ok()
        })
        .collect();
    buses.sort_unstable();
    buses.dedup();

    let aggregate_present = dev_root.join("usbmon0").exists() || debugfs_root.join("0u").exists();
    if aggregate_present && buses.first() != Some(&0) {
        buses.insert(0, 0);
    }
    buses
}

/// Whether some `/dev/usbmon*` node can actually be opened: `usbmon0` (the
/// kernel's aggregate) first, then each bus [`discover_buses`] found. Uses
/// [`open_nonblocking`], the same probe primitive
/// [`monitor::start_monitoring`] uses, and drops every handle immediately —
/// a probe must never pin the module the way a would-be reader could.
fn binary_interface_available(dev_root: &Path, buses: &[u8]) -> bool {
    std::iter::once(0)
        .chain(buses.iter().copied())
        .any(|bus| open_nonblocking(&dev_root.join(format!("usbmon{bus}"))).is_ok())
}

/// A permission problem is only nameable as such when it is the *only*
/// reason usbmon is unavailable: debugfs is mounted, its `usbmon` directory
/// exists but this user cannot read it, and no `/dev/usbmon*` node stood in
/// for it either. A root-only debugfs next to a working binary node is not
/// an error at all — the binary interface stands alone.
fn permission_denied_from(
    debugfs_mounted: bool,
    state: DebugfsState,
    binary_available: bool,
) -> bool {
    debugfs_mounted && state == DebugfsState::Unreadable && !binary_available
}

/// Whether usbmon is loaded, including kernels that build it in rather than
/// modprobe it: a built-in usbmon is never listed in `/proc/modules`, so its
/// presence there is joined with `/sys/module/usbmon` existing (module
/// parameters keep that directory around even for a built-in) and with
/// either interface already being reachable — if a device or debugfs file
/// works, the module is present in every sense that matters, whatever
/// `/proc/modules` says.
fn is_usbmon_module_loaded(binary_available: bool, text_available: bool) -> Result<bool> {
    is_usbmon_module_loaded_at(
        Path::new("/proc/modules"),
        Path::new("/sys/module/usbmon"),
        binary_available,
        text_available,
    )
}

/// [`is_usbmon_module_loaded`] with both filesystem roots injectable.
fn is_usbmon_module_loaded_at(
    proc_modules: &Path,
    sys_module_usbmon: &Path,
    binary_available: bool,
    text_available: bool,
) -> Result<bool> {
    let modules = fs::read_to_string(proc_modules)?;
    let listed_in_proc_modules = modules.lines().any(|line| line.starts_with("usbmon "));
    Ok(listed_in_proc_modules || sys_module_usbmon.exists() || binary_available || text_available)
}

fn is_debugfs_mounted() -> Result<bool> {
    let mounts = fs::read_to_string("/proc/mounts")?;
    Ok(mounts
        .lines()
        .any(|line| line.contains("debugfs") && line.contains("/sys/kernel/debug")))
}

/// What a stat of the usbmon debugfs directory tells us: present and
/// readable, absent entirely, or present but blocked by permissions.
/// `Path::exists()` collapses the latter two into one `false`, which is what
/// sends a non-root user to `modprobe`/`mount` instructions that cannot fix a
/// permission problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugfsState {
    Present,
    Absent,
    Unreadable,
}

fn classify_debugfs_path(path: &Path) -> DebugfsState {
    match std::fs::metadata(path) {
        Ok(_) => DebugfsState::Present,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => DebugfsState::Unreadable,
        Err(_) => DebugfsState::Absent,
    }
}

fn is_yes_response(input: &str) -> bool {
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

pub fn prompt_user_to_load_module() -> Result<bool> {
    println!("usbmon is not loaded, so usbtop-ng cannot read live USB traffic yet.");
    println!("usbtop-ng can run 'sudo modprobe usbmon' for you now.");
    println!("If debugfs is not mounted, it can also run:");
    println!("  sudo mount -t debugfs none /sys/kernel/debug");
    println!();
    println!("This may ask for your sudo password. Answer 'n' to leave the system unchanged.");
    print!("Load usbmon now? (y/N): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(is_yes_response(&input))
}

/// What the user is asked before an unload. It is a constant because it is
/// asked two ways: straight from stdin before the TUI starts, and over the UI
/// event channel after it exits, when stdin belongs to the input thread.
pub const UNLOAD_QUESTION: &str = concat!(
    "usbtop-ng loaded usbmon for this session.\n",
    "You can leave it loaded for future USB monitoring, or unload it now with:\n",
    "  sudo modprobe -r usbmon\n",
    "\n",
    "This may ask for your sudo password. Answer 'n' to leave usbmon loaded.\n",
    "Unload usbmon now? (y/N): ",
);

/// Ask about unloading by reading stdin. Only safe before the TUI starts:
/// once the input thread exists it owns stdin, and this would race it.
pub fn prompt_user_to_unload_module() -> Result<bool> {
    // `write!` rather than `print!`: this is an exit path, and `print!` turns a
    // failed write into a panic instead of the error this function already
    // reports.
    write!(io::stdout(), "{}", UNLOAD_QUESTION)?;
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(is_yes_response(&input))
}

pub fn attempt_load_usbmon() -> Result<()> {
    info!("Attempting to load usbmon kernel module");

    // Try to load usbmon module
    let output = Command::new("sudo")
        .args(["modprobe", "usbmon"])
        .output()
        .map_err(|e| anyhow!("Failed to run modprobe: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to load usbmon module: {}", stderr));
    }

    // Try to mount debugfs if needed
    if !is_debugfs_mounted()? {
        info!("Attempting to mount debugfs");
        let output = Command::new("sudo")
            .args(["mount", "-t", "debugfs", "none", "/sys/kernel/debug"])
            .output()
            .map_err(|e| anyhow!("Failed to mount debugfs: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "Failed to mount debugfs (may already be mounted): {}",
                stderr
            );
        }
    }

    Ok(())
}

/// Unload usbmon with `sudo modprobe -r`.
///
/// The progress line here is `debug!` where its counterpart in
/// [`attempt_load_usbmon`] is `info!`, and the asymmetry is deliberate rather
/// than an oversight. This is the only one of the two that runs on an *exit*
/// path, and the default filter level is Info — so at the default settings that
/// line was a write to stderr, in front of the unload, on a descriptor nobody
/// manages. On a terminal that is still open but has stopped reading it waited
/// there, and the unload it was announcing never happened; a pty check found it
/// parked in `write(2, …, 116)`. Nothing is lost by dropping it below the
/// default: the user is already told about an unload through stdout, by
/// [`announce_automatic_unload`] or by the question itself, and both of those
/// are skipped when the terminal cannot take them.
pub fn attempt_unload_usbmon() -> Result<()> {
    debug!("Attempting to unload usbmon kernel module");

    let output = Command::new("sudo")
        .args(["modprobe", "-r", "usbmon"])
        .output()
        .map_err(|e| anyhow!("Failed to run modprobe -r: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to unload usbmon module: {}", stderr));
    }

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum UnloadMode {
    Automatic,
    Ask,
}

pub fn unload_mode(preferences: &crate::config::Preferences) -> UnloadMode {
    if preferences.unload_usbmon_on_exit {
        UnloadMode::Automatic
    } else {
        UnloadMode::Ask
    }
}

/// Said when preferences have already decided, so that an unload the user did
/// not ask for right now is at least announced.
const AUTOMATIC_UNLOAD_NOTICE: &str =
    "unload_usbmon_on_exit=true, so usbtop-ng will try to unload usbmon now.";

/// Print the notice, unless there is nothing on the other end to read it.
///
/// Two different terminals fail here, and only one of them fails loudly. A
/// terminal that is *gone* makes the write return an error, so `writeln!` and
/// not `println!`: the print macros unwrap, and a panic on this line would skip
/// the very unload it is announcing and turn a clean exit into a 101. That is
/// the SIGHUP case — the emulator closed, so writes to the pty return EIO.
///
/// A terminal that has merely *stopped reading* fails silently and much worse.
/// It is still open, so the write does not fail — it waits, and by the time this
/// line runs `lifecycle::restore_terminal` has put stdout back to blocking, so
/// it waits forever. `terminal_reachable` is that terminal's answer: the restore
/// could not get twenty-odd bytes out inside its budget, so this notice would
/// not get out either. The unload still happens; only the sentence about it is
/// dropped.
fn announce_automatic_unload(out: &mut impl Write, terminal_reachable: bool) {
    if !terminal_reachable {
        return;
    }
    let _ = writeln!(out, "{AUTOMATIC_UNLOAD_NOTICE}");
}

/// Unload usbmon, logging a failure instead of propagating it: every caller is
/// on an exit path, where a module left loaded is a nuisance and not a reason
/// to fail.
///
/// This warning stays at its level, unlike the progress line inside
/// [`attempt_unload_usbmon`]. It is a real failure rather than routine
/// progress, and it is written *after* the attempt — so a terminal that has
/// stopped reading can delay this exit here, but it can no longer cost it the
/// unload.
fn unload_logging_failure() {
    if let Err(e) = attempt_unload_usbmon() {
        log::warn!("Failed to unload usbmon: {}", e);
    }
}

/// Offer to unload usbmon after a session in which usbtop-ng loaded it.
/// Called on every exit path that follows a successful load — including
/// startup failures after the module was loaded.
///
/// `ask` is how the question reaches the user, because that differs by exit
/// path: before the TUI starts it is a plain stdin read, and after it exits
/// stdin belongs to the input thread, so the answer comes back over the UI
/// event channel instead.
///
/// `terminal_reachable` is whether anything written here would actually arrive.
/// Before the TUI it is simply true. After it, it is
/// `tui::lifecycle::restore_landed` — because from the moment the terminal is
/// handed back, stdout is blocking again, and a notice written to a terminal
/// that has stopped reading is a process that never exits. What is dropped is
/// only the words: the unload itself does not depend on anyone seeing them.
pub fn offer_unload_after_session(
    preferences: &crate::config::Preferences,
    terminal_reachable: bool,
    ask: impl FnOnce() -> bool,
) {
    let should_unload = match unload_mode(preferences) {
        UnloadMode::Automatic => {
            announce_automatic_unload(&mut io::stdout(), terminal_reachable);
            true
        }
        UnloadMode::Ask => ask(),
    };
    if should_unload {
        unload_logging_failure();
    }
}

/// The unload path for exits with nobody to ask — a hangup, a dead terminal, a
/// failure inside the UI. The same flow, with the question already answered:
/// a standing `unload_usbmon_on_exit` is honored, and anything that would have
/// needed an answer leaves usbmon loaded, because silence is not consent.
pub fn unload_without_asking(preferences: &crate::config::Preferences, terminal_reachable: bool) {
    offer_unload_after_session(preferences, terminal_reachable, || false);
}

pub fn print_setup_instructions() {
    println!("Linux setup for live USB monitoring:");
    println!("1. Make the usbmon kernel module available:");
    println!("   sudo modprobe usbmon");
    println!(
        "2. Needed only if /dev/usbmon* is still unavailable, for the debugfs text interface:"
    );
    println!("   sudo mount -t debugfs none /sys/kernel/debug");
    println!(
        "3. Run usbtop-ng with permission to read /dev/usbmon* or /sys/kernel/debug/usb/usbmon"
    );
    println!("   The simplest test is: sudo usbtop-ng");
    println!(
        "usbtop-ng can prompt for step 1 at startup and can optionally unload usbmon on quit."
    );
}

/// Printed when usbmon is present but this user cannot read it. The tool needs
/// root, so the only remedy is `sudo`.
pub fn print_permission_remedy() {
    println!("usbmon is present but this user cannot read it.");
    println!("Run usbtop-ng with sudo:");
    println!("  sudo usbtop-ng");
}

#[cfg(test)]
mod tests {
    use super::is_yes_response;
    use super::{announce_automatic_unload, unload_mode, UnloadMode, AUTOMATIC_UNLOAD_NOTICE};
    use std::io::{self, Write};

    /// A terminal that has already gone away: every write fails, as writes to a
    /// pty whose master closed do (EIO).
    struct GoneTerminal;

    impl Write for GoneTerminal {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn the_automatic_unload_notice_survives_a_dead_terminal() {
        // The one exit path that reaches this line without a user is the one
        // where the terminal is gone, so the announcement must not panic:
        // `println!` would, and would skip the unload it announces.
        announce_automatic_unload(&mut GoneTerminal, true);
    }

    #[test]
    fn the_automatic_unload_notice_is_not_written_to_a_terminal_that_stopped_reading() {
        // The worse terminal: still open, so the write would not fail — it
        // would wait, on a descriptor the teardown has already put back to
        // blocking, on the last path of a process that is trying to leave.
        let mut out = Vec::new();
        announce_automatic_unload(&mut out, false);
        assert!(out.is_empty(), "not one byte: {out:?}");
    }

    #[test]
    fn the_automatic_unload_notice_is_written_when_the_terminal_is_reachable() {
        // The gate must not silence the ordinary case: an unload the user did
        // not ask for right now is still announced.
        let mut out = Vec::new();
        announce_automatic_unload(&mut out, true);
        let said = String::from_utf8(out).expect("the notice is utf-8");
        assert!(said.contains(AUTOMATIC_UNLOAD_NOTICE), "{said:?}");
        assert!(said.ends_with('\n'), "and it closes its own line: {said:?}");
    }

    #[test]
    fn yes_response_accepts_y_and_yes_case_insensitively() {
        assert!(is_yes_response("y"));
        assert!(is_yes_response("YES"));
        assert!(is_yes_response(" yes \n"));
    }

    #[test]
    fn yes_response_rejects_other_answers() {
        assert!(!is_yes_response(""));
        assert!(!is_yes_response("n"));
        assert!(!is_yes_response("sure"));
    }

    #[test]
    fn unload_mode_follows_preferences() {
        let auto = crate::config::Preferences {
            auto_load_usbmon: false,
            unload_usbmon_on_exit: true,
            hide_idle_devices: false,
            usbids_path: None,
        };
        assert_eq!(unload_mode(&auto), UnloadMode::Automatic);
        let ask = crate::config::Preferences::default();
        assert_eq!(unload_mode(&ask), UnloadMode::Ask);
    }

    #[test]
    fn debugfs_state_reads_present_and_absent() {
        let temp = tempfile::tempdir().unwrap();
        let present = temp.path().join("usbmon");
        std::fs::create_dir_all(&present).unwrap();
        assert_eq!(
            super::classify_debugfs_path(&present),
            super::DebugfsState::Present
        );
        assert_eq!(
            super::classify_debugfs_path(&temp.path().join("missing")),
            super::DebugfsState::Absent
        );
    }

    #[cfg(unix)]
    #[test]
    fn debugfs_state_reads_permission_denied() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses directory permissions, so this check cannot be made to
        // fail as root. Skip it there.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("locked");
        std::fs::create_dir_all(parent.join("usbmon")).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();

        let state = super::classify_debugfs_path(&parent.join("usbmon"));
        // Restore so the tempdir can be cleaned up.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(state, super::DebugfsState::Unreadable);
    }

    /// A FIFO opened `O_RDONLY | O_NONBLOCK` succeeds immediately even with no
    /// writer attached (see `binary.rs`'s `wouldblock_retry_reassembles_partial_events`),
    /// which is what makes it stand in for a live `/dev/usbmonN` character
    /// device in a hermetic test.
    fn touch_fifo(path: &std::path::Path) {
        assert!(std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn discover_buses_lists_sysfs_root_hubs_and_prepends_the_aggregate_via_a_binary_node() {
        let temp = tempfile::tempdir().unwrap();
        let sysfs_root = temp.path().join("sysfs");
        let dev_root = temp.path().join("dev");
        let debugfs_root = temp.path().join("debugfs"); // never created: no debugfs at all
        std::fs::create_dir_all(sysfs_root.join("usb1")).unwrap();
        std::fs::create_dir_all(sysfs_root.join("usb3")).unwrap();
        // A device (not a root hub) directory, which must not be mistaken
        // for a bus.
        std::fs::create_dir_all(sysfs_root.join("1-1")).unwrap();
        std::fs::create_dir_all(&dev_root).unwrap();
        touch_fifo(&dev_root.join("usbmon0"));

        let buses = super::discover_buses(&sysfs_root, &dev_root, &debugfs_root);
        assert_eq!(buses, vec![0, 1, 3]);
    }

    #[test]
    fn discover_buses_prepends_the_aggregate_via_debugfs_0u_without_a_binary_node() {
        let temp = tempfile::tempdir().unwrap();
        let sysfs_root = temp.path().join("sysfs");
        let dev_root = temp.path().join("dev"); // no usbmon* nodes at all
        let debugfs_root = temp.path().join("debugfs/usb/usbmon");
        std::fs::create_dir_all(sysfs_root.join("usb1")).unwrap();
        std::fs::create_dir_all(sysfs_root.join("usb3")).unwrap();
        std::fs::create_dir_all(&dev_root).unwrap();
        std::fs::create_dir_all(&debugfs_root).unwrap();
        std::fs::write(debugfs_root.join("0u"), "").unwrap();
        std::fs::write(debugfs_root.join("1u"), "").unwrap();
        std::fs::write(debugfs_root.join("3u"), "").unwrap();
        // A stale debugfs file with no matching sysfs root hub: bus discovery
        // is driven by sysfs, not by scanning the debugfs directory, so this
        // must not appear in the result.
        std::fs::write(debugfs_root.join("5u"), "").unwrap();

        let buses = super::discover_buses(&sysfs_root, &dev_root, &debugfs_root);
        assert_eq!(
            buses,
            vec![0, 1, 3],
            "buses come from sysfs, not a scan of the debugfs directory"
        );
    }

    #[test]
    fn discover_buses_with_nothing_present_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let sysfs_root = temp.path().join("sysfs");
        let dev_root = temp.path().join("dev");
        let debugfs_root = temp.path().join("debugfs");
        std::fs::create_dir_all(&sysfs_root).unwrap();
        std::fs::create_dir_all(&dev_root).unwrap();

        assert!(super::discover_buses(&sysfs_root, &dev_root, &debugfs_root).is_empty());
    }

    #[test]
    fn binary_interface_available_finds_a_fifo_standing_in_for_usbmon0() {
        let temp = tempfile::tempdir().unwrap();
        let dev_root = temp.path().join("dev");
        std::fs::create_dir_all(&dev_root).unwrap();
        touch_fifo(&dev_root.join("usbmon0"));

        assert!(super::binary_interface_available(&dev_root, &[1, 3]));
    }

    #[test]
    fn binary_interface_available_finds_a_per_bus_node_without_the_aggregate() {
        let temp = tempfile::tempdir().unwrap();
        let dev_root = temp.path().join("dev");
        std::fs::create_dir_all(&dev_root).unwrap();
        touch_fifo(&dev_root.join("usbmon3"));

        assert!(super::binary_interface_available(&dev_root, &[1, 3]));
    }

    #[test]
    fn binary_interface_available_is_false_with_no_dev_nodes() {
        let temp = tempfile::tempdir().unwrap();
        let dev_root = temp.path().join("dev");
        std::fs::create_dir_all(&dev_root).unwrap();

        assert!(!super::binary_interface_available(&dev_root, &[1, 3]));
    }

    /// The full scenario from the spec: a host with sysfs buses and a working
    /// binary interface but no debugfs at all. `usbmon_available` must come
    /// out true from the binary side alone.
    #[test]
    fn a_binary_only_host_discovers_buses_without_debugfs() {
        let temp = tempfile::tempdir().unwrap();
        let sysfs_root = temp.path().join("sysfs");
        let dev_root = temp.path().join("dev");
        let debugfs_root = temp.path().join("debugfs"); // never mounted
        std::fs::create_dir_all(sysfs_root.join("usb1")).unwrap();
        std::fs::create_dir_all(sysfs_root.join("usb3")).unwrap();
        std::fs::create_dir_all(&dev_root).unwrap();
        touch_fifo(&dev_root.join("usbmon0"));

        let buses = super::discover_buses(&sysfs_root, &dev_root, &debugfs_root);
        assert_eq!(buses, vec![0, 1, 3]);

        let binary_available = super::binary_interface_available(&dev_root, &buses);
        let text_available =
            super::classify_debugfs_path(&debugfs_root) == super::DebugfsState::Present;
        assert!(
            binary_available,
            "the fifo standing in for usbmon0 must open"
        );
        assert!(!text_available);
        assert!(
            binary_available || text_available,
            "usbmon_available is the OR"
        );
    }

    /// The mirror scenario: debugfs is the only working interface, and bus
    /// discovery still comes from sysfs (see the "not the old debugfs scan"
    /// assertion in `discover_buses_prepends_the_aggregate_via_debugfs_0u_without_a_binary_node`).
    #[test]
    fn a_debugfs_only_host_still_discovers_buses_from_sysfs() {
        let temp = tempfile::tempdir().unwrap();
        let sysfs_root = temp.path().join("sysfs");
        let dev_root = temp.path().join("dev"); // no usbmon* nodes at all
        let debugfs_root = temp.path().join("debugfs/usb/usbmon");
        std::fs::create_dir_all(sysfs_root.join("usb1")).unwrap();
        std::fs::create_dir_all(sysfs_root.join("usb3")).unwrap();
        std::fs::create_dir_all(&dev_root).unwrap();
        std::fs::create_dir_all(&debugfs_root).unwrap();
        std::fs::write(debugfs_root.join("0u"), "").unwrap();

        let buses = super::discover_buses(&sysfs_root, &dev_root, &debugfs_root);
        assert_eq!(buses, vec![0, 1, 3]);

        let binary_available = super::binary_interface_available(&dev_root, &buses);
        let text_available =
            super::classify_debugfs_path(&debugfs_root) == super::DebugfsState::Present;
        assert!(!binary_available);
        assert!(text_available);
    }

    /// The `--force` contract this restores: on a host where neither usbmon
    /// interface is available, `available_buses` must come back empty even
    /// though sysfs still lists real buses -- otherwise `--force` spawns
    /// readers against interfaces that were never there, which is the bug
    /// this pins (see [`super::gate_available_buses`]'s doc comment).
    #[test]
    fn available_buses_is_empty_when_neither_interface_is_available_even_with_sysfs_buses() {
        let temp = tempfile::tempdir().unwrap();
        let sysfs_root = temp.path().join("sysfs");
        let dev_root = temp.path().join("dev"); // no usbmon* nodes at all
        let debugfs_root = temp.path().join("debugfs"); // never mounted
        std::fs::create_dir_all(sysfs_root.join("usb1")).unwrap();
        std::fs::create_dir_all(sysfs_root.join("usb3")).unwrap();
        std::fs::create_dir_all(&dev_root).unwrap();

        let discovered = super::discover_buses(&sysfs_root, &dev_root, &debugfs_root);
        assert_eq!(discovered, vec![1, 3], "sysfs still lists real buses");

        let binary_available = super::binary_interface_available(&dev_root, &discovered);
        let text_available =
            super::classify_debugfs_path(&debugfs_root) == super::DebugfsState::Present;
        let usbmon_available = binary_available || text_available;
        assert!(
            !usbmon_available,
            "neither interface is available in this scenario"
        );

        let available_buses = super::gate_available_buses(usbmon_available, discovered);
        assert!(
            available_buses.is_empty(),
            "an unavailable host must report an empty bus list, not a stale discovery result"
        );
    }

    #[test]
    fn permission_denied_requires_binary_to_also_be_unavailable() {
        assert!(super::permission_denied_from(
            true,
            super::DebugfsState::Unreadable,
            false
        ));
        assert!(
            !super::permission_denied_from(true, super::DebugfsState::Unreadable, true),
            "a working /dev/usbmon node means this is not a permission error"
        );
        assert!(!super::permission_denied_from(
            true,
            super::DebugfsState::Present,
            false
        ));
        assert!(!super::permission_denied_from(
            false,
            super::DebugfsState::Unreadable,
            false
        ));
    }

    #[test]
    fn module_loaded_true_from_a_proc_modules_line() {
        let temp = tempfile::tempdir().unwrap();
        let proc_modules = temp.path().join("modules");
        std::fs::write(&proc_modules, "usbmon 45056 0 - Live 0x0000000000000000\n").unwrap();
        let sys_module_usbmon = temp.path().join("sys_module_usbmon"); // never created

        assert!(
            super::is_usbmon_module_loaded_at(&proc_modules, &sys_module_usbmon, false, false)
                .unwrap()
        );
    }

    #[test]
    fn module_loaded_true_from_a_sys_module_usbmon_directory() {
        // A kernel with usbmon compiled in never lists it in /proc/modules,
        // so a fake /sys/module/usbmon must be enough on its own.
        let temp = tempfile::tempdir().unwrap();
        let proc_modules = temp.path().join("modules");
        std::fs::write(&proc_modules, "other_module 12345 0 - Live 0x0\n").unwrap();
        let sys_module_usbmon = temp.path().join("sys_module_usbmon");
        std::fs::create_dir_all(&sys_module_usbmon).unwrap();

        assert!(
            super::is_usbmon_module_loaded_at(&proc_modules, &sys_module_usbmon, false, false)
                .unwrap()
        );
    }

    #[test]
    fn module_loaded_true_when_either_interface_is_already_present() {
        let temp = tempfile::tempdir().unwrap();
        let proc_modules = temp.path().join("modules");
        std::fs::write(&proc_modules, "other_module 12345 0 - Live 0x0\n").unwrap();
        let sys_module_usbmon = temp.path().join("sys_module_usbmon"); // never created

        assert!(
            super::is_usbmon_module_loaded_at(&proc_modules, &sys_module_usbmon, true, false)
                .unwrap(),
            "a working binary interface implies the module is present"
        );
        assert!(
            super::is_usbmon_module_loaded_at(&proc_modules, &sys_module_usbmon, false, true)
                .unwrap(),
            "a working text interface implies the module is present"
        );
    }

    #[test]
    fn module_loaded_false_when_nothing_indicates_it() {
        let temp = tempfile::tempdir().unwrap();
        let proc_modules = temp.path().join("modules");
        std::fs::write(&proc_modules, "other_module 12345 0 - Live 0x0\n").unwrap();
        let sys_module_usbmon = temp.path().join("sys_module_usbmon"); // never created

        assert!(!super::is_usbmon_module_loaded_at(
            &proc_modules,
            &sys_module_usbmon,
            false,
            false
        )
        .unwrap());
    }
}

#[cfg(all(test, feature = "integration"))]
mod integration_tests {
    use super::*;

    /// Requires: usbmon loaded, read access to at least one interface
    /// (typically root). On this development host usbmon loads as a module
    /// and debugfs is mounted, so under root both interfaces come back
    /// available; under a plain user neither is readable and the check below
    /// skips.
    /// Run: cargo test --features integration
    #[test]
    fn live_usbmon_status_and_interfaces() {
        let status = check_usbmon_status().expect("status check must run");
        if !status.usbmon_available {
            eprintln!("usbmon not available; live checks skipped");
            return;
        }
        assert!(
            status.text_available || status.binary_available,
            "usbmon_available must be backed by at least one real interface"
        );

        if status.text_available {
            let bus = status.available_buses.first().copied().unwrap_or(0);
            let text = crate::usbmon::reader::UsbmonReader::new(bus);
            assert!(
                text.is_available(),
                "text_available true but the debugfs file is missing: {}",
                text.path.display()
            );
        }

        if status.binary_available {
            // Re-probe the same candidates `binary_interface_available` did
            // (aggregate first, then each discovered bus) rather than
            // assuming bus 0 specifically: a binary-only host with no
            // aggregate interface can still be `binary_available` on a
            // per-bus node alone.
            let opened = std::iter::once(0)
                .chain(status.available_buses.iter().copied())
                .any(|bus| {
                    let path = std::path::PathBuf::from(format!("/dev/usbmon{bus}"));
                    crate::usbmon::open_nonblocking(&path).is_ok()
                });
            assert!(
                opened,
                "binary_available true but no /dev/usbmon* node reopened"
            );
        }
    }
}

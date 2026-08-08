use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub mod binary;
pub mod monitor;
pub mod parser;
pub mod reader;

/// Generic Linux value of `O_NONBLOCK`. Hardcoded because usbtop-ng has no
/// libc dependency; the value differs only on mips/alpha/sparc, which this
/// tool does not target.
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;

/// How long a reader parks between polls when the interface has nothing to
/// give (EAGAIN or EOF). Also the worst-case latency of a shutdown request.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Open a usbmon interface non-blocking on Linux so an idle bus cannot pin the
/// reader thread inside `read`: without `O_NONBLOCK` a thread parked on a
/// silent `Nu` file or `/dev/usbmonN` device keeps it (and therefore the usbmon
/// module) open indefinitely. Regular files never report `WouldBlock`, so
/// fixture-backed tests behave exactly as they would with a plain open.
///
/// Shared by the text ([`reader`]) and binary ([`binary`]) readers, and used by
/// [`monitor::start_monitoring`] to probe whether the binary interface exists.
pub(crate) fn open_nonblocking(path: &Path) -> std::io::Result<fs::File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_NONBLOCK)
            .open(path)
    }

    #[cfg(not(target_os = "linux"))]
    {
        fs::File::open(path)
    }
}

#[derive(Debug, Clone)]
pub struct UsbmonStatus {
    pub module_loaded: bool,
    pub debugfs_mounted: bool,
    pub usbmon_available: bool,
    pub available_buses: Vec<u8>,
}

pub fn check_usbmon_status() -> Result<UsbmonStatus> {
    debug!("Checking usbmon kernel module status");

    let module_loaded = is_usbmon_module_loaded()?;
    let debugfs_mounted = is_debugfs_mounted()?;
    let usbmon_available = debugfs_mounted && check_usbmon_debugfs_exists()?;
    let available_buses = if usbmon_available {
        get_available_buses()?
    } else {
        Vec::new()
    };

    Ok(UsbmonStatus {
        module_loaded,
        debugfs_mounted,
        usbmon_available,
        available_buses,
    })
}

fn is_usbmon_module_loaded() -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let modules = fs::read_to_string("/proc/modules")?;
        Ok(modules.lines().any(|line| line.starts_with("usbmon ")))
    }

    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        // BSD systems may have USB monitoring built-in or use different mechanisms
        let output = Command::new("kldstat")
            .output()
            .map_err(|e| anyhow!("Failed to run kldstat: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains("usb") || stdout.contains("ugen"))
    }

    #[cfg(target_os = "macos")]
    {
        // macOS doesn't have usbmon, but we can still detect USB via system_profiler
        warn!("macOS does not support usbmon kernel module");
        Ok(false)
    }
}

fn is_debugfs_mounted() -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let mounts = fs::read_to_string("/proc/mounts")?;
        Ok(mounts
            .lines()
            .any(|line| line.contains("debugfs") && line.contains("/sys/kernel/debug")))
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Non-Linux systems use different paths
        Ok(true)
    }
}

fn check_usbmon_debugfs_exists() -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        Ok(Path::new("/sys/kernel/debug/usb/usbmon").exists())
    }

    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        // BSD systems may use /dev/ugen* or similar
        Ok(Path::new("/dev").exists())
    }

    #[cfg(target_os = "macos")]
    {
        Ok(false)
    }
}

fn get_available_buses() -> Result<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        let mut buses = Vec::new();

        if let Ok(entries) = fs::read_dir("/sys/kernel/debug/usb/usbmon") {
            for entry in entries.flatten() {
                let filename = entry.file_name();
                let filename_str = filename.to_string_lossy();

                // Look for files like "0u", "1u", "2u", etc.
                if filename_str.ends_with('u') && filename_str.len() >= 2 {
                    if let Ok(bus_num) = filename_str[0..filename_str.len() - 1].parse::<u8>() {
                        buses.push(bus_num);
                    }
                }
            }
        }

        buses.sort();
        Ok(buses)
    }

    #[cfg(not(target_os = "linux"))]
    {
        // For non-Linux systems, we'll implement bus discovery differently
        Ok(vec![0])
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

    #[cfg(target_os = "linux")]
    {
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

    #[cfg(not(target_os = "linux"))]
    {
        Err(anyhow!(
            "Automatic module loading is only supported on Linux"
        ))
    }
}

pub fn attempt_unload_usbmon() -> Result<()> {
    info!("Attempting to unload usbmon kernel module");

    #[cfg(target_os = "linux")]
    {
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

    #[cfg(not(target_os = "linux"))]
    {
        Err(anyhow!(
            "Automatic module unloading is only supported on Linux"
        ))
    }
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

pub fn print_platform_instructions() {
    #[cfg(target_os = "linux")]
    {
        println!("Linux setup for live USB monitoring:");
        println!("1. Make the usbmon kernel module available:");
        println!("   sudo modprobe usbmon");
        println!("2. Make the usbmon debugfs files available:");
        println!("   sudo mount -t debugfs none /sys/kernel/debug");
        println!("3. Run usbtop-ng with permission to read /sys/kernel/debug/usb/usbmon");
        println!("   The simplest test is: sudo usbtop-ng");
        println!(
            "usbtop-ng can prompt for step 1 at startup and can optionally unload usbmon on quit."
        );
    }

    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        println!("📋 BSD Setup Instructions:");
        println!("1. Ensure USB support is enabled in kernel");
        println!("2. Check available USB devices with: usbconfig");
        println!("3. Run usbtop-ng with appropriate permissions");
    }

    #[cfg(target_os = "macos")]
    {
        println!("📋 macOS Setup Instructions:");
        println!("⚠️  Note: macOS does not have usbmon equivalent");
        println!("Consider using alternative tools like:");
        println!("- USB Prober (part of Additional Tools for Xcode)");
        println!("- system_profiler SPUSBDataType");
        println!("- ioreg -p IOUSB");
    }
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
        };
        assert_eq!(unload_mode(&auto), UnloadMode::Automatic);
        let ask = crate::config::Preferences::default();
        assert_eq!(unload_mode(&ask), UnloadMode::Ask);
    }
}

#[cfg(all(test, feature = "integration", target_os = "linux"))]
mod integration_tests {
    use super::*;

    /// Requires: Linux, usbmon loaded, read access (typically root).
    /// Run: cargo test --features integration
    #[test]
    fn live_usbmon_status_and_interfaces() {
        let status = check_usbmon_status().expect("status check must run");
        if !status.usbmon_available {
            eprintln!("usbmon not available; live checks skipped");
            return;
        }
        let bus = status.available_buses.first().copied().unwrap_or(0);
        let text = crate::usbmon::reader::UsbmonReader::new(bus);
        assert!(
            text.is_available(),
            "text interface file missing: {}",
            text.path.display()
        );
        let binary = std::path::Path::new("/dev/usbmon0");
        if binary.exists() {
            eprintln!("binary node present: {}", binary.display());
            match crate::usbmon::open_nonblocking(binary) {
                Ok(_) => eprintln!("binary node opened ok: {}", binary.display()),
                Err(e) => eprintln!("binary node open failed: {}: {}", binary.display(), e),
            }
        }
    }
}

use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use std::fs;
use std::path::Path;
use std::process::Command;

pub mod monitor;
pub mod parser;
pub mod reader;

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
    use std::io::{self, Write};

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

pub fn prompt_user_to_unload_module() -> Result<bool> {
    use std::io::{self, Write};

    println!("usbtop-ng loaded usbmon for this session.");
    println!("You can leave it loaded for future USB monitoring, or unload it now with:");
    println!("  sudo modprobe -r usbmon");
    println!();
    println!("This may ask for your sudo password. Answer 'n' to leave usbmon loaded.");
    print!("Unload usbmon now? (y/N): ");
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

/// Offer to unload usbmon after a session in which usbtop-ng loaded it.
/// Called on every exit path that follows a successful load — including
/// startup failures after the module was loaded.
pub fn offer_unload_after_session(preferences: &crate::config::Preferences) {
    let should_unload = match unload_mode(preferences) {
        UnloadMode::Automatic => {
            println!("unload_usbmon_on_exit=true, so usbtop-ng will try to unload usbmon now.");
            true
        }
        UnloadMode::Ask => prompt_user_to_unload_module().unwrap_or(false),
    };
    if should_unload {
        if let Err(e) = attempt_unload_usbmon() {
            log::warn!("Failed to unload usbmon: {}", e);
        }
    }
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
    use super::{unload_mode, UnloadMode};

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

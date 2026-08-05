#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use anyhow::Result;
use clap::Parser;
use log::{error, info, warn};
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::process;

mod config;
mod device;
mod stats;
mod ui;
mod usbmon;

use std::time::Duration;

use config::{load_or_create_default_at, Preferences};
use ui::{run_ui, UsbTopApp};
use usbmon::{
    attempt_load_usbmon, attempt_unload_usbmon, check_usbmon_status, print_platform_instructions,
    prompt_user_to_load_module, prompt_user_to_unload_module,
};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(name = "usbtop-ng")]
#[command(about = "Next-generation USB monitoring tool with real-time bandwidth tracking")]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Preferences file path (default: ~/.usbtop-ng/preferences.toml)
    #[arg(short, long)]
    config: Option<String>,

    /// Refresh rate in milliseconds
    #[arg(short, long, default_value = "1000")]
    refresh: u64,

    /// Force run without usbmon (limited functionality)
    #[arg(long)]
    force: bool,

    /// Show platform-specific setup instructions
    #[arg(long)]
    setup: bool,

    /// Create shell alias for 'usbtop' command
    #[arg(long)]
    create_alias: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    if cli.verbose {
        env_logger::Builder::from_default_env()
            .filter_level(log::LevelFilter::Debug)
            .init();
    } else {
        env_logger::Builder::from_default_env()
            .filter_level(log::LevelFilter::Info)
            .init();
    }

    info!("Starting usbtop-ng v{}", env!("CARGO_PKG_VERSION"));

    // Show setup instructions if requested
    if cli.setup {
        print_platform_instructions();
        return Ok(());
    }

    // Create shell alias if requested
    if cli.create_alias {
        create_shell_alias()?;
        return Ok(());
    }

    let preferences = match &cli.config {
        Some(path) => load_or_create_default_at(Path::new(path))?,
        None => Preferences::load_or_create_default()?,
    };

    // Check usbmon status
    let mut usbmon_status = match check_usbmon_status() {
        Ok(status) => status,
        Err(e) => {
            error!("Failed to check usbmon status: {}", e);
            if !cli.force {
                process::exit(1);
            }
            warn!("Continuing in force mode with limited functionality");
            usbmon::UsbmonStatus {
                module_loaded: false,
                debugfs_mounted: false,
                usbmon_available: false,
                available_buses: Vec::new(),
            }
        }
    };
    let mut loaded_usbmon_for_this_run = false;

    // Handle usbmon not being available
    if !usbmon_status.usbmon_available && !cli.force {
        if !usbmon_status.module_loaded {
            let should_load = if preferences.auto_load_usbmon {
                println!("usbmon is not loaded; auto_load_usbmon=true, so usbtop-ng will try to load it now.");
                true
            } else {
                prompt_user_to_load_module()?
            };

            if should_load {
                if let Err(e) = attempt_load_usbmon() {
                    error!("Failed to load usbmon: {}", e);
                    println!();
                    print_platform_instructions();
                    process::exit(1);
                }
                loaded_usbmon_for_this_run = true;

                // Re-check status after loading
                usbmon_status = check_usbmon_status()?;
                if !usbmon_status.usbmon_available {
                    error!(
                        "usbmon was loaded, but the usbmon debugfs interface is still unavailable"
                    );
                    print_platform_instructions();
                    process::exit(1);
                }

                info!("usbmon module loaded successfully");
            } else {
                println!("usbmon was not loaded; live USB monitoring is unavailable.");
                println!("Run with --force to open the UI with limited functionality, or run --setup for manual setup steps.");
                process::exit(1);
            }
        } else if !usbmon_status.debugfs_mounted {
            error!("debugfs is not mounted, so /sys/kernel/debug/usb/usbmon is unavailable");
            print_platform_instructions();
            process::exit(1);
        } else {
            error!("usbmon is loaded, but /sys/kernel/debug/usb/usbmon is unavailable");
            print_platform_instructions();
            process::exit(1);
        }
    }

    // Log available buses
    if !usbmon_status.available_buses.is_empty() {
        info!("Available USB buses: {:?}", usbmon_status.available_buses);
    } else if !cli.force {
        warn!("No USB buses detected");
    }

    let (packets, monitor) = usbmon::monitor::start_monitoring(&usbmon_status.available_buses);
    let manager = device::manager::DeviceManager::new();
    let app = UsbTopApp::new(Duration::from_millis(cli.refresh));
    let run_result = run_ui(app, manager, packets);

    // Close the usbmon files before anything tries to unload the module: an
    // open debugfs `Nu` file pins usbmon, so `modprobe -r` would fail EBUSY.
    monitor.stop();

    if loaded_usbmon_for_this_run {
        let should_unload = if preferences.unload_usbmon_on_exit {
            println!("unload_usbmon_on_exit=true, so usbtop-ng will try to unload usbmon now.");
            true
        } else {
            prompt_user_to_unload_module()?
        };

        if should_unload {
            if let Err(e) = attempt_unload_usbmon() {
                warn!("Failed to unload usbmon: {}", e);
            }
        }
    }

    run_result?;

    Ok(())
}

fn create_shell_alias() -> Result<()> {
    println!("🔗 Creating shell alias for 'usbtop' command...\n");

    // Get the current executable path
    let current_exe = env::current_exe()?;
    let exe_path = current_exe.to_string_lossy();

    println!("Current executable: {}", exe_path);
    println!("This will create an alias so you can run 'usbtop' instead of 'usbtop-ng'\n");

    // Detect shell
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let shell_name = Path::new(&shell).file_name().unwrap().to_string_lossy();

    println!("Detected shell: {} ({})", shell_name, shell);

    // Determine config file based on shell
    let home = env::var("HOME")?;
    let config_file = match shell_name.as_ref() {
        "bash" => format!("{}/.bashrc", home),
        "zsh" => format!("{}/.zshrc", home),
        "fish" => format!("{}/.config/fish/config.fish", home),
        "tcsh" | "csh" => format!("{}/.cshrc", home),
        _ => format!("{}/.profile", home),
    };

    println!("Will add alias to: {}", config_file);

    // Ask for confirmation
    print!("\nDo you want to create the alias? (y/N): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    if !["y", "yes"].contains(&input.trim().to_lowercase().as_str()) {
        println!("Alias creation cancelled.");
        return Ok(());
    }

    // Generate alias command based on shell
    let alias_command = match shell_name.as_ref() {
        "fish" => format!("alias usbtop '{}'", exe_path),
        _ => format!("alias usbtop='{}'", exe_path),
    };

    // Check if alias already exists
    if Path::new(&config_file).exists() {
        let content = std::fs::read_to_string(&config_file)?;
        if content.contains("alias usbtop") {
            println!("⚠️  An 'usbtop' alias already exists in {}!", config_file);
            print!("Do you want to replace it? (y/N): ");
            io::stdout().flush()?;

            let mut replace_input = String::new();
            io::stdin().read_line(&mut replace_input)?;

            if !["y", "yes"].contains(&replace_input.trim().to_lowercase().as_str()) {
                println!("Alias creation cancelled.");
                return Ok(());
            }
        }
    }

    // Add the alias to the config file
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config_file)?;

    writeln!(
        file,
        "\n# usbtop-ng alias (added by usbtop-ng --create-alias)"
    )?;
    writeln!(file, "{}", alias_command)?;

    println!("✅ Successfully added alias to {}", config_file);
    println!("\nTo use the alias in your current session, run:");
    println!("  source {}", config_file);
    println!("\nOr start a new terminal session.");
    println!("\nYou can now run: usbtop");

    Ok(())
}

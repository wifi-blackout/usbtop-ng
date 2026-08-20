// usbmon is the only packet source usbtop-ng has, and usbmon is a Linux
// kernel module. Every other platform once carried stub checks that passed
// without a source behind them, so the UI opened onto a table that could never
// fill. This says so at compile time instead.
#[cfg(not(target_os = "linux"))]
compile_error!("usbtop-ng supports Linux only.");

use anyhow::Result;
use clap::Parser;
use log::{error, info, warn};
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::process;
use std::sync::Arc;

mod config;
mod device;
mod filter;
mod headless;
mod stats;
mod tui;
mod ui;
mod usbmon;

use std::time::Duration;

use config::{ensure_private_config_dir, load_or_create_default_at, preferences_path};
use tui::lifecycle::{unload_policy, UnloadPolicy};
use tui::{effective_refresh_ms, run_ui};
use ui::UsbTopApp;
use usbmon::{
    attempt_load_usbmon, check_usbmon_status, print_setup_instructions, prompt_user_to_load_module,
    prompt_user_to_unload_module,
};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(name = "usbtop-ng")]
#[command(about = "Live USB bandwidth monitor for Linux")]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Preferences file path (default: ~/.usbtop-ng/preferences.toml)
    #[arg(short, long)]
    config: Option<String>,

    /// Refresh rate in milliseconds (floored at 100ms)
    #[arg(short, long, default_value = "1000")]
    refresh: u64,

    /// Force run without usbmon (limited functionality)
    #[arg(long)]
    force: bool,

    /// Show setup instructions for live monitoring
    #[arg(long)]
    setup: bool,

    /// Create shell alias for 'usbtop' command
    #[arg(long)]
    create_alias: bool,

    /// Show only traffic matching KEY=VALUE terms (repeatable, expressions OR)
    #[arg(long, value_name = "KEY=VALUE[,KEY=VALUE...]")]
    filter: Vec<String>,

    /// Sample one window, print a report, and exit
    #[arg(long)]
    once: bool,

    /// Print a report every window until interrupted
    #[arg(long, conflicts_with = "once")]
    batch: bool,

    /// Print reports as JSON (one document per report)
    #[arg(long)]
    json: bool,

    /// Sample window in seconds (default: 5 with --once, 1 with --batch)
    #[arg(long, value_name = "SECONDS")]
    window: Option<f64>,
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
        print_setup_instructions();
        return Ok(());
    }

    // Create shell alias if requested
    if cli.create_alias {
        create_shell_alias()?;
        return Ok(());
    }

    let config_path = match &cli.config {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            let path = preferences_path()?;
            if let Some(parent) = path.parent() {
                ensure_private_config_dir(parent)?;
            }
            path
        }
    };
    let preferences = load_or_create_default_at(&config_path)?;

    let filter = filter::FilterSet::parse(&cli.filter).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(2);
    });

    // `--once`/`--batch` select a headless report instead of the TUI; `--json`
    // and `--window` only make sense alongside one of them.
    let headless = cli.once || cli.batch;
    if (cli.json || cli.window.is_some()) && !headless {
        eprintln!("error: --json and --window need --once or --batch");
        process::exit(2);
    }
    let window = Duration::from_secs_f64(
        cli.window
            .unwrap_or(if cli.batch { 1.0 } else { 5.0 })
            .max(0.25),
    );

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
                permission_denied: false,
                available_buses: Vec::new(),
            }
        }
    };
    let mut loaded_usbmon_for_this_run = false;

    // Handle usbmon not being available
    if !usbmon_status.usbmon_available && !cli.force {
        if !usbmon_status.module_loaded {
            let should_load = if preferences.auto_load_usbmon {
                if !headless {
                    println!("usbmon is not loaded; auto_load_usbmon=true, so usbtop-ng will try to load it now.");
                }
                true
            } else if headless {
                eprintln!("error: usbmon is not available and this mode never prompts.");
                eprintln!("Set auto_load_usbmon = true in the preferences file, run 'sudo modprobe usbmon' first, or run 'usbtop-ng --setup' for the manual steps.");
                process::exit(1);
            } else {
                prompt_user_to_load_module()?
            };

            if should_load {
                if let Err(e) = attempt_load_usbmon() {
                    error!("Failed to load usbmon: {}", e);
                    if headless {
                        print_remedy_to_stderr(false);
                    } else {
                        println!();
                        print_setup_instructions();
                    }
                    process::exit(1);
                }
                loaded_usbmon_for_this_run = true;

                // Re-check status after loading
                usbmon_status = check_usbmon_status()?;
                if !usbmon_status.usbmon_available {
                    error!(
                        "usbmon was loaded, but the usbmon debugfs interface is still unavailable"
                    );
                    if headless {
                        print_remedy_to_stderr(usbmon_status.permission_denied);
                    } else if usbmon_status.permission_denied {
                        usbmon::print_permission_remedy();
                    } else {
                        print_setup_instructions();
                    }
                    // Still before the TUI, so stdin is nobody else's yet — and
                    // stdout is the plain blocking one this process started
                    // with, which nothing has had a chance to wedge. Headless
                    // mode has nobody to read a prompt either way, so it never
                    // asks: it silently follows whatever `unload_usbmon_on_exit`
                    // already decided instead of blocking a script on stdin.
                    if may_prompt_before_unload(headless) {
                        usbmon::offer_unload_after_session(&preferences, true, || {
                            prompt_user_to_unload_module().unwrap_or(false)
                        });
                    } else {
                        usbmon::unload_without_asking(&preferences, true);
                    }
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
            if headless {
                print_remedy_to_stderr(false);
            } else {
                print_setup_instructions();
            }
            process::exit(1);
        } else {
            error!("usbmon is loaded, but /sys/kernel/debug/usb/usbmon is unavailable");
            if headless {
                print_remedy_to_stderr(usbmon_status.permission_denied);
            } else if usbmon_status.permission_denied {
                usbmon::print_permission_remedy();
            } else {
                print_setup_instructions();
            }
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

    if headless {
        let mut manager = device::manager::DeviceManager::new();
        manager.set_filter(filter.clone());
        let result = headless::run(
            manager,
            packets,
            Arc::clone(&monitor.dropped),
            Arc::clone(&monitor.text_active),
            filter,
            headless::HeadlessOptions {
                json: cli.json,
                batch: cli.batch,
                window,
            },
        );
        monitor.stop();
        if loaded_usbmon_for_this_run {
            usbmon::unload_without_asking(&preferences, true);
        }
        return result;
    }

    let mut manager = device::manager::DeviceManager::new();
    manager.set_filter(filter.clone());
    // The readers discard packets rather than block when the channel fills, so
    // the UI needs the count to say so in its header.
    let app = UsbTopApp::new(Duration::from_millis(effective_refresh_ms(cli.refresh)))
        .with_dropped_counter(Arc::clone(&monitor.dropped))
        .with_idle_setting(
            preferences.hide_idle_devices,
            config_path,
            preferences.clone(),
        )
        .with_filter(filter)
        .with_text_source_flag(Arc::clone(&monitor.text_active));
    let session = run_ui(app, manager, packets);

    // Close the usbmon files before anything tries to unload the module: an
    // open debugfs `Nu` file pins usbmon, so `modprobe -r` would fail EBUSY.
    // This runs on every exit path, prompts or no prompts.
    monitor.stop();

    // Everything below writes to a stdout that the teardown has already put
    // back to blocking, so a terminal that stopped reading would swallow the
    // rest of this exit rather than fail. This is the same answer the prompt
    // path uses, asked once for all of them.
    let terminal_reachable = tui::lifecycle::restore_landed();

    match &session {
        Ok(session) => match unload_policy(&session.reason, loaded_usbmon_for_this_run) {
            // The session ended with a user still in front of it, so the
            // answer comes back over the event channel: the input thread owns
            // stdin until the process exits.
            UnloadPolicy::PromptFlow => {
                usbmon::offer_unload_after_session(&preferences, terminal_reachable, || {
                    session.confirm(usbmon::UNLOAD_QUESTION)
                });
            }
            UnloadPolicy::AutoOnly => {
                usbmon::unload_without_asking(&preferences, terminal_reachable);
            }
            UnloadPolicy::Skip => {}
        },
        // A failure inside run_ui leaves the terminal in an unknown state and
        // may have left no input thread to read an answer, so this path never
        // asks either.
        Err(_) if loaded_usbmon_for_this_run => {
            usbmon::unload_without_asking(&preferences, terminal_reachable);
        }
        Err(_) => {}
    }

    session?;

    Ok(())
}

/// Same guidance as [`print_setup_instructions`]/[`usbmon::print_permission_remedy`],
/// routed to stderr instead of stdout. A headless run's stdout is either a
/// report stream or, on this failing exit, silent, and setup prose belongs on
/// neither: a script reading `--once --json` must not see it mixed in.
fn print_remedy_to_stderr(permission_denied: bool) {
    let _ = write_remedy(&mut io::stderr(), permission_denied);
}

/// The text [`print_remedy_to_stderr`] writes, factored out onto an injected
/// writer so its content is testable without capturing the real stderr (see
/// the `remedy_text_*` tests below). Every write is a plain `writeln!`, and
/// like the other exit-path writers in this codebase (e.g.
/// `usbmon::announce_automatic_unload`), a failed write here changes nothing:
/// this runs right before `process::exit(1)`, and the exit code is the part
/// of this message that always gets through.
fn write_remedy(out: &mut impl Write, permission_denied: bool) -> io::Result<()> {
    if permission_denied {
        writeln!(out, "usbmon is present but this user cannot read it.")?;
        writeln!(out, "Run usbtop-ng with sudo:")?;
        writeln!(out, "  sudo usbtop-ng")?;
    } else {
        writeln!(out, "Linux setup for live USB monitoring:")?;
        writeln!(out, "1. Make the usbmon kernel module available:")?;
        writeln!(out, "   sudo modprobe usbmon")?;
        writeln!(out, "2. Make the usbmon debugfs files available:")?;
        writeln!(out, "   sudo mount -t debugfs none /sys/kernel/debug")?;
        writeln!(
            out,
            "3. Run usbtop-ng with permission to read /sys/kernel/debug/usb/usbmon"
        )?;
        writeln!(out, "   The simplest test is: sudo usbtop-ng")?;
    }
    Ok(())
}

/// Whether the "usbmon loaded but still unavailable" exit path may ask an
/// interactive question before it decides whether to unload what this run
/// loaded. A headless run has nobody to answer a stdin prompt — asking here
/// would hang a script or a cron job forever on a question nobody will ever
/// answer — so it must always take the silent path instead
/// ([`usbmon::unload_without_asking`], which still honors a standing
/// `unload_usbmon_on_exit = true`; it only skips the question).
fn may_prompt_before_unload(headless: bool) -> bool {
    !headless
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_never_prompts_before_the_unload_offer() {
        assert!(
            !may_prompt_before_unload(true),
            "a headless run must never block on a stdin prompt"
        );
        assert!(
            may_prompt_before_unload(false),
            "the interactive TUI path keeps asking"
        );
    }

    #[test]
    fn remedy_text_differs_by_permission_denied() {
        let mut denied = Vec::new();
        write_remedy(&mut denied, true).unwrap();
        let denied = String::from_utf8(denied).unwrap();
        assert!(denied.contains("sudo usbtop-ng"));
        assert!(!denied.contains("modprobe"), "{denied}");

        let mut missing = Vec::new();
        write_remedy(&mut missing, false).unwrap();
        let missing = String::from_utf8(missing).unwrap();
        assert!(missing.contains("sudo modprobe usbmon"));
        assert!(missing.contains("sudo mount -t debugfs"));
    }
}

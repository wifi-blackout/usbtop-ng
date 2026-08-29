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
mod snapshot;
mod stats;
mod tui;
mod ui;
mod usbids;
mod usbmon;

use std::time::Duration;

use config::{
    chown_to_invoker, config_home, ensure_private_config_dir, load_or_create_default_at,
    preferences_path,
};
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

    /// Print the man page to stdout
    #[arg(long)]
    print_man: bool,

    /// Print a completion script to stdout for the named shell (e.g. bash, zsh, fish)
    #[arg(long, value_name = "SHELL")]
    print_completions: Option<clap_complete::Shell>,

    /// usb.ids database file for device names (overrides every other source)
    #[arg(long, value_name = "PATH")]
    usbids: Option<String>,

    /// Check for a newer usb.ids ('check', the default) or fetch it ('pull')
    #[arg(long, value_name = "MODE", num_args = 0..=1, default_missing_value = "check")]
    update_usbids: Option<UpdateUsbidsMode>,

    /// Record every currently attached device as internal, then exit
    #[arg(long)]
    snapshot_internal: bool,
}

/// `--update-usbids`'s optional mode. Bare `--update-usbids` (no value)
/// parses as `Check` via `default_missing_value`.
#[derive(Clone, clap::ValueEnum)]
enum UpdateUsbidsMode {
    Check,
    Pull,
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

    // Print the man page if requested
    if cli.print_man {
        use clap::CommandFactory;
        let man = clap_mangen::Man::new(Cli::command());
        let mut rendered = Vec::new();
        man.render(&mut rendered)?;
        io::stdout().write_all(&rendered)?;
        return Ok(());
    }

    // Print a shell completion script if requested
    if let Some(shell) = cli.print_completions {
        use clap::CommandFactory;
        clap_complete::generate(shell, &mut Cli::command(), "usbtop-ng", &mut io::stdout());
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

    // `--update-usbids` short-circuits everything else: it needs the
    // resolution chain and the home-copy destination (both derived from
    // `preferences_path`, not `--config`, since the download always lands
    // in the standard `~/.usbtop-ng` location), but never touches usbmon.
    if let Some(mode) = &cli.update_usbids {
        // Unlike the monitoring path below, this genuinely needs a home: the
        // download always lands at `~/.usbtop-ng/usb.ids`, so a missing HOME
        // propagates as an error here rather than silently dropping the
        // source.
        let home_copy = preferences_path()?.with_file_name("usb.ids");
        let chain = usbids::source_chain(
            cli.usbids.as_deref().map(Path::new),
            preferences.usbids_path.as_deref().map(Path::new),
            Some(&home_copy),
        );
        let chain_refs: Vec<&Path> = chain.iter().map(|p| p.as_path()).collect();
        let result = match mode {
            UpdateUsbidsMode::Check => usbids::check_usbids(&chain_refs),
            UpdateUsbidsMode::Pull => usbids::pull_usbids(&home_copy, &chain_refs),
        };
        if let Err(e) = result {
            eprintln!("error: {e}");
            process::exit(1);
        }
        return Ok(());
    }

    // `--snapshot-internal` also short-circuits: it needs the home
    // directory (the snapshot file always lives beside the preferences
    // file, resolved the same way `--update-usbids`'s home copy is), but
    // it never touches usbmon or the network -- it only reads sysfs.
    if cli.snapshot_internal {
        let snapshot = snapshot::Snapshot::capture(None)?;
        if snapshot.devices.is_empty() {
            eprintln!("error: no USB devices found to snapshot");
            process::exit(1);
        }
        let dest = snapshot::snapshot_path()?;
        // Mirrors the monitoring path's usbids resolution further down (CLI
        // flag, preferences key, home copy via the same `.ok()` pattern): the
        // printed lines get the same resolved names a live session would
        // show. Unlike this handler's own `dest` above, a missing HOME here
        // just means the printed lines carry no names -- it does not fail
        // the snapshot.
        let usbids_home_copy = preferences_path().ok().map(|p| p.with_file_name("usb.ids"));
        let usbids = usbids::resolve_database(
            cli.usbids.as_deref().map(Path::new),
            preferences.usbids_path.as_deref().map(Path::new),
            usbids_home_copy.as_deref(),
        );
        // This loop is the untestable part of the handler (real stdout, and
        // `snapshot::Snapshot::capture` above already read real sysfs); the
        // name composition it calls out to is covered on its own in
        // `snapshot::describe`'s unit tests.
        for device in &snapshot.devices {
            let name = snapshot::describe(device, usbids.as_ref());
            let suffix = if name.is_empty() {
                String::new()
            } else {
                format!("  {name}")
            };
            println!(
                "  {}  {}:{}{}",
                device.port_path,
                device.vendor_id.as_deref().unwrap_or("----"),
                device.product_id.as_deref().unwrap_or("----"),
                suffix,
            );
        }
        // `write_to` itself does not create directories (see its doc
        // comment); `~/.usbtop-ng` may not exist yet on a first run that
        // goes straight for `--snapshot-internal`, the same gap
        // `--update-usbids pull` closes for its own destination.
        if let Some(parent) = dest.parent() {
            ensure_private_config_dir(parent)?;
        }
        snapshot.write_to(&dest)?;
        chown_to_invoker(&dest);
        println!(
            "{} devices recorded as internal in {}",
            snapshot.devices.len(),
            dest.display()
        );
        return Ok(());
    }

    let filter = filter::FilterSet::parse(&cli.filter).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(2);
    });

    // Mirrors the usbids home-copy pattern just below: a missing HOME must
    // not fail the monitoring path, it just means no snapshot to load. Kept
    // as its own binding (rather than folded straight into
    // `internal_snapshot` below) so the error path can still name the path
    // it searched when one was resolved.
    let snapshot_path_result = snapshot::snapshot_path();
    let internal_snapshot = snapshot_path_result
        .as_ref()
        .ok()
        .and_then(|p| snapshot::Snapshot::load(p))
        .map(Arc::new);
    if filter.uses_internal() && internal_snapshot.is_none() {
        match &snapshot_path_result {
            Ok(path) => eprintln!(
                "error: an internal= filter needs a snapshot and none was found at {}. Run usbtop-ng --snapshot-internal first, with external devices unplugged.",
                path.display()
            ),
            Err(_) => eprintln!(
                "error: an internal= filter needs a snapshot. Run usbtop-ng --snapshot-internal first, with external devices unplugged."
            ),
        }
        process::exit(2);
    }

    // `--once`/`--batch` select a headless report instead of the TUI; `--json`
    // and `--window` only make sense alongside one of them.
    let headless = cli.once || cli.batch;
    if (cli.json || cli.window.is_some()) && !headless {
        eprintln!("error: --json and --window need --once or --batch");
        process::exit(2);
    }
    let window = resolve_window(cli.window, cli.batch).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(2);
    });

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
                binary_available: false,
                text_available: false,
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
                        "usbmon was loaded, but no usbmon interface (binary or text) is available"
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
                        // headless: still unload if the preferences say so,
                        // but never print the notice — a headless run's
                        // stdout is either a report stream or silent, per
                        // `print_remedy_to_stderr`'s doc comment above.
                        usbmon::unload_without_asking(&preferences, false);
                    }
                    process::exit(1);
                }

                info!("usbmon module loaded successfully");
            } else {
                println!("usbmon was not loaded; live USB monitoring is unavailable.");
                println!("Run with --force to open the UI with limited functionality, or run --setup for manual setup steps.");
                process::exit(1);
            }
        } else if !usbmon_status.debugfs_mounted && !usbmon_status.binary_available {
            // The debugfs-specific remedy only makes sense when the binary
            // path is unavailable too -- a binary-only host (debugfs never
            // mounted) is caught by the `usbmon_available` OR above and never
            // reaches this block at all.
            error!("no usbmon interface is available: debugfs is not mounted and no /dev/usbmon* device was found");
            if headless {
                print_remedy_to_stderr(false);
            } else {
                print_setup_instructions();
            }
            process::exit(1);
        } else {
            // debugfs is mounted but its usbmon directory is not usable
            // (unreadable, or otherwise not present), and the binary
            // interface is unavailable too -- see the comment above.
            error!("usbmon is loaded, but no usbmon interface (binary or text) is available");
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
        info!(
            "Available USB buses: {:?} (binary interface: {}, text interface: {})",
            usbmon_status.available_buses,
            usbmon_status.binary_available,
            usbmon_status.text_available
        );
    } else if !cli.force {
        warn!("No USB buses detected");
    }

    let (packets, monitor) = usbmon::monitor::start_monitoring(&usbmon_status.available_buses);

    // Resolved once for the whole run and shared by both the headless and
    // TUI managers below; monitoring never re-resolves or touches the
    // network -- that only happens under `--update-usbids`, handled earlier.
    // Unlike that earlier branch, a missing HOME here must not fail the run:
    // `--config` points at an explicit preferences file, so monitoring has
    // to work even when `preferences_path()` cannot locate `~/.usbtop-ng` --
    // the home-copy source is simply dropped from the chain (see
    // `usbids::source_chain`), leaving the CLI flag, preferences key, and
    // distro paths still in play.
    let usbids_home_copy = preferences_path().ok().map(|p| p.with_file_name("usb.ids"));
    let usbids = usbids::resolve_database(
        cli.usbids.as_deref().map(Path::new),
        preferences.usbids_path.as_deref().map(Path::new),
        usbids_home_copy.as_deref(),
    )
    .map(Arc::new);

    if headless {
        let mut manager = device::manager::DeviceManager::new();
        manager.set_filter(filter.clone());
        manager.set_usbids(usbids.clone());
        manager.set_internal_snapshot(internal_snapshot.clone());
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
                // Readers spawn only for detected buses, and `available_buses`
                // is empty both on a genuinely busless host and on one where
                // usbmon itself is unavailable (see
                // `usbmon::gate_available_buses`) -- so `--force` on either
                // gets empty, intended-output reports; with buses, a dead
                // channel means capture failed and the run must say so (see
                // headless::run).
                expect_capture: !usbmon_status.available_buses.is_empty(),
            },
        );
        monitor.stop();
        if loaded_usbmon_for_this_run {
            // `terminal_reachable = false`: headless unloads silently. The
            // automatic-unload notice is stdout prose that would otherwise
            // land after the report, corrupting `--once --json > file`.
            usbmon::unload_without_asking(&preferences, false);
        }
        return result;
    }

    let mut manager = device::manager::DeviceManager::new();
    manager.set_filter(filter.clone());
    manager.set_usbids(usbids.clone());
    manager.set_internal_snapshot(internal_snapshot.clone());
    // The readers discard packets rather than block when the channel fills, so
    // the UI needs the count to say so in its header.
    let mut app = UsbTopApp::new(Duration::from_millis(effective_refresh_ms(cli.refresh)))
        .with_dropped_counter(Arc::clone(&monitor.dropped))
        .with_idle_setting(
            preferences.hide_idle_devices,
            config_path,
            preferences.clone(),
        )
        .with_filter(filter)
        .with_text_source_flag(Arc::clone(&monitor.text_active));
    // Mirrors the usbids/internal-snapshot home-copy pattern above: a
    // missing HOME must not fail the TUI, it just means `S`'s `y` has
    // nowhere to write and lands in `Done` saying so (see
    // `ui::confirm_snapshot`).
    if let Ok(dest) = snapshot::snapshot_path() {
        app = app.with_snapshot_dest(dest);
    }
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
        writeln!(
            out,
            "2. Needed only if /dev/usbmon* is still unavailable, for the debugfs text interface:"
        )?;
        writeln!(out, "   sudo mount -t debugfs none /sys/kernel/debug")?;
        writeln!(
            out,
            "3. Run usbtop-ng with permission to read /dev/usbmon* or /sys/kernel/debug/usb/usbmon"
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

/// Resolve `--window` into a [`Duration`], applying the default (5s for
/// `--once`, 1s for `--batch`) and the 0.25s floor. `Duration::from_secs_f64`
/// panics on a NaN, an infinity, or a finite value too large for a `Duration`
/// to represent — `--window inf` reached it directly and turned an argument
/// error into a panic (exit 101). This validates first and reports a normal
/// exit-2 error instead, the same way an invalid `--filter` expression does.
fn resolve_window(window: Option<f64>, batch: bool) -> Result<Duration, String> {
    let seconds = window.unwrap_or(if batch { 1.0 } else { 5.0 });
    if !seconds.is_finite() {
        return Err(format!(
            "--window must be a finite number of seconds, got {seconds}"
        ));
    }
    let floored = seconds.max(0.25);
    Duration::try_from_secs_f64(floored).map_err(|_| format!("--window {floored} is out of range"))
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

    // Determine config file based on shell. Under sudo this follows the
    // invoking user's home (see `config::config_home`), not root's, so the
    // rc edit lands where that user's shell actually reads it.
    let home = config_home()?.to_string_lossy().into_owned();
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

    // Add the alias to the config file. `created` decides whether this run
    // owns the file below: chown a freshly created rc, but leave an
    // existing one's ownership untouched (appending to it needs no call).
    let created = !Path::new(&config_file).exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config_file)?;

    writeln!(
        file,
        "\n# usbtop-ng alias (added by usbtop-ng --create-alias)"
    )?;
    writeln!(file, "{}", alias_command)?;
    if created {
        chown_to_invoker(Path::new(&config_file));
    }

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
    fn cli_parses_print_completions_shell() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["usbtop-ng", "--print-completions", "bash"]).unwrap();
        assert!(cli.print_completions.is_some());
        assert!(Cli::try_parse_from(["usbtop-ng", "--print-completions", "nosuch"]).is_err());
    }

    #[test]
    fn cli_parses_update_usbids_modes() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["usbtop-ng", "--update-usbids"]).unwrap();
        assert!(matches!(bare.update_usbids, Some(UpdateUsbidsMode::Check)));

        let pull = Cli::try_parse_from(["usbtop-ng", "--update-usbids", "pull"]).unwrap();
        assert!(matches!(pull.update_usbids, Some(UpdateUsbidsMode::Pull)));

        assert!(Cli::try_parse_from(["usbtop-ng", "--update-usbids", "bogus"]).is_err());

        let absent = Cli::try_parse_from(["usbtop-ng"]).unwrap();
        assert!(absent.update_usbids.is_none());
    }

    #[test]
    fn cli_parses_snapshot_internal() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["usbtop-ng", "--snapshot-internal"]).unwrap();
        assert!(cli.snapshot_internal);

        let absent = Cli::try_parse_from(["usbtop-ng"]).unwrap();
        assert!(!absent.snapshot_internal);
    }

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
    fn resolve_window_applies_the_right_default_per_mode() {
        assert_eq!(
            resolve_window(None, false).unwrap(),
            Duration::from_secs_f64(5.0),
            "--once defaults to a 5s window"
        );
        assert_eq!(
            resolve_window(None, true).unwrap(),
            Duration::from_secs_f64(1.0),
            "--batch defaults to a 1s window"
        );
    }

    #[test]
    fn resolve_window_floors_a_too_small_value() {
        assert_eq!(
            resolve_window(Some(0.1), false).unwrap(),
            Duration::from_secs_f64(0.25)
        );
        assert_eq!(
            resolve_window(Some(-5.0), false).unwrap(),
            Duration::from_secs_f64(0.25),
            "a negative window floors the same as a too-small positive one"
        );
    }

    #[test]
    fn resolve_window_passes_through_an_ordinary_value() {
        assert_eq!(
            resolve_window(Some(10.0), true).unwrap(),
            Duration::from_secs_f64(10.0)
        );
    }

    #[test]
    fn resolve_window_rejects_non_finite_values_instead_of_panicking() {
        assert!(resolve_window(Some(f64::INFINITY), false).is_err());
        assert!(resolve_window(Some(f64::NEG_INFINITY), false).is_err());
        assert!(resolve_window(Some(f64::NAN), false).is_err());
    }

    #[test]
    fn resolve_window_rejects_a_finite_value_too_large_for_duration() {
        // Finite, but far beyond what `Duration` (u64 seconds) can hold.
        assert!(resolve_window(Some(1e300), false).is_err());
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

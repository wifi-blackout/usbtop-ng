# Changelog

All notable changes to usbtop-ng will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Degraded-terminal robustness chassis (`src/tui/`), so a terminal that cannot keep up degrades the display instead of the session:
  - Non-blocking output stage (`ShedWriter`) over an `O_NONBLOCK` stdout, holding a queue of whole frames. When the backlog outgrows its watermark (tmux's `1 + cols * rows * 8`, with a 4096-byte floor so a 0x0 or 1x1 screen cannot shed-storm) the queued frames are dropped rather than buffered forever, and a full repaint replaces them. Frame granularity makes truncation mid-escape-sequence impossible, and a one-second grace period keeps the recovery frame from being shed in its turn
  - `shed: N` in the header once frames have been dropped, alongside the existing `dropped: N` — the numbers are current, the screen is N frames behind, and neither undercount is silent
  - Write-failure recovery: a write that fails without the terminal being gone invalidates the screen and costs a full repaint instead of drawing diffs against a display that no longer matches; `EPIPE`/`EIO`, or 30 unclassified failures in a row with nothing landing in between, end the session through the normal teardown
  - Synchronized output (mode 2026) when the terminal answers a DECRQM handshake, with a DA1 marker so the probe costs 100ms only against terminals that answer nothing at all. Frames are bracketed at staging time, so begin, diff and end are one indivisible queue entry. A remote session (`SSH_TTY`/`SSH_CONNECTION`/`SSH_CLIENT`) is not probed at all
  - Terminal restore on panic, `SIGHUP`, `SIGINT` and `SIGTERM`: signals arrive as ordinary UI events and leave through the same teardown as `q`. The restore is idempotent and bounded four ways — its own writes give up after 250ms, it trips a latch that stops the output stage writing before it hands the descriptor back to blocking, and both remaining exit-path stdout writes (the usbmon question and the automatic-unload notice) are skipped when the restore could not get its own bytes out. Every write usbtop-ng makes to **stdout** after teardown is therefore bounded or skipped. **stderr is deliberately not** — log lines and a panic's backtrace are diagnostics written by the logger and the Rust runtime, and a non-blocking stderr would truncate them, so on a terminal that is still open but has stopped reading those wait like any program's would
- `Ctrl-L`: wipe the screen and repaint it from scratch, without the cursor-position round trip that `Terminal::clear` would need (and that fails exactly when a repaint is wanted)
- `Ctrl-C` quits: raw mode disables ISIG, so `^C` never becomes a `SIGINT` — it arrives as a key event and is bound accordingly
- 100ms floor on `--refresh`: below that the loop spends more time waking up than the terminal can usefully repaint, so lower values are clamped rather than honored literally
- Live USB bandwidth monitoring pipeline wired end-to-end: usbmon reader thread(s) → mpsc channel → DeviceManager aggregation → per-tick UI refresh
- Real binary usbmon interface (`/dev/usbmonN`): reads the kernel's 48-byte native-endian event headers directly and drains each event's captured payload, used automatically when the device can be opened, with transparent fallback to the debugfs `Nu` text interface otherwise (one `info!` log line states which interface was chosen)
- Full parser for usbmon's `Nu` text interface format
- Controller-grouped, physically port-ordered device list: devices are listed under a `═ controller ═` heading and `▶ Bus NN (USB2 side/USB3 side)` bus headers, in physical port order (parsed from the resolved sysfs directory name), with the USB2-side and USB3-side buses of a shared xHCI controller listed as adjacent sibling buses; the list's vertical scroll follows the selected device so it can't be walked off-screen
- Per-device and per-bus %busy, measured against each USB speed's practical (protocol-overhead-adjusted) bandwidth and rendered in the device list and bus headers (`-- busy` when the bus speed is unknown)
- ⚡ high-utilization (>80% busy) and 🔺 capability-exceeds-bus indicators in the device list's `!` column, the latter driven by a best-effort capability signal (sysfs `version`, i.e. bcdUSB >= 3.00) read once via the device's resolved sysfs path
- Color-coded USB link speeds: the Speed cell of every device row, the speed figure in every bus header, and a color legend in the controls block
- Split bandwidth chart pane: the aggregate total chart on the left, the selected device's rx/tx rate history on the right ([-60, 0]s window), with a placeholder when no device is selected or the selection disappears
- `integration` cargo feature: opt-in tests that exercise the real usbmon interfaces on a live Linux system (`cargo test --features integration`), skipping gracefully when usbmon isn't available; the default `cargo test`/`cargo test --all-targets` suite and CI are unaffected
- Device metadata (vendor, product, speed) resolved from sysfs by busnum/devnum topology
- Interactive terminal UI with ratatui
- Cross-platform support: live monitoring via usbmon is Linux-only; on BSD/macOS the UI can open (with `--force` where needed) but shows no devices
- Device disconnection tracking with a 5-second grace period before removal from the UI
- Bandwidth history visualization with a 60-second sliding window
- Clear usbmon detection with explicit prompts before running sudo
- Preferences in `~/.usbtop-ng/preferences.toml` for automatic usbmon load and unload behavior, now honored on exit paths that follow a post-load failure as well
- Comprehensive help system and keyboard navigation
- Command-line interface with multiple options
- Platform-specific setup instructions
- Unit test coverage across parsing, aggregation, config, and UI state

### Changed
- The UI loop is event-driven instead of a fixed 50ms poll: it sleeps until the earliest deadline it owes (the next data tick, or a pending frame one 33ms frame interval after the last), folds a whole batch of queued events into a single repaint, and draws only when something changed. An idle session no longer redraws at all between refresh ticks; a burst of resize events costs one frame rather than one per event. The wait is capped at 50ms because nothing wakes the loop when a packet arrives, so that cap doubles as the packet-drain cadence
- Mouse capture is no longer enabled. Nothing was bound to it, and holding it meant the terminal's own selection and copy stopped working inside the UI
- The terminal setup/teardown and event loop moved out of `src/ui/mod.rs` into `src/tui/` (`mod`, `events`, `output`, `sync`, `lifecycle`); `src/ui/` keeps app state, key handling and widget rendering
- Post-session prompts (the usbmon unload question) are answered through the UI event channel rather than by reading stdin: the input thread parks on a terminal read for the life of the process, so a `read_line` on the exit path would race it for every keystroke and usually lose
- Preferences-directory permission hardening (0700) now applies only when usbtop-ng creates the default `~/.usbtop-ng` directory; existing or custom-path directories are left untouched
- `PUBLIC/LICENSE` now matches the root `LICENSE` verbatim (BSD-3-Clause, copyright usbtop-ng contributors)
- Documentation (README, CONTRIBUTING, ARCHITECTURE) reconciled with the implemented thread + mpsc + dual-interface (binary preferred, text fallback) design
- `UsbTopApp`'s flat device map replaced with a per-tick `sync_from(&DeviceManager)` snapshot (`ControllerView`/`BusView`/`DeviceRow`) that also drives the new controller/bus grouping and port ordering

### Fixed
- The header's stats line — `Total` / `Peak` / `Devices`, and the `dropped:` and `shed:` counters — is now actually visible. The layout gave the header three rows for two content lines inside a border, so the whole second line had been clipped away; every test that drew the header did so into a rect of its own choosing and never saw it
- Per-packet bandwidth accounting is now O(1): `BandwidthStats` keeps the 10-second window as fixed 250ms buckets with a running sum instead of one entry per packet plus a full-window rescan on every URB, so sustained traffic no longer costs work proportional to the packets already in the window
- The reader→UI packet channel is bounded (`sync_channel`, 16384 packets) and readers hand packets over with `try_send`: a consumer that cannot keep up (busy bus, slow terminal over SSH) can no longer grow memory without bound. Packets that do not fit are counted, not waited on — readers never park, so `MonitorHandle::stop()` still joins promptly — and the UI header shows `dropped: N` whenever the count is above zero. The UI also applies at most 8192 packets per event-loop pass, so a burst cannot stall a frame
- A bus whose `/dev/usbmonN` cannot be opened now falls back to that bus's debugfs `Nu` text interface instead of leaving the bus dark: the interface probe at startup only tried the first target bus, and a per-bus open failure used to kill just that reader thread with a warning
- The 🔺 indicator no longer guesses: the old `bcdDevice`/`bMaxPacketSize0` heuristic flagged ordinary full-speed devices (a `bcdDevice` of 0x0300+ is a vendor firmware revision, and `bMaxPacketSize0 == 64` is legal at full speed). It is now a best-effort signal from the device's declared bcdUSB version (sysfs `version` >= 3.00) and simply stays silent when there is no signal
- Both 60-second histories are now trimmed by age instead of by sample count, so `bandwidth_history` and `rate_history` really cover 60 seconds at any `--refresh` rate (a 60-sample cap meant 15s at `--refresh 250` and 120s at `--refresh 2000`, while the charts kept claiming a minute)

### Removed
- Tokio and chrono dependencies

### Technical Details
- Built with Rust 2021 edition
- Dedicated blocking reader thread(s), opened `O_NONBLOCK` with a shutdown handle, feeding an `mpsc` channel read by the UI thread
- Terminal UI powered by ratatui and crossterm, drawn by a deadline-driven event loop with a dirty gate and a ~30 FPS cap
- Two new dependencies for the TUI chassis: `libc` (`fcntl` for the non-blocking descriptor and the flags to restore, `write(2)` for the frame drain, and the `EIO`/`SIGHUP` constants the failure classification is written against) and `signal-hook` (a safe iterator-shaped signal API, in place of hand-rolled `sigaction` handlers)
- USB packet parsing for both the binary (`/dev/usbmonN`) and text (`Nu`) usbmon interfaces, selected automatically at startup
- Multi-threaded architecture with proper error handling
- Modular codebase with clear separation of concerns

## [0.1.0] - 2024-07-30

### Added
- Initial release of usbtop-ng
- Core USB monitoring functionality
- Terminal user interface
- Cross-platform compatibility layer
- Documentation and build system

---

## Release Notes

### Version 0.1.0

This is the initial release of usbtop-ng, a next-generation USB monitoring tool designed to replace and enhance the original usbtop utility.

#### Key Features
- **Real-time monitoring**: Live USB bandwidth tracking with sub-second updates
- **Rich terminal UI**: Colorful, interactive interface inspired by modern system monitors
- **Cross-platform**: Native support for Linux, BSD variants, and macOS
- **Smart detection**: usbmon module detection, clear setup assistance, and optional saved load/unload preferences
- **Visual feedback**: Device status indicators (connected/disconnected)
- **Historical data**: Bandwidth graphs with configurable time windows

#### System Requirements
- Rust 1.88 or later (for building from source)
- Linux: usbmon kernel module and debugfs
- BSD: Native USB monitoring interfaces
- macOS: No live monitoring (no usbmon equivalent); the UI opens with `--force` but shows no devices

#### Installation
```bash
git clone https://github.com/wifi-blackout/usbtop-ng.git
cd usbtop-ng
cargo install --path .
```

The crate is not currently published on crates.io; `cargo install usbtop-ng` will work once it is. Pre-built binaries may also be available from the releases page.

#### Usage
```bash
# Basic usage (will prompt for usbmon setup if needed)
usbtop-ng

# Show help
usbtop-ng --help

# Platform-specific setup instructions
usbtop-ng --setup
```

#### Known Limitations
- Requires root privileges on most systems
- macOS support is limited due to lack of usbmon equivalent
- Some USB controllers may not be fully supported
- Both usbmon interfaces (binary `/dev/usbmonN`, text `Nu`) are supported, but only on Linux; there is no live-monitoring equivalent on BSD/macOS

#### Roadmap
- Additional platform-specific optimizations  
- Enhanced device filtering and search
- Export functionality for bandwidth data
- Plugin system for custom monitors
- Network-based monitoring for remote systems

---

For detailed technical information, see the [README.md](README.md) and [documentation](docs/).

Report issues and feature requests on [GitHub Issues](https://github.com/wifi-blackout/usbtop-ng/issues).
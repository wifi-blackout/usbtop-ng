# Changelog

All notable changes to usbtop-ng are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Degraded-terminal chassis (`src/tui/`), so a terminal that cannot keep up
  costs the display rather than the session:
  - Non-blocking output stage (`ShedWriter`) over an `O_NONBLOCK` stdout,
    holding a queue of whole frames. Once the backlog outgrows its watermark,
    the stage drops the queued frames instead of buffering them forever. It
    then asks for a full repaint in their place. The watermark is tmux's rule,
    `1 + cols * rows * 8`, with a 4096-byte floor so a 0x0 or 1x1 screen cannot
    shed-storm. Frame granularity makes truncation mid-escape-sequence
    impossible, and a one-second grace period keeps the recovery frame from
    being shed in its turn
  - `shed: N` in the header once frames have been dropped, alongside the
    existing `dropped: N`. The numbers are current and the screen is N frames
    behind. Neither undercount is silent
  - Write-failure recovery. A write that fails without the terminal being gone
    invalidates the screen and costs a full repaint. Drawing diffs against a
    display that no longer matches is the alternative. `EPIPE`, `EIO`, or 30
    unclassified failures in a row with nothing landing in between end the
    session through the normal teardown
  - Synchronized output (mode 2026) when the terminal answers a DECRQM
    handshake. A DA1 marker keeps the probe to 100ms only against terminals
    that answer nothing at all. Frames are bracketed at staging time, so begin,
    diff, and end form one indivisible queue entry. usbtop-ng does not probe a
    remote session (`SSH_TTY`, `SSH_CONNECTION`, or `SSH_CLIENT`) at all
  - Terminal restore on panic, `SIGHUP`, `SIGINT`, and `SIGTERM`. Signals
    arrive as ordinary UI events and leave through the same teardown as `q`.
    The restore is idempotent and bounded four ways. Its own writes give up
    after 250ms. It trips a latch that stops the output stage writing before it
    hands the descriptor back to blocking mode. And it skips both remaining
    exit-path stdout writes, the usbmon question and the automatic-unload
    notice, when the restore could not get its own bytes out
  - Every write usbtop-ng makes to **stdout** after teardown is therefore
    bounded or skipped. The exit flow reaches the unload without writing a
    routine diagnostic in front of it. `attempt_unload_usbmon`'s progress line
    is `debug!`, below the default filter, for exactly that reason. **stderr
    itself is deliberately not bounded.** Log lines and a panic's backtrace are
    diagnostics written by the logger and the Rust runtime, and a non-blocking
    stderr would truncate them. On a terminal that is still open but has
    stopped reading, those wait like any program's would
- `Ctrl-L`: wipe the screen and repaint it from scratch. It skips the
  cursor-position round trip that `Terminal::clear` would need, and that fails
  exactly when a repaint is wanted
- `Ctrl-C` quits. Raw mode disables ISIG, so `^C` never becomes a `SIGINT`. It
  arrives as a key event and is bound accordingly
- 100ms floor on `--refresh`. Below that the loop spends more time waking up
  than the terminal can usefully repaint, so usbtop-ng clamps lower values
  rather than honoring them literally
- Live USB bandwidth monitoring pipeline wired end to end: usbmon reader
  threads → mpsc channel → DeviceManager aggregation → per-interval UI refresh
- Real binary usbmon interface (`/dev/usbmonN`). It reads the kernel's 48-byte
  native-endian event headers directly and drains each event's captured
  payload. usbtop-ng uses it whenever the device opens, and falls back to the
  debugfs `Nu` text interface otherwise. One `info!` log line states which
  interface was chosen
- Full parser for usbmon's `Nu` text interface format
- Controller-grouped, physically port-ordered device table. Devices list under
  a `═ controller ═` heading and `▶ Bus NN (USB2 side/USB3 side)` bus headers,
  in physical port order parsed from the resolved sysfs directory name. The
  USB2-side and USB3-side buses of a shared xHCI controller list as adjacent
  sibling buses. The table's vertical scroll follows the selected device, so
  the selection cannot be walked off screen
- Per-device and per-bus %busy, measured against each USB speed's practical,
  protocol-overhead-adjusted bandwidth. It renders in the device table and in
  the bus headers, as `-- busy` when the bus speed is unknown
- ⚡ high-utilization (above 80% busy) and 🔺 capability-exceeds-bus indicators
  in the device table's `!` column. A best-effort capability signal drives 🔺:
  sysfs `version`, that is bcdUSB 3.00 or higher, read once through the
  device's resolved sysfs path
- Color-coded USB link speeds: the Speed cell of every device row, the speed
  figure in every bus header, and a color legend in the controls bar
- Split chart pane: the aggregate total chart on the left, and the selected
  device's rx/tx rate history on the right over a [-60, 0]s window. A
  placeholder fills the right chart when no device is selected, and again when
  the selection disappears
- `integration` cargo feature: opt-in tests that exercise the real usbmon
  interfaces on a live Linux system (`cargo test --features integration`),
  skipping cleanly when usbmon is not available. The default `cargo test` and
  `cargo test --all-targets` suite and CI are unaffected
- Device metadata (vendor, product, speed) resolved from sysfs by busnum and
  devnum topology
- Interactive terminal UI with ratatui
- Device disconnection tracking, with a 5-second grace period before removal
  from the UI
- Bandwidth history visualization over a 60-second sliding window
- usbmon detection with explicit prompts before running sudo
- Preferences in `~/.usbtop-ng/preferences.toml` for automatic usbmon load and
  unload behavior, now honored on exit paths that follow a post-load failure as
  well
- Help overlay and keyboard navigation
- Command-line interface with multiple options
- Setup instructions printed by `--setup`
- Unit test coverage across parsing, aggregation, config, and UI state

### Changed
- The help overlay states `Linux only` in place of a line about which platforms
  list no devices. `--setup` describes itself as showing setup instructions for
  live monitoring, rather than platform-specific ones
- The UI loop is event-driven instead of a fixed 50ms poll. It sleeps until the
  earliest deadline it owes, either the next refresh interval or a pending
  frame one 33ms frame interval after the last. It folds a whole batch of queued events
  into a single repaint, and draws only when something changed. An idle session
  no longer redraws at all between refresh intervals, and a burst of resize
  events costs one frame rather than one per event. The wait is capped at 50ms
  because nothing wakes the loop when a packet arrives, so that cap doubles as
  the packet-drain cadence
- Mouse capture is no longer enabled. Nothing was bound to it, and holding it
  stopped the terminal's own selection and copy working inside the UI
- The terminal setup, teardown, and event loop moved out of `src/ui/mod.rs`
  into `src/tui/` (`mod`, `events`, `output`, `sync`, `lifecycle`). `src/ui/`
  keeps app state, key handling, and widget rendering
- Post-session prompts, that is the usbmon unload question, are answered
  through the UI event channel rather than by reading stdin. The input thread
  parks on a terminal read for the life of the process. A `read_line` on the
  exit path would therefore race it for every keystroke, and usually lose
- Preferences-directory permission hardening (0700) now applies only when
  usbtop-ng creates the default `~/.usbtop-ng` directory. Existing directories
  and custom paths are left untouched
- `PUBLIC/LICENSE` now matches the root `LICENSE` verbatim (BSD-3-Clause,
  copyright usbtop-ng contributors)
- Documentation (README, CONTRIBUTING, ARCHITECTURE) reconciled with the
  implemented thread, mpsc, and dual-interface design. usbtop-ng prefers the
  binary interface and falls back to the text interface
- Documentation rewritten to one voice, with a complete features list in
  `README.md` and `PUBLIC/README.md`, and numbered procedures in
  `docs/INSTALL.md` and `docs/CONTRIBUTING.md`
- `UsbTopApp`'s flat device map replaced with a per-interval
  `sync_from(&DeviceManager)` snapshot (`ControllerView`, `BusView`,
  `DeviceRow`) that also drives the controller and bus grouping and the port
  ordering

### Fixed
- The header's stats line, that is `Total`, `Peak`, `Devices`, and the
  `dropped:` and `shed:` counters, is now actually visible. The layout gave the
  header three rows for two content lines inside a border, so the whole second
  line had been clipped away. Every test that drew the header did so into a
  rect of its own choosing and never saw it
- Per-packet bandwidth accounting is now O(1). `BandwidthStats` keeps the
  10-second window as fixed 250ms buckets with a running sum. The old shape was
  one entry per packet plus a full-window rescan on every URB. Sustained
  traffic no longer costs work proportional to the packets already in the
  window
- The reader-to-UI packet channel is bounded (`sync_channel`, 16384 packets)
  and readers hand packets over with `try_send`. A consumer that cannot keep up
  (a busy bus, a slow terminal over SSH) can no longer grow memory without
  bound. Packets that do not fit are counted rather than waited on, so readers
  never park and `MonitorHandle::stop()` still joins promptly. The UI header
  shows `dropped: N` whenever the count is above zero. The UI also
  applies at most 8192 packets per event-loop pass, so a burst cannot stall a
  frame
- A bus whose `/dev/usbmonN` cannot be opened now falls back to that bus's
  debugfs `Nu` text interface instead of leaving the bus dark. The interface
  probe at startup only tried the first target bus. A per-bus open failure used
  to kill only that reader thread, with a warning
- The 🔺 indicator no longer guesses. The old `bcdDevice` and
  `bMaxPacketSize0` heuristic flagged ordinary full-speed devices. A
  `bcdDevice` of 0x0300 or higher is a vendor firmware revision, and a
  `bMaxPacketSize0` of 64 is legal at full speed. The indicator now reads the
  device's declared bcdUSB version (sysfs `version` 3.00 or higher). It stays
  silent when there is no signal
- Both 60-second histories are now trimmed by age instead of by sample count,
  so `bandwidth_history` and `rate_history` really cover 60 seconds at any
  `--refresh` rate. A 60-sample cap meant 15s at `--refresh 250` and 120s at
  `--refresh 2000`, while the charts kept claiming a minute

### Removed
- The BSD and macOS code paths. They were stubs with no packet source behind
  them: `kldstat` and `/dev` existence checks that could pass without a usbmon
  equivalent, a `/dev/ugen{bus}.0` reader path, a `/dev/null` placeholder path,
  and `update_bsd_device_info` and `update_macos_device_info`, both of which
  returned `Ok(())` and populated nothing. The UI opened onto a device table
  that could never fill
- The BSD and macOS setup text printed by `--setup`, which pointed at
  `usbconfig`, `system_profiler`, `ioreg`, and USB Prober. Nothing in usbtop-ng
  ever called them
- Every non-Linux `cfg` gate, including the `#[cfg(not(unix))]` fallbacks in
  the TUI chassis and the preferences directory. `src/main.rs` now carries
  `#[cfg(not(target_os = "linux"))] compile_error!`, so an unsupported target
  fails at compile time instead of building a binary that lists no devices
- Tokio and chrono dependencies

### Technical Details
- Built with Rust 2021 edition
- Dedicated blocking reader threads, opened `O_NONBLOCK` with a shutdown
  handle, feeding an `mpsc` channel read by the UI thread
- Terminal UI powered by ratatui and crossterm, drawn by a deadline-driven
  event loop with a dirty gate and a 30 FPS cap
- Two new dependencies for the TUI chassis. `libc` supplies `fcntl` for the
  non-blocking descriptor and the flags to restore. It also supplies `write(2)`
  for the frame drain, and the `EIO` and `SIGHUP` constants the failure
  classification is written against. `signal-hook` supplies a safe
  iterator-shaped signal API, in place of hand-rolled `sigaction` handlers
- USB packet parsing for both the binary (`/dev/usbmonN`) and text (`Nu`)
  usbmon interfaces, selected automatically at startup
- Multi-threaded architecture with explicit error handling
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

This is the initial release of usbtop-ng, a USB bandwidth monitor with a
terminal UI, written to replace and extend the original usbtop utility.

#### Key Features
- **Real-time monitoring**: live USB bandwidth tracking with sub-second updates
- **Terminal UI**: a color, interactive interface in the shape of a modern
  system monitor
- **Cross-platform**: 0.1.0 built for Linux, BSD variants, and macOS. Live
  monitoring was Linux-only
- **usbmon detection**: usbmon module detection, printed setup steps, and
  optional saved load and unload preferences
- **Visual feedback**: device status indicators, connected and disconnected
- **Historical data**: bandwidth graphs over a 60-second window

#### System Requirements
- Rust 1.88 or later, to build from source
- Linux: the usbmon kernel module and debugfs
- BSD: native USB monitoring interfaces
- macOS: no live monitoring, because macOS had no usbmon equivalent. The UI
  opened with `--force` and showed no devices

#### Installation
```bash
git clone https://github.com/wifi-blackout/usbtop-ng.git
cd usbtop-ng
cargo install --path .
```

The crate is not published on crates.io yet. `cargo install usbtop-ng` will
work once it is. Pre-built binaries may also be available from the releases
page.

#### Usage
```bash
# Basic usage (prompts for usbmon setup if needed)
usbtop-ng

# Show help
usbtop-ng --help

# Platform-specific setup instructions
usbtop-ng --setup
```

#### Known Limitations
- Needs root privileges on most systems
- macOS support was limited, because macOS had no usbmon equivalent
- Some USB controllers may not be fully supported
- Both usbmon interfaces (binary `/dev/usbmonN`, text `Nu`) were supported, but
  on Linux only. Neither BSD nor macOS had a live-monitoring equivalent

#### Roadmap
- Additional platform-specific optimizations
- Device filtering and search
- Export of bandwidth data
- Plugin system for custom monitors
- Network-based monitoring for remote systems

---

For technical detail, see [README.md](README.md) and the
[documentation](docs/).

Report issues and feature requests on
[GitHub Issues](https://github.com/wifi-blackout/usbtop-ng/issues).

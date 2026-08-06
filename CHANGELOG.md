# Changelog

All notable changes to usbtop-ng will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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
- Preferences-directory permission hardening (0700) now applies only when usbtop-ng creates the default `~/.usbtop-ng` directory; existing or custom-path directories are left untouched
- `PUBLIC/LICENSE` now matches the root `LICENSE` verbatim (BSD-3-Clause, copyright usbtop-ng contributors)
- Documentation (README, CONTRIBUTING, ARCHITECTURE) reconciled with the implemented thread + mpsc + dual-interface (binary preferred, text fallback) design
- `UsbTopApp`'s flat device map replaced with a per-tick `sync_from(&DeviceManager)` snapshot (`ControllerView`/`BusView`/`DeviceRow`) that also drives the new controller/bus grouping and port ordering

### Fixed
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
- Terminal UI powered by ratatui and crossterm, redrawn on a per-tick refresh
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
# Changelog

All notable changes to usbtop-ng will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Live USB bandwidth monitoring pipeline wired end-to-end: usbmon reader thread(s) → mpsc channel → DeviceManager aggregation → per-tick UI refresh
- Full parser for usbmon's `Nu` text interface format
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
- Documentation (README, CONTRIBUTING, ARCHITECTURE) reconciled with the implemented thread + mpsc + text-interface design

### Removed
- Tokio and chrono dependencies
- The broken binary-mode usbmon reader; only the debugfs `Nu` text interface is read
- Per-speed row coloring from the UI legend and help overlay (the feature was never actually implemented)

### Technical Details
- Built with Rust 2021 edition
- Dedicated blocking reader thread(s), opened `O_NONBLOCK` with a shutdown handle, feeding an `mpsc` channel read by the UI thread
- Terminal UI powered by ratatui and crossterm, redrawn on a per-tick refresh
- USB packet parsing for the usbmon `Nu` text format only (binary `/dev/usbmonN` is not supported)
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
- Only the usbmon text (`Nu`) interface is supported; the binary `/dev/usbmonN` interface is not used

#### Roadmap
- Additional platform-specific optimizations  
- Enhanced device filtering and search
- Export functionality for bandwidth data
- Plugin system for custom monitors
- Network-based monitoring for remote systems

---

For detailed technical information, see the [README.md](README.md) and [documentation](docs/).

Report issues and feature requests on [GitHub Issues](https://github.com/wifi-blackout/usbtop-ng/issues).
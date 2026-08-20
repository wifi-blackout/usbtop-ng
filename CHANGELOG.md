# Changelog

All notable changes to usbtop-ng are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `--filter` expressions down to endpoint, direction, and transfer type.
- `--once` and `--batch` reports, with `--json` output (documented in
  docs/SCRIPTING.md).
- `--print-man` prints the man page. install.sh installs it.
- `--print-completions` prints a shell completion script for bash, zsh, or
  fish.
- Per-bus rx/tx figures in bus heading rows.
- `~` estimate markers for isochronous rates on the text interface.
- A release workflow that attaches Linux x86_64 binaries to tagged releases.

### Fixed

- A headless run whose capture readers all stopped now fails with an error
  instead of printing zero reports forever.
- install.sh refuses to replace a `usbtop.1` man page it does not own, the
  same guard the `usbtop` command already had.

## [1.1.1] - 2026-08-10

### Added

- `install.sh`. It builds the release binary, copies it to `/usr/local/bin`,
  and creates a `usbtop` symlink, so `usbtop` and `sudo usbtop` both work.

### Changed

- INSTALL and README use `install.sh` and add update steps. The binary is
  unchanged.

## [1.1.0] - 2026-08-09

### Added

- Idle-device enumeration. Every connected USB device shows a row, at zero
  bandwidth until it transfers. On by default.
- `hide_idle_devices` preference and the `i` key. `i` hides devices with no
  current traffic and saves the choice to `~/.usbtop-ng/preferences.toml`.

### Fixed

- The setup message when usbmon is present but unreadable. usbtop-ng now tells
  the user to run with `sudo`, instead of pointing at `modprobe` and `mount`,
  which cannot fix a permission problem.

## [1.0.0] - 2026-08-09

Initial release. usbtop-ng is a USB bandwidth monitor for Linux with a
terminal UI. It supports Linux only and refuses other targets at compile
time.

### Monitoring

- Live USB bandwidth per device and per bus, read from the kernel's usbmon
  interfaces.
- usbtop-ng prefers the binary node (`/dev/usbmon<bus>`) and reads its
  48-byte native-endian event headers directly. A bus whose node does not
  open falls back to the debugfs text interface (`Nu`).
- Full parser for the `Nu` text format: setup packets, status words, and
  isochronous descriptors.
- Packet accounting costs O(1) per packet. `BandwidthStats` keeps a
  10-second window as 250ms buckets with a running sum.
- The reader-to-UI channel holds 16384 packets. Overflow increments the
  header's `dropped: N` counter instead of growing memory.
- Device metadata (vendor, product, speed) comes from sysfs, matched by
  busnum and devnum.
- Disconnect tracking removes a device 5 seconds after its sysfs path
  disappears.

### Display

- Controller-grouped device table in physical port order. Sibling USB2-side
  and USB3-side buses of one xHCI controller list adjacently with side
  labels.
- Per-device and per-bus %busy, measured against each speed's practical
  bandwidth. The figure reads `-- busy` when the bus speed is unknown.
- ⚡ marks a device above 80% busy. 🔺 marks a device that declares bcdUSB
  3.00 or higher while linked on a slower bus.
- Color-coded link speeds in the Speed cell, the bus headers, and the
  controls-bar legend.
- Split chart pane: the aggregate total on the left, the selected device's
  rx/tx history on the right, both over 60-second windows trimmed by age.
- The table's vertical scroll follows the selected device.
- Help overlay listing every key: `↑`, `↓`, `h`, `Ctrl-L`, `q`, `Esc`, and
  `Ctrl-C`.

### Terminal behavior

- The UI loop is event-driven. It draws only when something changed, at most
  once per 33ms, and drains packets at least every 50ms. An idle session
  does not redraw between refresh intervals.
- Output goes through a non-blocking stage that queues whole frames. When
  the backlog passes `1 + cols * rows * 8` bytes (4096-byte floor), the
  stage sheds the queued frames and requests one full repaint. The header
  shows `shed: N`.
- A failed write invalidates the screen and costs a full repaint. `EPIPE`,
  `EIO`, or 30 consecutive unclassified failures end the session through
  the normal teardown.
- Synchronized output (mode 2026) when the terminal answers a DECRQM probe
  within 100ms. usbtop-ng does not probe remote sessions (`SSH_TTY`,
  `SSH_CONNECTION`, or `SSH_CLIENT`).
- The terminal is restored on quit, panic, `SIGHUP`, `SIGINT`, and
  `SIGTERM`. Restore writes give up after 250ms on a terminal that stopped
  reading.
- `Ctrl-L` wipes and repaints the screen. `Ctrl-C` quits.

### Setup and configuration

- usbmon detection at startup, with an explicit prompt before any sudo
  command.
- Preferences in `~/.usbtop-ng/preferences.toml`: `auto_load_usbmon` and
  `unload_usbmon_on_exit`. usbtop-ng creates the default directory with
  mode 0700.
- A hangup exit honors `unload_usbmon_on_exit` without prompting.
- Command-line options: `--refresh` (100ms floor), `--config`, `--force`,
  `--setup`, `--create-alias`, and `--verbose`.

### Testing

- 188 hermetic tests run with `cargo test --all-targets`.
- The `integration` cargo feature adds live usbmon checks on a prepared
  Linux system.

### Requirements

- Linux with the usbmon kernel module and debugfs.
- Rust 1.88 or later, to build from source.
- Read access to the usbmon interfaces, typically root.

---

For detail, see [README.md](README.md) and the [documentation](docs/).

Report issues and feature requests on
[GitHub Issues](https://github.com/wifi-blackout/usbtop-ng/issues).

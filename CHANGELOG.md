# Changelog

All notable changes to usbtop-ng are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.5.0] - 2026-08-29

### Added

- A usbmon mmap-ring reader, preferred when the kernel supports it: it reads event headers through `MON_IOCX_MFETCH` and never copies the captured payload. A bus that cannot use the ring falls back to the read()-based binary interface, then the debugfs text interface.
- A kernel-side drop counter from `MON_IOCG_STATS`, shown as `kdropped: N` in the header when above zero and as `kernel_dropped_packets` in JSON reports.

## [1.4.1] - 2026-08-28

### Fixed

- `sudo usbtop` now follows the invoking user's configuration: preferences, the internal-device snapshot, the downloaded usb.ids copy, and `--create-alias`'s rc file all resolve against that user's home instead of root's, and files created there while root belong to that user, not root. Data previously written to `/root/.usbtop-ng` by earlier `sudo` sessions is no longer read under `sudo`; a direct root login is unchanged. `sudo -E` is no longer needed for this.

### Testing

- Two committed integration harnesses join the default suite: a pipe-based regression guard proving the terminal-restore bytes reach the terminal while the process is still alive (not just buffered until exit), and a PTY harness covering the wedged-terminal checks (quit, `SIGHUP`, and a terminal that stops reading) that previously ran by hand.

## [1.4.0] - 2026-08-28

### Added

- TUI: the selected device's endpoints auto-expand into dimmed rows directly below it, one per endpoint and direction, each showing its transfer type and rate in Bw↓ (IN) or Bw↑ (OUT). Collapses when the selection moves away.
- TUI: `/` opens a live search over the device table. Typed characters filter the table as you type, matching vendor, product, `vid:pid`, port chain, or `bus:address`. Enter commits the filter and closes input. Esc clears the query while editing, or the committed filter once one is active.
- Bus discovery and interface availability no longer require debugfs. usbtop-ng discovers buses from sysfs and starts on a binary-only host (usbmon loaded, debugfs never mounted), and it now detects a kernel with usbmon built in even though `/proc/modules` never lists it.

### Fixed

- Exact 20 Gbps and faster link speeds. The old model halved the displayed speed and doubled `%busy` at 20 Gbps, and read faster links as unknown.
- 1.5 Mbps text-report speeds no longer round up to "2 Mbps".
- `usb.ids` first pull now floors on the active source's date too, not just a replaced copy, so a replayed older payload can't shadow a newer distro copy.

### Changed

- TUI: speed cells and bus headings print integral values bare ("480 Mbps", "20000 Mbps") and keep one decimal only for fractional values ("1.5 Mbps"), so high speeds no longer overflow into the truncation ellipsis.
- TUI: `q`/`Esc` close the help overlay instead of quitting the app while it's open.
- TUI: the `S` confirmation overlay now names both keys -- `y` records, `n` cancels.
- TUI: the `S` confirmation overlay lists the devices it will record, not just the count.
- TUI: `dropped:`/`shed:` counters get their own warning color instead of sharing the Peak figure's color.
- Truncated table cells now end in `…` instead of clipping silently.
- Bus headings no longer show an empty `()` when the bus speed is unknown.

## [1.3.0] - 2026-08-22

### Added

- Internal-device snapshot: `--snapshot-internal` (CLI) or `S` inside the TUI
  (with a confirmation overlay) records every currently attached USB device
  as internal, stored at `~/.usbtop-ng/internal-devices.toml`.
- TUI: internal devices' Port cell renders in blue.
- Text reports (`--once`/`--batch`) mark internal rows with an `i` cell
  between the address and `vendor_id:product_id` columns.
- JSON reports gain a per-device `"internal": true|false|null` field.
- `--filter internal=yes|no` narrows the device table, on every surface, to
  internal or external devices.

## [1.2.0] - 2026-08-21

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
- Device names resolved from a usb.ids database (`lsusb` parity), with a
  `--usbids <PATH>` flag, a `usbids_path` preference key, and a source chain
  falling through to the downloaded copy, then the distro package.
- `--update-usbids [check|pull]`: `check` prints the local sources and the
  upstream date and advises the distro package route first; `pull` fetches,
  validates in quarantine, diffs, and installs by atomic rename.

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

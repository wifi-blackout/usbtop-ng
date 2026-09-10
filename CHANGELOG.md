# Changelog

All notable changes to usbtop-ng are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- `--capture-fixture <DIR>`: the fixture is created readable (`0755`/`0644`, the umask applying) and handed to the sudo invoker like a support bundle, where it used to stay root-owned; a symlink at `DIR` itself is refused, and a file already present at any name the capturer writes is an error rather than overwritten (use a fresh directory, as the stale-`sysfs` rule already required).

### Security

- The fixture capturer (`--capture-fixture`, and the fixture a `--support` bundle embeds) now writes every file, directory, and symlink of the fixture tree relative to one pinned directory descriptor, resolving each path component without following links and creating every file fresh, and replays the goldens through that descriptor. Previously the support bundle pinned only its root and the capturer wrote by path beneath `/proc/self/fd/<n>/fixture`. No reachable escape was identified in that code (the bundle root and `fixture/` stay root-owned and private until the ownership pass at the very end of the run), so this removes path-based writes as a class rather than fixing a known hole.

## [1.7.0] - 2026-09-08

### Added

- User-named connectors: a `[connector_names]` table in the preferences file labels a physical connector by its position (`"3:1" = "Left Type-A"`, either side's bus) or by its kernel port object name, and the device table's connector heading then leads with the name. A support bundle's copy of the preferences file masks these labels (they are free text and can name a room or a person) while keeping the keys. See the README's Preferences file section.

### Fixed

- Loading, unloading, and the debugfs mount no longer prepend `sudo` when the effective uid is already 0, so a root login, a container, or a rescue shell without `sudo` installed can load usbmon instead of failing with "No such file or directory". Both the load and the unload prompt name the command that will actually run, and a root run looks for `modprobe` and `mount` in their canonical locations before falling back to `PATH`.
- Under `sudo`, the invoking user's home is now resolved through the system user database (`getpwuid_r`, so LDAP, SSSD, and other directory-backed accounts resolve) instead of a scan of `/etc/passwd`. Previously a directory-backed user's preferences, snapshot, and usb.ids copy landed in root's home, owned by root.

### Security

- `--output PATH` no longer follows a symbolic link at `PATH`: the report file is opened with `O_NOFOLLOW`, so a link planted in a shared directory cannot redirect a root run's output onto another file. A symlink there is refused with an error that says so; a symlinked parent directory still resolves, and `/dev/stdout`, `/dev/stderr`, and `/dev/fd/N` keep working because the sink duplicates the descriptor they name instead of opening a path. The directories leading to `PATH` are trusted as given.
- USB string descriptors (`manufacturer`, `product`, `serial`) are firmware-controlled, and usb.ids names come from an editable text file; any control character in either (a terminal escape, a BEL, a DEL) and any bidirectional override or isolate is now replaced with U+FFFD as the strings enter the process (the sysfs read and the usb.ids parser), so a hostile device or a tampered usb.ids cannot drive the terminal through the text report, the snapshot commands, or the TUI, reverse a line, or misalign the columns. Printable names, including non-ASCII, are unchanged.
- The config directory (`~/.usbtop-ng`) is now created directly with mode 0700 and made private through a descriptor opened with `O_NOFOLLOW`, instead of a path-based chmod after a umask-wide `mkdir`. A symlink swapped in between the two steps is refused rather than followed, so a sudo invoker cannot have root chmod an arbitrary file.
- Under `sudo`, an existing `~/.usbtop-ng` that resolves outside the invoking user's home (the directory replaced by a symlink to somewhere else) is refused, with an error naming both paths, at startup and again at every write: the preferences, snapshot, lock, and usb.ids files are now created, renamed, and removed relative to a directory descriptor that was verified through `/proc/self/fd`, so a directory swapped for a symlink after the check cannot redirect them, and the ownership handoff to the invoker is decided from that same verified descriptor rather than from a path resolved again. A symlink that stays inside the home, such as a dotfiles checkout, is fine; a path chosen with `--config` or `--usbids` is not second-guessed. Previously only the chown was skipped for such a path; the writes still landed there as root.

## [1.6.0] - 2026-09-07

### Added

- The public repository was re-established at this release with a fresh history; releases 1.0.0 through 1.5.0 were published from the retired repository.

- An opt-in eBPF capture backend (`ebpf` cargo feature): a kprobe on `__usb_hcd_giveback_urb` aggregates bytes per bus, device, endpoint, direction, and transfer type in a kernel hash map, polled into the same accounting path as usbmon. Throughput only; it does not attribute traffic to a process. The default build is unaffected -- zero libbpf dependencies, no BPF toolchain needed. Building the feature needs clang and libbpf-dev on an x86-64 host (the BPF program's kprobe context is x86-64-specific for now) and adds no Rust-version requirement beyond the crate's 1.88 MSRV; running it needs root (or `CAP_BPF`) and a BTF-enabled kernel. A load or attach failure logs a warning and falls back to the usbmon chain. If its bounded in-kernel aggregation map ever fills under an extreme device/endpoint count, the unaccounted URBs surface through the same `kdropped:` header field and `kernel_dropped_packets` JSON field as usbmon's kernel drops, rather than being lost silently. See [docs/INSTALL.md](docs/INSTALL.md#building-the-ebpf-backend).
- A fixture-capture & golden-replay harness (`capture-fixture` cargo feature): `--capture-fixture <DIR>` records one real host+topology into a committed bundle -- a materialized sysfs snapshot, sanitized usbmon traces, and two golden reports generated by replaying them -- under `tests/fixtures/hosts/`. Two seed bundles ship in this release. The default test suite discovers and replays every committed bundle hermetically, no root and no real hardware needed, and fails if the capture-to-report pipeline's deterministic output for that real input ever changes. Every fixture is enforced payload-free (SEC-1: no captured USB data in either trace file) and path-contained (SEC-2: no symlink inside the bundle escapes it), both by the capturer and by an independent test over the committed corpus. See [docs/TESTING.md](docs/TESTING.md#capturing-hardware-fixtures).
- Fixture bundles for the development host's ground-truth isochronous stream (`devhost`), and for the powered-hub deep chain and saturation-through-hub stages on `tgl-tb4`.
- `--support [PATH]` gathers a diagnostic bundle for a bug report: build, host, and usbmon details, the backend the monitor would select, the USB lines of the kernel log, every USB device's full self-description with its raw descriptors and the Thunderbolt and Type-C attribute trees, the configuration with home paths rewritten, the terminal setup, a replayable fixture (with a short usbmon capture as root), a replayed `report.json`, the run's log, and a manifest. Host identity (hostname, machine-id, DMI serial, host MACs, IPs, user names) is never collected; device identity is kept. It prints a summary and the filing steps. See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md#bug-reports).
- `--output PATH` for `--once` and `--batch` writes the reports to a file, led by a run record (version, features, start time, window, filters, command, backend, kernel, OS, arch, buses). See [docs/SCRIPTING.md](docs/SCRIPTING.md#--output-path-write-to-a-file).
- A GitHub bug-report form that asks for the `--support` summary and bundle.
- The device table groups devices by physical connector: one heading per
  receptacle that has a device on it, pairing the USB2 and USB3 halves of a
  hub or a device through the kernel's port `peer` links at every hub
  level, with bus summary lines keeping the per-bus saturation figures.
  See the README's "The device table".
- `--forget-internal <PORT_PATH>` removes an entry from the internal-device
  snapshot, for the keyboard, mouse, or RF receiver that had to stay
  plugged in while the snapshot was taken; `--snapshot-internal` and the
  `S` overlay now say so. The snapshot file is replaced atomically through
  a same-directory temporary file, so an interrupted run leaves the
  previous snapshot in place, and the file is written with owner-only
  permissions (0600). See the README's "Adjusting the snapshot".
- Fixture bundles (and the fixture inside a `--support` bundle) now carry
  each hub's port objects and their `peer` links as relative in-bundle
  symlinks; three new bundles (the development desktop's nested hub chain,
  `rock5c`'s non-adjacent bus pairing, `tgl-x360`'s differing port
  numbers) pin the pairings by name.

### Changed

- Internal: traffic accounting now runs on backend-neutral `(key, bytes)` deltas, and the usbmon packet path is an adapter over that. No user-visible change on its own; it is what the eBPF backend above feeds through `apply_delta`.
- The `tgl-tb4` and `pi-400` stage2 bundles were recaptured with the enlarged ring; their earlier binary goldens had kept about a third of the traffic's URBs.
- The fixture capture and replay code is compiled into the default build (it is what `--support` embeds); the `capture-fixture` feature now gates only the `--capture-fixture` subcommand. No change to the shipped binary's behaviour.
- Fixture bundles no longer copy a device's `serial` attribute; the committed corpus was rewritten without them.
- The device table's bus headings are now summary lines; device rows moved
  under connector headings. Root hubs and devices sysfs could not resolve
  still list under their bus line.

### Fixed

- The `/` search box now filters as you type. Previously a keystroke's effect on which rows were shown waited for the next refresh tick -- up to a second at the default rate.
- When a `/dev/usbmon*` node exists but the current user cannot open it, the startup remedy now tells you to run with `sudo` instead of suggesting `modprobe`/`mount`, which cannot fix a permission problem.
- The mmap-ring reader's ioctl request casts now compile under musl (musl declares `ioctl`'s request parameter as `c_int` where glibc says `c_ulong`), unblocking the static armv6 build for the Pi Zero.
- Isochronous rates on the usbmon text fallback are now a sample-based estimate within about 1% of the binary interface, instead of the buffer size, which overcounted by 4x to 15x on webcams. The report still marks them `estimated`. See [docs/SCRIPTING.md](docs/SCRIPTING.md#the-estimated-field).
- The read()-based binary fallback reader now requests the same enlarged kernel ring as the mmap reader and feeds the `kdropped:` counter; on the default ring it lost most of a fast stream silently.
- `--capture-fixture` no longer captures on the default kernel ring (it kept about a third of an isochronous stream's events) and records the kernel's drop count as `binary_kernel_dropped` in each bundle's `meta.toml`.

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

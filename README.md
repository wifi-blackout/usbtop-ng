# usbtop-ng  by https://wifi-blackout.com

usbtop-ng reports live USB bandwidth per device in a terminal. It reads the
Linux usbmon interfaces and repaints when the numbers change.

usbtop-ng is an independent reimplementation of
[usbtop](https://github.com/aguinet/usbtop). It shares no code with the
original project. The original authors do not endorse usbtop-ng and have no
affiliation with it. I wanted to have a more robust modern TUI that also was
up-to-date with modern USB devices. 

## What the screen shows

Four blocks fill the window, top to bottom.

1. **The header** carries the total bandwidth, the peak total, and the device
   count. The drop counter and the shed counter join that line once either one
   rises above zero.
2. **The chart pane** plots the aggregate total over the last 60 seconds on the
   left. The selected device's rx and tx rates fill the right.
3. **The device table** gives each device one line, grouped under its host
   controller and its bus, in physical port order.
4. **The controls bar** holds the link-speed color legend and the key bindings.

## What you need

- Linux. usbtop-ng builds on no other platform.
- The usbmon kernel module. Debugfs mounted at `/sys/kernel/debug` matters
  only as a fallback, for hosts where the binary `/dev/usbmon*` interface
  usbtop-ng prefers is unavailable.
- Read access to the usbmon interfaces. Root has that access. Anything short of
  root depends on the distribution.
- Rust 1.88 or later, to build from source.
- Git, to clone the repository.

## Install

1. Clone the repository:
   ```bash
   git clone https://github.com/wifi-blackout/usbtop-ng.git
   ```
2. Enter the checkout:
   ```bash
   cd usbtop-ng
   ```
3. Run the install script:
   ```bash
   ./install.sh
   ```
   The script runs `cargo build --release`, then asks for `sudo` to copy the
   binary to `/usr/local/bin` and to create a `usbtop` symlink. If the build
   fails on a compiler error, check that `cargo --version` reports Rust 1.88 or
   later.

The install creates `/usr/local/bin/usbtop` as a symlink to `usbtop-ng`, so
`usbtop` and `sudo usbtop` both work. A shell alias would not work under
`sudo`.

`install.sh` also installs a man page, so `man usbtop-ng` works right after
install.

To install by hand instead, build and copy the binary yourself:

```bash
cargo build --release
sudo install -m 0755 target/release/usbtop-ng /usr/local/bin/usbtop-ng
sudo ln -sf usbtop-ng /usr/local/bin/usbtop
```

[docs/INSTALL.md](docs/INSTALL.md) covers the usbmon setup, shell completions,
the permission checks, and how to remove usbtop-ng again.

## Update

1. Pull the latest code:
   ```bash
   git pull
   ```
2. Reinstall:
   ```bash
   ./install.sh
   ```
   The script rebuilds and replaces the binary and the symlink. Confirm the
   version with `usbtop-ng --version`.

## Start it

```bash
usbtop
```

Run `sudo usbtop` when your account cannot read the usbmon interfaces. Both
the binary `/dev/usbmon*` nodes usbtop-ng prefers and the debugfs text
fallback are root-owned by default.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | Select a device. The device table scrolls to keep it visible. |
| `h` | Open or close the help overlay. |
| `i` | Show or hide idle devices. Saves the choice to `~/.usbtop-ng/preferences.toml`. |
| `/` | Open search input, prefilled with the active query if one is committed. See [Search](#search). |
| `S` | Snapshot every attached device as internal, after a confirmation. See [Device origin](#device-origin). |
| `Ctrl-L` | Wipe the screen and repaint it from scratch. |
| `q` or `Esc` | Quit. A committed search query is normal browsing, so `q` still quits; only while search input is open does `q` type into the query instead. `Esc` clears a committed query instead of quitting when one is active. |
| `Ctrl-C` | Quit, even while typing a search query. |

Raw mode turns off ISIG, so `Ctrl-C` never becomes a `SIGINT`. It arrives as a
key press and quits through the same teardown as `q`.

## Features

### Bandwidth measurement

- Live rx and tx bandwidth per device, plus a running total and the peak total
  across every device.
- Rates come from a 10 second sliding window.
- The window holds fixed 250 millisecond buckets and a running sum, so
  accounting one packet costs O(1). Nothing rescans the window per packet.
- Idle devices decay to zero, because each refresh interval re-evaluates the
  window rather than reusing the last rate.
- %busy per device and per bus, against the practical maximum for the link
  speed. The practical maximum adjusts the line rate for protocol overhead: 70%
  at low speed, 80% at full and high speed, 85% at SuperSpeed and SuperSpeed+.
- A device of unknown speed shows `--` in the %busy column. Its bus header
  shows `-- busy`.
- Two 60 second histories: the aggregate total, and per-device rx and tx.
  usbtop-ng evicts both by age, so each one covers 60 seconds at any
  `--refresh` value.

### The device table

- Devices group under a host controller heading such as `═ 0000:00:14.0 ═`,
  then under a bus header such as `▶ Bus 03 (USB3 side)`.
- usbtop-ng resolves the controller from the root hub's canonical sysfs parent
  directory. Buses that share a controller sort together by bus number.
- A bus whose controller does not resolve joins the `unknown` group, which
  sorts last.
- The USB2-side and USB3-side buses of one xHCI controller list as adjacent
  sibling buses. A bus at 480 Mbps or below takes the "USB2 side" label, and a
  faster bus takes "USB3 side". A bus of unknown speed takes no label.
- Devices sort by physical port chain, level by level. The Port column prints
  it: `1.4.2` for a hub chain, `-` for a root hub, `?` when sysfs did not
  resolve the device.
- Columns: Port, Device, Speed, Vendor, Product, Bw↓, Bw↑, %busy, `!`.
- Speed comes from sysfs, matched by busnum and devnum. Vendor and product
  come from sysfs too, unless a usb.ids database resolves a name for them;
  see [Device names](#device-names).
- ⚡ marks the `!` column once a device passes 80% busy.
- 🔺 marks the `!` column when the device's sysfs `version` reads 3.00 or
  higher and both its bus and its link run slower than SuperSpeed. 🔺 takes
  precedence over ⚡.
- Every link speed carries a color. It tints the Speed cell, the bus header's
  Mbps figure, and the controls bar legend. The legend reads 1.5M, 12M, 480M,
  5G, 10G+, and `?` for unknown.
- A device that disappears greys out for 5 seconds, then leaves the table.
- The table scrolls to follow the selection, so `↑` and `↓` cannot walk the
  selection off screen.
- Idle-device enumeration shows a row for every connected device, at zero
  bandwidth until it transfers. Enumeration runs by default.
- `i` hides devices with no current traffic and saves the choice to
  `~/.usbtop-ng/preferences.toml`.
- Selecting a device expands its endpoints as dimmed, non-selectable rows
  directly below it: transfer type, and rate in Bw↓ for IN or Bw↑ for OUT.
  Collapses when the selection moves away.
- `/` narrows the table to devices matching a typed query. See
  [Search](#search).

### Device origin

usbtop-ng can remember which devices are built into the machine, then mark
them apart from external gear everywhere a device shows up:

1. Unplug every external hub and device, leaving only what's built in.
2. Trigger a snapshot: run `usbtop-ng --snapshot-internal` from a shell, or
   press `S` inside the TUI and confirm with `y`. Either one records every
   currently attached device, so anything still plugged in gets captured as
   internal — the TUI's confirmation overlay says so before you commit.
3. Plug your external gear back in.

From then on, internal devices render their Port cell in blue in the TUI.
Text reports (`--once`/`--batch`) carry an `i` marker in a fixed-width cell
next to the address; JSON reports gain an `"internal": true|false|null`
field per device (`null` means no snapshot exists yet); and `--filter
internal=yes` (or `no`) narrows either surface to just one origin. See
[docs/SCRIPTING.md](docs/SCRIPTING.md) for the JSON field and the
Filtering section below for the filter key.

The snapshot lives at `~/.usbtop-ng/internal-devices.toml`, separate from
the preferences file — `--config` never moves it. A new snapshot overwrites
the old one; deleting the file clears it, and every device goes back to
looking external.

That path resolves against the invoking user's home, not root's: `sudo
usbtop` reads and writes the same `~/.usbtop-ng` a plain `usbtop` session
would, including this snapshot, the preferences file, and the downloaded
usb.ids copy, and every file it creates there belongs to that user, not
root. `sudo -E` is not needed for this. A direct root login (no `sudo`) is
unchanged: it still resolves against `/root`.

### Device names

- usbtop-ng names devices the way `lsusb` does, by resolving VID:PID against
  a [usb.ids](https://www.linux-usb.org/usb.ids) database. Applies to the TUI,
  `--once`/`--batch` reports, and the `vendor`/`product` JSON fields alike.
- Source order, first that exists and parses wins: `--usbids <PATH>`, the
  `usbids_path` preferences key, the downloaded copy at
  `~/.usbtop-ng/usb.ids`, then the distro package
  (`/usr/share/misc/usb.ids` on Debian and Ubuntu,
  `/usr/share/hwdata/usb.ids` on Fedora and openSUSE). No source found means
  names come from device strings alone, exactly as before this feature.
- Per field, per device: the database name wins when it has one, otherwise
  usbtop-ng keeps the device's own manufacturer or product string, and
  vendor and product resolve independently of each other.
- `--update-usbids` (or `--update-usbids check`, the default) prints the
  local sources with their `# Date:` and which one is active, checks the
  upstream date with a single HTTPS HEAD request, and advises how to catch
  up: the distro package route first (the exact `apt`/`dnf`/`zypper`/`pacman`
  command for whichever is on PATH), `--update-usbids pull` as the fallback.
  It never writes a file.
- `--update-usbids pull` is the explicit, hardened fetch: it skips with
  "already up to date" when upstream is not newer, otherwise it downloads the
  payload into a quarantine file next to the destination, validates it with
  usbtop-ng's own parser (parses, at least 1000 vendors, not older than the
  copy it replaces), prints a diff summary (dates, vendor and product count
  deltas), and only then installs it to `~/.usbtop-ng/usb.ids` with an atomic
  rename. Any failure before that rename leaves every existing file
  untouched.
- Security posture: the upstream URL is `https://www.linux-usb.org/usb.ids`,
  compiled in and https-only. curl runs with `--proto =https --proto-redir
  =https --tlsv1.2`; wget runs with `--https-only --secure-protocol=PFS`. No
  redirect may leave https. The fetched payload is never executed and never
  installed as-is — it sits in quarantine until usbtop-ng's own memory-safe
  parser (text in, names out, no execution path) validates it, and only a
  passing, diffed payload gets the atomic install.

### Filtering

- `--filter KEY=VALUE[,KEY=VALUE...]` narrows the device table and the traffic
  it counts. Repeat the flag to add more expressions.
- Keys within one `--filter` term AND together: every key in the term must
  match. Separate `--filter` flags OR together: a device or packet counts if
  any term matches. No `--filter` flag shows and counts everything.
- Keys: `bus`, `dev`, `vid`, `pid`, `id`, `name`, `ep`, `dir`, `type`, `internal`.
- `bus` and `dev` match the USB bus number and device number.
- `vid` and `pid` match the 4 hex digit vendor and product ID, e.g.
  `vid=04f2`. `id` is shorthand for both together, e.g. `id=04f2:b71a`.
- `name` matches a case-insensitive substring of the vendor or product
  string as displayed — the usb.ids database name when one resolves,
  otherwise the device's own string; see [Device names](#device-names).
- `ep` matches the endpoint number, 0 through 15. `dir` matches transfer
  direction, `in` or `out`. `type` matches transfer type: `control` (or
  `ctrl`), `iso`, `bulk`, `interrupt` (or `int`).
- `internal` matches `yes`/`true` or `no`/`false` against a device's
  snapshot origin; it requires an internal-device snapshot (`--snapshot-internal`)
  to already exist, or usbtop-ng exits with an error naming that flag.
- `bus`, `dev`, `vid`, `pid`, `name`, and `internal` decide which devices show
  at all. `ep`, `dir`, and `type` narrow which packets on a visible device
  count, without hiding the device itself.

### Search

- `/` opens search input; typed characters build the query and the device
  table filters live as it changes. Backspace edits, Enter keeps the filter
  and closes input, Esc clears the query (while editing) or the active
  filter (once committed).
- A device matches if a case-insensitive substring of the query hits its
  vendor name, product name, `vid:pid`, port chain, or `bus:address` — the
  same text and format the table itself shows.
- Composes with `--filter` and hide-idle: both narrow the table first. A
  committed query that matches nothing shows an empty table, not an error.
- Display-only: accounting, header totals, and reports are unaffected.
- The controls bar shows the query while typing and, once committed, how to
  clear it.

### Scriptable output

- `--once` samples one window and prints a bandwidth report to stdout, then
  exits. `--batch` prints one report per window, repeated until `Ctrl-C`.
  Neither mode opens the TUI or prompts for anything.
- Add `--json` to either mode for one JSON document per report (NDJSON in
  `--batch`). `--window SECONDS` sets the sample length.
- See [docs/SCRIPTING.md](docs/SCRIPTING.md) for the full flag reference, the
  JSON field list, and an example document.

### The chart pane

- The left chart plots the session total in MB/s over the last 60 seconds.
- The right chart plots the selected device's rx and tx in MB/s over a
  [-60, 0] second window.
- The right chart shows "Select a device with ↑/↓" until you select one, and
  again if the selected device disappears.

### Reading usbmon

- usbtop-ng prefers the binary interface, `/dev/usbmonN`. It reads the kernel's
  48 byte native-endian event header and drains each event's captured payload
  rather than keeping it.
- One probe at startup opens the first target bus's binary node. A failure
  selects the text interface, debugfs `Nu`, for every bus. One `info!` line
  records the choice.
- Each reader thread re-opens its own binary node before it starts reading. If
  that open fails, the thread warns and reads that bus's text interface, so one
  unreadable node costs one bus rather than the session.
- Bus 0 is the kernel's aggregate interface. When it exists, a single reader
  covers every bus, so no traffic counts twice.
- One thread reads each interface. Every thread opens its file with
  `O_NONBLOCK` and polls every 50 milliseconds, so a shutdown request lands
  within one poll.
- The readers hand packets to the UI thread over a channel bounded at 16384
  packets. A reader never parks on a full channel. A packet that does not fit
  raises the drop counter, and the header then shows `dropped: N`.
- The event loop applies at most 8192 packets per pass. A pass that fills its
  batch comes straight back for the rest.
- usbtop-ng closes the usbmon files before any unload, because an open file
  pins the module and makes `modprobe -r usbmon` fail with `EBUSY`.

### Drawing the screen

- usbtop-ng draws only when something changed, and never more often than one
  frame per 33 milliseconds (about 30 frames per second).
- An idle session repaints once per refresh interval and not between.
- The event loop sleeps until its earliest deadline instead of polling. It
  folds a burst of events into one repaint, so fifty resize events cost one
  frame.
- Nothing wakes the loop when a packet arrives, so the sleep caps at 50
  milliseconds. That cap is the drain cadence.
- `--refresh` values below 100 milliseconds clamp to 100 milliseconds.
- At startup usbtop-ng asks the terminal whether it supports synchronized
  output, mode 2026. The query is DECRQM followed by a DA1 marker, and the
  answer has 100 milliseconds to arrive. usbtop-ng brackets whole frames only
  after a yes.
- usbtop-ng never asks a remote session, because the reply would cross a
  network. It reads `SSH_TTY`, `SSH_CONNECTION`, and `SSH_CLIENT` as the
  markers of one, and the allowlist of terminals to probe over SSH ships empty.

### Keeping up with a slow terminal

- Output goes to a non-blocking stdout through a queue of whole frames, so a
  terminal that stopped reading cannot stall the loop that reads usbmon.
- The backlog allowance is `1 + cols * rows * 8` bytes, with a floor of 4096
  bytes. The floor keeps a terminal reporting 0x0 or 1x1 from shedding every
  frame it stages.
- Past that allowance, usbtop-ng drops every queued frame that put no bytes on
  the wire. It counts them in the shed counter, shows `shed: N` in the header,
  and asks for one full repaint.
- A 1 second grace period after a shed keeps the recovery frame from being shed
  in its turn.
- Frame granularity makes truncation mid-escape-sequence impossible. A shed
  drops whole frames, and a partial write resumes inside one.
- A write that fails without the terminal being gone invalidates the screen and
  costs one full repaint.
- `EPIPE` and `EIO` end the session through the normal exit path. So do 30
  unclassified write failures in a row with nothing landing between them.
- `Ctrl-L` wipes the screen and repaints it. It asks the terminal nothing, so
  it works on the terminal that needs it most.

### Leaving the terminal as usbtop-ng found it

- A panic, a `SIGHUP`, a `SIGINT`, a `SIGTERM`, or a closed terminal emulator
  leaves through the same teardown as `q`. That teardown turns off raw mode,
  leaves the alternate screen, shows the cursor, and restores stdout's original
  flags.
- The restore spends at most 250 milliseconds on a terminal that will not read,
  as 25 attempts 10 milliseconds apart.
- The restore stops the output stage before it hands the descriptor back to
  blocking mode.
- When the restore could not get its own bytes out, usbtop-ng skips the two
  remaining stdout writes: the unload question and the automatic unload notice.
  The unload itself still runs.
- A hangup exit asks nothing, because nobody is left to answer. It honors a
  standing `unload_usbmon_on_exit` and otherwise leaves usbmon loaded.
- The exit question waits at most 60 seconds, then takes the answer that
  changes nothing.

### usbmon load and unload

- When usbmon is missing, usbtop-ng prints the command it wants to run and asks
  first:
  ```bash
  sudo modprobe usbmon
  ```
- If debugfs is not mounted, the same step also runs:
  ```bash
  sudo mount -t debugfs none /sys/kernel/debug
  ```
- When usbtop-ng loaded usbmon for the session, it asks on quit whether to
  unload it:
  ```bash
  sudo modprobe -r usbmon
  ```
- usbtop-ng reads the preferences file at `~/.usbtop-ng/preferences.toml` and
  creates it on first run.
- usbtop-ng creates `~/.usbtop-ng` with mode 0700. An existing directory keeps
  its own mode, and so does a directory named by `--config`.

### Tests

- `cargo test --all-targets` runs 408 hermetic tests against fixture files,
  FIFOs, and temporary paths. They need no `/dev` and no debugfs access.
- The `integration` cargo feature adds 1 test that reads the real usbmon
  interfaces. See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## Preferences file

usbtop-ng creates `~/.usbtop-ng/preferences.toml` on first run with these
values:

```toml
auto_load_usbmon = false
unload_usbmon_on_exit = false
hide_idle_devices = false
```

- Set `auto_load_usbmon = true` to load usbmon without the startup question.
- Set `unload_usbmon_on_exit = true` to unload usbmon on exit without the quit
  question. usbtop-ng unloads only when it loaded usbmon for that session.
- `hide_idle_devices` controls whether idle devices show. Idle devices show by
  default; press `i` to hide them, and usbtop-ng saves the choice here.
- `usbids_path` points at a usb.ids database file, ahead of the downloaded and
  distro copies in the [source order](#device-names). Unset by default, so
  the key is absent from the file above until you add it; the `--usbids`
  flag overrides it for one run.

`example-config.toml` in the repository root holds the same keys with
comments.

## Command line options

```
usbtop-ng [OPTIONS]

Options:
  -v, --verbose            Enable verbose logging
  -c, --config <CONFIG>    Preferences file path (default: ~/.usbtop-ng/preferences.toml)
  -r, --refresh <REFRESH>  Refresh rate in milliseconds (floored at 100ms) [default: 1000]
      --force              Force run without usbmon (limited functionality)
      --setup              Show setup instructions for live monitoring
      --create-alias       Create shell alias for 'usbtop' command
      --filter <KEY=VALUE[,KEY=VALUE...]>
                           Show only traffic matching KEY=VALUE terms (repeatable, expressions OR)
      --once               Sample one window, print a report, and exit
      --batch              Print a report every window until interrupted
      --json               Print reports as JSON (one document per report)
      --window <SECONDS>   Sample window in seconds (default: 5 with --once, 1 with --batch)
      --print-man          Print the man page to stdout
      --print-completions <SHELL>
                           Print a completion script to stdout for the named shell (e.g. bash, zsh, fish)
      --usbids <PATH>      usb.ids database file for device names (overrides every other source)
      --update-usbids [<MODE>]
                           Check for a newer usb.ids ('check', the default) or fetch it ('pull')
      --snapshot-internal  Record every currently attached device as internal, then exit
  -h, --help               Print help
  -V, --version            Print version
```

`--once` and `--batch` never open the TUI or prompt for anything, which makes
them safe in a script or a cron job. See
[docs/SCRIPTING.md](docs/SCRIPTING.md) for details.

## Limits

- usbtop-ng runs on Linux. A build for any other platform stops at a compile
  error.
- usbmon needs root, or read access granted to its interfaces some other way.
- `sudo` strips `SSH_TTY`, `SSH_CONNECTION`, and `SSH_CLIENT` under its default
  `env_reset`. A `sudo usbtop-ng` session over SSH therefore gets probed for
  mode 2026. The probe costs at most 100 milliseconds, and `sudo -E` keeps the
  variables and the conservative posture.
- usbtop-ng bounds or skips every stdout write it makes after teardown. It
  never bounds stderr, because a non-blocking stderr would truncate the log
  lines and backtraces worth having. A diagnostic therefore waits on a terminal
  that is open but has stopped reading, the way any program's would.
- `-v` puts a debug line in front of the unload. A reader thread that panicked
  makes `MonitorHandle::stop` log a warning in the same place. Either one can
  park a wedged terminal there.
- The Rust runtime writes a panic message and backtrace to stderr, not
  usbtop-ng.
- 🔺 is a best-effort signal. A device that declares no USB 3 support never
  carries it, so a missing 🔺 proves nothing.

## Documentation

- [docs/INSTALL.md](docs/INSTALL.md): install, usbmon setup, verification,
  removal.
- [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md): development environment, tests,
  pull requests.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): modules, data flow, the TUI
  chassis, known limitations.
- [docs/SCRIPTING.md](docs/SCRIPTING.md): `--once`/`--batch` reports, the
  `--json` field list, and exit behavior.
- [docs/ROADMAP.md](docs/ROADMAP.md): feature ideas and follow-up work.
- [CHANGELOG.md](CHANGELOG.md): what changed per release.

## Development

Build:

```bash
cargo build
```

Test:

```bash
cargo test --all-targets
```

## License

usbtop-ng carries the BSD 3-Clause License, matching the original usbtop
package's license family. You may use, modify, and distribute this code.
Include the copyright notice and the license notice in copies and in
substantial portions.

See [LICENSE](LICENSE) for the full text.

## Acknowledgments

- Adrien Guinet wrote the original [usbtop](https://github.com/aguinet/usbtop),
  which inspired this program.
- The Rust community built the crates this program stands on.

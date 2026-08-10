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
- The usbmon kernel module, and debugfs mounted at `/sys/kernel/debug`.
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
3. Build the release binary:
   ```bash
   cargo build --release
   ```
   The build writes `target/release/usbtop-ng`. If the build fails on a
   compiler error, check that `cargo --version` reports Rust 1.88 or later.
4. Copy the binary onto your path:
   ```bash
   sudo cp target/release/usbtop-ng /usr/local/bin/
   ```

[docs/INSTALL.md](docs/INSTALL.md) covers the usbmon setup, the permission
checks, and how to remove usbtop-ng again.

### Shell alias

usbtop-ng can add a `usbtop` alias to your shell configuration file.

1. Run the alias command:
   ```bash
   usbtop-ng --create-alias
   ```
2. Answer `y` at the confirmation prompt. usbtop-ng prints the file it wrote
   to. Record that path.
3. Load the alias into the current shell:
   ```bash
   source ~/.bashrc
   ```
   Use the path from step 2 in place of `~/.bashrc`.

To add the alias by hand instead, put this line in `~/.bashrc`, `~/.zshrc`, or
your shell's equivalent:

```bash
alias usbtop='usbtop-ng'
```

## Start it

```bash
usbtop-ng
```

Run `sudo usbtop-ng` when your account cannot read
`/sys/kernel/debug/usb/usbmon`.

## Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | Select a device. The device table scrolls to keep it visible. |
| `h` | Open or close the help overlay. |
| `i` | Show or hide idle devices. Saves the choice to `~/.usbtop-ng/preferences.toml`. |
| `Ctrl-L` | Wipe the screen and repaint it from scratch. |
| `q` or `Esc` | Quit. |
| `Ctrl-C` | Quit. |

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
- Vendor, product, and speed come from sysfs, matched by busnum and devnum.
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

- `cargo test --all-targets` runs 200 hermetic tests against fixture files,
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

`example-config.toml` in the repository root holds the same three keys with
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
  -h, --help               Print help
  -V, --version            Print version
```

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

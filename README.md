# usbtop-ng

usbtop-ng is a next-generation USB traffic monitoring tool, reimagined in Rust for speed, safety, and modern systems.

Inspired by the original [usbtop](https://github.com/aguinet/usbtop), usbtop-ng is an **independent reimplementation**.  
It does **not** share code with the original project and is **not affiliated with or endorsed by** its authors.

## Current Status

usbtop-ng builds, runs its setup checks, and opens the terminal UI with live packet-to-device monitoring wired end-to-end: usbmon reader thread(s) read the binary `/dev/usbmonN` interface when it's available, falling back automatically to the debugfs `Nu` text interface otherwise, hand packets to the UI thread over an `mpsc` channel, and `DeviceManager` aggregates them into per-device bandwidth stats, physical topology, and %busy on every refresh tick.

## ✨ Features

- Controller-grouped, physically port-ordered device list: devices are listed under their host controller and USB bus in physical port order, with the USB2-side and USB3-side buses of a shared xHCI controller shown as adjacent sibling buses
- Live per-device USB bandwidth (RX/TX), plus running totals and peak bandwidth across all devices
- Per-device and per-bus %busy, measured against each USB speed's practical (protocol-overhead-adjusted) bandwidth and shown in the device list and bus headers
- ⚡ high-utilization (>80% busy) and 🔺 capability-exceeds-bus indicators — the latter is a best-effort signal: a device whose sysfs `version` declares bcdUSB 3.x while it is linked (and bussed) slower. Devices that don't declare USB 3 support are never flagged, so a missing 🔺 proves nothing
- Color-coded USB link speeds in the device list, bus headers, and a legend in the controls bar
- Bounded packet queue with drop accounting: reader threads never block on a slow UI, and if the queue ever overflows the header adds `dropped: N` so a lossy session is never mistaken for a quiet one
- Split bandwidth chart pane: aggregate total on the left, the selected device's rx/tx history on the right
- Device metadata (vendor, product, speed) resolved from sysfs
- Disconnect detection: devices are shown greyed out for 5 seconds after disconnecting, then removed
- Managed usbmon load/unload, with the choice remembered in preferences
- Binary usbmon interface (`/dev/usbmonN`) used when available, with automatic fallback to the debugfs `Nu` text interface
- Lightweight, terminal-friendly interface
- Rust-powered performance and safety
- Cross-platform support (Linux, *BSD, macOS — Windows WIP); live monitoring via usbmon is Linux-only — on BSD/macOS the UI can open (with `--force` where needed) but shows no devices
- Low resource footprint

### Degraded-terminal robustness

A monitoring tool is most useful over the link that is least able to draw it. usbtop-ng is built to keep working — and to say what it lost — when the terminal cannot keep up:

- **Dirty-gated rendering**: frames are drawn only when something actually changed, and never more often than ~30 per second. An idle session repaints on its `--refresh` tick and not in between, so nothing is redrawn just because the loop woke up.
- **Synchronized output when the terminal supports it**: at startup usbtop-ng asks the terminal whether it understands mode 2026 (DECRQM, answered within 100ms) and brackets whole frames only if it says yes. A session that came in over ssh is not asked at all — the reply would cross a network, and a wrong answer costs more than the tearing it prevents. (That check reads `SSH_TTY`/`SSH_CONNECTION`/`SSH_CLIENT`, and `sudo`'s default `env_reset` strips all three — so `sudo usbtop-ng` over ssh does get asked. `sudo -E` keeps the conservative posture.)
- **Backpressure shedding**: output goes to a non-blocking descriptor through a queue of whole frames. When the terminal stops reading and the backlog outgrows its allowance, queued frames are dropped rather than buffered forever — a slow link can no longer stall the loop that is reading usbmon. The header then adds `shed: N`, because the numbers on screen are current but the screen itself is N frames behind.
- **Write-failure recovery**: a write that fails without the terminal being gone invalidates the screen, and the next pass wipes it and repaints from scratch instead of drawing diffs against a display that no longer matches. A terminal that is genuinely gone (`EPIPE`/`EIO`), or that fails every write for 30 attempts running, ends the session through the normal exit path.
- **`Ctrl-L` manual repaint**: wipes the screen and paints a full frame, for whatever another program scribbled across it. It asks the terminal nothing (no cursor-position round trip), so it also works on the terminal that needs it most.
- **Clean restore on panic and on signals**: a panic, a `SIGHUP`, a `SIGTERM` or a closing terminal emulator all leave through the same teardown as `q` — raw mode off, alternate screen left, cursor back, stdout's original flags restored. Everything usbtop-ng writes to **stdout** from then on is bounded or skipped: the restore sequences give up after a quarter of a second, the render pipeline is switched off before the descriptor goes back to blocking, and the exit's usbmon question and unload notice are skipped outright when the restore could not get its own bytes out. **stderr is deliberately left alone** — log lines and a panic's backtrace are diagnostics, written by the logger and by the Rust runtime, and making that descriptor non-blocking would truncate exactly the messages worth having. So on a terminal that is still open but has stopped reading, a diagnostic waits there, the way any program's would.
- **`--refresh` floor**: values below 100ms are clamped, because below that the loop spends more time waking up than the terminal can usefully repaint.

## 📦 Installation

```bash
# Clone the repository
git clone https://github.com/wifi-blackout/usbtop-ng.git
cd usbtop-ng

# Build and install
cargo build --release
sudo cp target/release/usbtop-ng /usr/local/bin/
```

### Creating a Shell Alias

For convenience, you can create an alias so you can run `usbtop` instead of `usbtop-ng`:

```bash
# Let usbtop-ng create the alias for you
usbtop-ng --create-alias

# Or manually add to your shell config (~/.bashrc, ~/.zshrc, etc.)
alias usbtop='usbtop-ng'
```

## 🚀 Usage

```bash
usbtop-ng
# or if you created the alias:
usbtop
```

Press `h` for help, `Ctrl-L` to repaint the screen, `q` (or `Esc`, or `Ctrl-C`) to quit.  
Run with `--help` to see all options.

### usbmon loading and unloading

On Linux, live monitoring uses the `usbmon` kernel module and the debugfs interface at `/sys/kernel/debug/usb/usbmon`.

If usbmon is missing, usbtop-ng explains the exact command it wants to run and asks before running:

```bash
sudo modprobe usbmon
```

If usbtop-ng loaded usbmon for the current session, it asks on quit whether to unload it again with:

```bash
sudo modprobe -r usbmon
```

Preferences are stored in `~/.usbtop-ng/preferences.toml` and are created automatically on first run:

```toml
auto_load_usbmon = false
unload_usbmon_on_exit = false
```

Set `auto_load_usbmon = true` to skip the startup prompt and load usbmon automatically when needed. Set `unload_usbmon_on_exit = true` to unload usbmon automatically on exit, but only when usbtop-ng loaded it for that session.

### Command Line Options

```
usbtop-ng [OPTIONS]

Options:
  -v, --verbose            Enable verbose logging
  -c, --config <CONFIG>    Preferences file path (default: ~/.usbtop-ng/preferences.toml)
  -r, --refresh <REFRESH>  Refresh rate in milliseconds (floored at 100ms) [default: 1000]
      --force              Force run without usbmon (limited functionality)
      --setup              Show platform-specific setup instructions
      --create-alias       Create shell alias for 'usbtop' command
  -h, --help               Print help
  -V, --version            Print version
```

## 🛠 Development

Requirements:
- Rust (latest stable)

Build:
```bash
cargo build
```

Run tests:
```bash
cargo test
```

## 📄 License

This project is licensed under the **BSD 3-Clause License**, matching the original usbtop package's license family.  
You are free to use, modify, and distribute this code, provided you include the original copyright and license notice in any copies or substantial portions.

See [LICENSE](LICENSE) for full details.

## 🙏 Acknowledgments

- Inspired by the original [usbtop](https://github.com/aguinet/usbtop) by Adrien Guinet.
- Thanks to the Rust community for making systems programming safer and fun.
# usbtop-ng

usbtop-ng is a next-generation USB traffic monitoring tool, reimagined in Rust for speed, safety, and modern systems.

Inspired by the original [usbtop](https://github.com/aguinet/usbtop), usbtop-ng is an **independent reimplementation**.  
It does **not** share code with the original project and is **not affiliated with or endorsed by** its authors.

## Current Status

usbtop-ng builds, runs its setup checks, and opens the terminal UI with live packet-to-device monitoring wired end-to-end: usbmon reader thread(s) parse the `Nu` text interface, hand packets to the UI thread over an `mpsc` channel, and `DeviceManager` aggregates them into per-device bandwidth stats on every refresh tick.

## ✨ Features

- Live per-device USB bandwidth (RX/TX), plus running totals and peak bandwidth across all devices
- Bandwidth history graph over a 60-second sliding window
- Device metadata (vendor, product, speed) resolved from sysfs
- Disconnect detection: devices are shown greyed out for 5 seconds after disconnecting, then removed
- Managed usbmon load/unload, with the choice remembered in preferences
- Lightweight, terminal-friendly interface
- Rust-powered performance and safety
- Cross-platform support (Linux, *BSD, macOS — Windows WIP); live monitoring via usbmon is Linux-only, other platforms get device enumeration only
- Low resource footprint

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

Press `q` to quit.  
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
  -r, --refresh <REFRESH>  Refresh rate in milliseconds [default: 1000]
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
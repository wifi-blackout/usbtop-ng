# usbtop-ng

usbtop-ng is a next-generation USB traffic monitoring tool, reimagined in Rust for speed, safety, and modern systems.

Inspired by the original [usbtop](https://github.com/aguinet/usbtop), usbtop-ng is an **independent reimplementation**.  
It does **not** share code with the original project and is **not affiliated with or endorsed by** its authors.

## Current Status

usbtop-ng builds, runs its setup checks, and opens the terminal UI. Live packet-to-device monitoring is still being wired into the UI, so expect limited runtime functionality until the monitoring loop is connected.

## ✨ Features

- USB traffic statistics with **%busy** calculations for devices and buses
- **Speed capability mismatch detection** - shows when devices could run faster
- Lightweight, terminal-friendly interface
- Rust-powered performance and safety
- Linux usbmon support; BSD/macOS support is currently limited
- Low resource footprint
- **Visual indicators** for high utilization and speed limitations

## 📦 Installation

```bash
# Clone the repository
git clone https://github.com/wifi-blackout/usbtop-ng.git
cd usbtop-ng

# Build and install
cargo build --release
sudo cp target/release/usbtop-ng /usr/local/bin/
```

## 🚀 Usage

```bash
usbtop-ng
```

Press `q` to quit.  
Run with `--help` to see all options.

On Linux, usbtop-ng asks before loading usbmon with `sudo modprobe usbmon` when live monitoring needs it. If usbtop-ng loaded usbmon for this session, it asks on quit whether to unload it with `sudo modprobe -r usbmon`. Preferences live in `~/.usbtop-ng/preferences.toml`.

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

See [LICENSE](../LICENSE) for full details.

## 🙏 Acknowledgments

- Inspired by the original [usbtop](https://github.com/aguinet/usbtop) by Adrien Guinet.
- Thanks to the Rust community for making systems programming safer and fun.

# usbtop-ng

usbtop-ng is a next-generation USB traffic monitoring tool, reimagined in Rust for speed, safety, and modern systems.

Inspired by the original [usbtop](https://github.com/aguinet/usbtop), usbtop-ng is an **independent reimplementation**.  
It does **not** share code with the original project and is **not affiliated with or endorsed by** its authors.

## Current Status

usbtop-ng builds, runs its setup checks, and opens the terminal UI with live packet-to-device monitoring wired end-to-end: usbmon reader thread(s) read the binary `/dev/usbmonN` interface when it's available, falling back automatically to the debugfs `Nu` text interface otherwise, hand packets to the UI thread over an `mpsc` channel, and `DeviceManager` aggregates them into per-device bandwidth stats, physical topology, and %busy on every refresh tick.

## ✨ Features

- Controller-grouped, physically port-ordered device list, with the USB2-side and USB3-side buses of a shared xHCI controller shown as adjacent sibling buses
- Live per-device USB bandwidth (RX/TX), plus running totals and peak bandwidth across all devices
- Per-device and per-bus %busy against each USB speed's practical bandwidth, plus ⚡ high-utilization and 🔺 capability-exceeds-bus indicators (🔺 is best-effort: it fires when a device's sysfs `version` declares bcdUSB 3.x but it is linked slower)
- Color-coded USB link speeds, and a split chart pane (aggregate total plus the selected device's rx/tx history)
- Bounded packet queue with drop accounting: readers never block on a slow UI, and the header shows `dropped: N` if anything was lost
- Device metadata (vendor, product, speed) resolved from sysfs
- Disconnect detection: devices are shown greyed out for 5 seconds after disconnecting, then removed
- Managed usbmon load/unload, with the choice remembered in preferences
- Linux usbmon support (binary `/dev/usbmonN` interface with automatic text fallback); live monitoring is Linux-only — on BSD/macOS the UI can open (with `--force` where needed) but shows no devices
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

# Installation Guide

This guide covers installing and setting up usbtop-ng from source.

## System Requirements

- Rust latest stable
- Git
- Linux kernel with usbmon support for live USB traffic monitoring

BSD and macOS support is currently limited. Linux usbmon is the primary supported monitoring backend.

## Build from Source

```bash
git clone https://github.com/wifi-blackout/usbtop-ng.git
cd usbtop-ng
cargo build --release
sudo cp target/release/usbtop-ng /usr/local/bin/
```

Or install directly from the checkout:

```bash
cargo install --path .
```

## Linux Setup

### 1. Kernel Module

usbtop-ng can load usbmon for you. When usbmon is not loaded, it asks before running:

```bash
sudo modprobe usbmon
```

If you prefer manual setup, run that command yourself. Verify it loaded with:

```bash
lsmod | grep usbmon
```

When usbtop-ng loaded usbmon for the current session, it asks on quit whether to leave usbmon loaded or run:

```bash
sudo modprobe -r usbmon
```

You can control these prompts in `~/.usbtop-ng/preferences.toml`:

```toml
auto_load_usbmon = false
unload_usbmon_on_exit = false
```

### 2. debugfs

```bash
mount | grep debugfs || sudo mount -t debugfs none /sys/kernel/debug
```

### 3. Permissions

The usbmon interfaces under `/sys/kernel/debug/usb/usbmon/` may require root or adjusted permissions depending on your distribution.

Simplest test:

```bash
sudo usbtop-ng
```

Limited-functionality UI smoke test without usbmon:

```bash
usbtop-ng --force
```

## Verification

```bash
usbtop-ng --version
usbtop-ng --help
usbtop-ng --setup
```

Development checks from the source tree:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## Uninstalling

If installed to `/usr/local/bin`:

```bash
sudo rm /usr/local/bin/usbtop-ng
```

If installed with Cargo:

```bash
cargo uninstall usbtop-ng
```

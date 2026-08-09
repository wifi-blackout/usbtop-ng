# Installation Guide

This guide installs usbtop-ng from source and prepares a Linux host for live
monitoring.

## System requirements

- Rust 1.88 or later.
- Git.
- Linux, with a kernel that provides usbmon.

usbmon is the only monitoring backend, and it is a Linux kernel module. A build
for any other platform stops at a compile error.

## Build from source

1. Clone the repository:
   ```bash
   git clone https://github.com/wifi-blackout/usbtop-ng.git
   ```
   The clone creates a `usbtop-ng` directory. If that directory already exists,
   skip to step 2.
2. Enter the checkout:
   ```bash
   cd usbtop-ng
   ```
3. Build the release binary:
   ```bash
   cargo build --release
   ```
   The build writes `target/release/usbtop-ng`. If the build fails on a
   compiler error, confirm that `cargo --version` reports Rust 1.88 or later.
4. Copy the binary onto your path:
   ```bash
   sudo cp target/release/usbtop-ng /usr/local/bin/
   ```
   Confirm the copy with `usbtop-ng --version`. If `sudo` refuses, copy the
   binary into a directory on your own path instead.

To install through Cargo instead of copying the binary, run this from the
checkout:

```bash
cargo install --path .
```

## Linux setup

### 1. Load usbmon

usbtop-ng loads usbmon for you. It prints the command and asks first, unless
`auto_load_usbmon = true` in the preferences file.

1. If you prefer to load usbmon yourself, run:
   ```bash
   sudo modprobe usbmon
   ```
   The command prints nothing on success. If it reports that the module was not
   found, your kernel lacks usbmon.
2. Confirm that the module is loaded:
   ```bash
   lsmod | grep usbmon
   ```
   The command prints one `usbmon` line. If it prints nothing, run
   `usbtop-ng --setup` for the manual setup steps.
3. When usbtop-ng loaded usbmon for the session, answer the question it asks on
   quit. Answer `y` to run:
   ```bash
   sudo modprobe -r usbmon
   ```

To control both questions, edit `~/.usbtop-ng/preferences.toml`:

```toml
auto_load_usbmon = false
unload_usbmon_on_exit = false
```

### 2. Mount debugfs

1. Check whether debugfs is mounted, and mount it if it is not:
   ```bash
   mount | grep debugfs || sudo mount -t debugfs none /sys/kernel/debug
   ```
   The command prints a `debugfs on /sys/kernel/debug` line when the mount is
   in place. If the mount fails, run `usbtop-ng --setup` for the manual setup
   steps.

### 3. Grant read access

The usbmon interfaces live under `/sys/kernel/debug/usb/usbmon/`. They need
root, or read access granted some other way, depending on the distribution.

1. Start usbtop-ng as root:
   ```bash
   sudo usbtop-ng
   ```
   The device table fills within one refresh interval. If it stays empty,
   generate USB traffic, such as a file copy to a USB drive.
2. To open the UI without usbmon, run:
   ```bash
   usbtop-ng --force
   ```
   The UI opens and the device table stays empty.

## Verification

1. Print the version:
   ```bash
   usbtop-ng --version
   ```
   Record the version. Bug reports need it.
2. Print the options:
   ```bash
   usbtop-ng --help
   ```
3. Print the setup steps:
   ```bash
   usbtop-ng --setup
   ```

From the source tree, run the same three checks CI runs:

1. Check formatting:
   ```bash
   cargo fmt --all -- --check
   ```
   The command prints nothing when formatting is correct. If it prints a diff,
   run `cargo fmt` and repeat.
2. Run clippy:
   ```bash
   cargo clippy --all-targets -- -D warnings
   ```
   Any warning fails the command. Fix the warning and repeat.
3. Run the tests:
   ```bash
   cargo test --all-targets
   ```
   The command reports 188 passed.

## Uninstall

1. If you copied the binary to `/usr/local/bin`, remove it:
   ```bash
   sudo rm /usr/local/bin/usbtop-ng
   ```
2. If you installed with Cargo, remove it:
   ```bash
   cargo uninstall usbtop-ng
   ```
3. To remove the preferences file and its directory, run:
   ```bash
   rm -r ~/.usbtop-ng
   ```

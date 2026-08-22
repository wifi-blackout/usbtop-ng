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
3. Run the install script:
   ```bash
   ./install.sh
   ```
   The script runs `cargo build --release`, then asks for `sudo` to copy the
   binary to `/usr/local/bin`, create a `usbtop` symlink, and install the man
   page. It prints the four paths it installed. If the build fails on a
   compiler error, confirm that `cargo --version` reports Rust 1.88 or later.
4. Confirm the install:
   ```bash
   usbtop-ng --version
   ```
   The command reports the installed version.

The symlink makes `usbtop` and `sudo usbtop` both work, because both resolve
through your path. A shell alias would not work under `sudo`.

The script also installs the man page to
`$(dirname "$PREFIX")/share/man/man1/usbtop-ng.1`, with a `usbtop.1` symlink
alongside it, so `man usbtop-ng` and `man usbtop` both work.

The script refuses to replace a `usbtop` command or a `usbtop.1` man page it
does not own. It stops before touching anything and names the conflicting
file. Remove that file yourself, or rerun with `FORCE_ALIAS=1 ./install.sh`
to replace it.

To install somewhere other than `/usr/local/bin`, set `PREFIX`:

```bash
PREFIX="$HOME/.local/bin" ./install.sh
```

To install by hand instead, build and copy the binary yourself:

```bash
cargo build --release
sudo install -m 0755 target/release/usbtop-ng /usr/local/bin/usbtop-ng
sudo ln -sf usbtop-ng /usr/local/bin/usbtop
```

## Shell completions

usbtop-ng prints a completion script for any shell `clap_complete` supports.
Pick the section for your shell.

1. Bash:
   ```bash
   usbtop-ng --print-completions bash | sudo tee /etc/bash_completion.d/usbtop-ng >/dev/null
   ```
   Start a new shell to pick it up.
2. Zsh:
   ```bash
   mkdir -p ~/.zfunc
   usbtop-ng --print-completions zsh > ~/.zfunc/_usbtop-ng
   ```
   Add `~/.zfunc` to `fpath` before `compinit` runs, in `~/.zshrc`:
   ```zsh
   fpath=(~/.zfunc $fpath)
   autoload -Uz compinit && compinit
   ```
   Start a new shell to pick it up.
3. Fish:
   ```bash
   usbtop-ng --print-completions fish > ~/.config/fish/completions/usbtop-ng.fish
   ```
   Fish picks up new completion files in the next shell.

## Update

1. Pull the latest code:
   ```bash
   git pull
   ```
2. Reinstall:
   ```bash
   ./install.sh
   ```
   The script rebuilds and replaces the binary, the symlink, and the man
   page.
3. Confirm the new version:
   ```bash
   usbtop-ng --version
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
2. Press `i` to hide idle devices, or press `i` again to show them. usbtop-ng
   saves the choice to the preferences file.
3. To open the UI without usbmon, run:
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
   The command reports 292 passed.

## Uninstall

1. If you installed to `/usr/local/bin`, remove the binary and the symlink:
   ```bash
   sudo rm /usr/local/bin/usbtop-ng /usr/local/bin/usbtop
   ```
2. Remove the man page and its symlink:
   ```bash
   sudo rm /usr/local/share/man/man1/usbtop-ng.1 /usr/local/share/man/man1/usbtop.1
   ```
3. To remove the preferences file and its directory, run:
   ```bash
   rm -r ~/.usbtop-ng
   ```

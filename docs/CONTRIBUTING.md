# Contributing to usbtop-ng

This document covers the development environment, the checks a change has to
pass, and how to send it.

## Table of contents

- [Getting started](#getting-started)
- [Development environment](#development-environment)
- [Code style](#code-style)
- [Testing](#testing)
- [Pull request process](#pull-request-process)
- [Issue reporting](#issue-reporting)
- [Architecture overview](#architecture-overview)
- [Linux development](#linux-development)
- [Release process](#release-process)
- [Getting help](#getting-help)
- [Conduct](#conduct)

## Getting started

### Prerequisites

- Rust 1.88 or later. Install it from [rustup.rs](https://rustup.rs/).
- Git.
- A Linux host, to test usbmon paths.
- Working knowledge of USB transfers and system monitoring.

### Fork and clone

1. Fork the repository on GitHub.
2. Clone your fork:
   ```bash
   git clone https://github.com/wifi-blackout/usbtop-ng.git
   ```
   Use your fork's URL in place of the one above.
3. Enter the checkout:
   ```bash
   cd usbtop-ng
   ```
4. Add the upstream remote:
   ```bash
   git remote add upstream https://github.com/wifi-blackout/usbtop-ng.git
   ```
   The command prints nothing on success. If it reports that the remote
   exists, run `git remote -v` and confirm the URL.

## Development environment

### Setup

1. Install the optional development tools:
   ```bash
   cargo install cargo-watch cargo-audit cargo-deny
   ```
   Each tool lands in `~/.cargo/bin`. If one fails to build, skip it. Nothing
   below needs it.
2. Build the project:
   ```bash
   cargo build
   ```
   The build writes `target/debug/usbtop-ng`. If it fails on a compiler error,
   confirm that `cargo --version` reports Rust 1.88 or later.
3. Run the tests:
   ```bash
   cargo test
   ```
   The unit suite reports 576 passed; the `tests/` directory adds the
   pipe and PTY harnesses alongside it. A failure names the test. Fix it and
   repeat.
4. To run with debug output, use:
   ```bash
   RUST_LOG=debug cargo run -- --verbose
   ```

### Other development commands

```bash
# Rebuild on every change
cargo watch -x build

# Type-check without building
cargo check

# Lint
cargo clippy -- -D warnings

# Format
cargo fmt

# Audit dependencies for advisories
cargo audit

# Build and open the API documentation
cargo doc --open
```

## Code style

### Rust guidelines

- Run `cargo fmt` before every commit.
- Follow Rust naming conventions: `snake_case` for functions, `PascalCase` for
  types.
- Run `cargo clippy` and fix every warning.
- Document public items with `///` doc comments.

### User-facing text

Every string a person can see follows one of three shapes. Tests assert
on a few of them, so a rewording is a code change like any other.

- **Error messages** (`anyhow!`, `bail!`, `warn!`, `error!`, and errors
  printed with `eprintln!`): start lowercase unless the first word is a
  proper name, acronym, or identifier (`SEC-1`, `MON_IOCG_STATS`, `USB`,
  `eBPF`); no trailing period; name the subject and the offending value;
  chain causes with `: ` (`could not open /dev/usbmon1: permission
  denied`); say "could not", never "Failed to"; no exclamation marks.
- **Remedies and prompts** (guidance text printed with `println!` or
  `eprintln!`): sentence case, imperative, one action per line, exact
  commands on their own line.
- **Log lines** (`info!`, `debug!`): lowercase, present tense, and name
  the interface and bus they concern (`using usbmon mmap-ring interface
  on bus 3`).

### Code organization

```
src/
├── main.rs           # Entry point, CLI, usbmon startup checks, exit flow
├── usbmon/           # USB monitoring core
│   ├── mod.rs        # Module detection, load/unload, setup instructions
│   ├── monitor.rs    # Interface probe, reader threads, bounded channel
│   ├── mmap_ring.rs  # Read loop over the usbmon binary interface's mmap ring
│   ├── ring.rs       # usbmon binary-interface ioctls: ring ladder, size, drop stats
│   ├── reader.rs     # Read loop over the usbmon Nu text interface
│   ├── binary.rs     # Read loop over the usbmon /dev/usbmonN binary interface via read()
│   └── parser.rs     # Nu text-format parsing, UsbSpeed bandwidth/color tables
├── capture/          # Fixture capture and assembly (shared by --capture-fixture and --support)
├── diag/             # --support: redaction rules, collectors, device inventory, bundle writer
│   ├── redact.rs     # Home paths to ~, MAC and UUID masking, the environment allowlist
│   ├── collect.rs    # Build, host, usbmon, backend probe, dmesg, config, terminal
│   ├── inventory.rs  # USB devices, interfaces, endpoints, hub ports, descriptors, Type-C, Thunderbolt
│   ├── bundle.rs     # Bundle directory, manifest, UTC stamp, tar archive
│   └── support.rs    # The orchestrator, the summary, the log tee
├── headless/         # --once and --batch reports
│   ├── mod.rs        # Report model, text renderer, the sampling loop
│   └── export.rs     # --output file sink and the run record
├── device/           # Device management
│   ├── mod.rs        # Device structure, sysfs metadata, %busy, indicators
│   └── manager.rs    # Bus aggregation, controller resolution, packet routing
├── stats/            # Statistics engine
│   └── mod.rs        # Bucketed sliding window, peak, rate history
├── tui/              # Terminal chassis
│   ├── mod.rs        # Terminal setup/teardown and the event loop
│   ├── events.rs     # Input thread, UiEvent, redraw scheduling
│   ├── output.rs     # ShedWriter, the non-blocking output stage
│   ├── sync.rs       # Mode-2026 handshake and probe policy
│   └── lifecycle.rs  # Restore, panic hook, signal thread, exit prompts
├── ui/               # Application state and widgets
│   ├── mod.rs        # App state, key handling, packet drain, rendering
│   └── colors.rs     # Color definitions
└── config/           # Preferences
    └── mod.rs        # ~/.usbtop-ng/preferences.toml handling
```

### Commit messages

Use the conventional commit format:

```
type(scope): brief description

Longer description if needed

- Bullet points for details
- Reference issues with #123
```

**Types:** feat, fix, docs, style, refactor, test, chore

**Examples:**

```
feat(ui): add device search functionality
fix(usbmon): handle malformed text-line parsing edge case
docs: update installation instructions
```

## Testing

### Test categories

1. **Unit tests** cover single functions and modules.
2. **Integration tests** cover how components fit together.

### Running tests

```bash
# Run the default suite
cargo test

# Run one module's tests
cargo test usbmon::parser

# Run tests with their output
cargo test -- --nocapture

# Run every target, which is what CI runs
cargo test --all-targets
```

`cargo test` and `cargo test --all-targets` run the same three suites, all
hermetic. The unit suite reports 576 passed, working against
fixture files, FIFOs, and `tempfile` paths, with no `/dev` and no debugfs
access. The `tests/` directory adds two more: `restore_pipe.rs` (2 tests),
proving the terminal-restore bytes reach a piped stdout while the process is
still alive, and `pty.rs` (3 tests), the wedged-terminal checks (quit,
`SIGHUP`, a terminal that stops reading) run against a real `openpty`-backed
child. Both spawn the real binary and pass on any Linux host; neither
touches the real `~/.usbtop-ng` or usbmon. CI runs this suite and no other.

### Live system tests (the `integration` feature)

The opt-in `integration` cargo feature adds 5 tests to the unit suite: one
that reads the real usbmon interfaces instead of fixtures, one that exercises
the real `fchown(2)` call behind `sudo`'s ownership fix-up and needs real root,
one that opens a real `/dev/usbmon0` and walks its mmap ring through the real
`mmap`, `MON_IOCX_MFETCH`, and `MON_IOCG_STATS` syscalls, one that proves
`kernel_dropped` is readable while that mmap reader is still running, not
only after it stops, and one that runs `--support`'s orchestrator live as
root and checks that the embedded fixture's goldens replay with zero kernel
drops on an idle bus.

1. Confirm that usbmon is loaded and that you can read
   `/sys/kernel/debug/usb/usbmon`. Root access is the usual route.
2. Run the suite with the feature:
   ```bash
   cargo test --features integration
   ```
   The unit suite reports 581 passed. Without usbmon, without root, or
   without a mmap-capable `/dev/usbmon0`, each of the five extra tests
   prints its own skip message and passes.

The live test is gated on the feature, so it compiles to nothing on default
builds. CI does not run this feature. It exists for manual checks on a real
machine with real USB traffic.

### Live system tests (the `ebpf` feature)

The opt-in `ebpf` cargo feature builds and tests the eBPF capture backend
covered in [INSTALL.md](INSTALL.md#building-the-ebpf-backend). Building it
needs clang (with the BPF target) and libbpf-dev.

1. Run the suite with the feature:
   ```bash
   cargo test --all-targets --features ebpf
   ```
   The unit suite reports 597 passed. One of those tests loads and attaches
   the real kprobe, needing real root and a BTF-enabled kernel
   (`/sys/kernel/btf/vmlinux`); without either, it prints its own skip
   message and passes, the same contract the `integration` feature's live
   tests use above.

Unlike the `integration` feature, CI does build and hermetic-test this
feature: `cargo build --features ebpf` and `cargo test --features ebpf` run
on every push, because attaching a kprobe unprivileged is the only part
that needs a real machine.

### Hermetic feature tests (the `capture-fixture` feature)

`capture-fixture` is a third opt-in feature build in the gate matrix,
alongside `integration` and `ebpf` above. The capture core itself
(`src/capture/`, `src/fixture_replay.rs`) is part of the default build,
since `--support` embeds a fixture bundle; the feature gates only the
`--capture-fixture` subcommand that records a hardware fixture into
`tests/fixtures/hosts/` (see
[TESTING.md](TESTING.md#capturing-hardware-fixtures) for the capture
procedure). Needs no extra toolchain: the feature builds with just the MSRV
Rust toolchain.

1. Run the suite with the feature:
   ```bash
   cargo clippy --features capture-fixture --all-targets -- -D warnings
   cargo test --features capture-fixture
   ```
   The unit suite reports 576 passed under the feature, the same as
   without it: the capture core it exercises is already part of the
   default build, and the feature adds only the `--capture-fixture`
   subcommand, not tests.

Like `ebpf`, CI builds and hermetic-tests this feature on every push:
`cargo clippy --features capture-fixture --all-targets -- -D warnings` and
`cargo test --features capture-fixture` run in their own job. Nothing in
this feature needs root or real hardware -- the committed fixture corpus is
what gets replayed.

### Writing tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usb_speed_parsing() {
        assert_eq!(UsbSpeed::from_speed_str("480").to_mbps(), 480.0);
        assert_eq!(UsbSpeed::from_mbps(480.0).class(), SpeedClass::High);
    }

    #[test]
    fn test_packet_reading() {
        // Reader tests point UsbmonReader at a fixture file via
        // UsbmonReader::with_path(bus_id, path, follow) instead of the real
        // debugfs path. See src/usbmon/reader.rs for examples.
        // BinaryReader::with_path(bus_id, path, follow) is the equivalent
        // seam for the binary interface. See src/usbmon/binary.rs.
    }
}
```

### Test coverage

Cover these areas first:

- USB packet parsing.
- Bandwidth calculation.
- Error handling paths.

## Pull request process

### Before you submit

1. Update from upstream:
   ```bash
   git fetch upstream
   ```
2. Rebase onto the upstream branch:
   ```bash
   git rebase upstream/main
   ```
   If the rebase stops on a conflict, resolve the files it names, run
   `git add` on each, then run `git rebase --continue`.
3. Check formatting, exactly as CI does:
   ```bash
   cargo fmt --all -- --check
   ```
   The command prints nothing when formatting is correct. If it prints a diff,
   run `cargo fmt` and repeat.
4. Lint, exactly as CI does:
   ```bash
   cargo clippy --all-targets -- -D warnings
   ```
   Any warning fails the command. Fix it and repeat.
5. Test, exactly as CI does:
   ```bash
   cargo test --all-targets
   ```
   The unit suite reports 576 passed; the `tests/` directory adds the
   pipe and PTY harnesses alongside it. A failure names the test. Fix it and
   repeat.
6. Update the documentation your change affects.
7. Add tests for new behavior.

### Pull request guidelines

1. Create a feature branch:
   ```bash
   git checkout -b feature/amazing-feature
   ```
2. Make focused commits with clear messages.
3. Add your user-facing changes to `CHANGELOG.md`.
4. Confirm that CI passes.
5. Request a review from a maintainer.

### Pull request template

```markdown
## Description
Brief description of changes

## Type of Change
- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation update

## Testing
- [ ] Unit tests pass
- [ ] Integration tests pass
- [ ] Manual testing completed

## Checklist
- [ ] Code follows style guidelines
- [ ] Self-review completed
- [ ] Documentation updated
- [ ] Tests added/updated
```

## Issue reporting

### Bug reports

Open a bug with the [bug report form](https://github.com/wifi-blackout/usbtop-ng/issues/new?template=bug_report.yml)
after running:

```bash
sudo usbtop-ng --support
```

It writes `usbtop-ng-support-<UTC time>/` and a `.tar.gz` beside it in the
current directory (or in the `PATH` you pass), prints a summary, and lists
every file it gathered. The summary's `bundle:` line names the archive
relative to the current directory when it lives there, or with the home
rewritten to `~` otherwise, so the pasted summary carries no home path.
Paste the summary into the form and attach the archive. Under `sudo`, a
bundle written inside your home directory is handed back to you; one
written elsewhere (`/tmp`, say) stays root-owned. Without `sudo` the
bundle still holds everything but the capture; `--no-capture` skips the
capture on purpose, `--window SECONDS` sets its length (default 5,
floor 0.1).

What the bundle holds: build and host details (`build.toml`, `host.toml`),
the usbmon probe and the backend the monitor would select (`usbmon.toml`),
the USB lines of the kernel log (`dmesg-usb.txt`), every device's full
self-description with its raw descriptors (`inventory/`), your preferences
and internal-device snapshot with home paths rewritten (`config/`), the
terminal setup (`terminal.toml`), the embedded fixture (`fixture/`, the same
layout as `tests/fixtures/hosts/`), a replayed report (`report.json`), the
printed summary saved as `SUMMARY.txt`, the run's debug log
(`usbtop-ng.log`), and a `manifest.toml` listing each file with its size,
the redaction counts, and everything that was unavailable.

What it never holds: the hostname, machine-id, DMI serial or UUID, any host
MAC address or IP address, or a user name. Device serial numbers and
Thunderbolt `unique_id` values are kept, because a cloned or re-badged
device is often only distinguishable by them. The `inventory/` files are for
the maintainer reading the issue and are never committed; the `fixture/`
directory carries no serial and is what becomes a regression fixture.

### Feature requests

Include:

- **Use case**: why the feature is needed.
- **Proposed solution**: how it should work.
- **Alternatives considered**: other approaches.
- **Additional context**: screenshots and examples.

### Performance issues

Include:

- **System specifications**: CPU, RAM, USB controller.
- **Performance figures**: CPU and memory use.
- **USB device count** and device types.
- **Profiling data**, if you have it.

## Architecture overview

### Core components

1. **usbmon module** reads packets from the Linux usbmon interfaces.
2. **Device manager** tracks devices, buses, and metadata.
3. **Statistics engine** calculates bandwidth.
4. **TUI chassis** owns terminal setup, the event loop, output, and teardown.
5. **UI layer** holds app state and draws the widgets with ratatui.
6. **Configuration** reads and writes the preferences file.

### Data flow

```
/dev/usbmonN mmap ring (binary, preferred) ─┐
/dev/usbmonN via read() (binary, fallback) ─┼─→ Reader thread → Parser → UsbPacket
usbmon Nu file (text, last resort)         ─┘                              │
                                                              bounded mpsc channel
                                                                           ↓
       UI thread ← DeviceManager (sysfs metadata, bandwidth stats, %busy,
                                  controller and port grouping)
```

[ARCHITECTURE.md](ARCHITECTURE.md) covers the modules, the TUI chassis, and the
known limitations.

### Where new work goes

1. **Kernel interfaces**: the usbmon readers under `src/usbmon/`, and the sysfs
   metadata under `src/device/`.
2. **UI components**: new `draw_*` functions in `src/ui/mod.rs`, and colors in
   `src/ui/colors.rs`.
3. **Packet analysis**: `src/usbmon/parser.rs` for the text interface,
   `src/usbmon/binary.rs` for the binary interface via `read()`, and
   `src/usbmon/mmap_ring.rs` for the binary interface via its mmap ring.
4. **Statistics**: `src/stats/mod.rs`.
5. **Diagnostics**: `src/diag/` for anything `--support` gathers. A new
   collector takes its filesystem roots as parameters, returns typed data
   plus notes, and never fails the bundle; add the file to the tree in
   CONTRIBUTING and to the manifest test in `src/diag/support.rs`.

### Dependencies

- `ratatui`: terminal UI framework.
- `crossterm`: terminal control.
- `clap`: command-line parsing.
- `serde` and `toml`: preferences serialization for
  `~/.usbtop-ng/preferences.toml`.
- `anyhow`, `log`, and `env_logger`: error handling and logging.
- `libc`: `fcntl` for the non-blocking descriptor, `write(2)` for the frame
  drain, the `EIO` and `SIGHUP` constants, and `mmap`/`munmap`/`ioctl` for the
  usbmon mmap ring reader.
- `signal-hook`: the signal thread behind the terminal restore.

## Linux development

usbtop-ng builds on Linux only. `src/main.rs` carries a `compile_error!` for
every other target, so a change never needs a second platform's arm.

- Test against more than one kernel version.
- Verify parsing on every interface: the binary `/dev/usbmonN` device's mmap
  ring, which usbtop-ng prefers when it is usable, the same device via
  `read()` when the ring is not, and the debugfs `Nu` text fallback.
- Check the debugfs mount requirements.
- Test the permission cases, as root and as a plain user.

## Release process

1. Update the version in `Cargo.toml`.
2. Record the changes in `CHANGELOG.md`.
3. Tag the release:
   ```bash
   git tag v0.x.y
   ```
   If the tag already exists, git says so. Choose the next version rather than
   moving the tag.
4. Create the GitHub release and attach the binaries.
5. Publish the crate:
   ```bash
   cargo publish
   ```
   The command uploads the crate. If it reports a missing login, run
   `cargo login` first.

## Getting help

- **GitHub Discussions**: questions and ideas.
- **GitHub Issues**: bug reports and feature requests.
- **Code review**: tag a maintainer on the pull request.

## Conduct

Be direct and be respectful in issues, pull requests, and reviews. Review the
change, not the person who wrote it.

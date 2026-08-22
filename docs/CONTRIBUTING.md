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
   The command reports 294 passed. A failure names the test. Fix it and repeat.
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

### Code organization

```
src/
├── main.rs           # Entry point, CLI, usbmon startup checks, exit flow
├── usbmon/           # USB monitoring core
│   ├── mod.rs        # Module detection, load/unload, setup instructions
│   ├── monitor.rs    # Interface probe, reader threads, bounded channel
│   ├── reader.rs     # Read loop over the usbmon Nu text interface
│   ├── binary.rs     # Read loop over the usbmon /dev/usbmonN binary interface
│   └── parser.rs     # Nu text-format parsing, UsbSpeed bandwidth/color tables
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

`cargo test` and `cargo test --all-targets` run the hermetic suite only. Every
test there works against fixture files, FIFOs, and `tempfile` paths. The suite
therefore passes on any operating system, with no `/dev` and no debugfs access.
It reports 294 passed. CI runs this suite and no other.

### Live system tests (the `integration` feature)

The opt-in `integration` cargo feature adds 1 test that reads the real usbmon
interfaces instead of fixtures.

1. Confirm that usbmon is loaded and that you can read
   `/sys/kernel/debug/usb/usbmon`. Root access is the usual route.
2. Run the suite with the feature:
   ```bash
   cargo test --features integration
   ```
   The command reports 295 passed. Without usbmon the extra test prints a skip
   message and passes.

The live test is gated on the feature, so it compiles to nothing on default
builds. CI does not run this feature. It exists for manual checks on a real
machine with real USB traffic.

### Writing tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usb_speed_parsing() {
        assert_eq!(UsbSpeed::from_speed_str("480"), UsbSpeed::High);
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
   The command reports 294 passed. A failure names the test. Fix it and repeat.
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

Use the bug report template and include:

- **Environment**: operating system, Rust version, usbtop-ng version.
- **Steps to reproduce** the bug.
- **Expected behavior and actual behavior.**
- **Log output**, captured with `RUST_LOG=debug`.
- **System information**: the output of `lsusb` and `lsmod | grep usbmon`.

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
/dev/usbmonN (binary, preferred) ─┐
                                  ├─→ Reader thread → Parser → UsbPacket
usbmon Nu file (text, fallback)  ─┘                              │
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
3. **Packet analysis**: `src/usbmon/parser.rs` for the text interface, and
   `src/usbmon/binary.rs` for the binary interface.
4. **Statistics**: `src/stats/mod.rs`.

### Dependencies

- `ratatui`: terminal UI framework.
- `crossterm`: terminal control.
- `clap`: command-line parsing.
- `serde` and `toml`: preferences serialization for
  `~/.usbtop-ng/preferences.toml`.
- `anyhow`, `log`, and `env_logger`: error handling and logging.
- `libc`: `fcntl` for the non-blocking descriptor, `write(2)` for the frame
  drain, and the `EIO` and `SIGHUP` constants.
- `signal-hook`: the signal thread behind the terminal restore.

## Linux development

usbtop-ng builds on Linux only. `src/main.rs` carries a `compile_error!` for
every other target, so a change never needs a second platform's arm.

- Test against more than one kernel version.
- Verify parsing on both interfaces: the binary `/dev/usbmonN` device, which
  usbtop-ng prefers when it opens, and the debugfs `Nu` text fallback.
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

# Contributing to usbtop-ng

Thank you for your interest in contributing to usbtop-ng! This document provides guidelines and information for contributors.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Environment](#development-environment)
- [Code Style](#code-style)
- [Testing](#testing)
- [Pull Request Process](#pull-request-process)
- [Issue Reporting](#issue-reporting)
- [Architecture Overview](#architecture-overview)

## Getting Started

### Prerequisites

- **Rust 1.70+** - Install from [rustup.rs](https://rustup.rs/)
- **Git** for version control
- **Linux system** for full testing (usbmon support)
- Basic understanding of USB protocols and system monitoring

### Fork and Clone

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/wifi-blackout/usbtop-ng.git
   cd usbtop-ng
   ```
3. Add the upstream repository:
   ```bash
   git remote add upstream https://github.com/wifi-blackout/usbtop-ng.git
   ```

## Development Environment

### Setup

```bash
# Install development dependencies
cargo install cargo-watch cargo-audit cargo-deny

# Build the project
cargo build

# Run tests
cargo test

# Run with debug output
RUST_LOG=debug cargo run -- --verbose
```

### Useful Development Commands

```bash
# Watch for changes and rebuild
cargo watch -x build

# Check code without building
cargo check

# Run clippy for linting
cargo clippy -- -D warnings

# Format code
cargo fmt

# Check for security vulnerabilities
cargo audit

# Generate documentation
cargo doc --open
```

## Code Style

### Rust Guidelines

We follow the official Rust style guidelines:

- Use `cargo fmt` for consistent formatting
- Follow Rust naming conventions (snake_case, PascalCase, etc.)
- Write idiomatic Rust code
- Use `cargo clippy` and address all warnings
- Document public APIs with doc comments (`///`)

### Code Organization

```
src/
├── main.rs           # Entry point and CLI
├── usbmon/           # USB monitoring core
│   ├── mod.rs        # Module detection and setup
│   ├── reader.rs     # Packet capture
│   └── parser.rs     # Packet parsing
├── device/           # Device management
│   ├── mod.rs        # Device structure and methods
│   └── manager.rs    # Device discovery
├── stats/            # Statistics engine
│   └── mod.rs        # Bandwidth calculations
├── ui/               # Terminal interface
│   ├── mod.rs        # Main UI logic
│   ├── colors.rs     # Color definitions
│   └── widgets.rs    # UI components
└── config/           # Preferences
    └── mod.rs        # ~/.usbtop-ng/preferences.toml handling
```

### Commit Messages

Use conventional commit format:

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
fix(usbmon): handle binary packet parsing edge case
docs: update installation instructions
```

## Testing

### Test Categories

1. **Unit Tests**: Test individual functions and modules
2. **Integration Tests**: Test component interactions
3. **Platform Tests**: Test platform-specific functionality

### Running Tests

```bash
# Run all tests
cargo test

# Run specific test module
cargo test usbmon::parser

# Run tests with output
cargo test -- --nocapture

# Run integration tests (requires usbmon)
cargo test --features integration

# Test with specific platform features
cargo test --features linux
```

### Writing Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_usb_speed_parsing() {
        assert_eq!(UsbSpeed::from_speed_str("480"), UsbSpeed::High);
    }
    
    #[tokio::test]
    async fn test_packet_reading() {
        // Async test example
    }
}
```

### Test Coverage

Aim for high test coverage, especially for:
- USB packet parsing logic
- Bandwidth calculations
- Error handling paths
- Platform-specific code

## Pull Request Process

### Before Submitting

1. **Update from upstream:**
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```

2. **Run full test suite:**
   ```bash
   cargo test
   cargo clippy
   cargo fmt --check
   ```

3. **Update documentation** if needed

4. **Add tests** for new functionality

### PR Guidelines

1. **Create a feature branch:**
   ```bash
   git checkout -b feature/amazing-feature
   ```

2. **Make focused commits** with clear messages

3. **Update CHANGELOG.md** for user-facing changes

4. **Ensure CI passes** on all platforms

5. **Request review** from maintainers

### PR Template

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

## Issue Reporting

### Bug Reports

Use the bug report template and include:

- **Environment**: OS, Rust version, usbtop-ng version
- **Steps to reproduce** the bug
- **Expected vs actual behavior**
- **Log output** with `RUST_LOG=debug`
- **System information**: `lsusb`, `lsmod | grep usbmon`

### Feature Requests

For new features, provide:

- **Use case**: Why is this needed?
- **Proposed solution**: How should it work?
- **Alternatives considered**: Other approaches
- **Additional context**: Screenshots, examples

### Performance Issues

Include:
- **System specs**: CPU, RAM, USB controller
- **Performance metrics**: CPU/memory usage
- **USB device count** and types
- **Profiling data** if available

## Architecture Overview

### Core Components

1. **usbmon Module**: Interfaces with Linux usbmon for packet capture
2. **Device Manager**: Tracks USB devices and metadata
3. **Stats Engine**: Calculates bandwidth statistics
4. **UI Layer**: Terminal interface with ratatui
5. **Config System**: TOML-based configuration

### Data Flow

```
USB Hardware → usbmon → Reader → Parser → Stats Engine → UI
                ↓              ↓           ↓
            Device Manager ← sysfs ← Platform Layer
```

### Adding New Features

1. **Platform Support**: Add new OS in platform-specific modules
2. **UI Components**: Extend widgets in `ui/widgets.rs`
3. **Monitoring**: Add new packet analysis in `usbmon/parser.rs`
4. **Statistics**: Enhance calculations in `stats/mod.rs`

### Dependencies

Key external dependencies:
- `ratatui`: Terminal UI framework
- `crossterm`: Cross-platform terminal manipulation
- `tokio`: Async runtime
- `clap`: Command-line parsing
- `serde`: Serialization (config files)
- `chrono`: Date/time handling

## Platform-Specific Development

### Linux

- Test with different kernel versions
- Verify usbmon binary/text format parsing
- Check debugfs mount requirements
- Test permission scenarios

### BSD Systems

- Use `usbconfig` for device enumeration
- Test on FreeBSD, OpenBSD, NetBSD
- Handle different device path formats

### macOS

- Limited functionality due to no usbmon
- Use system_profiler integration
- Test device enumeration only

## Release Process

1. **Version Bump**: Update `Cargo.toml` version
2. **Update CHANGELOG.md**: Document all changes
3. **Tag Release**: `git tag v0.x.y`
4. **GitHub Release**: Create release with binaries
5. **Crate Publication**: `cargo publish`

## Getting Help

- **GitHub Discussions**: General questions and ideas
- **GitHub Issues**: Bug reports and feature requests  
- **Code Review**: Tag maintainers in PRs
- **Discord/Matrix**: Real-time chat (links in README)

## Code of Conduct

Please follow our [Code of Conduct](CODE_OF_CONDUCT.md) in all interactions.

---

Thank you for contributing to usbtop-ng! 🚀
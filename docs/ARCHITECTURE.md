# usbtop-ng Architecture

This document provides a detailed overview of usbtop-ng's architecture, design decisions, and implementation details.

## Table of Contents

- [Overview](#overview)
- [System Architecture](#system-architecture)
- [Module Design](#module-design)
- [Data Flow](#data-flow)
- [Platform Abstraction](#platform-abstraction)
- [Performance Considerations](#performance-considerations)
- [Security Model](#security-model)

## Overview

usbtop-ng is designed as a modular USB monitoring tool with clear separation of concerns. Live monitoring is built on dedicated blocking reader threads (one per usbmon interface, or a single reader for the aggregate `0u` interface) that feed parsed packets to the main thread over an `mpsc` channel; there is no async runtime involved.

```
┌─────────────────────────────────────────────────────────────────┐
│                        usbtop-ng                                │
├─────────────────────────────────────────────────────────────────┤
│  Terminal UI (ratatui + crossterm)                             │
├─────────────────────────────────────────────────────────────────┤
│  Application Logic (UsbTopApp)                                 │
├─────────────────┬─────────────────┬─────────────────────────────┤
│  USB Monitor    │  Device Manager │  Statistics Engine          │
│  (usbmon)       │  (sysfs/udev)   │  (bandwidth calc)           │
├─────────────────┼─────────────────┼─────────────────────────────┤
│  Platform Abstraction Layer                                    │
├─────────────────────────────────────────────────────────────────┤
│  Operating System (Linux/BSD/macOS)                            │
└─────────────────────────────────────────────────────────────────┘
```

## System Architecture

### Core Principles

1. **Thread-Based I/O**: Dedicated blocking reader threads read usbmon and hand packets to the UI thread over an `mpsc` channel, so a stalled or idle interface never blocks rendering
2. **Modular Design**: Clear module boundaries with defined interfaces
3. **Cross-Platform**: Abstracted platform-specific code
4. **Memory Safe**: Rust's ownership system prevents common errors
5. **Performance**: Minimal overhead USB monitoring
6. **User-Friendly**: Rich terminal interface with intuitive controls

### Key Components

#### 1. USB Monitor (`usbmon/`)
- **Purpose**: Interface with kernel USB monitoring facilities
- **Components**:
  - `mod.rs`: Module detection, load/unload, and setup instructions
  - `monitor.rs`: Spawns one blocking reader thread per bus (or a single thread for the aggregate `0u` interface), owns the shutdown handle, and exposes the `mpsc` receiver the UI reads from
  - `reader.rs`: Blocking read loop over the usbmon `Nu` text interface, opened `O_NONBLOCK` and polled so it can be shut down promptly
  - `parser.rs`: Parses the usbmon `Nu` text-format lines into `UsbPacket`s

#### 2. Device Manager (`device/`)
- **Purpose**: USB device discovery and metadata management
- **Components**:
  - `mod.rs`: Device structure and lifecycle
  - `manager.rs`: Platform-specific device enumeration

#### 3. Statistics Engine (`stats/`)
- **Purpose**: Real-time bandwidth calculation and history
- **Features**:
  - Sliding window calculations
  - Peak tracking
  - Historical data management

#### 4. User Interface (`ui/`)
- **Purpose**: Terminal-based user interface
- **Components**:
  - `mod.rs`: App state, the packet-drain/refresh/draw event loop, and all widget rendering
  - `colors.rs`: Color scheme definitions

#### 5. Configuration (`config/`)
- **Purpose**: Settings management and persistence
- **Features**:
  - TOML-based configuration
  - Environment variable support
  - Runtime configuration updates

## Module Design

### USB Monitor Module

```rust
// High-level interface (see src/usbmon/reader.rs)
pub struct UsbmonReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool, // false only in tests, to make fixture reads terminate
}

impl UsbmonReader {
    pub fn read_packets<F>(&self, shutdown: &AtomicBool, callback: F) -> Result<()>
    where F: FnMut(UsbPacket) -> Result<()>;
}
```

**Design Decisions:**
- Each reader runs to completion on its own dedicated thread (spawned by `usbmon::monitor`), so a blocked or idle interface never blocks the UI thread
- Callback-based interface for flexible packet processing; the callback used in production forwards each `UsbPacket` over an `mpsc` channel
- Only the usbmon **text** interface (`Nu` files under `/sys/kernel/debug/usb/usbmon/`) is supported; the binary `/dev/usbmonN` character-device interface is not opened
- The file is opened `O_NONBLOCK` on Linux and polled every 50ms so `shutdown` can be observed promptly, instead of parking indefinitely inside `read()`
- Platform-specific path resolution

**Packet Flow:**
```
usbmon Nu file → Reader thread → Parser → UsbPacket → mpsc channel → DeviceManager
```

### Device Manager Module

```rust
pub struct UsbDevice {
    pub bus_id: u8,
    pub device_id: u8,
    pub speed: UsbSpeed,
    pub bandwidth_stats: BandwidthStats,
    pub is_disconnected: bool,
    // ... metadata fields
}
```

**Features:**
- Automatic device discovery via sysfs/udev
- Metadata extraction (vendor, product, speed)
- Disconnect detection and tracking
- Cross-platform device enumeration

### Statistics Engine

```rust
pub struct BandwidthStats {
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub current_bps: f64,
    pub peak_bps: f64,
    // ... historical data
}
```

**Algorithm:**
- Sliding window bandwidth calculation
- Exponential moving averages for smoothing
- Efficient circular buffer for history
- Real-time rate limiting

### User Interface

```rust
pub struct UsbTopApp {
    devices: HashMap<String, UsbDevice>,
    bandwidth_history: Vec<(f64, f64)>,
    selected_device: Option<String>,
    // ... UI state
}
```

**Architecture:**
- Event-driven UI updates
- Hierarchical layout system
- Color-coded device status
- Keyboard-based navigation

## Data Flow

### Primary Data Flow

```
1. USB Activity → usbmon kernel interface (debugfs `Nu` text file)
2. Reader thread → UsbmonReader::read_packets() (blocking, non-blocking-poll loop)
3. Raw text lines → UsbPacket parsing (usbmon/parser.rs)
4. UsbPacket → sent over an mpsc channel to the UI thread
5. UI thread → DeviceManager::apply_packet() aggregates into BandwidthStats,
   resolving new devices' metadata from sysfs by busnum/devnum
6. Per-tick refresh → UI rendering
```

### Event Processing

The UI thread owns a single loop (see `run_app` in `src/ui/mod.rs`): it drains
whatever the reader threads produced since the last pass, refreshes state on
a fixed tick, redraws, and polls for input — no async runtime is involved.

```rust
// Simplified event loop
loop {
    // Drain everything the reader threads produced since the last pass.
    while let Ok(packet) = packets.try_recv() {
        manager.apply_packet(&packet);
    }

    if app.last_update.elapsed() >= app.refresh_rate {
        for (bus_id, device_id) in manager.refresh() {
            app.remove_device(bus_id, device_id);
        }
        // ...sync manager's devices into app state, update history...
    }

    terminal.draw(|f| draw_ui(f, app))?;

    if app.handle_input()? {
        break; // 'q' or Esc
    }
}
```

### Memory Management

- **Bounded buffers**: Historical data limited to prevent memory growth
- **Device cleanup**: Automatic removal of stale devices
- **Packet pooling**: Reuse packet structures to reduce allocations
- **String interning**: Common strings (vendor names) are deduplicated

## Platform Abstraction

### Linux Implementation

```rust
#[cfg(target_os = "linux")]
mod linux {
    fn get_usbmon_path(bus_id: u8) -> String {
        format!("/sys/kernel/debug/usb/usbmon/{}u", bus_id)
    }
    
    fn enumerate_devices() -> Vec<UsbDevice> {
        // sysfs enumeration
    }
}
```

**Features:**
- Direct usbmon interface access
- sysfs device metadata extraction
- debugfs mount detection
- Module loading assistance

### BSD Implementation

```rust
#[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
mod bsd {
    fn enumerate_devices() -> Vec<UsbDevice> {
        // usbconfig/usbdevs integration
    }
}
```

**Features:**
- Native USB device enumeration
- Platform-specific monitoring interfaces
- Device permission handling

### macOS Implementation

```rust
#[cfg(target_os = "macos")]
mod macos {
    fn enumerate_devices() -> Vec<UsbDevice> {
        // IOKit/system_profiler integration
    }
}
```

**Limitations:**
- No real-time monitoring (no usbmon equivalent)
- Static device information only
- Limited bandwidth detection

## Performance Considerations

### Optimization Strategies

1. **Threaded I/O**: Dedicated blocking reader threads, decoupled from the UI thread via an `mpsc` channel
2. **Efficient Parsing**: Zero-copy packet parsing where possible
3. **Bounded Collections**: Prevent unbounded memory growth
4. **Lazy Evaluation**: Device metadata loaded on-demand
5. **Batch Processing**: Group UI updates to reduce flickering

### Memory Usage

- **Base overhead**: ~5-10 MB for core application
- **Per-device**: ~1-2 KB for metadata and statistics
- **History buffer**: ~100 bytes per data point
- **UI buffers**: ~500 KB for terminal rendering

### CPU Usage

- **Idle state**: <0.1% CPU usage
- **Active monitoring**: 0.5-2% depending on USB activity
- **UI updates**: Minimal overhead with 1Hz refresh rate
- **Packet processing**: ~1000 packets/second sustainable

### Scalability

- **Device limit**: 1000+ USB devices supported
- **Packet rate**: Up to 10,000 packets/second
- **History retention**: 60 seconds by default (configurable)
- **Memory ceiling**: ~50 MB maximum typical usage

## Security Model

### Privilege Requirements

usbtop-ng requires elevated privileges for USB monitoring:

**Linux:**
- Root access for `/sys/kernel/debug/usb/usbmon/` access
- Alternative: `plugdev` group membership (distribution-specific)

**BSD:**
- Root access for USB device enumeration
- Some BSDs allow user access to USB devices

**macOS:**
- Standard user access sufficient (limited functionality)

### Security Measures

1. **Minimal Privileges**: Drop privileges after initialization where possible
2. **Input Validation**: All user input and preference files validated
3. **Safe Parsing**: Robust packet parsing with bounds checking
4. **Error Handling**: Graceful degradation on permission errors
5. **No Network**: Local-only operation, no network communication

### Attack Surface

- **File System Access**: Limited to USB-related sysfs/debugfs paths
- **Kernel Interface**: Read-only access to usbmon interfaces
- **Configuration**: TOML parsing with safe defaults
- **Terminal**: Controlled terminal output through ratatui

## Error Handling

### Error Categories

1. **System Errors**: Permission denied, file not found
2. **Parsing Errors**: Malformed USB packets or configuration
3. **UI Errors**: Terminal size, color support issues
4. **Resource Errors**: Out of memory, too many open files

### Error Recovery

```rust
// Example error handling pattern: read_packets() runs the whole read loop on
// its thread. Per-line parse errors are logged and skipped inside the loop;
// only a fatal condition (interface gone, callback error) ends the loop.
match reader.read_packets(&shutdown, |packet| {
    tx.send(packet).map_err(|_| anyhow!("packet channel closed"))
}) {
    Ok(()) => debug!("usbmon reader for bus {bus} finished"),
    Err(e) => warn!("usbmon reader for bus {bus} stopped: {e}"),
}
```

### Logging Strategy

- **Error level**: Critical failures and security issues
- **Warn level**: Recoverable errors and degraded functionality
- **Info level**: Normal operations and state changes
- **Debug level**: Packet parsing and internal state
- **Trace level**: Detailed execution flow

## Extension Points

### Adding New Platforms

1. Implement `PlatformInterface` trait
2. Add platform-specific device enumeration
3. Update build configuration
4. Add platform-specific tests

### Custom Monitoring

1. Implement `MonitoringInterface` trait
2. Add custom packet sources
3. Integrate with existing statistics engine
4. Update UI to display custom metrics

### UI Customization

1. Extend `ui/widgets.rs` with new components
2. Add theme support in `ui/colors.rs`
3. Implement custom layout managers
4. Add configuration options

---

This architecture enables usbtop-ng to be:
- **Performant**: Minimal overhead USB monitoring
- **Reliable**: Robust error handling and recovery
- **Maintainable**: Clear module boundaries and interfaces
- **Extensible**: Easy to add new platforms and features
- **Secure**: Minimal attack surface and privilege requirements
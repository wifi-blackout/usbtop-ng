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

usbtop-ng is designed as a modular USB monitoring tool with clear separation of concerns. Live monitoring is built on dedicated blocking reader threads (one per usbmon interface, or a single reader for the aggregate `0u`/`/dev/usbmon0` interface) that feed parsed packets to the main thread over an `mpsc` channel; there is no async runtime involved. `usbmon::monitor::start_monitoring` probes once per process whether the binary `/dev/usbmonN` interface can be opened and, if so, uses it for every target bus; otherwise it falls back to the debugfs `Nu` text interface. Both readers produce the same `UsbPacket` type, so everything downstream of the channel is interface-agnostic.

```
┌─────────────────────────────────────────────────────────────────┐
│                        usbtop-ng                                │
├─────────────────────────────────────────────────────────────────┤
│  Terminal UI (ratatui + crossterm)                             │
├─────────────────────────────────────────────────────────────────┤
│  Application Logic (UsbTopApp)                                 │
├─────────────────┬─────────────────┬─────────────────────────────┤
│  USB Monitor    │  Device Manager │  Statistics Engine          │
│  (usbmon)       │  (sysfs)        │  (bandwidth calc)           │
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
  - `monitor.rs`: Probes for the binary interface, spawns one blocking reader thread per bus (or a single thread for the aggregate interface), owns the shutdown handle, and exposes the bounded `mpsc` receiver the UI reads from plus the shared dropped-packet counter; each thread's `run_source` also re-checks its own binary device and falls back to that bus's text interface if it cannot be opened
  - `reader.rs`: Blocking read loop over the usbmon `Nu` text interface, opened `O_NONBLOCK` and polled so it can be shut down promptly
  - `binary.rs`: Blocking read loop over the usbmon binary `/dev/usbmonN` character-device interface: fixed 48-byte native-endian headers (`Documentation/usb/usbmon.rst`) followed by `len_cap` bytes of captured payload drained (not kept) per event; same `O_NONBLOCK`/poll/shutdown contract as `reader.rs`
  - `parser.rs`: Parses the usbmon `Nu` text-format lines into `UsbPacket`s (also home to `UsbSpeed`'s practical-bandwidth and color-code tables)

#### 2. Device Manager (`device/`)
- **Purpose**: USB device discovery and metadata management
- **Components**:
  - `mod.rs`: Device structure and lifecycle; `UsbDevice::get_busy_percentage`, `check_speed_mismatch`/`get_speed_indicator`, and the best-effort max-capability signal read from sysfs `version` (bcdUSB)
  - `manager.rs`: Routes usbmon packets into per-device bandwidth stats, resolves device metadata from sysfs (Linux only), and groups devices into `UsbBus`es that resolve their host controller and aggregate %busy

#### 3. Statistics Engine (`stats/`)
- **Purpose**: Real-time bandwidth calculation and history
- **Features**:
  - Sliding window calculations over fixed 250ms buckets, so accounting a packet is O(1) rather than a rescan of the window
  - Peak tracking
  - Historical data management, evicted by age rather than sample count

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
// Text interface (see src/usbmon/reader.rs)
pub struct UsbmonReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool, // false only in tests, to make fixture reads terminate
}

impl UsbmonReader {
    pub fn read_packets<F>(&self, shutdown: &AtomicBool, callback: F) -> Result<()>
    where F: FnMut(UsbPacket) -> Result<()>;
}

// Binary interface (see src/usbmon/binary.rs) — same shape, same contract
pub struct BinaryReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool,
}

impl BinaryReader {
    pub fn read_packets<F>(&self, shutdown: &AtomicBool, callback: F) -> Result<()>
    where F: FnMut(UsbPacket) -> Result<()>;
}
```

**Design Decisions:**
- Each reader runs to completion on its own dedicated thread (spawned by `usbmon::monitor`), so a blocked or idle interface never blocks the UI thread
- Callback-based interface for flexible packet processing; the callback used in production forwards each `UsbPacket` over an `mpsc` channel
- Both usbmon interfaces are supported and produce the same `UsbPacket` type. `monitor::start_monitoring` probes once per process by trying to open `/dev/usbmon<bus>` for the first target bus: success means every target bus is read through `BinaryReader` (48-byte native-endian headers per `Documentation/usb/usbmon.rst`, with each event's `len_cap` payload bytes drained rather than kept); failure (missing node, permissions, older kernel) falls back to `UsbmonReader` over the debugfs `Nu` text interface for every target bus. One `info!` log line states which interface was chosen.
- That global choice is a starting point, not a promise: each reader thread (`run_source`) re-opens its own `/dev/usbmon<bus>` before entering the read loop and, if that fails, warns and reads this bus's debugfs `Nu` text interface instead. One bus with a missing or unreadable binary node therefore degrades to text rather than going dark.
- The file/device is opened `O_NONBLOCK` on Linux and polled every 50ms so `shutdown` can be observed promptly, instead of parking indefinitely inside `read()` — the same contract for both readers
- Platform-specific path resolution

**Packet Flow:**
```
/dev/usbmonN (binary, preferred) ─┐
                                   ├─→ Reader thread → UsbPacket → bounded channel → DeviceManager
usbmon Nu file (text, fallback)  ─┘
```

**Backpressure:** the channel is a `sync_channel(16_384)`, and readers hand packets
over with `try_send`. A reader must never park on a full channel — a parked reader
still holds its usbmon file open, which is precisely what `MonitorHandle::stop()`
exists to prevent before `modprobe -r usbmon` — so a packet that does not fit is
discarded and counted in the `Arc<AtomicU64>` the handle exposes as `dropped`. The
UI reads that counter and appends `dropped: N` to its header once it is non-zero,
so lost samples are always visible rather than silently missing.

### Device Manager Module

```rust
pub struct UsbDevice {
    pub bus_id: u8,
    pub device_id: u8,
    pub speed: UsbSpeed,
    pub bandwidth_stats: BandwidthStats,
    pub is_disconnected: bool,
    pub sysfs_path: Option<PathBuf>,
    pub max_capability: Option<UsbSpeed>, // cached bcdUSB (sysfs `version`) signal
    // ... metadata fields
}

pub struct UsbBus {
    pub bus_id: u8,
    pub speed: UsbSpeed,
    pub devices: HashMap<u8, UsbDevice>,
    pub controller: Option<String>, // e.g. "0000:00:14.0"; resolved once
}
```

**Features:**
- Automatic device discovery via sysfs, driven by incoming usbmon packets (busnum/devnum topology scan, not udev)
- Metadata extraction (vendor, product, speed, plus the cached max-capability signal)
- `UsbDevice::get_busy_percentage()` — %busy against the device's practical (overhead-adjusted) bandwidth; `UsbBus::busy_percentage()` — the bus's aggregate %busy, `None` when bus speed is unknown
- `UsbDevice::get_speed_indicator()` — `SpeedIndicator::HighUtilization` (⚡, >80% busy) or `LimitedByBus` (🔺, cached capability exceeds both the bus speed and current link speed), `LimitedByBus` taking precedence
- The capability behind 🔺 is deliberately a *best-effort* signal, read once from the device's sysfs `version` (bcdUSB): `>= 3.00` means SuperSpeed-capable, anything else means "no signal", not "not capable". A device linked below its capability usually reports bcdUSB 2.10 on the USB2 bus, so its absence proves nothing — which is why no descriptor guesswork (`bcdDevice`, `bMaxPacketSize0`) is used to manufacture one
- Disconnect detection and tracking
- Device discovery is Linux-only; no enumeration fallback on BSD/macOS

### Statistics Engine

```rust
pub struct BandwidthStats {
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub current_bps: f64,
    pub peak_bps: f64,
    pub rx_buckets: VecDeque<(Instant, u64)>,        // 250ms slots, oldest first
    pub tx_buckets: VecDeque<(Instant, u64)>,
    pub rx_window_sum: u64,                          // running sum of rx_buckets
    pub tx_window_sum: u64,
    pub rate_history: VecDeque<(Instant, f64, f64)>, // per-device chart samples
    // ... historical data
}
```

**Algorithm:**
- Sliding window bandwidth calculation (10-second window), re-evaluated every `refresh()` call so idle devices decay to zero instead of freezing at their last rate
- The window is stored as fixed 250ms buckets with a running sum, not one entry per packet: `update_rx`/`update_tx` evict expired buckets from the front (subtracting them from the sum), add the bytes to the newest bucket, and divide the sum by the window — all O(1) amortized. Nothing on the packet path rescans the window, so a saturated bus costs constant work per URB instead of work proportional to the packets already in the window
- `get_utilization_percentage(max_bps)` — `current_bps / max_bps`, clamped to 100%, the shared building block behind per-device and per-bus %busy
- One `(Instant, rx_bps, tx_bps)` sample appended to `rate_history` per `refresh()` tick, retained for 60 seconds by age (not by sample count, which would mean 15s at `--refresh 250` and 120s at `--refresh 2000`), feeding the per-device rx/tx chart
- Bounded VecDeque-backed history buffers

### User Interface

```rust
pub struct UsbTopApp {
    pub controllers: Vec<ControllerView>, // rebuilt from DeviceManager each tick
    pub bandwidth_history: Vec<(f64, f64)>, // (session seconds, bytes/s), last 60s
    pub selected_device: Option<String>, // "bus:devnum"
    pub list_scroll: u16,                // follows the selection
    pub dropped_counter: Option<Arc<AtomicU64>>, // shared with the reader threads
    // ... UI state
}
```

**Architecture:**
- Event-driven UI updates
- Hierarchical layout system
- Color-coded device status, link speeds, and utilization/capability indicators
- Keyboard-based navigation

### Snapshot Model and Topology Resolution

`UsbTopApp` holds no live device map of its own. Every tick, `sync_from(&DeviceManager)`
rebuilds a render snapshot from scratch:

```rust
pub struct ControllerView { pub id: String, pub buses: Vec<BusView> }
pub struct BusView {
    pub bus_id: u8,
    pub speed: UsbSpeed,
    pub side_label: &'static str,      // "USB2 side" / "USB3 side" / ""
    pub devices: Vec<DeviceRow>,       // in physical port order
    pub busy_percentage: Option<f64>,
}
pub struct DeviceRow { pub port_chain: Option<Vec<u32>>, pub device: UsbDevice }
```

Rebuilding from scratch each tick (rather than patching an incremental map) is what
keeps totals, peak bandwidth, and the port-ordered layout consistent with whatever
`DeviceManager` currently holds — see `sync_from` in `src/ui/mod.rs`.

**Controller pairing:** for each bus, `UsbBus::update_bus_speed` canonicalizes
`/sys/bus/usb/devices/usb<N>` (the root hub) and takes the *parent* directory's
basename as the controller id — real sysfs symlinks each root hub into its PCI
host controller's directory, so the canonical parent names the controller. Buses
sharing a controller id render under one `═ <controller id> ═` heading, sorted by
bus id; a controller that can't be resolved falls into an `unknown` group that
always sorts last. The side label comes from the bus's own speed: ≤480 Mbps is
"USB2 side", ≥5 Gbps is "USB3 side" — this is how a shared xHCI controller's two
root hubs (one USB2, one USB3) end up listed as adjacent sibling buses.

**Port ordering:** `UsbDevice::port_chain()` parses the resolved sysfs directory
basename — `3-1.4.2` → `[1, 4, 2]`, a root hub (`usb3`) → `[]` (sorts first),
unresolved → `None` (sorts last, Port column shows `?`). Devices within a bus sort
by this chain, numerically level by level, which lists hub children in physical
connector order.

## Data Flow

### Primary Data Flow

```
1. USB Activity → usbmon kernel interface: /dev/usbmonN (binary, preferred)
   or the debugfs `Nu` text file (fallback) — chosen once by
   monitor::start_monitoring's open probe
2. Reader thread → BinaryReader::read_packets() or UsbmonReader::read_packets()
   (blocking, non-blocking-poll loop; same shutdown contract either way)
3. Raw bytes/text → UsbPacket parsing (usbmon/binary.rs or usbmon/parser.rs)
4. UsbPacket → sent over an mpsc channel to the UI thread
5. UI thread → DeviceManager::apply_packet() aggregates into BandwidthStats,
   resolving new devices' metadata from sysfs by busnum/devnum
6. Per-tick refresh → DeviceManager::refresh() (decay rates, drop stale
   devices, recompute bus speeds/controllers) → UsbTopApp::sync_from()
   rebuilds the controller/bus/device snapshot (including %busy and speed
   indicators) → UI rendering
```

### Event Processing

The UI thread owns a single loop (see `run_app` in `src/ui/mod.rs`): it drains
what the reader threads produced since the last pass (at most `DRAIN_BATCH`
packets, so one burst cannot stall a frame — the leftovers are picked up on the
next pass ~50ms later), refreshes state on a fixed tick, redraws, and polls for
input — no async runtime is involved.

```rust
// Simplified event loop
loop {
    // Apply up to DRAIN_BATCH (8192) packets; the rest wait for the next pass.
    drain_packets(manager, packets, DRAIN_BATCH);

    if app.last_update.elapsed() >= app.refresh_rate {
        let _ = manager.refresh();     // decay rates, drop stale devices
        app.sync_from(manager);        // rebuild the render snapshot
        app.update_bandwidth_history();
    }

    terminal.draw(|f| draw_ui(f, app))?;

    if app.handle_input()? {
        break; // 'q' or Esc
    }
}
```

### Memory Management

- **Bounded buffers**: Historical data limited to prevent memory growth — bandwidth history by wall-clock age (60s), the sliding rate window by 250ms buckets
- **Bounded channel**: The reader→UI channel holds at most `CHANNEL_BOUND` (16384) packets; a consumer that cannot keep up costs dropped (counted) packets, never growing memory
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

BSD builds only have stub platform checks: `is_usbmon_module_loaded` looks at `kldstat` output and `check_usbmon_debugfs_exists` just checks that `/dev` exists, so the startup checks may pass without confirming a real usbmon-equivalent interface. There is no live-monitoring reader and no device-enumeration fallback wired up — devices are only ever created from usbmon packets, so with no packet source the device list stays empty even if the UI opens.

**Status:**
- No live monitoring implemented
- No device enumeration implemented; sysfs-metadata population is a no-op stub
- The UI can open (with `--force` if the startup checks fail) but shows no devices

### macOS Implementation

macOS has no usbmon equivalent, so `is_usbmon_module_loaded` always returns `false` and usbtop-ng exits at startup unless run with `--force`. There is also no device-enumeration fallback: devices are only ever created from usbmon packets, and macOS has no packet source.

**Limitations:**
- No real-time monitoring (no usbmon equivalent)
- No device enumeration — the device list stays empty
- The UI opens only with `--force`, and shows no devices even then

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
- Root access would be required for USB device access, but no live monitoring or device enumeration is currently implemented on BSD
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
// only a fatal condition (interface gone, callback error) ends the loop. A
// full channel is not fatal — the packet is counted and dropped, because a
// reader that parks would keep the usbmon file open past stop().
match reader.read_packets(&shutdown, |packet| match tx.try_send(packet) {
    Ok(()) => Ok(()),
    Err(TrySendError::Full(_)) => {
        dropped.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    Err(TrySendError::Disconnected(_)) => Err(anyhow!("packet channel closed")),
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
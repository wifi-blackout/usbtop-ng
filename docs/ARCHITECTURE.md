# usbtop-ng Architecture

This document describes usbtop-ng's modules, its data flow, and the decisions
behind them.

## Table of contents

- [Overview](#overview)
- [System architecture](#system-architecture)
- [Module design](#module-design)
- [TUI chassis](#tui-chassis)
- [Data flow](#data-flow)
- [Linux integration](#linux-integration)
- [Performance](#performance)
- [Security model](#security-model)
- [Error handling](#error-handling)
- [Extension points](#extension-points)

## Overview

usbtop-ng splits into modules with one job each. Live monitoring runs on
dedicated blocking reader threads. One thread reads each usbmon interface, or a
single thread reads the aggregate `0u` and `/dev/usbmon0` interface. The
threads hand parsed packets to the main thread over an `mpsc` channel. No async
runtime is involved.

`usbmon::monitor::start_monitoring` probes once per process whether it can open
the binary interface. On success it uses the binary interface for every target
bus. On failure it falls back to the text interface. Both readers produce the
same `UsbPacket` type, so everything downstream of the channel treats the two
interfaces alike.

```
┌─────────────────────────────────────────────────────────────────┐
│                        usbtop-ng                                │
├─────────────────────────────────────────────────────────────────┤
│  TUI chassis (src/tui): loop, output, restore                   │
├─────────────────────────────────────────────────────────────────┤
│  Terminal UI (ratatui + crossterm)                              │
├─────────────────────────────────────────────────────────────────┤
│  Application state (UsbTopApp)                                  │
├─────────────────┬─────────────────┬─────────────────────────────┤
│  USB Monitor    │  Device Manager │  Statistics Engine          │
│  (usbmon)       │  (sysfs)        │  (bandwidth calc)           │
├─────────────────┴─────────────────┴─────────────────────────────┤
│  Operating System (Linux)                                       │
└─────────────────────────────────────────────────────────────────┘
```

## System architecture

### Core principles

1. **Thread-based I/O.** Dedicated blocking reader threads read usbmon and pass
   packets over an `mpsc` channel. A stalled or idle interface never blocks
   rendering.
2. **Module boundaries.** Each module exposes a small interface and hides its
   internals.
3. **Linux only.** `src/main.rs` carries a `compile_error!` for every other
   target.
4. **Memory safety.** Rust's ownership rules prevent the common errors.
5. **Bounded work.** Every queue, window, and history has a limit.
6. **Keyboard-driven UI.** Seven keys cover the whole interface: `↑`, `↓`,
   `h`, `Ctrl-L`, `q`, `Esc`, and `Ctrl-C`.

### Key components

#### 1. USB monitor (`usbmon/`)

- `mod.rs`: usbmon detection, load and unload, and the printed setup
  instructions.
- `monitor.rs`: probes for the binary interface and spawns the reader threads,
  one per bus, or one for the aggregate interface. It owns the shutdown handle.
  It exposes the bounded `mpsc` receiver and the shared drop counter. Each
  thread's `run_source` re-checks its own binary node, and falls back to that
  bus's text interface when it cannot open it.
- `reader.rs`: the blocking read loop over the text interface. The file opens
  `O_NONBLOCK` and polls, so a shutdown request lands promptly.
- `binary.rs`: the blocking read loop over the binary interface. Each event is
  a fixed 48 byte native-endian header, per `Documentation/usb/usbmon.rst`,
  followed by `len_cap` bytes of captured payload. The reader drains that
  payload rather than keeping it, because the next header starts right after
  it. It honors the same `O_NONBLOCK`, poll, and shutdown contract as
  `reader.rs`.
- `parser.rs`: parses the text interface's `Nu` lines into `UsbPacket`s. It
  also holds `UsbSpeed`'s practical-bandwidth and color tables.

#### 2. Device manager (`device/`)

- `mod.rs`: the device structure and its lifecycle. It holds
  `UsbDevice::get_busy_percentage`, `check_speed_mismatch`,
  `get_speed_indicator`, and the best-effort capability signal read from sysfs
  `version` (bcdUSB).
- `manager.rs`: routes usbmon packets into per-device bandwidth stats, and
  resolves device metadata from sysfs. When a usb.ids database is set
  (`set_usbids`), it overlays `UsbDevice::apply_usbids` on every newly
  populated device, so a resolved name wins over the sysfs string per field.
  It groups devices into `UsbBus`es, which resolve their host controller and
  their aggregate %busy.

#### 3. Statistics engine (`stats/`)

- A sliding window over fixed 250 millisecond buckets, so accounting a packet
  costs O(1) rather than a rescan of the window.
- Peak tracking.
- History buffers evicted by age rather than by sample count.

#### 4. User interface (`ui/`)

- `mod.rs`: app state (`UsbTopApp`), the per-interval render snapshot, key
  handling (`apply_key`), the packet drain (`drain_packets`), and every widget.
- `colors.rs`: the color scheme.

#### 5. TUI chassis (`tui/`)

Everything between the app state and the terminal device: when to draw, how the
bytes get out, and how the terminal is handed back.

- `mod.rs`: terminal setup and teardown, plus the deadline-driven event loop
  (`run_app`).
- `events.rs`: the input thread, the `UiEvent` type every wake source arrives
  as, and the redraw scheduling arithmetic.
- `output.rs`: `ShedWriter`, the non-blocking output stage with backpressure
  shedding.
- `sync.rs`: the mode-2026 handshake and the policy for when to run it.
- `lifecycle.rs`: terminal restore, the panic hook, the signal thread, and what
  an exit path may still ask the user.

See [TUI chassis](#tui-chassis) for how these fit together.

#### 6. Configuration (`config/`)

- Two boolean preferences in TOML, read from
  `~/.usbtop-ng/preferences.toml` or from the path given to `--config`.
- The file is created with both keys set to `false` when it does not exist.
- `ensure_private_config_dir` creates the default `~/.usbtop-ng` directory with
  mode 0700. An existing directory keeps its own mode, and so does a custom
  `--config` parent.
- `HOME` locates the default path. usbtop-ng fails with a message naming `HOME`
  when it is unset.

## Module design

### USB monitor module

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

// Binary interface (see src/usbmon/binary.rs). Same shape, same contract.
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

**Design decisions:**

- Each reader runs to completion on its own thread, spawned by
  `usbmon::monitor`, so a blocked or idle interface never blocks the UI thread.
- The callback interface keeps packet handling flexible. The production
  callback forwards each `UsbPacket` over an `mpsc` channel.
- Both usbmon interfaces produce the same `UsbPacket` type.
  `monitor::start_monitoring` probes once per process by opening the first
  target bus's binary node, `/dev/usbmon<bus>`. Success means every target bus
  is read through `BinaryReader`. Failure, from a missing node, from
  permissions, or from an older kernel, falls back to `UsbmonReader` over the
  text interface for every target bus. One `info!` line states the choice.
- That process-wide choice is a starting point rather than a promise. Each
  reader thread re-opens its own binary node before it enters the read
  loop. If that open fails, the thread warns and reads this bus's text
  interface instead. One bus with a missing or unreadable binary node therefore
  degrades to text rather than going dark.
- Both readers open their file `O_NONBLOCK` and poll every 50 milliseconds, so
  `shutdown` is observed within one poll instead of parking inside `read()`.

**Packet flow:**

```
/dev/usbmonN (binary, preferred) ─┐
                                  ├─→ Reader thread → UsbPacket
usbmon Nu file (text, fallback)  ─┘         → bounded channel → DeviceManager
```

**Backpressure:** the channel is a `sync_channel(16_384)`, and readers hand
packets over with `try_send`. A reader must never park on a full channel. A
parked reader still holds its usbmon file open, which is exactly what
`MonitorHandle::stop()` exists to prevent before `modprobe -r usbmon` runs. A
packet that does not fit is discarded and counted in the `Arc<AtomicU64>` the
handle exposes as `dropped`. The UI reads that counter and appends `dropped: N`
to the header once it is non-zero, so lost samples stay visible.

### Device manager module

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
    pub controller: Option<String>, // e.g. "0000:00:14.0", resolved once
}
```

**Features:**

- Device discovery through sysfs, driven by incoming usbmon packets. The
  manager scans by busnum and devnum rather than listening to udev.
- Metadata extraction: vendor, product, speed, and the cached capability
  signal.
- `UsbDevice::get_busy_percentage()` reports %busy against the device's
  practical, overhead-adjusted bandwidth. `UsbBus::busy_percentage()` reports
  the bus aggregate, and returns `None` when the bus speed is unknown.
- `UsbDevice::get_speed_indicator()` returns
  `SpeedIndicator::HighUtilization` (⚡, above 80% busy) or `LimitedByBus`
  (🔺, cached capability above both the bus speed and the current link speed).
  `LimitedByBus` takes precedence.
- The capability behind 🔺 is a best-effort signal, read once from the device's
  sysfs `version` (bcdUSB). A value of 3.00 or higher means SuperSpeed-capable.
  Anything else means no signal rather than no capability. A device linked
  below its capability usually reports bcdUSB 2.10 on the USB2 bus, so the
  absence of 🔺 proves nothing. That is why no descriptor guesswork
  (`bcdDevice`, `bMaxPacketSize0`) manufactures one.
- Disconnect detection and tracking, with removal 5 seconds after the
  disconnect.

### Statistics engine

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

- A 10 second sliding window, re-evaluated on every `refresh()` call, so idle
  devices decay to zero instead of freezing at their last rate.
- The window holds fixed 250 millisecond buckets and a running sum rather than
  one entry per packet. `update_rx` and `update_tx` evict expired buckets from
  the front and subtract them from the sum. They then add the bytes to the
  newest bucket and divide the sum by the window. All of that is O(1)
  amortized. Nothing on the packet path rescans the window, so a saturated bus
  costs constant work per URB.
- `get_utilization_percentage(max_bps)` returns `current_bps / max_bps`,
  clamped to 100%. Per-device and per-bus %busy both build on it.
- Each `refresh()` appends one `(Instant, rx_bps, tx_bps)` sample to
  `rate_history`, retained for 60 seconds by age. A sample count would mean 15
  seconds at `--refresh 250` and 120 seconds at `--refresh 2000`.
- Every history buffer is a bounded `VecDeque`.

### User interface

```rust
pub struct UsbTopApp {
    pub controllers: Vec<ControllerView>, // rebuilt from DeviceManager each interval
    pub bandwidth_history: Vec<(f64, f64)>, // (session seconds, bytes/s), last 60s
    pub selected_device: Option<String>, // "bus:devnum"
    pub list_scroll: u16,                // follows the selection
    pub dropped_counter: Option<Arc<AtomicU64>>, // shared with the reader threads
    pub shed_counter: Option<Arc<AtomicU64>>,    // shared with the output stage
    // ... UI state
}
```

**Architecture:**

- The UI redraws on events and on the refresh interval, never on a poll.
- The layout nests: header, chart pane, device table, controls bar.
- Color marks device status, link speeds, and the utilization and capability
  indicators.
- Navigation is keyboard-only.

### Snapshot model and topology resolution

`UsbTopApp` holds no live device map of its own. Every refresh interval,
`sync_from(&DeviceManager)` rebuilds a render snapshot from scratch:

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

Rebuilding from scratch, rather than patching an incremental map, keeps totals,
peak bandwidth, and the port-ordered layout consistent with whatever
`DeviceManager` currently holds. See `sync_from` in `src/ui/mod.rs`.

**Controller pairing:** for each bus, `UsbBus::update_bus_speed` canonicalizes
`/sys/bus/usb/devices/usb<N>`, the root hub, and takes the parent directory's
basename as the controller id. Real sysfs symlinks each root hub into its PCI
host controller's directory, so the canonical parent names the controller.
Buses that share a controller id render under one `═ <controller id> ═`
heading, sorted by bus id. A controller that does not resolve falls into an
`unknown` group, which always sorts last.

The side label comes from the bus's own speed. A bus at 480 Mbps or below takes
"USB2 side", and a faster bus takes "USB3 side". An unknown bus speed gets no
label.
That is how a shared xHCI controller's two root hubs list as adjacent sibling
buses.

**Port ordering:** `UsbDevice::port_chain()` parses the resolved sysfs
directory basename. `3-1.4.2` becomes `[1, 4, 2]`. A root hub (`usb3`) becomes
`[]` and sorts first. An unresolved device becomes `None`, sorts last, and
shows `?` in the Port column. Devices within a bus sort by this chain,
numerically level by level, which lists hub children in physical connector
order.

## TUI chassis

`src/tui/` holds everything between the app state and the terminal device. It
exists because the interesting failures of a monitoring tool are not USB
failures. They are a terminal that stopped reading, a link that went away
mid-frame, and a panic with the alternate screen still up. Two rules run
through the design. The loop must never block on the display, and it must never
claim to show something it is not showing.

### Threads and wake sources

```
  input thread (detached)         signal thread (detached)
  parks in event::read()          parks in signal-hook's iterator
  owns stdin for the              SIGHUP / SIGINT / SIGTERM
  life of the process                      │
          │ UiEvent::Input                 │ UiEvent::Signal(n)
          │ UiEvent::TerminalDead          │
          └──────────────┬─────────────────┘
                         ▼
                mpsc::channel<UiEvent>          sync_channel<UsbPacket>(16384)
                         │                        (usbmon reader threads)
                         ▼                                  │
        ┌────────────────────────────────────┐              │
        │  deadline loop  (tui::run_app)     │◀─ drained ───┘
        │                                    │   every pass, ≤8192/pass
        │  recv_timeout(next_wait) ──▶ fold  │
        │  drain packets ──▶ tick? ──▶ draw? │
        └───────────────┬────────────────────┘
                        │ ratatui frame
                        ▼
             ShedWriter ──▶ stdout (O_NONBLOCK)
```

The loop does not poll. Every pass it sleeps until the earliest instant at
which it owes something. That is the next refresh interval, or, when the screen
is dirty, one frame interval after the last frame. It folds whatever arrives into
a single repaint. A burst of fifty resize events costs one frame rather than
fifty. A session where nothing changed costs no frames at all between refresh
intervals.

The one thing that cannot wake the loop is a packet. The reader threads push
onto their own bounded channel and discard when it fills, with nothing to
signal on. So the wait is capped at `PACKET_DRAIN_INTERVAL`, a bare wake that
drains the channel without touching the dirty flag or the frame cap.

The numbers, all exercised by tests:

| Constant | Value | What it governs |
| --- | --- | --- |
| `events::MIN_FRAME_INTERVAL` | 33ms | Shortest gap between two frames (~30 FPS) |
| `events::PACKET_DRAIN_INTERVAL` | 50ms | Longest the loop may sleep, so packets drain |
| `tui::REFRESH_FLOOR_MS` | 100ms | Floor for `--refresh` |
| `ui::DRAIN_BATCH` | 8192 | Most packets applied in one pass |
| `output::WATERMARK_FLOOR` | 4096 bytes | Smallest output backlog allowed before shedding |
| `output::SHED_GRACE` | 1s | How long after a shed the writer will not shed again |
| `output::MAX_CONSECUTIVE_WRITE_FAILURES` | 30 | Unclassified write failures in a row that mean death |
| `sync::PROBE_TIMEOUT` | 100ms | Longest the mode-2026 handshake waits for a reply |
| `lifecycle` restore budget | 250ms | Longest teardown spends on a terminal that will not read |
| `lifecycle::PROMPT_TIMEOUT` | 60s | Longest an exit question waits for an answer |
| `usbmon::POLL_INTERVAL` | 50ms | Longest a reader waits before it re-checks shutdown |
| `monitor::CHANNEL_BOUND` | 16384 | Packets the reader-to-UI channel holds |

### Output stage: `ShedWriter`

A terminal is a pipe, and a pipe fills up. Writing to a blocking stdout stops
the render loop dead whenever the far end stops reading. That end may be a
scrolled-back tmux pane, a laggy ssh link, or a suspended emulator. A stopped
render loop is also a stopped input loop. `ShedWriter` takes the descriptor non-blocking and absorbs
the difference:

```
  ratatui write()  ──▶ staging buffer (bytes, no I/O yet)
  ratatui flush()  ──▶ stage_frame:  staging becomes ONE queue entry
                       (bracketed with ESC[?2026h / ESC[?2026l here, if
                        the terminal said it supports synchronized output,
                        so begin+diff+end are indivisible)
                   ──▶ shed check:   backlog > watermark and not in grace?
                                     drop every queued frame that has put
                                     no bytes on the wire, count them,
                                     ask the loop for a full repaint,
                                     start a 1s grace period
                   ──▶ drain:        non-blocking write(2) from the head of
                                     the queue as far as it will go
```

Frame granularity makes truncation mid-escape-sequence impossible. A queue
entry is a whole frame, so a shed drops whole frames and a partial write
resumes inside one. The watermark is tmux's rule, `1 + cols * rows * 8`, which
is roughly two full repaints' worth of escapes. A 4096-byte floor sits under
it. A terminal reporting 0x0 mid-resize, or 1x1 for a pane dragged shut, would
otherwise get a backlog allowance smaller than one cursor move. It would then
shed every frame it ever staged.

What the drain does with each outcome:

| From `write(2)` | Meaning | Response |
| --- | --- | --- |
| `Ok(n)` | Progress | Advance the cursor, forget any earlier failures |
| `WouldBlock` | Terminal is full | Stop. The next flush resumes at the cursor |
| `Interrupted` | A signal landed mid-write | Retry immediately |
| `EPIPE` / `EIO` | The terminal is gone | Set `terminal_dead`, and the loop leaves |
| anything else | This write failed | Set `invalidated`, drop the frame. After 30 in a row with nothing landing in between, set `terminal_dead` |

Nothing here reports failure through `io::Write`. Returning an error to ratatui
mid-frame would take its teardown off the healthy path, for something the loop
can handle better a few microseconds later. So `write` and `flush` are
infallible, and the shared atomics behind `ShedHandles` carry the signal.

The loop reads those atomics after every pass. `terminal_dead` ends the
session. `take_repaint_request`, set by a shed or by a failed write, forces a
full repaint. In both cases the screen no longer matches ratatui's mirror of
it.

That full repaint is deliberately not `Terminal::clear`, which snapshots the
cursor position first. That snapshot writes `ESC[6n` and waits for the terminal
to answer on stdin, and both halves fail exactly when a repaint is most needed.
`force_full_repaint` calls `Terminal::resize` to the size already in force
instead. That clears the screen and resets the diff's baseline while asking the
terminal nothing.

### Synchronized output

`ESC[?2026h` and `ESC[?2026l` tell a terminal to stop presenting until the
frame is whole. That is the difference between a clean update and a visibly
half-drawn table over a slow link. Emitting it blind is not safe, because a
terminal that mishandles it leaves the user looking at a screen that has
stopped updating. So `sync::probe_sync_mode` asks first, with DECRQM
(`ESC[?2026$p`) followed by DA1 (`ESC[c`).

DA1 is what makes the query cheap. DECRQM has no negative answer, so without a
marker every terminal that does not know the mode would cost the full timeout.
Reading up to the DA1 reply also keeps the reply bytes out of the input
thread's keystrokes.

The handshake runs in the one window where all three of its preconditions hold.
Raw mode is on, stdout is still blocking, and the input thread has not been
spawned.

usbtop-ng does not probe a remote session, which it detects from `SSH_TTY`,
`SSH_CONNECTION`, or `SSH_CLIENT`. The one exception is a session whose `TERM`
sits on a known-good list, and that list ships empty. No synchronized output
over ssh is today's policy rather than an oversight. `sudo`'s default
`env_reset` strips all three variables, so `sudo usbtop-ng` over ssh does get
probed. That costs at most one `PROBE_TIMEOUT`, and `sudo -E` restores the
conservative posture.

### Lifecycle hooks

`lifecycle::arm_restore` runs immediately after `enable_raw_mode`, before
anything else can fail, and saves stdout's pre-TUI file-status flags. From then
on `restore_terminal` has something to undo, and it is idempotent. Teardown and
the panic hook both race for it, and only the first does any work. The restore
leaves raw mode, closes any open synchronized update, leaves the alternate
screen, and shows the cursor.

It puts the original descriptor flags back last, after the escape sequences
have gone out on a bounded non-blocking retry. The order matters. Restoring the
flags first would mean writing the restore into a blocking descriptor. A
terminal that has stopped reading would then hold the process there forever.
That path only became reachable once sessions started surviving a wedged
terminal all the way to exit.

For the same reason, ratatui's `Terminal` is dropped before the restore. Its
destructor shows the cursor again, which is a write through `ShedWriter`. Done
while the descriptor is still non-blocking, that write either goes out or is
shed, and it cannot fail. That keeps ratatui's "failed to show the cursor"
branch unreachable, which matters because the branch calls `eprintln!` and so
would panic inside a destructor.

That ordering is only available to the ordinary exit path. So the restore also
trips an abandon latch before it touches the flags. That latch is
`ShedHandles::abandon_latch`, registered with `lifecycle::arm_output_latch`.

Nothing can be reordered on the panic path. The hook runs, and unwinding drops
the `Terminal` afterwards. By then the descriptor is blocking again, and the
destructor's write would be unbounded against a terminal that may have stopped
reading. With the latch tripped, `ShedWriter::flush_at` drops whatever is
staged or queued and writes nothing.

The same reasoning decides the exit question. If the restore's own bytes did
not land inside the budget, the terminal is not reading. `prompt_via_events`
then declines rather than writing a question into it. So does
`usbmon::offer_unload_after_session`, whose automatic-unload notice is the last
stdout write on the `SIGHUP` plus `unload_usbmon_on_exit` path. The unload
still runs, and only the sentence about it is dropped.

Both directions were verified on a pty. With the latch, the panicking process
leaves in 0.26s. Without it, it stops forever inside ratatui's destructor. With
the notice gated, the hangup exit leaves in 0.26s having attempted the unload.
Without the gate, it stops forever in `write(1, …)`.

A `signal-hook` thread turns signals into ordinary `UiEvent`s. A `SIGTERM`
therefore leaves through the same teardown as `q`, instead of dropping the
process on an alternate screen. Raw mode disables ISIG, so a `^C` typed at the
UI never becomes a `SIGINT` at all. It arrives as a key event, and it is bound
to quit.

What an exit path may still ask the user is `lifecycle::unload_policy`'s
decision, and it turns on who is left to answer. A hangup or a dead terminal
unloads usbmon only if preferences already said to. Prompting there would park
the process forever on an answer nobody can type, holding usbmon loaded and the
reader files open. A prompt that does reach a user waits at most 60 seconds,
then takes the answer that changes nothing.

### Known limitations

- **Signal handlers stay registered for the life of the process.** The signal
  thread parks in `signal-hook`'s iterator and is never torn down. After the
  TUI exits, a `SIGINT` still goes into a channel nobody reads. The visible
  consequence is that `Ctrl-C` cannot interrupt the post-session
  `sudo modprobe -r usbmon`. Deregistering would mean racing the thread that is
  parked inside the iterator, which is a worse trade than the one being made.
- **The input thread is detached and never joined.** A blocked read on a tty
  cannot be cancelled portably, because there is no cross-platform cancel and
  no timeout on `read` itself. The thread therefore lives until the process
  exits. Two consequences carry weight. It owns stdin after the TUI comes up,
  which is why
  post-exit prompts route through the `UiEvent` channel (`UiSession::confirm`)
  rather than reading stdin directly. And the loop's receiver stays alive past
  teardown so those keystrokes still have somewhere to land.
- **Shedding is unix-only.** Off unix there is no `fcntl`, so `StdoutRaw` is an
  ordinary blocking stdout. Nothing can tell that the terminal is behind,
  because a blocking write returns only once the bytes are gone. That is the
  pre-chassis behavior, kept as it was.
- **The bound is drawn at stdout, and stops there.** The rule is mechanical,
  which is what makes it checkable. usbtop-ng manages fd 1, the TUI's channel.
  It never touches fd 2, which carries diagnostics.

  Every write usbtop-ng makes to stdout after teardown is bounded or skipped.
  `write_within_budget` bounds the restore sequences, and the abandon latch
  stops the render pipeline. `restore_landed` gates the two remaining
  exit-flow writes, which are `prompt_via_events`'s question and
  `usbmon::announce_automatic_unload`'s notice.

  Nothing bounds stderr, on purpose. A panic's message and backtrace come from
  std's default hook writing there, and `log::info!` and `log::warn!` go there
  through `env_logger`. Making that descriptor non-blocking would truncate
  exactly the messages worth having. `O_NONBLOCK` on a shared descriptor also
  outlives the process onto the shell, which is the hazard `save_output_flags`
  exists to prevent. So on a terminal that is still open but has stopped
  reading, a diagnostic waits, the way any program's would.

  A pty check finds a panicking process parked in `write(2, …, 115)`, writing
  the trace itself. The hook writes it, so it lands before unwinding gets
  anywhere near the latch.

  What that costs is bounded by keeping the exit flow clear of routine
  diagnostics, which is a different discipline from bounding writes.
  `attempt_unload_usbmon`'s progress line is `debug!`, below the default
  filter, precisely because an `info!` there sat between the exit and the
  unload. On a terminal that was wedged but open, the process parked in
  `write(2, …, 116)` and the module stayed loaded.

  Genuine warnings keep their level. The unload's own failure warning is one,
  and it is written after the attempt. It can therefore delay that exit, but it
  can no longer cost it the unload. Two rules cover anything added to this
  path. Routine progress goes to `debug!`. A warning goes after the work it
  might have to report on.

  One warning predates the rule and breaks it. If a reader thread panicked,
  `MonitorHandle::stop` logs `warn!("usbmon reader thread panicked")` before it
  returns, and `stop()` itself runs before the unload flow. On a terminal that
  is wedged but open, this warning can park in front of the unload it was
  supposed to leave clear. So `-v`'s `debug!` line is not the only risk there.

  Child processes are their own business too, though less so than it looks.
  `attempt_unload_usbmon` uses `Command::output()`, which pipes `modprobe`'s
  stdout and stderr rather than letting them reach the terminal. What can still
  touch the terminal is `sudo` opening `/dev/tty` itself to ask for a password.
  That is sudo's write, not usbtop-ng's.

### Dependencies

The chassis added two crates, both already common in the tree's dependency
graph:

- **`libc`**: `fcntl(F_GETFL/F_SETFL)` for the non-blocking descriptor and the
  flags that have to go back. Also `write(2)` for the frame drain, and the
  `EIO` and `SIGHUP` constants the failure classification is written against.
  The drain deliberately avoids `io::Stdout`, which is a `LineWriter` and would
  buffer escape sequences with no newline to flush them.
- **`signal-hook`**: a safe, iterator-shaped signal API. The alternative is
  `sigaction` by hand plus async-signal-safe handler code. `signal-hook` moves
  the whole problem onto a thread that can send on an ordinary channel.

## Data flow

### Primary data flow

```
1. USB activity → usbmon kernel interface: /dev/usbmonN (binary, preferred)
   or the debugfs `Nu` text file (fallback), chosen once by
   monitor::start_monitoring's open probe
2. Reader thread → BinaryReader::read_packets() or UsbmonReader::read_packets()
   (blocking loop over a non-blocking file, same shutdown contract either way)
3. Raw bytes or text → UsbPacket parsing (usbmon/binary.rs or usbmon/parser.rs)
4. UsbPacket → sent over an mpsc channel to the UI thread
5. UI thread → DeviceManager::apply_packet() aggregates into BandwidthStats,
   resolving new devices' metadata from sysfs by busnum and devnum
6. Per-interval refresh → DeviceManager::refresh() (decay rates, drop stale
   devices, recompute bus speeds and controllers) → UsbTopApp::sync_from()
   rebuilds the controller, bus, and device snapshot (including %busy and the
   speed indicators) → UI rendering
```

Only callbacks carry a transferred length, so `apply_packet` counts callbacks
alone. Counting submissions as well would double every URB.

### Event processing

The UI thread owns a single loop, `run_app` in `src/tui/mod.rs`. It sleeps
until its earliest deadline, then folds whatever events arrived into one
repaint. It drains what the reader threads produced since the last pass. It
refreshes state on the refresh interval, and redraws only when something
changed.

The drain takes at most `DRAIN_BATCH` packets, so one burst cannot stall a
frame, and the leftovers wait for the next pass. No async runtime is involved.
See
[TUI chassis](#tui-chassis) for the wake sources and the numbers.

```rust
// Simplified event loop (src/tui/mod.rs)
loop {
    // Sleep until the next tick, or the pending frame, or the drain cadence,
    // whichever comes first.
    match ui_events.recv_timeout(events::next_wait(now, dirty, next_tick, last_draw)) {
        // Fold the whole queued batch, not only the event that woke us: that
        // is what turns a burst of resizes into a single repaint.
        Ok(event) => { /* fold_events -> exit? resize? clear? dirty? */ }
        Err(RecvTimeoutError::Timeout) => {}
        Err(RecvTimeoutError::Disconnected) => return Ok(ExitReason::TerminalDead),
    }

    // Apply up to DRAIN_BATCH (8192) packets; the rest wait for the next pass.
    drain_packets(manager, packets, DRAIN_BATCH);

    if now >= next_tick {
        let _ = manager.refresh();     // decay rates, drop stale devices
        app.sync_from(manager);        // rebuild the render snapshot
        app.update_bandwidth_history();
        next_tick = now + app.refresh_rate;
        dirty = true;
    }

    // Dirty gate plus frame cap: nothing repaints unless something changed,
    // and never faster than MIN_FRAME_INTERVAL.
    if events::should_draw(now, dirty, last_draw) {
        terminal.draw(|f| draw_ui(f, app))?;
        last_draw = now;
        dirty = false;
    }

    // The output stage reports through flags, not errors, so this is where
    // the writes issued above are answered for.
    if shed.terminal_dead() {
        return Ok(ExitReason::TerminalDead);
    }
    if shed.take_repaint_request() {
        force_full_repaint(terminal)?;
        dirty = true;
    }
}
```

### Memory management

- **Bounded buffers.** History is limited to prevent memory growth. The
  bandwidth history is bounded by wall-clock age at 60 seconds, and the sliding
  rate window by 250 millisecond buckets over 10 seconds.
- **Bounded channel.** The reader-to-UI channel holds at most `CHANNEL_BOUND`
  (16384) packets. A consumer that cannot keep up costs dropped packets, which
  are counted, and never unbounded memory.
- **Device cleanup.** Stale devices leave the map 5 seconds after their
  disconnect, and a bus with no devices left is dropped with them.
- **No packet retention.** A `UsbPacket` is parsed per event, moved through the
  bounded channel, folded into `BandwidthStats` by `apply_packet`, and dropped.
  Nothing accumulates per packet. The binary reader drains each event's
  captured payload rather than keeping it. The sliding window holds 250
  millisecond buckets rather than one entry per URB.
- **Per-device metadata.** Vendor, product, and serial are plain owned
  `Option<String>` fields on `UsbDevice`, with no interning and no shared
  table. usbtop-ng reads them from sysfs when it first sees the device. It
  retries on later intervals only while the sysfs path is still unresolved,
  then reuses them for the life of that device. The cost is a handful of
  allocations per device, not per packet and not per frame.
- **Render snapshot.** `UsbTopApp::sync_from` rebuilds the whole
  `ControllerView`, `BusView`, and `DeviceRow` tree each refresh interval and
  drops the previous one, which keeps totals consistent. See
  [Snapshot model](#snapshot-model-and-topology-resolution). Its size follows
  the device count, not the session length.

## Linux integration

usbtop-ng targets Linux and nothing else. `src/main.rs` opens with a
`compile_error!` for every other target, which is the only platform `cfg` in
the tree. No code path stands in for an interface that does not exist.

```rust
fn get_usbmon_path(bus_id: u8) -> PathBuf {
    PathBuf::from(format!("/sys/kernel/debug/usb/usbmon/{}u", bus_id))
}
```

**What the kernel supplies:**

- Direct access to both usbmon interfaces.
- Device metadata from sysfs.
- debugfs mount detection, by reading `/proc/mounts`.
- Module detection, by reading `/proc/modules`, plus load and unload through
  `sudo modprobe`.

## Performance

### Where the work is bounded

1. **Threaded I/O.** Dedicated blocking reader threads, decoupled from the UI
   thread by an `mpsc` channel.
2. **Constant-time accounting.** One packet costs one bucket update and one sum
   adjustment, whatever the packet rate.
3. **Bounded collections.** The channel holds 16384 packets, and one pass
   applies at most 8192 of them. Both 60 second histories evict by age.
4. **Payload discarded, not copied.** The binary reader drains each event's
   captured bytes rather than carrying them into a `UsbPacket`.
5. **Metadata read once.** usbtop-ng reads a device's sysfs metadata when it
   first sees the device, and retries only while the path is unresolved.
6. **Dirty-gated drawing.** An idle session repaints once per refresh interval,
   and never faster than one frame per 33 milliseconds.

### What a slow terminal costs

A terminal that stops reading costs frames rather than packets. The output
stage sheds whole frames past its watermark and reports the count as `shed: N`.
The reader threads keep reading throughout, because the render loop and the
usbmon loop are separate threads.

### What a full channel costs

A UI thread that cannot keep up costs packets rather than memory. Readers
discard on a full channel and count the loss, which the header reports as
`dropped: N`.

### What this section does not claim

No benchmark exists in this tree. This section states the limits the code
enforces, and nothing about throughput, memory use, or CPU use. Add a benchmark
before adding a figure of that kind.

## Security model

### Privilege requirements

usbtop-ng needs elevated privileges to read usbmon.

- Root access, or read access to `/sys/kernel/debug/usb/usbmon/` granted some
  other way. Which non-root routes exist depends on the distribution.

### Security measures

1. **No privilege changes inside the process.** usbtop-ng calls neither
   `setuid` nor `setgid`. It runs `sudo modprobe` and `sudo mount` as child
   processes, and it prints the command it wants to run before it asks.
2. **Parsing that rejects rather than guesses.** A malformed text line returns
   an error, which the reader logs and skips. The binary reader reads a fixed
   48 byte header and drains exactly `len_cap` bytes. A truncated capture ends
   the loop instead of desynchronizing it.
3. **Preferences parsed through `toml` and `serde`.** A file that does not
   parse aborts startup with a message naming the path.
4. **Directory permissions.** usbtop-ng creates its own `~/.usbtop-ng` with
   mode 0700, and leaves an existing or custom directory alone.
5. **No network.** usbtop-ng opens no sockets.

### Attack surface

- **File system access**: the USB-related sysfs and debugfs paths, plus the
  preferences file.
- **Kernel interface**: read-only access to the usbmon interfaces.
- **Configuration**: TOML parsing, with both defaults false.
- **Terminal**: output through ratatui and the `ShedWriter` stage.
- **Child processes**: `sudo modprobe` and `sudo mount`, run with
  `Command::output()`, which pipes their stdout and stderr.

## Error handling

### Error categories

1. **System errors**: permission denied, file not found.
2. **Parsing errors**: malformed USB events or a malformed preferences file.
3. **UI errors**: terminal size and color support.
4. **Resource errors**: out of memory, too many open files.

### Error recovery

```rust
// Example error handling pattern: read_packets() runs the whole read loop on
// its thread. Per-line parse errors are logged and skipped inside the loop.
// Only a fatal condition (interface gone, callback error) ends the loop. A
// full channel is not fatal: the packet is counted and dropped, because a
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

### Logging strategy

- **Error level**: failures that end the run.
- **Warn level**: recoverable errors and degraded function, such as a bus that
  fell back to the text interface.
- **Info level**: normal operation and state changes, such as which interface
  was chosen.
- **Debug level**: packet parsing, reader start and finish, and the unload
  attempt.

`env_logger` reads `RUST_LOG`. The default filter is Info, and `-v` lowers it
to Debug. Every log line goes to stderr.

## Extension points

### Adding a packet source

1. Follow the `UsbmonReader` and `BinaryReader` shape: a `new(bus_id)`
   constructor, a `with_path` test seam, and `read_packets`.
2. Poll `shutdown` at least once per `POLL_INTERVAL`, so `MonitorHandle::stop`
   returns promptly.
3. Send through `try_send` and count a full channel rather than parking.

### Customizing the UI

1. Add `draw_*` functions in `src/ui/mod.rs` and call them from `draw_ui`.
2. Add colors in `src/ui/colors.rs`.
3. Adjust the layout constraints in `draw_ui`. The header needs four rows,
   because it holds a title line and a stats line inside a border.
4. Add preferences keys in `src/config/mod.rs`, which serializes the
   `Preferences` struct straight to TOML.

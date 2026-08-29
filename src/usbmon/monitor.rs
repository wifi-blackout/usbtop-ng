use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::anyhow;
use log::{debug, info, warn};

use super::binary::BinaryReader;
use super::mmap_ring::MmapReader;
use super::open_nonblocking;
use super::parser::UsbPacket;
use super::reader::UsbmonReader;
use crate::device::manager::TrafficDelta;

/// How many packets may sit unread on the channel before readers start
/// discarding. A busy bus can outrun a slow consumer (a redraw over SSH, say)
/// indefinitely, and an unbounded queue would turn that into unbounded memory;
/// this caps the backlog at a fixed few hundred kilobytes instead — over a
/// second of traffic at the packet rates this tool targets, far more slack
/// than the UI's ~50ms pass needs.
const CHANNEL_BOUND: usize = 16_384;

/// Ownership of the spawned reader threads.
///
/// Callers must `stop()` this before doing anything that requires the usbmon
/// files to be closed — an open debugfs `Nu` file pins the usbmon module, so a
/// still-running reader makes `modprobe -r usbmon` fail with EBUSY.
pub struct MonitorHandle {
    shutdown: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    /// Packets discarded because the channel was full, across every reader.
    /// Clone the `Arc` before `stop()` to keep reading it (the UI surfaces the
    /// count in its header so a lossy session is never silently lossy).
    pub dropped: Arc<AtomicU64>,
    /// Kernel-side drops the mmap ring readers' `MON_IOCG_STATS` reported —
    /// traffic the kernel itself discarded before this process ever saw it,
    /// distinct from [`Self::dropped`] (a full channel, after delivery).
    /// Readers on the text or read()-based binary interface never touch this
    /// counter, so it stays at zero unless the mmap interface is in use.
    /// Clone the `Arc` before `stop()` to keep reading it, same as
    /// [`Self::dropped`].
    pub kernel_dropped: Arc<AtomicU64>,
    /// Set once any reader ends up on the debugfs text interface — either
    /// because that is the source it was given, or because a binary source
    /// fell back to it (see `run_source_with_fallback`). The text interface
    /// reports isochronous buffer sizes, not bytes moved, so headless reports
    /// use this to mark iso devices' rates `estimated`.
    pub text_active: Arc<AtomicBool>,
}

impl MonitorHandle {
    /// Ask every reader to stop and wait for it. Readers notice the request
    /// within one poll interval (50 ms), so this returns promptly even when the
    /// monitored buses are completely idle.
    pub fn stop(self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for thread in self.threads {
            if thread.join().is_err() {
                warn!("usbmon reader thread panicked");
            }
        }
    }
}

/// One bus's packet feed, from whichever usbmon interface is usable.
///
/// The two readers are interchangeable to everything downstream: both hand back
/// [`UsbPacket`]s and both honour the same shutdown contract, they just differ
/// in which kernel interface they consume.
pub enum PacketSource {
    /// debugfs `Nu` text interface.
    Text(UsbmonReader),
    /// `/dev/usbmonN` binary interface, read via `read(2)`.
    Binary(BinaryReader),
    /// `/dev/usbmonN` binary interface, read via its mmap ring and
    /// `MON_IOCX_MFETCH` — no payload copy, and the only source of
    /// [`MonitorHandle::kernel_dropped`].
    Mmap(MmapReader),
}

impl PacketSource {
    fn bus_id(&self) -> u8 {
        match self {
            PacketSource::Text(reader) => reader.bus_id,
            PacketSource::Binary(reader) => reader.bus_id,
            PacketSource::Mmap(reader) => reader.bus_id,
        }
    }

    fn kind(&self) -> SourceKind {
        match self {
            PacketSource::Text(_) => SourceKind::Text,
            PacketSource::Binary(_) => SourceKind::Binary,
            PacketSource::Mmap(_) => SourceKind::Mmap,
        }
    }
}

/// Which usbmon interface a [`PacketSource`] reads, without the reader's own
/// state — the shape [`next_fallback`] reasons over to walk the same
/// mmap -> binary -> text chain `run_source_with_fallback`'s pre-probe
/// already prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Mmap,
    Binary,
    Text,
}

/// Given the source kind whose `read_packets` just returned `Err` and
/// whether shutdown was requested, decides what to try next: `None` when
/// nothing is left in the chain (already `Text`), or when shutdown was
/// requested — the `Err` may just be the close race of the file
/// disappearing out from under the reader as it stops rather than a real
/// capture failure, and spinning up another reader only to immediately tell
/// it to stop again buys nothing. `Some` names the next interface to try,
/// one step down the chain.
fn next_fallback(failed: SourceKind, shutdown_requested: bool) -> Option<SourceKind> {
    if shutdown_requested {
        return None;
    }
    match failed {
        SourceKind::Mmap => Some(SourceKind::Binary),
        SourceKind::Binary => Some(SourceKind::Text),
        SourceKind::Text => None,
    }
}

/// Spawn background reader threads for the given buses and return the channel
/// their parsed packets arrive on, plus the handle that shuts them down.
///
/// Bus 0 is the kernel's aggregate interface (0u carries every bus), so when
/// it is available a single reader is used to avoid double-counting.
///
/// The mmap ring is preferred when it is usable — it hands back event offsets
/// without ever copying the captured payload — then the read()-based binary
/// `/dev/usbmonN` interface when it can be opened but the ring cannot, and the
/// debugfs text interface last, for kernels or permission setups where
/// neither binary interface can.
pub fn start_monitoring(buses: &[u8]) -> (Receiver<UsbPacket>, MonitorHandle) {
    let targets: Vec<u8> = if buses.contains(&0) {
        info!("monitoring aggregate usbmon interface 0u for all buses");
        vec![0]
    } else {
        buses.to_vec()
    };

    // No buses to monitor (e.g. `--force` with none detected): skip the
    // interface probe and its `info!` entirely rather than announcing a
    // choice of interface for zero readers that are about to spawn.
    if targets.is_empty() {
        return start_sources(vec![]);
    }

    // Probing the first target is enough: the binary devices are created by the
    // same module for the same set of buses. The probe handles are dropped
    // right away so they cannot pin usbmon.
    let use_mmap = targets
        .first()
        .is_some_and(|bus| MmapReader::probe(Path::new(&format!("/dev/usbmon{bus}"))));
    let use_binary = !use_mmap
        && targets
            .first()
            .is_some_and(|bus| open_nonblocking(Path::new(&format!("/dev/usbmon{bus}"))).is_ok());
    info!(
        "using usbmon {} interface",
        if use_mmap {
            "mmap-ring"
        } else if use_binary {
            "binary"
        } else {
            "text"
        }
    );

    let sources = targets
        .iter()
        .map(|&bus| {
            if use_mmap {
                PacketSource::Mmap(MmapReader::new(bus))
            } else if use_binary {
                PacketSource::Binary(BinaryReader::new(bus))
            } else {
                PacketSource::Text(UsbmonReader::new(bus))
            }
        })
        .collect();
    start_sources(sources)
}

/// One capture backend's output stream: usbmon's per-packet feed, or the
/// eBPF backend's per-key delta feed (see `usbmon::ebpf::EbpfSource`).
///
/// `start_capture` is the only thing that decides which variant a session
/// gets; everything downstream (the drain seam in `ui::drain_capture` and
/// its `headless`/`tui` callers) dispatches on this enum instead of caring
/// which backend is live. usbmon's `Packets` path and its "every packet
/// marks the device seen" behaviour are unchanged by this enum existing --
/// `start_monitoring` and `start_sources` below, which produce it, are
/// untouched.
pub enum CaptureStream {
    /// debugfs text, `/dev/usbmonN` read(), or `/dev/usbmonN` mmap ring --
    /// whichever [`start_monitoring`] picked.
    Packets(Receiver<UsbPacket>),
    /// The eBPF backend's per-key cumulative-bytes deltas (see
    /// `usbmon::ebpf::EbpfSource`).
    Deltas(Receiver<TrafficDelta>),
}

/// Start capturing traffic for `buses`, preferring the eBPF backend when the
/// `ebpf` feature is compiled in and the program actually loads and
/// attaches (BTF present, the kprobe resolvable, sufficient privilege), and
/// falling back to [`start_monitoring`]'s usbmon chain otherwise -- eBPF is
/// opt-in and never the default, and any load/attach failure degrades to
/// the existing chain rather than failing the program (see
/// `usbmon::ebpf::EbpfSource::load_and_attach`).
pub fn start_capture(buses: &[u8]) -> (CaptureStream, MonitorHandle) {
    if let Some((deltas, handle)) = try_ebpf_capture() {
        return (CaptureStream::Deltas(deltas), handle);
    }
    let (packets, handle) = start_monitoring(buses);
    (CaptureStream::Packets(packets), handle)
}

/// The `ebpf`-feature half of [`start_capture`]: load and attach the
/// `usbrate` skeleton and, on success, spawn its poller thread. `None` when
/// the backend's load/attach failed -- the caller falls back to the usbmon
/// chain.
///
/// Mirrors [`start_sources_with_bound`]'s shape (a bounded channel, a
/// shutdown flag, a named thread folded into a [`MonitorHandle`]) with one
/// source instead of one per bus: the eBPF backend aggregates every bus
/// itself (the kprobe fires for the whole host), so there is nothing to
/// spawn per bus here.
#[cfg(feature = "ebpf")]
fn try_ebpf_capture() -> Option<(Receiver<TrafficDelta>, MonitorHandle)> {
    let mut source = match super::ebpf::EbpfSource::load_and_attach() {
        Ok(source) => source,
        Err(e) => {
            warn!("eBPF backend unavailable, falling back to the usbmon chain: {e}");
            return None;
        }
    };
    info!("using the eBPF capture backend");

    let (tx, rx) = sync_channel(CHANNEL_BOUND);
    let shutdown = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    // No kernel-side (MON_IOCG_STATS) or text-estimate concept applies to
    // the eBPF backend: its counters are exact, and a full kernel map (see
    // `src/bpf/usbrate.bpf.c`'s `max_entries`) is a silent loss this poller
    // cannot observe from userspace, distinct from the channel-full drops
    // `dropped` below counts. Both stay at their zero/false defaults for
    // the life of the session.
    let kernel_dropped = Arc::new(AtomicU64::new(0));
    let text_active = Arc::new(AtomicBool::new(false));

    let poller_shutdown = Arc::clone(&shutdown);
    let poller_dropped = Arc::clone(&dropped);
    let spawned = thread::Builder::new()
        .name("usbmon-ebpf".to_string())
        .spawn(move || {
            source.run(&poller_shutdown, |delta| match tx.try_send(delta) {
                Ok(()) => {}
                // The receiver has been dropped -- nothing will ever consume
                // another delta -- so ask the poll loop to stop now instead
                // of spinning until `stop()`, matching how the packet readers'
                // `send` closure in `run_source_chain` exits on disconnect.
                // `run` only holds a shared `&` borrow of the same flag, so
                // setting it here is sound.
                Err(TrySendError::Disconnected(_)) => {
                    poller_shutdown.store(true, Ordering::Relaxed);
                }
                // Same bargain as the packet readers' `send` closure in
                // `run_source_chain`: a reader must never park on a full
                // channel, so the delta is dropped and counted instead.
                Err(TrySendError::Full(_)) => {
                    poller_dropped.fetch_add(1, Ordering::Relaxed);
                }
            });
        });
    match spawned {
        Ok(thread) => Some((
            rx,
            MonitorHandle {
                shutdown,
                threads: vec![thread],
                dropped,
                kernel_dropped,
                text_active,
            },
        )),
        Err(e) => {
            warn!("failed to spawn the eBPF poller thread: {e}; falling back to the usbmon chain");
            None
        }
    }
}

/// [`try_ebpf_capture`] with the feature off: the backend does not exist to
/// try, so this always falls back to usbmon.
#[cfg(not(feature = "ebpf"))]
fn try_ebpf_capture() -> Option<(Receiver<TrafficDelta>, MonitorHandle)> {
    None
}

/// Spawn one thread per source, funnelling every packet onto a single channel.
///
/// Split out from [`start_monitoring`] so tests can drive the spawn/shutdown
/// path with fixture-backed readers instead of real usbmon interfaces.
pub fn start_sources(sources: Vec<PacketSource>) -> (Receiver<UsbPacket>, MonitorHandle) {
    start_sources_with_bound(sources, CHANNEL_BOUND)
}

/// [`start_sources`] with an explicit channel bound, so tests can fill the
/// channel without producing 16k packets.
fn start_sources_with_bound(
    sources: Vec<PacketSource>,
    bound: usize,
) -> (Receiver<UsbPacket>, MonitorHandle) {
    let (tx, rx) = sync_channel(bound);
    let shutdown = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let kernel_dropped = Arc::new(AtomicU64::new(0));
    let text_active = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    for source in sources {
        let tx = tx.clone();
        let shutdown = Arc::clone(&shutdown);
        let dropped = Arc::clone(&dropped);
        let kernel_dropped = Arc::clone(&kernel_dropped);
        let text_active = Arc::clone(&text_active);
        let bus = source.bus_id();
        match thread::Builder::new()
            .name(format!("usbmon-bus-{bus}"))
            .spawn(move || {
                run_source(
                    source,
                    &shutdown,
                    &tx,
                    &dropped,
                    &kernel_dropped,
                    &text_active,
                )
            }) {
            Ok(handle) => threads.push(handle),
            Err(e) => warn!("failed to spawn usbmon reader for bus {bus}: {e}"),
        }
    }
    (
        rx,
        MonitorHandle {
            shutdown,
            threads,
            dropped,
            kernel_dropped,
            text_active,
        },
    )
}

/// Read one source to completion on the calling thread, funnelling its packets
/// onto `tx`, with this bus's binary and text interfaces standing by if a
/// preferred source turns out to be unusable.
fn run_source(
    source: PacketSource,
    shutdown: &AtomicBool,
    tx: &SyncSender<UsbPacket>,
    dropped: &AtomicU64,
    kernel_dropped: &AtomicU64,
    text_active: &AtomicBool,
) {
    let fallback = UsbmonReader::new(source.bus_id());
    run_source_with_fallback(
        source,
        fallback,
        shutdown,
        tx,
        dropped,
        kernel_dropped,
        text_active,
    );
}

/// [`run_source`] with the fallback reader supplied by the caller, so tests can
/// point it at a fixture instead of the real debugfs path.
///
/// Two fallback checks apply, in order: this pre-probe below, before ever
/// calling `read_packets`, and [`run_source_chain`]'s post-run cascade,
/// which handles a source that passed this pre-probe but still failed once
/// running (a race between the probe and the read, or a fatal mid-run
/// error).
fn run_source_with_fallback(
    source: PacketSource,
    fallback: UsbmonReader,
    shutdown: &AtomicBool,
    tx: &SyncSender<UsbPacket>,
    dropped: &AtomicU64,
    kernel_dropped: &AtomicU64,
    text_active: &AtomicBool,
) {
    let bus = source.bus_id();
    // `start_monitoring`'s probe only tried the first target bus, and a
    // per-bus `/dev/usbmonN` can still be missing, unreadable, or openable but
    // not mmap-capable (an older kernel, mmap denied). Check this bus's
    // device before committing to it: without this, that one bus's reader
    // would exit with a warning and the bus would silently go dark, even
    // though a source further down the chain is right there. Every probe
    // handle here is dropped immediately so it cannot pin usbmon.
    //
    // `fallback` is threaded through as the second tuple element: consumed
    // here (`None` left behind) exactly when a branch already commits to
    // `PacketSource::Text`, since nothing past `Text` is ever tried again;
    // kept (`Some`) whenever the source is still `Mmap` or `Binary`, so
    // `run_source_chain` still has it on hand if that source fails once
    // running.
    let (source, fallback) = match source {
        PacketSource::Mmap(reader) if !MmapReader::probe(&reader.path) => {
            warn!(
                "cannot use the mmap ring at {} for bus {bus}; falling back to the usbmon binary interface",
                reader.path.display()
            );
            let binary = BinaryReader::new(bus);
            if open_nonblocking(&binary.path).is_err() {
                warn!(
                    "cannot open {} for bus {bus}; falling back to the usbmon text interface",
                    binary.path.display()
                );
                (PacketSource::Text(fallback), None)
            } else {
                (PacketSource::Binary(binary), Some(fallback))
            }
        }
        PacketSource::Binary(reader) if open_nonblocking(&reader.path).is_err() => {
            warn!(
                "cannot open {} for bus {bus}; falling back to the usbmon text interface",
                reader.path.display()
            );
            (PacketSource::Text(fallback), None)
        }
        source => (source, Some(fallback)),
    };
    run_source_chain(
        source,
        fallback,
        shutdown,
        tx,
        dropped,
        kernel_dropped,
        text_active,
    );
}

/// Runs `source` to completion, and if `read_packets` returns `Err` while
/// shutdown was not requested, retries the bus one step down the same
/// mmap -> binary -> text chain [`run_source_with_fallback`]'s pre-probe
/// already prefers (decided by [`next_fallback`]), each level at most once.
/// `fallback` is consumed the one time (if any) the chain reaches `Text`;
/// see [`run_source_with_fallback`]'s doc for why it can already be `None`
/// on entry.
///
/// Split out from `run_source_with_fallback` so this cascade — the part
/// finding 2 adds — is callable, and testable, on its own: a fixture can
/// drive it directly with a source whose `read_packets` fails at run time,
/// without needing a probe that a plain file can never pass.
fn run_source_chain(
    mut source: PacketSource,
    mut fallback: Option<UsbmonReader>,
    shutdown: &AtomicBool,
    tx: &SyncSender<UsbPacket>,
    dropped: &AtomicU64,
    kernel_dropped: &AtomicU64,
    text_active: &AtomicBool,
) {
    let bus = source.bus_id();
    loop {
        if matches!(source, PacketSource::Text(_)) {
            text_active.store(true, Ordering::Relaxed);
        }
        let kind = source.kind();
        let send = |packet| match tx.try_send(packet) {
            Ok(()) => Ok(()),
            // The channel is bounded on purpose (see CHANNEL_BOUND) and a reader
            // must never park on it: a parked reader holds the usbmon file open,
            // which is exactly what `MonitorHandle::stop` exists to prevent. Losing
            // the packet is the lesser evil, and the count makes the loss visible.
            Err(TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(anyhow!("packet channel closed")),
        };
        let result = match source {
            PacketSource::Text(reader) => reader.read_packets(shutdown, send),
            PacketSource::Binary(reader) => reader.read_packets(shutdown, send),
            PacketSource::Mmap(reader) => reader.read_packets(shutdown, kernel_dropped, send),
        };
        let Err(e) = result else {
            debug!("usbmon reader for bus {bus} finished");
            return;
        };
        warn!("usbmon reader for bus {bus} stopped: {e}");
        let shutdown_requested = shutdown.load(Ordering::Relaxed);
        match next_fallback(kind, shutdown_requested) {
            Some(SourceKind::Binary) => {
                warn!("retrying bus {bus} on the usbmon binary interface after a capture failure");
                source = PacketSource::Binary(BinaryReader::new(bus));
            }
            Some(SourceKind::Text) => {
                warn!("retrying bus {bus} on the usbmon text interface after a capture failure");
                source = PacketSource::Text(fallback.take().expect(
                    "fallback is only None when the chain already started at Text, which \
                     next_fallback never routes back to",
                ));
            }
            Some(SourceKind::Mmap) | None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::reader::UsbmonReader;
    use std::io::Write;
    use std::time::{Duration, Instant};

    /// One 48-byte binary event, enough to exercise the spawn path; the
    /// field-level coverage lives in `usbmon::binary`'s tests.
    fn binary_event(bus: u8, device: u8, length: u32) -> Vec<u8> {
        let mut b = vec![0u8; 48];
        b[8] = b'C';
        b[10] = 0x81;
        b[11] = device;
        b[12..14].copy_from_slice(&u16::from(bus).to_ne_bytes());
        b[14] = 1; // setup not captured
        b[32..36].copy_from_slice(&length.to_ne_bytes());
        b
    }

    #[test]
    fn packets_from_multiple_readers_arrive_on_one_channel() {
        let temp = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for bus in [1u8, 2u8] {
            let path = temp.path().join(format!("{bus}u"));
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "ffff0000dddd000{bus} 500 C Bi:{bus}:003:1 0 32 <").unwrap();
            sources.push(PacketSource::Text(UsbmonReader::with_path(
                bus, path, false,
            )));
        }

        let (rx, handle) = start_sources(sources);
        let mut packets: Vec<_> = rx.iter().collect(); // ends when both threads finish and drop senders
        packets.sort_by_key(|p| p.bus_id);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].bus_id, 1);
        assert_eq!(packets[1].bus_id, 2);
        handle.stop();
    }

    #[test]
    fn packets_from_multiple_binary_sources_arrive_on_one_channel() {
        let temp = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for bus in [1u8, 2u8] {
            let path = temp.path().join(format!("usbmon{bus}"));
            std::fs::write(&path, binary_event(bus, 3, 32)).unwrap();
            sources.push(PacketSource::Binary(BinaryReader::with_path(
                bus, path, false,
            )));
        }

        let (rx, handle) = start_sources(sources);
        let mut packets: Vec<_> = rx.iter().collect(); // ends when both threads finish and drop senders
        packets.sort_by_key(|p| p.bus_id);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].bus_id, 1);
        assert_eq!(packets[1].bus_id, 2);
        assert_eq!(packets[1].data_length, 32);
        handle.stop();
    }

    #[test]
    fn stop_joins_binary_sources_still_following_an_idle_device() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon3");
        std::fs::write(&path, binary_event(3, 3, 32)).unwrap();

        // follow = true: after the one event the thread parks polling for more,
        // which is exactly the state that used to keep the usbmon file open.
        let (rx, handle) = start_sources(vec![PacketSource::Binary(BinaryReader::with_path(
            3, path, true,
        ))]);
        let packet = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(packet.bus_id, 3);

        let started = Instant::now();
        handle.stop(); // returns only once the reader thread has been joined
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stop() must wake and join a parked binary reader promptly"
        );
    }

    /// A reader must never park waiting for room on the channel — that would
    /// hold the usbmon file open past `stop()` if the UI ever stalled. Once the
    /// bound is reached the surplus is counted and discarded instead.
    #[test]
    fn full_channel_drops_packets_instead_of_blocking_the_reader() {
        const BOUND: usize = 3;
        const EVENTS: usize = 10;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("1u");
        let mut f = std::fs::File::create(&path).unwrap();
        for _ in 0..EVENTS {
            writeln!(f, "ffff0000dddd0001 500 C Bi:1:003:1 0 32 <").unwrap();
        }

        // Nothing drains `rx` until the reader thread has finished, so every
        // event past the bound has to go somewhere other than a blocked send.
        let (rx, handle) = start_sources_with_bound(
            vec![PacketSource::Text(UsbmonReader::with_path(1, path, false))],
            BOUND,
        );
        let dropped = Arc::clone(&handle.dropped);

        // A blocking send would park here forever instead of counting: the
        // channel is full from the fourth event on and nobody is draining it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while dropped.load(Ordering::Relaxed) < (EVENTS - BOUND) as u64 {
            assert!(
                Instant::now() < deadline,
                "reader parked on the full channel instead of dropping"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        handle.stop();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stop() must join a reader that filled the channel"
        );

        let received: Vec<_> = rx.try_iter().collect();
        assert_eq!(received.len(), BOUND, "the channel holds exactly its bound");
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            (EVENTS - BOUND) as u64,
            "every packet that did not fit is counted as dropped"
        );
    }

    /// The global probe can succeed while one bus's `/dev/usbmonN` is missing
    /// or unreadable. That bus must not simply go dark: the reader falls back
    /// to its own text interface instead of dying with a warning.
    #[test]
    fn binary_source_that_cannot_be_opened_falls_back_to_text() {
        let temp = tempfile::tempdir().unwrap();
        let text_path = temp.path().join("7u");
        let mut f = std::fs::File::create(&text_path).unwrap();
        writeln!(f, "ffff0000dddd0007 500 C Bi:7:003:1 0 32 <").unwrap();
        let missing_device = temp.path().join("usbmon7"); // never created

        let (tx, rx) = sync_channel(4);
        let text_active = AtomicBool::new(false);
        run_source_with_fallback(
            PacketSource::Binary(BinaryReader::with_path(7, missing_device, false)),
            UsbmonReader::with_path(7, text_path, false),
            &AtomicBool::new(false),
            &tx,
            &AtomicU64::new(0),
            &AtomicU64::new(0),
            &text_active,
        );

        let packets: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            packets.len(),
            1,
            "the fallback reader's packets must arrive"
        );
        assert_eq!(packets[0].bus_id, 7);
        assert_eq!(packets[0].data_length, 32);
        assert!(
            text_active.load(Ordering::Relaxed),
            "falling back to the text interface must raise the flag"
        );
    }

    /// The extended chain from the mmap spec: an `Mmap` source whose ring
    /// cannot be probed (a fixture path, exactly like a kernel or permission
    /// setup with no mmap interface) falls all the way through the binary
    /// interface (also unusable here — no real `/dev/usbmon{bus}` exists at
    /// this bus number) to the text fixture, and still delivers.
    #[test]
    fn mmap_source_that_cannot_be_probed_falls_back_to_text() {
        let temp = tempfile::tempdir().unwrap();
        let text_path = temp.path().join("201u");
        let mut f = std::fs::File::create(&text_path).unwrap();
        writeln!(f, "ffff0000dddd00c9 500 C Bi:201:003:1 0 32 <").unwrap();
        let missing_ring = temp.path().join("usbmon201"); // never created

        let (tx, rx) = sync_channel(4);
        let text_active = AtomicBool::new(false);
        let kernel_dropped = AtomicU64::new(0);
        run_source_with_fallback(
            PacketSource::Mmap(MmapReader::with_path(201, missing_ring, false)),
            UsbmonReader::with_path(201, text_path, false),
            &AtomicBool::new(false),
            &tx,
            &AtomicU64::new(0),
            &kernel_dropped,
            &text_active,
        );

        let packets: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            packets.len(),
            1,
            "the fallback reader's packets must arrive"
        );
        assert_eq!(packets[0].bus_id, 201);
        assert_eq!(packets[0].data_length, 32);
        assert!(
            text_active.load(Ordering::Relaxed),
            "falling back all the way to the text interface must raise the flag"
        );
        assert_eq!(
            kernel_dropped.load(Ordering::Relaxed),
            0,
            "a source that fell back before ever reading the ring reports no kernel drops"
        );
    }

    #[test]
    fn next_fallback_walks_mmap_then_binary_then_text() {
        assert_eq!(
            next_fallback(SourceKind::Mmap, false),
            Some(SourceKind::Binary)
        );
        assert_eq!(
            next_fallback(SourceKind::Binary, false),
            Some(SourceKind::Text)
        );
        assert_eq!(
            next_fallback(SourceKind::Text, false),
            None,
            "nothing is left past the text interface"
        );
    }

    #[test]
    fn next_fallback_never_falls_back_once_shutdown_is_requested() {
        for kind in [SourceKind::Mmap, SourceKind::Binary, SourceKind::Text] {
            assert_eq!(
                next_fallback(kind, true),
                None,
                "a shutdown-requested Err may just be a close race, not a real failure"
            );
        }
    }

    /// [`run_source_chain`] is the post-run half of the fallback: given a
    /// source whose `read_packets` fails at run time (not caught by
    /// `run_source_with_fallback`'s pre-probe, which this test bypasses by
    /// calling the chain directly — a probe-passes/read-fails fixture needs
    /// a real device and cannot be built hermetically) and shutdown not
    /// requested, it must retry down the chain and the fallback's packets
    /// must arrive.
    #[test]
    fn run_source_chain_falls_back_to_text_when_a_source_fails_at_run_time() {
        let temp = tempfile::tempdir().unwrap();
        let text_path = temp.path().join("50u");
        let mut f = std::fs::File::create(&text_path).unwrap();
        writeln!(f, "ffff0000dddd0032 500 C Bi:50:003:1 0 32 <").unwrap();
        let missing_device = temp.path().join("usbmon50"); // never created

        let (tx, rx) = sync_channel(4);
        let text_active = AtomicBool::new(false);
        run_source_chain(
            PacketSource::Binary(BinaryReader::with_path(50, missing_device, false)),
            Some(UsbmonReader::with_path(50, text_path, false)),
            &AtomicBool::new(false),
            &tx,
            &AtomicU64::new(0),
            &AtomicU64::new(0),
            &text_active,
        );

        let packets: Vec<_> = rx.try_iter().collect();
        assert_eq!(packets.len(), 1, "the fallback's packets must arrive");
        assert_eq!(packets[0].bus_id, 50);
        assert!(
            text_active.load(Ordering::Relaxed),
            "falling back to the text interface must raise the flag"
        );
    }

    /// The other half of the same contract: a shutdown-requested `Err` must
    /// not fall back, even though a fallback with real packets sits right
    /// there — the error may just be the close race of the file
    /// disappearing out from under the reader as it stops.
    #[test]
    fn run_source_chain_does_not_fall_back_once_shutdown_is_requested() {
        let temp = tempfile::tempdir().unwrap();
        let text_path = temp.path().join("51u");
        let mut f = std::fs::File::create(&text_path).unwrap();
        writeln!(f, "ffff0000dddd0033 500 C Bi:51:003:1 0 32 <").unwrap();
        let missing_device = temp.path().join("usbmon51"); // never created

        let (tx, rx) = sync_channel(4);
        let text_active = AtomicBool::new(false);
        run_source_chain(
            PacketSource::Binary(BinaryReader::with_path(51, missing_device, false)),
            Some(UsbmonReader::with_path(51, text_path, false)),
            &AtomicBool::new(true), // shutdown already requested
            &tx,
            &AtomicU64::new(0),
            &AtomicU64::new(0),
            &text_active,
        );

        let packets: Vec<_> = rx.try_iter().collect();
        assert!(
            packets.is_empty(),
            "no fallback must run once shutdown was requested, even though \
             one was available"
        );
        assert!(
            !text_active.load(Ordering::Relaxed),
            "the text fallback must never run once shutdown was requested"
        );
    }

    /// The fallback is only for a binary source that cannot be opened; a
    /// working one is read as-is and the text reader stays untouched.
    #[test]
    fn openable_binary_source_is_not_replaced_by_the_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let binary_path = temp.path().join("usbmon8");
        std::fs::write(&binary_path, binary_event(8, 3, 64)).unwrap();
        let unused_text = temp.path().join("8u");
        std::fs::write(&unused_text, "ffff0000dddd0008 500 C Bi:8:009:1 0 32 <\n").unwrap();

        let (tx, rx) = sync_channel(4);
        let text_active = AtomicBool::new(false);
        run_source_with_fallback(
            PacketSource::Binary(BinaryReader::with_path(8, binary_path, false)),
            UsbmonReader::with_path(8, unused_text, false),
            &AtomicBool::new(false),
            &tx,
            &AtomicU64::new(0),
            &AtomicU64::new(0),
            &text_active,
        );

        let packets: Vec<_> = rx.try_iter().collect();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].device_id, 3, "read from the binary source");
        assert_eq!(packets[0].data_length, 64);
        assert!(
            !text_active.load(Ordering::Relaxed),
            "an openable binary source must not raise the text flag"
        );
    }

    #[test]
    fn text_sources_raise_the_text_active_flag() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("1u");
        std::fs::write(&path, "ffff0000dddd0001 500 C Bi:1:003:1 0 32 <\n").unwrap();
        let (rx, handle) = start_sources(vec![PacketSource::Text(UsbmonReader::with_path(
            1, path, false,
        ))]);
        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(handle.text_active.load(Ordering::Relaxed));
        handle.stop();
    }

    #[test]
    fn binary_sources_leave_the_text_active_flag_down() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        std::fs::write(&path, binary_event(1, 3, 32)).unwrap();
        let (rx, handle) = start_sources(vec![PacketSource::Binary(BinaryReader::with_path(
            1, path, false,
        ))]);
        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!handle.text_active.load(Ordering::Relaxed));
        handle.stop();
    }

    #[test]
    fn start_monitoring_with_no_buses_spawns_no_readers() {
        // The `--force` path can call this with an empty bus list when no
        // buses were detected. No sources means no reader threads, and the
        // channel's sender side drops as soon as `start_monitoring` returns,
        // so the receiver must already read as disconnected.
        let (rx, handle) = start_monitoring(&[]);
        assert!(
            handle.threads.is_empty(),
            "no targets must spawn no reader threads"
        );
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        handle.stop(); // no threads to join; must still return promptly
    }

    #[test]
    fn stop_joins_readers_still_following_an_idle_interface() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("3u");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "ffff0000dddd0003 500 C Bi:3:003:1 0 32 <").unwrap();

        // follow = true: after the one packet the thread parks polling for more,
        // which is exactly the state that used to keep the usbmon file open.
        let (rx, handle) = start_sources(vec![PacketSource::Text(UsbmonReader::with_path(
            3, path, true,
        ))]);
        let packet = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(packet.bus_id, 3);

        let started = Instant::now();
        handle.stop(); // returns only once the reader thread has been joined
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stop() must wake and join a parked reader promptly"
        );
    }
}

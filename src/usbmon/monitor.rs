use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::anyhow;
use log::{debug, info, warn};

use super::binary::BinaryReader;
use super::open_nonblocking;
use super::parser::UsbPacket;
use super::reader::UsbmonReader;

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
    /// Set once any reader ends up on the debugfs text interface — either
    /// because that is the source it was given, or because a binary source
    /// fell back to it (see `run_source_with_fallback`). The text interface
    /// cannot see individual isochronous packets, only aggregate byte counts,
    /// so headless reports use this to mark iso devices' rates `estimated`.
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
    /// `/dev/usbmonN` binary interface.
    Binary(BinaryReader),
}

impl PacketSource {
    fn bus_id(&self) -> u8 {
        match self {
            PacketSource::Text(reader) => reader.bus_id,
            PacketSource::Binary(reader) => reader.bus_id,
        }
    }
}

/// Spawn background reader threads for the given buses and return the channel
/// their parsed packets arrive on, plus the handle that shuts them down.
///
/// Bus 0 is the kernel's aggregate interface (0u carries every bus), so when
/// it is available a single reader is used to avoid double-counting.
///
/// The binary `/dev/usbmonN` interface is preferred when it can be opened —
/// it reports every event as a fixed-size record instead of a formatted line —
/// and the debugfs text interface is the fallback for kernels or permission
/// setups where it cannot.
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
    // same module for the same set of buses. The probe handle is dropped right
    // away so it cannot pin usbmon.
    let use_binary = targets
        .first()
        .is_some_and(|bus| open_nonblocking(Path::new(&format!("/dev/usbmon{bus}"))).is_ok());
    info!(
        "using usbmon {} interface",
        if use_binary { "binary" } else { "text" }
    );

    let sources = targets
        .iter()
        .map(|&bus| {
            if use_binary {
                PacketSource::Binary(BinaryReader::new(bus))
            } else {
                PacketSource::Text(UsbmonReader::new(bus))
            }
        })
        .collect();
    start_sources(sources)
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
    let text_active = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    for source in sources {
        let tx = tx.clone();
        let shutdown = Arc::clone(&shutdown);
        let dropped = Arc::clone(&dropped);
        let text_active = Arc::clone(&text_active);
        let bus = source.bus_id();
        match thread::Builder::new()
            .name(format!("usbmon-bus-{bus}"))
            .spawn(move || run_source(source, &shutdown, &tx, &dropped, &text_active))
        {
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
            text_active,
        },
    )
}

/// Read one source to completion on the calling thread, funnelling its packets
/// onto `tx`, with this bus's text interface standing by if a binary source
/// turns out to be unusable.
fn run_source(
    source: PacketSource,
    shutdown: &AtomicBool,
    tx: &SyncSender<UsbPacket>,
    dropped: &AtomicU64,
    text_active: &AtomicBool,
) {
    let fallback = UsbmonReader::new(source.bus_id());
    run_source_with_fallback(source, fallback, shutdown, tx, dropped, text_active);
}

/// [`run_source`] with the fallback reader supplied by the caller, so tests can
/// point it at a fixture instead of the real debugfs path.
fn run_source_with_fallback(
    source: PacketSource,
    fallback: UsbmonReader,
    shutdown: &AtomicBool,
    tx: &SyncSender<UsbPacket>,
    dropped: &AtomicU64,
    text_active: &AtomicBool,
) {
    let bus = source.bus_id();
    // `start_monitoring`'s probe only tried the first target bus, and a
    // per-bus `/dev/usbmonN` can still be missing or unreadable. Check this
    // bus's device before committing to it: without this, that one bus's
    // reader would exit with a warning and the bus would silently go dark,
    // even though its text interface is right there. The probe handle is
    // dropped immediately so it cannot pin usbmon.
    let source = match source {
        PacketSource::Binary(reader) if open_nonblocking(&reader.path).is_err() => {
            warn!(
                "cannot open {} for bus {bus}; falling back to the usbmon text interface",
                reader.path.display()
            );
            PacketSource::Text(fallback)
        }
        source => source,
    };
    if matches!(source, PacketSource::Text(_)) {
        text_active.store(true, Ordering::Relaxed);
    }
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
    };
    match result {
        Ok(()) => debug!("usbmon reader for bus {bus} finished"),
        Err(e) => warn!("usbmon reader for bus {bus} stopped: {e}"),
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

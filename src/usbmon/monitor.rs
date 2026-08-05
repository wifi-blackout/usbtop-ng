use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::anyhow;
use log::{debug, info, warn};

use super::binary::BinaryReader;
use super::open_nonblocking;
use super::parser::UsbPacket;
use super::reader::UsbmonReader;

/// Ownership of the spawned reader threads.
///
/// Callers must `stop()` this before doing anything that requires the usbmon
/// files to be closed — an open debugfs `Nu` file pins the usbmon module, so a
/// still-running reader makes `modprobe -r usbmon` fail with EBUSY.
pub struct MonitorHandle {
    shutdown: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
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
    let (tx, rx) = channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    for source in sources {
        let tx = tx.clone();
        let shutdown = Arc::clone(&shutdown);
        let bus = source.bus_id();
        match thread::Builder::new()
            .name(format!("usbmon-bus-{bus}"))
            .spawn(move || {
                let send = |packet| {
                    tx.send(packet)
                        .map_err(|_| anyhow!("packet channel closed"))
                };
                let result = match source {
                    PacketSource::Text(reader) => reader.read_packets(&shutdown, send),
                    PacketSource::Binary(reader) => reader.read_packets(&shutdown, send),
                };
                match result {
                    Ok(()) => debug!("usbmon reader for bus {bus} finished"),
                    Err(e) => warn!("usbmon reader for bus {bus} stopped: {e}"),
                }
            }) {
            Ok(handle) => threads.push(handle),
            Err(e) => warn!("failed to spawn usbmon reader for bus {bus}: {e}"),
        }
    }
    (rx, MonitorHandle { shutdown, threads })
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

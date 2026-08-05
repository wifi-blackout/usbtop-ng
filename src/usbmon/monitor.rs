use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::anyhow;
use log::{debug, info, warn};

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

/// Spawn background reader threads for the given buses and return the channel
/// their parsed packets arrive on, plus the handle that shuts them down.
///
/// Bus 0 is the kernel's aggregate interface (0u carries every bus), so when
/// it is available a single reader is used to avoid double-counting.
pub fn start_monitoring(buses: &[u8]) -> (Receiver<UsbPacket>, MonitorHandle) {
    let targets: Vec<u8> = if buses.contains(&0) {
        info!("monitoring aggregate usbmon interface 0u for all buses");
        vec![0]
    } else {
        buses.to_vec()
    };
    start_readers(targets.iter().map(|&b| UsbmonReader::new(b)).collect())
}

pub fn start_readers(readers: Vec<UsbmonReader>) -> (Receiver<UsbPacket>, MonitorHandle) {
    let (tx, rx) = channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    for reader in readers {
        let tx = tx.clone();
        let shutdown = Arc::clone(&shutdown);
        let bus = reader.bus_id;
        match thread::Builder::new()
            .name(format!("usbmon-bus-{bus}"))
            .spawn(move || {
                let result = reader.read_packets(&shutdown, |packet| {
                    tx.send(packet)
                        .map_err(|_| anyhow!("packet channel closed"))
                });
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

    #[test]
    fn packets_from_multiple_readers_arrive_on_one_channel() {
        let temp = tempfile::tempdir().unwrap();
        let mut readers = Vec::new();
        for bus in [1u8, 2u8] {
            let path = temp.path().join(format!("{bus}u"));
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "ffff0000dddd000{bus} 500 C Bi:{bus}:003:1 0 32 <").unwrap();
            readers.push(UsbmonReader::with_path(bus, path, false));
        }

        let (rx, handle) = start_readers(readers);
        let mut packets: Vec<_> = rx.iter().collect(); // ends when both threads finish and drop senders
        packets.sort_by_key(|p| p.bus_id);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].bus_id, 1);
        assert_eq!(packets[1].bus_id, 2);
        handle.stop();
    }

    #[test]
    fn stop_joins_readers_still_following_an_idle_interface() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("3u");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "ffff0000dddd0003 500 C Bi:3:003:1 0 32 <").unwrap();

        // follow = true: after the one packet the thread parks polling for more,
        // which is exactly the state that used to keep the usbmon file open.
        let (rx, handle) = start_readers(vec![UsbmonReader::with_path(3, path, true)]);
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

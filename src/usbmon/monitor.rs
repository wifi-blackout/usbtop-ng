use std::sync::mpsc::{channel, Receiver};
use std::thread;

use anyhow::anyhow;
use log::{debug, info, warn};

use super::parser::UsbPacket;
use super::reader::UsbmonReader;

/// Spawn background reader threads for the given buses and return the
/// channel their parsed packets arrive on.
///
/// Bus 0 is the kernel's aggregate interface (0u carries every bus), so when
/// it is available a single reader is used to avoid double-counting.
pub fn start_monitoring(buses: &[u8]) -> Receiver<UsbPacket> {
    let targets: Vec<u8> = if buses.contains(&0) {
        info!("monitoring aggregate usbmon interface 0u for all buses");
        vec![0]
    } else {
        buses.to_vec()
    };
    start_readers(targets.iter().map(|&b| UsbmonReader::new(b)).collect())
}

pub fn start_readers(readers: Vec<UsbmonReader>) -> Receiver<UsbPacket> {
    let (tx, rx) = channel();
    for reader in readers {
        let tx = tx.clone();
        let bus = reader.bus_id;
        if let Err(e) = thread::Builder::new()
            .name(format!("usbmon-bus-{bus}"))
            .spawn(move || {
                let result = reader.read_packets(|packet| {
                    tx.send(packet)
                        .map_err(|_| anyhow!("packet channel closed"))
                });
                match result {
                    Ok(()) => debug!("usbmon reader for bus {bus} finished"),
                    Err(e) => warn!("usbmon reader for bus {bus} stopped: {e}"),
                }
            })
        {
            warn!("failed to spawn usbmon reader for bus {bus}: {e}");
        }
    }
    rx
}

// (Reader threads are detached on purpose: reads on usbmon files block
// indefinitely between events, so the threads exit either when the channel
// closes after the next event, or when the process exits.)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::reader::UsbmonReader;
    use std::io::Write;

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

        let rx = start_readers(readers);
        let mut packets: Vec<_> = rx.iter().collect(); // ends when both threads finish and drop senders
        packets.sort_by_key(|p| p.bus_id);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].bus_id, 1);
        assert_eq!(packets[1].bus_id, 2);
    }
}

use anyhow::{anyhow, Result};
use log::{debug, error};
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use super::parser::{TransferType, UrbType, UsbPacket};
use super::{open_nonblocking, POLL_INTERVAL};

/// Bytes of the `usbmon_packet` header that a plain `read(2)` on the binary
/// interface returns per event.
///
/// The kernel struct is 64 bytes, but `Documentation/usb/usbmon.rst` pins the
/// read side at 48: "the read(2) system call returns the first 48 bytes of the
/// header". The remaining fields are only reachable through the ioctl API.
const HEADER_LEN: usize = 48;

/// Largest chunk read at a time when discarding an event's captured payload.
const DRAIN_CHUNK: usize = 512;

/// Reads usbmon's binary interface: the `/dev/usbmonN` character devices
/// described in `Documentation/usb/usbmon.rst`. Each event is a 48-byte header
/// in *kernel-native* byte order followed by `len_cap` bytes of captured data,
/// so the payload must be consumed even for events we drop — the next header
/// starts right after it and skipping the drain would desynchronise framing.
///
/// Like the text reader, the device is opened non-blocking and polled every
/// [`POLL_INTERVAL`], so a silent bus never parks the reader thread inside a
/// `read` where it could neither be joined nor release the usbmon module.
#[derive(Debug, Clone)]
pub struct BinaryReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool,
}

/// Why a fixed-size read stopped: either the buffer was filled, or the loop
/// must unwind (shutdown requested, EOF without follow, or an I/O error).
enum Fill {
    Filled,
    Stopped,
}

impl BinaryReader {
    pub fn new(bus_id: u8) -> Self {
        Self {
            bus_id,
            path: PathBuf::from(format!("/dev/usbmon{}", bus_id)),
            follow: true,
        }
    }

    /// Test seam: point the reader at a fixture byte stream instead of the real
    /// character device, and optionally disable follow-on-EOF so tests over a
    /// fixed file terminate.
    #[cfg(test)]
    pub fn with_path(bus_id: u8, path: PathBuf, follow: bool) -> Self {
        Self {
            bus_id,
            path,
            follow,
        }
    }

    /// Read loop over the usbmon binary interface. Runs to completion on the
    /// calling thread; callers that want this alongside other work should spawn
    /// it on a dedicated thread.
    ///
    /// `shutdown` is polled whenever the device has nothing to give — between
    /// events, mid-header, and mid-drain — so a caller can stop the loop within
    /// [`POLL_INTERVAL`] and join the thread.
    ///
    /// Events of unknown type are skipped (their payload is still drained). A
    /// callback `Err` stops the loop early and still returns `Ok(())`.
    pub fn read_packets<F>(&self, shutdown: &AtomicBool, mut callback: F) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        debug!(
            "Starting binary packet capture from {}",
            self.path.display()
        );

        let mut file = open_nonblocking(&self.path)
            .map_err(|e| anyhow!("Failed to open {}: {}", self.path.display(), e))?;
        let mut header = [0u8; HEADER_LEN];

        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // A partial header followed by a clean EOF (the tail of a truncated
            // capture) ends the loop here without reporting an error.
            if let Fill::Stopped = self.fill(&mut file, &mut header, shutdown) {
                break;
            }

            let parsed = parse_binary_header(&header);
            // Read from the header rather than `parsed`: skipped events carry a
            // payload too, and the next header only starts once it is consumed.
            if let Fill::Stopped = self.drain(&mut file, len_cap(&header), shutdown) {
                break;
            }

            if let Some((packet, _)) = parsed {
                if let Err(e) = callback(packet) {
                    debug!("Packet callback error: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    /// Fill `buf` completely, surviving short reads: `WouldBlock` can land
    /// mid-header, and the bytes already read must not be re-read or dropped,
    /// so the fill position is carried across retries.
    fn fill(&self, file: &mut File, buf: &mut [u8], shutdown: &AtomicBool) -> Fill {
        let mut filled = 0;
        while filled < buf.len() {
            match file.read(&mut buf[filled..]) {
                Ok(0) => {
                    if !self.follow {
                        return Fill::Stopped;
                    }
                    if !park(shutdown) {
                        return Fill::Stopped;
                    }
                }
                Ok(n) => filled += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if !park(shutdown) {
                        return Fill::Stopped;
                    }
                }
                Err(e) => {
                    error!("Failed to read from {}: {}", self.path.display(), e);
                    return Fill::Stopped;
                }
            }
        }
        Fill::Filled
    }

    /// Discard `len_cap` bytes of captured payload in bounded chunks, so an
    /// event claiming a large capture cannot make the reader allocate for it.
    ///
    /// Shutdown is checked explicitly at the top of each chunk rather than
    /// relying solely on `fill`'s park path: `fill` only consults `shutdown`
    /// when a `read` comes up short (`WouldBlock` or a follow-mode EOF). A
    /// source that keeps a large `len_cap` fully supplied — hostile or just
    /// unlucky framing — would otherwise let every chunk return `Filled`
    /// without ever parking, deferring shutdown for the whole drain.
    fn drain(&self, file: &mut File, len_cap: u32, shutdown: &AtomicBool) -> Fill {
        let mut scratch = [0u8; DRAIN_CHUNK];
        let mut remaining = len_cap as usize;
        while remaining > 0 {
            if shutdown.load(Ordering::Relaxed) {
                return Fill::Stopped;
            }
            let chunk = remaining.min(DRAIN_CHUNK);
            if let Fill::Stopped = self.fill(file, &mut scratch[..chunk], shutdown) {
                return Fill::Stopped;
            }
            remaining -= chunk;
        }
        Fill::Filled
    }
}

/// Park for one poll interval unless shutdown was requested. Returns `false`
/// when the caller should stop instead of retrying, which bounds the worst-case
/// shutdown latency of every blocking state at one [`POLL_INTERVAL`].
fn park(shutdown: &AtomicBool) -> bool {
    if shutdown.load(Ordering::Relaxed) {
        return false;
    }
    std::thread::sleep(POLL_INTERVAL);
    true
}

/// Bytes of captured data following this header, i.e. the `len_cap` field.
fn len_cap(buf: &[u8; HEADER_LEN]) -> u32 {
    u32::from_ne_bytes(bytes_at(buf, 36))
}

/// Copies a fixed-size field out of the header. Every offset used here is a
/// compile-time constant well inside `HEADER_LEN`, so the length always matches.
fn bytes_at<const N: usize>(buf: &[u8; HEADER_LEN], offset: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&buf[offset..offset + N]);
    out
}

/// Parse one 48-byte `usbmon_packet` header.
///
/// Returns the packet plus its `len_cap`, or `None` for an event type usbtop-ng
/// does not track — in which case the caller must still drain `len_cap` bytes
/// (see [`len_cap`]) to stay aligned with the next header.
///
/// All multi-byte fields are in the kernel's native byte order (the interface
/// is a memory image, not a wire format), hence `from_ne_bytes` throughout.
pub fn parse_binary_header(buf: &[u8; HEADER_LEN]) -> Option<(UsbPacket, u32)> {
    let urb_type = match buf[8] {
        b'S' => UrbType::Submission,
        b'C' => UrbType::Callback,
        b'E' => UrbType::Error,
        _ => return None,
    };

    let packet = UsbPacket {
        urb_type,
        // `busnum` is a u16 in the kernel struct; usbtop-ng keys buses by u8,
        // matching the text interface's bus numbering.
        bus_id: u8::try_from(u16::from_ne_bytes(bytes_at(buf, 12))).unwrap_or(0),
        device_id: buf[11],
        // Bit 7 of `epnum` is the direction bit: set = IN (device to host).
        direction: buf[10] & 0x80 != 0,
        // `length` (bytes the URB carried), not `len_cap` (bytes captured).
        data_length: u32::from_ne_bytes(bytes_at(buf, 32)),
        // Bits 0-6 of `epnum`; bit 7 is the direction flag handled above.
        endpoint: buf[10] & 0x7f,
        // `xfer_type` byte: 0=Iso, 1=Interrupt, 2=Control, 3=Bulk (see
        // `TransferType::from_binary_code`). Unrecognized codes stay `None`
        // rather than guessing a transfer type.
        transfer_type: TransferType::from_binary_code(buf[9]),
        #[cfg(test)]
        urb_tag: format!("{:016x}", u64::from_ne_bytes(bytes_at(buf, 0))),
        #[cfg(test)]
        status: i32::from_ne_bytes(bytes_at(buf, 28)),
        // `flag_setup` is 0 exactly when a control setup packet was captured;
        // any other value (usually '-') means the union holds nothing useful.
        #[cfg(test)]
        setup_packet: if buf[14] == 0 {
            Some(buf[40..48].to_vec())
        } else {
            None
        },
        // The binary interface can return payload bytes, but usbtop-ng only
        // needs transfer sizes, so captured data is drained rather than kept.
        #[cfg(test)]
        data: None,
    };

    Some((packet, len_cap(buf)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::parser::{TransferType, UrbType};

    fn event(
        t: u8,
        epnum: u8,
        devnum: u8,
        busnum: u16,
        status: i32,
        length: u32,
        data: &[u8],
    ) -> Vec<u8> {
        let mut b = vec![0u8; 48];
        b[0..8].copy_from_slice(&0xdeadbeefu64.to_ne_bytes());
        b[8] = t;
        b[9] = 3;
        b[10] = epnum;
        b[11] = devnum;
        b[12..14].copy_from_slice(&busnum.to_ne_bytes());
        b[14] = 1; // setup not captured
        b[28..32].copy_from_slice(&status.to_ne_bytes());
        b[32..36].copy_from_slice(&length.to_ne_bytes());
        b[36..40].copy_from_slice(&(data.len() as u32).to_ne_bytes());
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn reads_binary_events_and_skips_unknown_types() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        let mut stream = Vec::new();
        stream.extend(event(b'S', 0x81, 3, 1, -115, 512, &[]));
        stream.extend(event(b'X', 0x01, 4, 1, 0, 8, &[0xAA; 8])); // unknown type WITH data: drain must keep framing
        stream.extend(event(b'C', 0x81, 3, 1, 0, 512, &[0xBB; 16])); // len_cap 16 < length 512
        std::fs::write(&path, &stream).unwrap();

        let reader = BinaryReader::with_path(1, path, false);
        let shutdown = std::sync::atomic::AtomicBool::new(false);
        let mut got = Vec::new();
        reader
            .read_packets(&shutdown, |p| {
                got.push(p);
                Ok(())
            })
            .unwrap();

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].urb_type, UrbType::Submission);
        assert!(got[0].direction);
        assert_eq!(got[1].urb_type, UrbType::Callback);
        assert_eq!(got[1].data_length, 512);
        assert_eq!(got[1].device_id, 3);
        assert_eq!(got[1].bus_id, 1);
    }

    #[test]
    fn setup_flag_zero_captures_setup_bytes() {
        let mut e = event(b'S', 0x00, 2, 1, -115, 8, &[]);
        e[14] = 0;
        e[40..48].copy_from_slice(&[0xa3, 0, 0, 0, 3, 0, 4, 0]);
        let (p, len_cap) = parse_binary_header(&e[..48].try_into().unwrap()).unwrap();
        assert_eq!(
            p.setup_packet.as_deref(),
            Some(&[0xa3, 0, 0, 0, 3, 0, 4, 0][..])
        );
        assert_eq!(len_cap, 0);
    }

    #[test]
    fn truncated_trailing_header_stops_cleanly() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        let mut stream = event(b'C', 0x81, 3, 1, 0, 64, &[]);
        stream.extend_from_slice(&[0u8; 20]); // partial next header, then EOF
        std::fs::write(&path, &stream).unwrap();
        let reader = BinaryReader::with_path(1, path, false);
        let shutdown = std::sync::atomic::AtomicBool::new(false);
        let mut n = 0;
        reader
            .read_packets(&shutdown, |_| {
                n += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn header_fields_map_to_packet_fields() {
        let e = event(b'C', 0x83, 7, 2, -32, 64, &[0xCC; 4]);
        let (p, len_cap) = parse_binary_header(&e[..48].try_into().unwrap()).unwrap();
        assert_eq!(p.urb_tag, "00000000deadbeef");
        assert_eq!(p.endpoint, 3);
        assert!(p.direction);
        assert_eq!(p.status, -32);
        assert_eq!(p.bus_id, 2);
        assert_eq!(p.device_id, 7);
        assert_eq!(p.data_length, 64);
        assert_eq!(p.setup_packet, None);
        assert_eq!(p.data, None);
        assert_eq!(len_cap, 4);
    }

    #[test]
    fn binary_header_maps_xfer_type_and_endpoint_into_production_fields() {
        let e = event(b'C', 0x81, 3, 1, 0, 512, &[]);
        let (p, _) = parse_binary_header(&e[..48].try_into().unwrap()).unwrap();
        assert_eq!(p.endpoint, 1);
        assert_eq!(p.transfer_type, Some(TransferType::Bulk));

        let mut iso = event(b'C', 0x81, 3, 1, 0, 512, &[]);
        iso[9] = 0;
        let (p, _) = parse_binary_header(&iso[..48].try_into().unwrap()).unwrap();
        assert_eq!(p.transfer_type, Some(TransferType::Isochronous));

        let mut unknown = event(b'C', 0x81, 3, 1, 0, 512, &[]);
        unknown[9] = 9;
        let (p, _) = parse_binary_header(&unknown[..48].try_into().unwrap()).unwrap();
        assert_eq!(
            p.transfer_type, None,
            "unrecognized codes stay honest: None"
        );
    }

    #[test]
    fn out_endpoint_and_error_event_parse() {
        let e = event(b'E', 0x02, 1, 1, -108, 0, &[]);
        let (p, _) = parse_binary_header(&e[..48].try_into().unwrap()).unwrap();
        assert_eq!(p.urb_type, UrbType::Error);
        assert!(!p.direction);
        assert_eq!(p.endpoint, 2);
    }

    /// Bus numbers above 255 cannot be represented in `UsbPacket`; they fall
    /// back to 0 rather than wrapping onto an unrelated bus.
    #[test]
    fn oversized_busnum_falls_back_to_zero() {
        let e = event(b'S', 0x81, 3, 300, -115, 8, &[]);
        let (p, _) = parse_binary_header(&e[..48].try_into().unwrap()).unwrap();
        assert_eq!(p.bus_id, 0);
    }

    #[test]
    fn preset_shutdown_flag_stops_follow_mode_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon4");
        std::fs::write(&path, []).unwrap();

        // follow = true would otherwise poll this empty stream forever.
        let reader = BinaryReader::with_path(4, path, true);
        let started = std::time::Instant::now();
        reader
            .read_packets(&AtomicBool::new(true), |_| Ok(()))
            .unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a set shutdown flag must end the loop without polling"
        );
    }

    #[test]
    fn callback_error_stops_reading() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        let mut stream = event(b'C', 0x81, 3, 1, 0, 64, &[]);
        stream.extend(event(b'C', 0x81, 4, 1, 0, 64, &[]));
        std::fs::write(&path, &stream).unwrap();

        let reader = BinaryReader::with_path(1, path, false);
        let mut count = 0;
        reader
            .read_packets(&AtomicBool::new(false), |_| {
                count += 1;
                Err(anyhow!("stop"))
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Covers the `WouldBlock` retry path, which regular fixture files never
    /// reach: a FIFO opened `O_RDONLY | O_NONBLOCK` succeeds with no writer
    /// attached and then answers reads with EAGAIN, just like an idle
    /// `/dev/usbmonN`. The event is split so a retry lands mid-header and
    /// mid-payload, proving the fill position survives both.
    #[test]
    fn wouldblock_retry_reassembles_partial_events() {
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon5");
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());

        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let reader = BinaryReader::with_path(5, path.clone(), true);
        let flag = std::sync::Arc::clone(&shutdown);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            reader
                .read_packets(&flag, |p| {
                    tx.send(p).unwrap();
                    Ok(())
                })
                .unwrap();
        });

        // The writer opens after the reader is already polling.
        let mut w = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let first = event(b'C', 0x81, 3, 5, 0, 32, &[0xEE; 8]);
        w.write_all(&first[..20]).unwrap(); // partial header
        w.flush().unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        w.write_all(&first[20..50]).unwrap(); // rest of header + partial payload
        w.flush().unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        w.write_all(&first[50..]).unwrap(); // rest of payload
        w.write_all(&event(b'C', 0x81, 4, 5, 0, 64, &[])).unwrap();
        w.flush().unwrap();

        let first = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(
            first.data_length, 32,
            "partial header must survive the WouldBlock retry"
        );
        let second = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(
            second.device_id, 4,
            "the drained payload must leave the next header aligned"
        );

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }
}

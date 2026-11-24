use anyhow::{anyhow, Result};
use log::{debug, error};
use std::io::{ErrorKind, Read};
use std::ops::ControlFlow;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use super::parser::{TransferType, UrbType, UsbPacket};
use super::ring;
use super::{open_nonblocking, POLL_INTERVAL};

/// Bytes of the `usbmon_packet` header that a plain `read(2)` on the binary
/// interface returns per event.
///
/// The kernel struct is 64 bytes, but `Documentation/usb/usbmon.rst` pins the
/// read side at 48: "the read(2) system call returns the first 48 bytes of the
/// header". The remaining fields are only reachable through the ioctl API.
///
/// `pub(crate)`: the mmap ring reader ([`super::mmap_ring`]) copies the same
/// 48 bytes out of each ring offset before calling [`parse_binary_header`], so
/// this length is shared rather than duplicated as a second magic number.
pub(crate) const HEADER_LEN: usize = 48;

/// Bytes asked of the device in one read.
///
/// `mon_bin_read` in `drivers/usb/mon/mon_bin.c` gives one read the bytes of
/// one event and no other: the 48 header bytes, then as much of the captured
/// payload as the caller has room for, and it moves on to the next event only
/// when the last byte of this one has been read. So an event costs one system
/// call when its header and payload fit in the buffer, and one more for every
/// further buffer's worth when they do not.
///
/// The kernel caps a capture at a fifth of the ring plus 2 KiB of isochronous
/// packet descriptors (`mon_bin_event`), which on the enlarged ring is far
/// more than this. What matters in practice is the transfers devices make: a
/// high-speed camera's isochronous transfer is 32 packets of up to 3072
/// bytes, 96 KiB captured whole, and that fits with room to spare.
const READ_LEN: usize = 128 * 1024;

/// Reads usbmon's binary interface: the `/dev/usbmonN` character devices
/// described in `Documentation/usb/usbmon.rst`. Each event is a 48-byte header
/// in *kernel-native* byte order followed by `len_cap` bytes of captured data,
/// so the payload must be read even for events we drop: the next header
/// starts right after it. usbtop-ng uses none of it, and reads it in the same
/// system call as the header (see [`READ_LEN`]).
///
/// The device is opened non-blocking and, when it has nothing, polled every
/// [`POLL_INTERVAL`], so a silent bus never parks the reader thread inside a
/// `read` where it could neither be joined nor release the usbmon module.
#[derive(Debug, Clone)]
pub struct BinaryReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool,
}

/// An event whose header has been read and whose captured payload has not
/// all gone by yet.
struct Open {
    /// `None` for an event type usbtop-ng does not track. Its payload is
    /// passed over all the same.
    packet: Option<UsbPacket>,
    payload_left: usize,
}

/// Cuts what the reads return into events.
///
/// The device hands out one event per read, but a fixture file or a FIFO
/// returns whatever bytes it has: several events at once, or part of a
/// header. So this keeps the bytes of an unfinished header between reads and
/// counts down a payload that is still arriving, and never trusts a read to
/// end where an event does.
struct Frames {
    buf: Box<[u8]>,
    /// `buf[start..end]` is what has been read and not yet looked at. Once
    /// [`Frames::filled`] returns to go on, it is empty while a payload is
    /// still arriving and otherwise shorter than a header. When `filled`
    /// returns to stop, whole events can be left in it, and the reader does
    /// not come back for them.
    start: usize,
    end: usize,
    open: Option<Open>,
}

impl Frames {
    fn new() -> Self {
        Self {
            buf: vec![0u8; READ_LEN].into_boxed_slice(),
            start: 0,
            end: 0,
            open: None,
        }
    }

    /// Where the next read goes: the whole buffer, less the bytes of an
    /// unfinished header, which move to the front first.
    fn space(&mut self) -> &mut [u8] {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        &mut self.buf[self.end..]
    }

    /// Takes in the `n` bytes a read just put in [`Frames::space`] and hands
    /// `each` every event they complete. An event counts as complete when
    /// the last byte of its payload has gone by, so one cut short by the end
    /// of a file is not delivered. Stops early when `each` says to.
    fn filled<F>(&mut self, n: usize, each: &mut F) -> ControlFlow<()>
    where
        F: FnMut(UsbPacket) -> ControlFlow<()>,
    {
        self.end += n;
        loop {
            if let Some(open) = &mut self.open {
                let passed = open.payload_left.min(self.end - self.start);
                self.start += passed;
                open.payload_left -= passed;
                if open.payload_left > 0 {
                    return ControlFlow::Continue(());
                }
                if let Some(Open {
                    packet: Some(packet),
                    ..
                }) = self.open.take()
                {
                    each(packet)?;
                }
            }
            let Some(header) = self.buf[self.start..self.end].first_chunk::<HEADER_LEN>() else {
                return ControlFlow::Continue(());
            };
            // The length comes from the header and not from the parse: an
            // event that is skipped carries a payload too.
            self.open = Some(Open {
                packet: parse_binary_header(header).map(|(packet, _)| packet),
                payload_left: len_cap(header) as usize,
            });
            self.start += HEADER_LEN;
        }
    }
}

impl BinaryReader {
    pub fn new(bus_id: u8) -> Self {
        Self {
            bus_id,
            path: PathBuf::from(format!("/dev/usbmon{}", bus_id)),
            follow: true,
        }
    }

    /// Test/capture seam: point the reader at a fixture byte stream instead of
    /// the real character device, and optionally disable follow-on-EOF so reads
    /// over a fixed file terminate.
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
    /// `shutdown` is checked before every read, before every event handed to
    /// the callback, and whenever the device has nothing to give, so a
    /// caller can stop the loop within [`POLL_INTERVAL`] and join the
    /// thread.
    ///
    /// The kernel ring is enlarged before the first read (see
    /// [`ring::request_ring_ladder`]): the ring is the same per-open buffer
    /// whether it is drained by `read(2)` or by mmap, and on its ~300 KiB
    /// default five isochronous callbacks fill it. `MON_IOCG_STATS` is read at
    /// most once per [`POLL_INTERVAL`] and once more at exit, and each
    /// read-and-clear `dropped` count is summed into `kernel_dropped` via
    /// [`ring::add_kernel_drops`], exactly as the mmap reader does. On a
    /// regular file (fixtures) both ioctls answer `ENOTTY` and are ignored.
    ///
    /// Events of unknown type are skipped, their payload read past like any
    /// other. A callback `Err` stops the loop early and still returns
    /// `Ok(())`.
    pub fn read_packets<F>(
        &self,
        shutdown: &AtomicBool,
        kernel_dropped: &AtomicU64,
        callback: F,
    ) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        debug!(
            "starting binary packet capture from {}",
            self.path.display()
        );

        let mut file = open_nonblocking(&self.path)
            .map_err(|e| anyhow!("could not open {}: {}", self.path.display(), e))?;
        let fd = file.as_raw_fd();
        ring::request_ring_ladder(fd, &self.path);
        let read_stats = || match ring::stats(fd) {
            Ok(s) => ring::add_kernel_drops(kernel_dropped, s.dropped),
            Err(e) => debug!("MON_IOCG_STATS on {}: {}", self.path.display(), e),
        };

        self.read_from(&mut file, shutdown, read_stats, callback);

        Ok(())
    }

    /// The read loop proper, over anything that reads like the device.
    /// `read_stats` is called at most once per [`POLL_INTERVAL`] while the
    /// loop runs and once more when it ends.
    fn read_from<R, S, F>(
        &self,
        source: &mut R,
        shutdown: &AtomicBool,
        mut read_stats: S,
        mut callback: F,
    ) where
        R: Read,
        S: FnMut(),
        F: FnMut(UsbPacket) -> Result<()>,
    {
        let mut frames = Frames::new();
        let mut last_stats_at = Instant::now();
        // One read of a file can hold thousands of events, so the stop flag
        // is looked at before each one is handed on, as it was when every
        // event had a read of its own.
        let mut deliver = |packet| {
            if shutdown.load(Ordering::Relaxed) {
                return ControlFlow::Break(());
            }
            match callback(packet) {
                Ok(()) => ControlFlow::Continue(()),
                Err(e) => {
                    debug!("packet callback error: {}", e);
                    ControlFlow::Break(())
                }
            }
        };

        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // Bounded to at most once per `POLL_INTERVAL` regardless of event
            // rate, matching the mmap reader.
            if last_stats_at.elapsed() >= POLL_INTERVAL {
                last_stats_at = Instant::now();
                read_stats();
            }

            match source.read(frames.space()) {
                // The end of a file. Part of an event left over at the end
                // of a truncated capture is dropped without an error.
                Ok(0) => {
                    if !self.follow || !park(shutdown) {
                        break;
                    }
                }
                Ok(n) => {
                    if frames.filled(n, &mut deliver).is_break() {
                        break;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if !park(shutdown) {
                        break;
                    }
                }
                Err(e) => {
                    error!("could not read {}: {}", self.path.display(), e);
                    break;
                }
            }
        }

        // One more read-and-clear at exit, so whatever the kernel lost since
        // the last periodic read is not lost from `kernel_dropped` too.
        read_stats();
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
        // needs transfer sizes, so captured data is read past and not kept.
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
            .read_packets(&shutdown, &AtomicU64::new(0), |p| {
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
            .read_packets(&shutdown, &AtomicU64::new(0), |_| {
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
            .read_packets(&AtomicBool::new(true), &AtomicU64::new(0), |_| Ok(()))
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
            .read_packets(&AtomicBool::new(false), &AtomicU64::new(0), |_| {
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
                .read_packets(&flag, &AtomicU64::new(0), |p| {
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

    /// Stands in for `/dev/usbmonN`. Like `mon_bin_read` in
    /// `drivers/usb/mon/mon_bin.c`, one read returns bytes of one event and
    /// no other: as much of what is left of it as the caller has room for.
    struct OneEventPerRead {
        events: std::collections::VecDeque<Vec<u8>>,
        taken: usize,
        reads: usize,
    }

    impl OneEventPerRead {
        fn new(events: Vec<Vec<u8>>) -> Self {
            Self {
                events: events.into(),
                taken: 0,
                reads: 0,
            }
        }
    }

    impl Read for OneEventPerRead {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            let Some(event) = self.events.front() else {
                return Ok(0);
            };
            let n = buf.len().min(event.len() - self.taken);
            buf[..n].copy_from_slice(&event[self.taken..self.taken + n]);
            self.taken += n;
            if self.taken == event.len() {
                self.events.pop_front();
                self.taken = 0;
            }
            Ok(n)
        }
    }

    /// Reads `events` the way the device hands them out and returns the
    /// device numbers delivered, in order, with the number of reads made.
    fn devices_and_reads(events: Vec<Vec<u8>>) -> (Vec<u8>, usize) {
        let mut device = OneEventPerRead::new(events);
        let mut devices = Vec::new();
        BinaryReader::with_path(1, PathBuf::from("unused"), false).read_from(
            &mut device,
            &AtomicBool::new(false),
            || {},
            |packet| {
                devices.push(packet.device_id);
                Ok(())
            },
        );
        (devices, device.reads)
    }

    /// The defect this pins: the reader used to take an event's header in one
    /// read and its captured payload in further reads of 512 bytes, seven
    /// reads for an event with 3000 bytes captured.
    #[test]
    fn an_event_and_its_payload_take_one_read() {
        let (devices, reads) = devices_and_reads(vec![
            event(b'S', 0x81, 3, 1, -115, 3000, &[]),
            event(b'C', 0x81, 4, 1, 0, 3000, &[0xAB; 3000]),
            event(b'C', 0x81, 5, 1, 0, 64, &[0xCD; 64]),
        ]);
        assert_eq!(devices, [3, 4, 5]);
        assert_eq!(
            reads, 4,
            "one read for each event and one that finds the end"
        );
    }

    #[test]
    fn an_event_that_just_fits_the_buffer_takes_one_read() {
        let payload = vec![0x11; READ_LEN - HEADER_LEN];
        let (devices, reads) = devices_and_reads(vec![
            event(b'C', 0x81, 6, 1, 0, 64, &payload),
            event(b'C', 0x81, 7, 1, 0, 64, &[]),
        ]);
        assert_eq!(devices, [6, 7]);
        assert_eq!(reads, 3);
    }

    /// A payload longer than the buffer is read past a buffer at a time, and
    /// the event after it is found where it starts. The first event is of a
    /// type that is skipped, the second is delivered only once its last byte
    /// has gone by.
    #[test]
    fn a_payload_longer_than_the_buffer_takes_a_read_per_buffer() {
        // A header and two and a half buffers of payload: three reads.
        let long = vec![0x22; READ_LEN * 5 / 2];
        let (devices, reads) = devices_and_reads(vec![
            event(b'X', 0x81, 8, 1, 0, 64, &long),
            event(b'C', 0x81, 9, 1, 0, 64, &long),
            event(b'C', 0x81, 10, 1, 0, 64, &[0x33; 8]),
        ]);
        assert_eq!(devices, [9, 10]);
        assert_eq!(reads, 3 + 3 + 1 + 1);
    }

    /// A read that fills the buffer and ends 17 bytes into a header: those
    /// bytes move to the front and the next read goes in behind them.
    #[test]
    fn a_header_cut_by_the_end_of_a_full_buffer_is_put_together() {
        let mut stream = Vec::new();
        // Two events that end 17 bytes short of a buffer's length.
        let first = vec![0x12; READ_LEN - 17 - 2 * HEADER_LEN];
        stream.extend(event(b'C', 0x81, 1, 1, 0, 10, &first));
        stream.extend(event(b'C', 0x81, 2, 1, 0, 20, &[]));
        assert_eq!(stream.len(), READ_LEN - 17);
        stream.extend(event(b'C', 0x81, 3, 1, 0, 30, &[0x34; 100]));
        stream.extend(event(b'C', 0x81, 4, 1, 0, 40, &[]));

        assert_eq!(
            delivered_in_steps(&stream, READ_LEN),
            [(1, 10), (2, 20), (3, 30), (4, 40)]
        );
    }

    /// Stands in for a file or a FIFO: a read returns the next `step` bytes
    /// of the stream wherever the events in it begin and end.
    struct Steps {
        stream: Vec<u8>,
        at: usize,
        step: usize,
    }

    impl Read for Steps {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.step.min(buf.len()).min(self.stream.len() - self.at);
            buf[..n].copy_from_slice(&self.stream[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    /// Reads `stream` in reads of `step` bytes and returns what was
    /// delivered, as (device number, length).
    fn delivered_in_steps(stream: &[u8], step: usize) -> Vec<(u8, u32)> {
        let mut source = Steps {
            stream: stream.to_vec(),
            at: 0,
            step,
        };
        let mut delivered = Vec::new();
        BinaryReader::with_path(1, PathBuf::from("unused"), false).read_from(
            &mut source,
            &AtomicBool::new(false),
            || {},
            |packet| {
                delivered.push((packet.device_id, packet.data_length));
                Ok(())
            },
        );
        delivered
    }

    /// Reads that end in the middle of a header, in the middle of a payload,
    /// between two events and after several of them all give the same
    /// events.
    #[test]
    fn events_are_found_wherever_the_reads_cut_the_stream() {
        let mut stream = Vec::new();
        stream.extend(event(b'S', 0x81, 3, 1, -115, 100, &[]));
        stream.extend(event(b'C', 0x81, 4, 1, 0, 200, &[0x44; 200]));
        stream.extend(event(b'X', 0x01, 5, 1, 0, 300, &[0x55; 61]));
        stream.extend(event(b'C', 0x02, 6, 1, 0, 400, &[]));
        stream.extend(event(b'C', 0x81, 7, 1, 0, 500, &[0x66; 5000]));
        stream.extend(event(b'E', 0x81, 8, 1, -32, 600, &[0x77; 1]));

        let whole = [(3, 100), (4, 200), (6, 400), (7, 500), (8, 600)];
        for step in [1, 7, 47, 48, 49, 96, 297, 4096, stream.len(), READ_LEN] {
            assert_eq!(delivered_in_steps(&stream, step), whole, "step {step}");
        }
    }

    /// An event is delivered when the last byte of its payload has been
    /// read, so one whose payload the end of a file cuts short is not.
    #[test]
    fn an_event_cut_short_by_the_end_of_the_file_is_not_delivered() {
        let mut stream = event(b'C', 0x81, 3, 1, 0, 64, &[0x88; 16]);
        stream.extend(event(b'C', 0x81, 4, 1, 0, 64, &[0x99; 16]));

        let whole = stream.len();
        assert_eq!(delivered_in_steps(&stream, whole), [(3, 64), (4, 64)]);
        stream.truncate(whole - 1);
        assert_eq!(delivered_in_steps(&stream, whole), [(3, 64)]);
        stream.truncate(whole - 16);
        assert_eq!(delivered_in_steps(&stream, whole), [(3, 64)]);

        // The same for a payload of several buffers: nothing is delivered
        // on the strength of the header or of the first buffer.
        let mut long = event(b'C', 0x81, 5, 1, 0, 64, &vec![0x9A; READ_LEN * 5 / 2]);
        assert_eq!(delivered_in_steps(&long, READ_LEN), [(5, 64)]);
        long.pop();
        assert_eq!(delivered_in_steps(&long, READ_LEN), []);
    }

    /// One read of a file returns many events. A stop asked for while the
    /// first of them is being handled is seen before the second is handed
    /// on, as it was when each event took a read of its own.
    #[test]
    fn a_stop_is_seen_between_two_events_of_one_read() {
        let mut stream = Vec::new();
        for device in 1..=3 {
            stream.extend(event(b'C', 0x81, device, 1, 0, 64, &[]));
        }
        let mut source = Steps {
            stream,
            at: 0,
            step: READ_LEN,
        };
        let shutdown = AtomicBool::new(false);
        let mut delivered = Vec::new();
        BinaryReader::with_path(1, PathBuf::from("unused"), false).read_from(
            &mut source,
            &shutdown,
            || {},
            |packet| {
                delivered.push(packet.device_id);
                shutdown.store(true, Ordering::Relaxed);
                Ok(())
            },
        );
        assert_eq!(delivered, [1]);
    }

    /// What one read of a [`Script`] does.
    enum Act {
        Give(Vec<u8>),
        Fail(ErrorKind),
    }

    /// A source that plays a list of reads back in order. When the list runs
    /// out it asks for a stop and has nothing more, so a reader in follow
    /// mode ends there. `made` counts the reads made of it so far, where
    /// something outside the reader can see it.
    struct Script<'a> {
        acts: std::collections::VecDeque<Act>,
        shutdown: &'a AtomicBool,
        made: &'a std::cell::Cell<usize>,
    }

    impl Read for Script<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.made.set(self.made.get() + 1);
            match self.acts.pop_front() {
                Some(Act::Give(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Some(Act::Fail(kind)) => Err(kind.into()),
                None => {
                    self.shutdown.store(true, Ordering::Relaxed);
                    Err(ErrorKind::WouldBlock.into())
                }
            }
        }
    }

    /// Plays `acts` to a reader in follow mode and returns the device
    /// numbers delivered and the acts left unread when it stopped.
    fn devices_and_acts_left(acts: Vec<Act>) -> (Vec<u8>, usize) {
        let shutdown = AtomicBool::new(false);
        let made = std::cell::Cell::new(0);
        let mut source = Script {
            acts: acts.into(),
            shutdown: &shutdown,
            made: &made,
        };
        let mut devices = Vec::new();
        BinaryReader::with_path(1, PathBuf::from("unused"), true).read_from(
            &mut source,
            &shutdown,
            || {},
            |packet| {
                devices.push(packet.device_id);
                Ok(())
            },
        );
        (devices, source.acts.len())
    }

    /// An interrupted read, a read that would block and, in follow mode, a
    /// read at the end of the file each leave the bytes already taken where
    /// they are, in the middle of a header and in the middle of a payload.
    #[test]
    fn reads_that_bring_nothing_lose_nothing_of_an_event_under_way() {
        let first = event(b'C', 0x81, 3, 1, 0, 64, &[0xE1; 40]);
        let mut rest = first[60..].to_vec();
        rest.extend(event(b'C', 0x81, 4, 1, 0, 64, &[]));

        let (devices, left) = devices_and_acts_left(vec![
            Act::Give(first[..20].to_vec()),
            Act::Fail(ErrorKind::Interrupted),
            Act::Fail(ErrorKind::WouldBlock),
            Act::Give(Vec::new()),
            Act::Give(first[20..60].to_vec()),
            Act::Fail(ErrorKind::Interrupted),
            Act::Fail(ErrorKind::WouldBlock),
            Act::Give(Vec::new()),
            Act::Give(rest),
        ]);
        assert_eq!(devices, [3, 4]);
        assert_eq!(left, 0);
    }

    /// A read that fails for any other reason ends the loop there: the
    /// event under way is not delivered and nothing more is read.
    #[test]
    fn a_failed_read_ends_the_loop_with_the_event_under_way_undelivered() {
        let first = event(b'C', 0x81, 3, 1, 0, 64, &[0xE2; 40]);
        let second = event(b'C', 0x81, 4, 1, 0, 64, &[]);

        let (devices, left) = devices_and_acts_left(vec![
            Act::Give(second.clone()),
            Act::Give(first[..60].to_vec()),
            Act::Fail(ErrorKind::InvalidInput),
            Act::Give(first[60..].to_vec()),
        ]);
        assert_eq!(devices, [4]);
        assert_eq!(left, 1, "the read after the failure was never made");
    }

    /// A source that does not run dry: one header that claims the largest
    /// capture there is, then payload for as long as anyone reads. It asks
    /// for a stop during its `stop_at`th read, and fails a few reads later so
    /// that a reader which missed the stop ends the test and does not hang
    /// it.
    struct Endless<'a> {
        reads: usize,
        stop_at: usize,
        shutdown: &'a AtomicBool,
    }

    impl Read for Endless<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            if self.reads == self.stop_at {
                self.shutdown.store(true, Ordering::Relaxed);
            }
            if self.reads > self.stop_at + 3 {
                return Err(std::io::Error::other("read on after the stop"));
            }
            buf.fill(0);
            if self.reads == 1 {
                let header = event(b'C', 0x81, 3, 1, 0, 64, &[]);
                buf[..HEADER_LEN].copy_from_slice(&header);
                buf[36..40].copy_from_slice(&u32::MAX.to_ne_bytes());
            }
            Ok(buf.len())
        }
    }

    /// The reader only ever sleeps when a read comes back empty-handed, so a
    /// source that always has more must not be able to keep it from seeing
    /// a stop: the flag is looked at before every read.
    #[test]
    fn a_stop_is_seen_before_the_next_read_of_a_payload_that_never_ends() {
        let shutdown = AtomicBool::new(false);
        let mut source = Endless {
            reads: 0,
            stop_at: 5,
            shutdown: &shutdown,
        };
        let mut delivered = 0;
        BinaryReader::with_path(1, PathBuf::from("unused"), true).read_from(
            &mut source,
            &shutdown,
            || {},
            |_| {
                delivered += 1;
                Ok(())
            },
        );
        assert_eq!(source.reads, 5, "no read after the one that asked");
        assert_eq!(delivered, 0, "its payload never ended");
    }

    /// The ring's statistics are read by the clock, not by the event: a
    /// thousand events read them no more often than the poll intervals that
    /// went by, and once when the loop ends.
    #[test]
    fn the_statistics_are_not_read_once_an_event() {
        let events = (0..1000)
            .map(|_| event(b'C', 0x81, 3, 1, 0, 64, &[0xAA; 32]))
            .collect();
        let mut device = OneEventPerRead::new(events);
        let mut stats_reads = 0u32;
        let mut delivered = 0u32;
        let started = Instant::now();
        BinaryReader::with_path(1, PathBuf::from("unused"), false).read_from(
            &mut device,
            &AtomicBool::new(false),
            || stats_reads += 1,
            |_| {
                delivered += 1;
                Ok(())
            },
        );
        let intervals = started.elapsed().as_nanos() / POLL_INTERVAL.as_nanos();
        assert_eq!(delivered, 1000);
        assert!(
            (1..=intervals + 1).contains(&u128::from(stats_reads)),
            "{stats_reads} reads of the statistics in {intervals} intervals"
        );
    }

    /// A reader that waits three poll intervals and then finds eight events
    /// reads the statistics after each wait, and once more after the last
    /// read, when the loop has ended. The test goes by where in the script
    /// each read of the statistics fell: a machine that stalls for a whole
    /// interval adds one somewhere else, and that is the clock doing its
    /// job. However many there are, the periodic ones cannot outnumber the
    /// poll intervals that went by, which is what catches a reader that
    /// stops resetting its clock and samples on every pass.
    #[test]
    fn the_statistics_are_read_after_each_wait_and_once_at_the_end() {
        let shutdown = AtomicBool::new(false);
        let made = std::cell::Cell::new(0);
        // Twelve reads: three that would block, eight events, and the one
        // that finds the script over and asks for the stop.
        let mut acts: Vec<Act> = (0..3).map(|_| Act::Fail(ErrorKind::WouldBlock)).collect();
        acts.extend((1..=8).map(|device| Act::Give(event(b'C', 0x81, device, 1, 0, 64, &[]))));
        let mut source = Script {
            acts: acts.into(),
            shutdown: &shutdown,
            made: &made,
        };
        // The number of reads made when each read of the statistics fell.
        let mut stats_after = Vec::new();
        let mut delivered = 0u32;
        let started = Instant::now();
        BinaryReader::with_path(1, PathBuf::from("unused"), true).read_from(
            &mut source,
            &shutdown,
            || stats_after.push(made.get()),
            |_| {
                delivered += 1;
                Ok(())
            },
        );
        let intervals = started.elapsed().as_nanos() / POLL_INTERVAL.as_nanos();
        assert_eq!(delivered, 8);
        let times = |reads: usize| stats_after.iter().filter(|&&at| at == reads).count();
        for waited in 1..=3 {
            assert_eq!(times(waited), 1, "after wait {waited}: {stats_after:?}");
        }
        assert_eq!(times(12), 1, "once the loop has ended: {stats_after:?}");
        assert_eq!(stats_after.last(), Some(&12), "{stats_after:?}");
        assert!(
            stats_after.len() as u128 <= intervals + 1,
            "{stats_after:?} in {intervals} intervals"
        );
    }

    /// A regular file has no usbmon ring: the ladder and stats ioctls fail
    /// with ENOTTY and are ignored, so a fixture-driven read still delivers
    /// every event and reports zero kernel drops through the counter.
    #[test]
    fn fixture_reads_report_zero_kernel_drops() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        let mut stream = Vec::new();
        stream.extend(event(b'C', 0x81, 3, 1, 0, 512, &[]));
        stream.extend(event(b'C', 0x81, 3, 1, 0, 256, &[]));
        std::fs::write(&path, &stream).unwrap();

        let reader = BinaryReader::with_path(1, path, false);
        let kernel_dropped = AtomicU64::new(0);
        let mut delivered = 0;
        reader
            .read_packets(&AtomicBool::new(false), &kernel_dropped, |_| {
                delivered += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered, 2);
        assert_eq!(kernel_dropped.load(Ordering::Relaxed), 0);
    }
}

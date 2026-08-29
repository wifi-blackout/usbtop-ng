//! Reads usbmon's binary interface through its mmap ring and
//! `MON_IOCX_MFETCH`, instead of `read(2)`.
//!
//! [`binary::BinaryReader`](super::binary::BinaryReader) copies each event's
//! header with `read(2)` and then copies and discards its captured payload in
//! chunks to stay framed for the next header — real work spent on bytes the
//! caller never wants. The kernel's mmap interface (see
//! `Documentation/usb/usbmon.rst`) instead maps the ring read-only into this
//! process and hands back byte *offsets* of completed events through
//! `MON_IOCX_MFETCH`; the payload is simply never read. Only the 48-byte
//! header prefix at each offset is copied, and
//! [`parse_binary_header`](super::binary::parse_binary_header) — the same
//! function the read()-based reader uses — decodes it, so the two readers stay
//! byte-for-byte consistent in what they extract from an event.
//!
//! The ioctl numbers and struct layouts below were verified against a live
//! `/dev/usbmon1` (see `tmp/specs/usbmon-mmap.md`); [`ioctl_numbers_match_the_verified_constants`](tests::ioctl_numbers_match_the_verified_constants)
//! pins them.

use std::io;
use std::mem::size_of;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{anyhow, Result};
use log::{debug, error};

use super::binary::{parse_binary_header, HEADER_LEN};
use super::parser::UsbPacket;
use super::{open_nonblocking, POLL_INTERVAL};

/// Number of ring offsets fetched per `MON_IOCX_MFETCH` call. Small enough to
/// live on the stack, large enough that a busy bus does not need a syscall per
/// event.
const OFFSETS_CAP: usize = 64;

// --- ioctl numbers, derived rather than hard-coded ---------------------

/// `_IOC_NONE`: no argument copied either direction.
const IOC_NONE: u32 = 0;
/// `_IOC_WRITE`: the caller's struct is copied into the kernel.
const IOC_WRITE: u32 = 1;
/// `_IOC_READ`: the kernel's struct is copied back to the caller.
const IOC_READ: u32 = 2;

/// usbmon's ioctl magic byte (`'\x92'` in the kernel headers).
const USBMON_IOC_MAGIC: u32 = 0x92;

/// asm-generic `_IOC(dir, type, nr, size)`: the same formula the kernel uses
/// to build ioctl request numbers, so ours match without copying literals out
/// of a header. `size` must come from `size_of::<T>()` for any struct-bearing
/// request, since it differs between 32- and 64-bit targets (a pointer field
/// changes size) and the kernel decodes it out of the request number to size
/// its `copy_from_user`/`copy_to_user`.
const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (ty << 8) | nr
}

/// Returns the ring's size in bytes as the ioctl's own return value (it takes
/// no argument, so there is nothing to `_IOR`/`_IOW`).
const MON_IOCQ_RING_SIZE: u32 = ioc(IOC_NONE, USBMON_IOC_MAGIC, 5, 0);

/// Releases previously fetched events back to the ring by count.
/// [`MonBinMfetch::nflush`] does this same job between two fetches; this
/// stand-alone form is what [`read_packets`](MmapReader::read_packets) calls
/// once at the end, to release the final batch a fetch already handed out but
/// no further fetch will ever flush.
const MON_IOCH_MFLUSH: u32 = ioc(IOC_NONE, USBMON_IOC_MAGIC, 8, 0);

/// Reads a [`MonBinStats`]: `queued` and kernel-side `dropped` counts.
const MON_IOCG_STATS: u32 = ioc(
    IOC_READ,
    USBMON_IOC_MAGIC,
    3,
    size_of::<MonBinStats>() as u32,
);

/// Fetches a batch of ring offsets via a [`MonBinMfetch`]. A `fn` rather than
/// a `const`, matching the verified spec: the struct's size (and therefore
/// this number) depends on the target's pointer width through the `offvec`
/// field, so the value must be computed per-build rather than pinned once.
fn mon_iocx_mfetch() -> u32 {
    ioc(
        IOC_READ | IOC_WRITE,
        USBMON_IOC_MAGIC,
        7,
        size_of::<MonBinMfetch>() as u32,
    )
}

// --- kernel structs (native endian, exact layout) -----------------------

/// Argument to `MON_IOCX_MFETCH`. `offvec` points at a caller-owned array the
/// kernel fills with event byte offsets; `nfetch` is capacity in, fetched
/// count out; `nflush` is the count from the *previous* fetch to release back
/// to the ring (0 on the first call).
#[repr(C)]
struct MonBinMfetch {
    offvec: *mut u32,
    nfetch: u32,
    nflush: u32,
}

/// Argument to `MON_IOCG_STATS`.
#[repr(C)]
struct MonBinStats {
    queued: u32,
    dropped: u32,
}

// --- the mapped ring ------------------------------------------------------

/// Owns an `mmap`'d usbmon ring and `munmap`s it on drop, so every exit path
/// out of [`MmapReader::read_packets`] (shutdown, a callback error, a fatal
/// ioctl error) releases the mapping exactly once.
struct RingMapping {
    ptr: *mut libc::c_void,
    len: usize,
}

impl RingMapping {
    /// Maps `len` bytes of `fd` read-only and shared. `len` must be the value
    /// `MON_IOCQ_RING_SIZE` returned for this same fd.
    fn map(fd: RawFd, len: usize) -> io::Result<Self> {
        // SAFETY: `fd` is a valid, open descriptor for the usbmon ring device
        // for the duration of this call (the caller keeps its owning `File`
        // alive); `len` is a size this same fd already reported via
        // `MON_IOCQ_RING_SIZE`. `PROT_READ | MAP_SHARED` requests a read-only
        // mapping, so this process can never write into kernel-owned ring
        // memory. The returned pointer is checked against `MAP_FAILED` before
        // it is trusted as a valid mapping.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(RingMapping { ptr, len })
    }

    /// The mapped bytes, valid for as long as `self` is not dropped.
    fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` was returned by a successful `mmap` of exactly `len`
        // read-only bytes in `map`, above, and nothing in this process ever
        // writes through it (the mapping is `PROT_READ`, and no other code in
        // this module holds a mutable pointer to it). The returned slice's
        // lifetime is tied to `&self`, so it cannot outlive the mapping.
        unsafe { std::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) }
    }
}

impl Drop for RingMapping {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` are exactly what the successful `mmap` in `map`
        // returned and was asked to map; `RingMapping` has no `Clone`/`Copy`
        // impl and this is its only `Drop`, so this fd/address range is
        // unmapped exactly once.
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

// --- the pure ring walk (hermetically tested) -----------------------------

/// Bounds-checks and parses every offset in `offsets` against `ring`, calling
/// `emit` once per event `parse_binary_header` recognizes.
///
/// Only the 48 header bytes at each offset are ever read — the captured
/// payload that follows them in the ring is never touched, which is the whole
/// point of the mmap interface over `read(2)`. An offset whose header would
/// run past `ring`'s end is skipped rather than read out of bounds: the
/// kernel is trusted for the ring's *contents*, but a defensive bounds check
/// costs nothing and turns a hypothetical kernel bug into a dropped event
/// instead of undefined behaviour.
fn packets_from_offsets(ring: &[u8], offsets: &[u32], mut emit: impl FnMut(UsbPacket)) {
    for &off in offsets {
        let start = off as usize;
        let Some(end) = start.checked_add(HEADER_LEN) else {
            continue;
        };
        if end > ring.len() {
            continue;
        }
        let mut header = [0u8; HEADER_LEN];
        header.copy_from_slice(&ring[start..end]);
        if let Some((packet, _len_cap)) = parse_binary_header(&header) {
            emit(packet);
        }
    }
}

// --- syscall helpers --------------------------------------------------

/// `MON_IOCQ_RING_SIZE`: the ring's size in bytes, returned as the ioctl's own
/// return value rather than through an output argument.
fn ring_size(fd: RawFd) -> io::Result<usize> {
    // SAFETY: `fd` is a valid, open descriptor for the duration of this call
    // (owned by the caller's `File`). `MON_IOCQ_RING_SIZE` takes no argument,
    // so the last parameter is inert; the kernel reports the size through the
    // call's return value, which is checked for the negative-on-error
    // convention before being trusted.
    let ret = unsafe { libc::ioctl(fd, MON_IOCQ_RING_SIZE as libc::c_ulong, 0) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret as usize)
}

/// `MON_IOCX_MFETCH`: flushes the previous batch (`nflush`) and fetches up to
/// [`OFFSETS_CAP`] new event offsets into `offsets`, returning how many were
/// fetched.
fn mfetch(fd: RawFd, offsets: &mut [u32; OFFSETS_CAP], nflush: u32) -> io::Result<u32> {
    let mut req = MonBinMfetch {
        offvec: offsets.as_mut_ptr(),
        nfetch: offsets.len() as u32,
        nflush,
    };
    // SAFETY: `fd` is a valid, open descriptor for the duration of this call.
    // `req.offvec` points at `offsets`, a stack array of `OFFSETS_CAP` `u32`s
    // that outlives this call and whose length matches `req.nfetch`, so the
    // kernel writes at most `offsets.len()` entries into it. `&mut req` is a
    // valid pointer to a correctly `#[repr(C)]`-sized `MonBinMfetch` for the
    // whole call; the kernel reads `nflush`/`offvec`/`nfetch` from it and
    // writes the fetched count back into `nfetch` in place.
    let ret = unsafe {
        libc::ioctl(
            fd,
            mon_iocx_mfetch() as libc::c_ulong,
            std::ptr::addr_of_mut!(req),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    if req.nfetch as usize > offsets.len() {
        // Defense against a buggy or hostile kernel: `nfetch` must never
        // exceed the capacity handed in via the same field, but trusting it
        // anyway would let the caller's `offsets[..n]` slice panic instead of
        // failing cleanly.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MON_IOCX_MFETCH returned more events than the offset buffer holds",
        ));
    }
    Ok(req.nfetch)
}

/// `MON_IOCH_MFLUSH`: releases `count` previously fetched events back to the
/// ring. Unlike [`mfetch`] and [`stats`], this request carries no direction
/// bits (`_IO`, not `_IOR`/`_IOWR`), so the kernel takes `count` as a plain
/// integer argument rather than dereferencing a pointer.
fn mflush(fd: RawFd, count: u32) -> io::Result<()> {
    // SAFETY: `fd` is a valid, open descriptor for the duration of this call.
    // `MON_IOCH_MFLUSH` is a direction-less `_IO` request, so the kernel reads
    // `count` back out of the syscall's argument register as a plain integer;
    // there is no user-space buffer for it to read through or write into, so
    // nothing here can be read or written out of bounds.
    let ret = unsafe { libc::ioctl(fd, MON_IOCH_MFLUSH as libc::c_ulong, count as libc::c_ulong) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `MON_IOCG_STATS`: the kernel's queued/dropped counters for this ring.
fn stats(fd: RawFd) -> io::Result<MonBinStats> {
    let mut stats = MonBinStats {
        queued: 0,
        dropped: 0,
    };
    // SAFETY: `fd` is a valid, open descriptor for the duration of this call.
    // `&mut stats` points at a stack value exactly `size_of::<MonBinStats>()`
    // bytes, matching the size `MON_IOCG_STATS` was built with, so the
    // kernel's write back into it cannot overrun.
    let ret = unsafe {
        libc::ioctl(
            fd,
            MON_IOCG_STATS as libc::c_ulong,
            std::ptr::addr_of_mut!(stats),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stats)
}

/// Park for one poll interval unless shutdown was requested. Returns `false`
/// when the caller should stop instead of retrying.
///
/// Mirrors [`super::binary::BinaryReader`]'s private `park` helper rather than
/// reusing it: the two readers share a contract (bound shutdown latency to one
/// [`POLL_INTERVAL`]), not an implementation worth exporting across modules.
fn park(shutdown: &AtomicBool) -> bool {
    if shutdown.load(Ordering::Relaxed) {
        return false;
    }
    std::thread::sleep(POLL_INTERVAL);
    true
}

// --- the reader -------------------------------------------------------

/// Reads usbmon's binary interface through its mmap ring, so an event's
/// captured payload is never copied — only the 48-byte header prefix that
/// [`parse_binary_header`] decodes.
///
/// Same shutdown/poll contract as
/// [`BinaryReader`](super::binary::BinaryReader): the device is opened
/// non-blocking and a `WouldBlock` fetch parks the thread for one
/// [`POLL_INTERVAL`] before checking `shutdown` again, so `stop()` joins
/// within one interval and the usbmon module can unload.
#[derive(Debug, Clone)]
pub struct MmapReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool,
}

impl MmapReader {
    pub fn new(bus_id: u8) -> Self {
        Self {
            bus_id,
            path: PathBuf::from(format!("/dev/usbmon{}", bus_id)),
            follow: true,
        }
    }

    /// Test seam: point the reader at a fixture path, and optionally disable
    /// follow mode so a fetch that comes back empty ends the loop instead of
    /// parking forever.
    #[cfg(test)]
    pub fn with_path(bus_id: u8, path: PathBuf, follow: bool) -> Self {
        Self {
            bus_id,
            path,
            follow,
        }
    }

    /// True when `path` opens *and* both `MON_IOCQ_RING_SIZE` and `mmap`
    /// succeed on it — i.e. the mmap interface is actually usable here, not
    /// just present. The probe's file handle and mapping are dropped before
    /// this returns, so a probe never pins the usbmon module the way a
    /// running reader would.
    pub fn probe(path: &Path) -> bool {
        let file = match open_nonblocking(path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let fd = file.as_raw_fd();
        let ring_len = match ring_size(fd) {
            Ok(n) if n > 0 => n,
            _ => return false,
        };
        RingMapping::map(fd, ring_len).is_ok()
    }

    /// Read loop over the usbmon mmap ring. Runs to completion on the calling
    /// thread; callers that want this alongside other work should spawn it on
    /// a dedicated thread.
    ///
    /// `shutdown` is polled whenever a fetch comes back empty, so a caller can
    /// stop the loop within one [`POLL_INTERVAL`] and join the thread. Once
    /// the loop ends (for any reason) `MON_IOCG_STATS` is read once and its
    /// `dropped` count is added to `kernel_dropped` — kernel-side drops the
    /// `read()`-based reader has no way to see.
    ///
    /// A callback `Err` stops the loop early and still returns `Ok(())`,
    /// matching `BinaryReader`.
    pub fn read_packets<F>(
        &self,
        shutdown: &AtomicBool,
        kernel_dropped: &AtomicU64,
        mut callback: F,
    ) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        debug!("Starting mmap packet capture from {}", self.path.display());

        let file = open_nonblocking(&self.path)
            .map_err(|e| anyhow!("Failed to open {}: {}", self.path.display(), e))?;
        let fd = file.as_raw_fd();
        let ring_len = ring_size(fd)
            .map_err(|e| anyhow!("MON_IOCQ_RING_SIZE on {}: {}", self.path.display(), e))?;
        if ring_len == 0 {
            return Err(anyhow!(
                "MON_IOCQ_RING_SIZE on {} reported an empty ring",
                self.path.display()
            ));
        }
        let mapping = RingMapping::map(fd, ring_len)
            .map_err(|e| anyhow!("mmap {}: {}", self.path.display(), e))?;
        let ring = mapping.as_slice();

        let mut offsets = [0u32; OFFSETS_CAP];
        let mut nflush = 0u32;

        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            match mfetch(fd, &mut offsets, nflush) {
                Ok(n) => {
                    // Flush this batch on the *next* call, whether or not it
                    // turned out to hold any events.
                    nflush = n;
                    if n == 0 {
                        if !self.follow || !park(shutdown) {
                            break;
                        }
                        continue;
                    }

                    // A callback `Err` must stop calling the callback for the
                    // rest of this batch. `packets_from_offsets`'s `emit`
                    // closure returns nothing, so the remaining offsets in the
                    // batch are still parsed (cheap, read-only, no side
                    // effects) but no longer delivered once `stop` is set.
                    let mut stop = false;
                    packets_from_offsets(ring, &offsets[..n as usize], |packet| {
                        if stop {
                            return;
                        }
                        if let Err(e) = callback(packet) {
                            debug!("Packet callback error: {}", e);
                            stop = true;
                        }
                    });
                    if stop {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if !self.follow || !park(shutdown) {
                        break;
                    }
                }
                Err(e) => {
                    error!("MON_IOCX_MFETCH on {}: {}", self.path.display(), e);
                    break;
                }
            }
        }

        // The last successful fetch's batch is only ever flushed by the
        // *next* fetch's `nflush` field — and there is no next fetch once the
        // loop above has ended. Release it explicitly so this reader does not
        // leave events permanently marked "fetched but never flushed" in a
        // ring another reader could reopen after this fd closes.
        if nflush > 0 {
            if let Err(e) = mflush(fd, nflush) {
                debug!("MON_IOCH_MFLUSH on {}: {}", self.path.display(), e);
            }
        }

        match stats(fd) {
            Ok(s) => {
                kernel_dropped.fetch_add(u64::from(s.dropped), Ordering::Relaxed);
            }
            Err(e) => {
                debug!("MON_IOCG_STATS on {}: {}", self.path.display(), e);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a 64-byte `mon_bin_hdr` at `off` in `ring` (its first 48 bytes
    /// are the same layout `binary.rs`'s `event()` fixture builds), followed
    /// by `payload`. Mirrors the subset of fields `parse_binary_header` reads:
    /// `type@8`, `epnum@10` (direction bit set, an IN endpoint), `devnum@11`,
    /// `busnum@12`, `flag_setup@14` (not captured), `len_urb@32`.
    fn write_ring_event(
        ring: &mut [u8],
        off: usize,
        t: u8,
        devnum: u8,
        busnum: u16,
        len_urb: u32,
        payload: &[u8],
    ) {
        let mut hdr = [0u8; 64];
        hdr[0..8].copy_from_slice(&0xdeadbeefu64.to_ne_bytes());
        hdr[8] = t;
        hdr[9] = 3; // xfer_type: Bulk
        hdr[10] = 0x81; // direction bit set (IN), endpoint 1
        hdr[11] = devnum;
        hdr[12..14].copy_from_slice(&busnum.to_ne_bytes());
        hdr[14] = 1; // flag_setup: not a captured setup packet
        hdr[32..36].copy_from_slice(&len_urb.to_ne_bytes());
        ring[off..off + 64].copy_from_slice(&hdr);
        if !payload.is_empty() {
            ring[off + 64..off + 64 + payload.len()].copy_from_slice(payload);
        }
    }

    #[test]
    fn ring_walk_parses_headers_and_never_reads_payload() {
        let mut ring = vec![0u8; 4096];
        // Poison payload bytes that would corrupt data_length if they were
        // ever mistaken for header bytes or otherwise read.
        write_ring_event(&mut ring, 0, b'C', 4, 1, 1000, &[0xFF; 64]);
        write_ring_event(&mut ring, 128, b'C', 4, 1, 500, &[0xFF; 64]);

        let mut got = Vec::new();
        packets_from_offsets(&ring, &[0, 128], |p| got.push(p));

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data_length, 1000);
        assert_eq!(got[1].data_length, 500);
        assert_eq!(got[0].device_id, 4);
        assert_eq!(got[0].bus_id, 1);
    }

    #[test]
    fn ring_walk_skips_filler_and_unknown_types() {
        let mut ring = vec![0u8; 4096];
        write_ring_event(&mut ring, 0, b'@', 0, 0, 0, &[]); // filler
        write_ring_event(&mut ring, 128, b'C', 3, 1, 64, &[]);

        let mut n = 0;
        packets_from_offsets(&ring, &[0, 128], |_| n += 1);
        assert_eq!(n, 1, "filler skipped, callback kept");
    }

    #[test]
    fn ring_walk_bounds_checks_a_bad_offset() {
        let ring = vec![0u8; 100];
        let mut n = 0;
        packets_from_offsets(&ring, &[80], |_| n += 1); // 80 + 48 > 100
        assert_eq!(
            n, 0,
            "an offset whose header runs past the ring is skipped, not an OOB read"
        );
    }

    #[test]
    fn ring_walk_handles_an_overflowing_offset_without_panicking() {
        let ring = vec![0u8; 100];
        let mut n = 0;
        // On a 32-bit target `u32::MAX as usize + HEADER_LEN` would overflow
        // `usize` outright; `checked_add` must catch that rather than panic
        // or wrap into an in-bounds-looking value.
        packets_from_offsets(&ring, &[u32::MAX], |_| n += 1);
        assert_eq!(n, 0);
    }

    #[test]
    fn ioctl_numbers_match_the_verified_constants() {
        assert_eq!(MON_IOCQ_RING_SIZE, 0x9205);
        assert_eq!(MON_IOCH_MFLUSH, 0x9208);
        assert_eq!(MON_IOCG_STATS, 0x8008_9203);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(mon_iocx_mfetch(), 0xc010_9207);
    }

    #[test]
    fn new_builds_the_default_device_path() {
        let reader = MmapReader::new(3);
        assert_eq!(reader.bus_id, 3);
        assert_eq!(reader.path, PathBuf::from("/dev/usbmon3"));
    }

    #[test]
    fn probe_is_false_for_a_path_that_will_not_open() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");
        assert!(!MmapReader::probe(&missing));
    }

    /// A plain regular file opens fine under `open_nonblocking` — the same as
    /// a live `/dev/usbmonN` would — but the kernel answers `MON_IOCQ_RING_SIZE`
    /// on it with `ENOTTY` (verified above by hand), exactly as it would for
    /// any non-usbmon device. This is what makes a bus without the mmap
    /// interface (or without a live device to test against at all) probe
    /// false and fall back cleanly, without needing real hardware to prove it.
    #[test]
    fn probe_is_false_for_a_device_that_does_not_support_the_usbmon_ioctls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("not-usbmon");
        std::fs::write(&path, []).unwrap();
        assert!(!MmapReader::probe(&path));
    }

    #[test]
    fn read_packets_errors_when_the_device_will_not_open() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");
        let reader = MmapReader::with_path(1, missing, false);
        let shutdown = AtomicBool::new(false);
        let kernel_dropped = AtomicU64::new(0);
        assert!(reader
            .read_packets(&shutdown, &kernel_dropped, |_| Ok(()))
            .is_err());
    }

    /// Mirrors `probe_is_false_for_a_device_that_does_not_support_the_usbmon_ioctls`:
    /// `read_packets` must surface the same `MON_IOCQ_RING_SIZE` failure as an
    /// `Err`, matching `BinaryReader`'s contract that a setup failure is
    /// reported rather than silently producing zero packets.
    #[test]
    fn read_packets_errors_when_the_device_does_not_support_the_usbmon_ioctls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("not-usbmon");
        std::fs::write(&path, []).unwrap();
        let reader = MmapReader::with_path(1, path, false);
        let shutdown = AtomicBool::new(false);
        let kernel_dropped = AtomicU64::new(0);
        assert!(reader
            .read_packets(&shutdown, &kernel_dropped, |_| Ok(()))
            .is_err());
        assert_eq!(
            kernel_dropped.load(Ordering::Relaxed),
            0,
            "setup failed before any ring existed to report stats for"
        );
    }
}

#[cfg(all(test, feature = "integration"))]
mod integration_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Requires: usbmon loaded, `/dev/usbmon0` openable (typically root). This
    /// proves the syscalls wire up against the real device — `mmap`,
    /// `MON_IOCX_MFETCH`, `MON_IOCG_STATS` — not that traffic is flowing: the
    /// hermetic tests above already prove the parsing, and the host may be
    /// completely idle for the whole run.
    /// Run: cargo test --features integration
    #[test]
    fn live_ring_reads_without_error() {
        let path = Path::new("/dev/usbmon0");
        if open_nonblocking(path).is_err() {
            eprintln!("usbmon0 not openable; live mmap ring check skipped");
            return;
        }

        if !MmapReader::probe(path) {
            eprintln!("usbmon0 not mmap-capable; live mmap ring check skipped");
            return;
        }

        let reader = MmapReader::with_path(0, path.to_path_buf(), true);
        let shutdown = Arc::new(AtomicBool::new(false));
        let kernel_dropped = Arc::new(AtomicU64::new(0));
        let loop_shutdown = Arc::clone(&shutdown);
        let loop_dropped = Arc::clone(&kernel_dropped);
        let handle = std::thread::spawn(move || {
            reader
                .read_packets(&loop_shutdown, &loop_dropped, |_| Ok(()))
                .expect("read_packets must not error against a live device")
        });

        std::thread::sleep(Duration::from_millis(200));
        shutdown.store(true, Ordering::Relaxed);
        handle.join().expect("reader thread must not panic");

        // Readable, not necessarily nonzero: an idle host drops nothing.
        let _ = kernel_dropped.load(Ordering::Relaxed);
    }
}

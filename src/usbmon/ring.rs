//! The usbmon binary interface's ioctl surface (`drivers/usb/mon/mon_bin.c`):
//! request numbers derived the way the kernel derives them, the argument
//! structs in kernel layout, the ring-size ladder, and the drop counter.
//! Shared by the mmap-ring reader ([`super::mmap_ring`]), the read()-based
//! reader ([`super::binary`]), and the fixture capturer, so every consumer of
//! `/dev/usbmonN` asks for the same enlarged ring and reports kernel drops
//! the same way. The read()-based path is not exempt: the ring is the same
//! per-open buffer whichever way it is drained, and on the default ~300 KiB
//! ring one isochronous callback can occupy a fifth of it.
//!
//! The ioctl numbers and struct layouts were verified against a live
//! `/dev/usbmon1`; [`tests::ioctl_numbers_match_the_verified_constants`]
//! pins them.

use std::io;
use std::mem::size_of;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use log::debug;

/// Ring sizes to request via [`MON_IOCT_RING_SIZE`] before mapping, largest
/// first. The kernel's default ring is ~300 KiB, which overflows within about
/// a millisecond at USB3 throughput -- a 336 MB/s bulk read dropped ~87% of
/// packets on it (a smaller 16 MiB ring cut that ~15x but still dropped a
/// couple thousand events) -- so a reader thread that is briefly descheduled
/// loses events the kernel had nowhere to keep. 64 MiB is this kernel's own
/// maximum and, live-tested, drops zero packets across a full 4 GiB read at
/// 5 Gbps and a 12 GiB read at 10 Gbps, matching a concurrent eBPF capture to
/// the byte.
///
/// The list is a ladder, not a single value, because the kernel does NOT clamp
/// an over-`BUFF_MAX` request down: it rejects it outright with `EINVAL` and
/// leaves the ring at its previous (default) size. A lone 64 MiB request would
/// therefore silently keep the tiny default ring -- reintroducing the drop bug
/// -- on any kernel whose `BUFF_MAX` is below 64 MiB. Stepping down lands on
/// the largest ring the running kernel actually accepts. Each reader that maps
/// a ring requests its own, so in the (rare) non-aggregate multi-bus topology
/// this is per-bus kernel memory; the common case is a single aggregate reader
/// on `usbmon0` (one ring). All sizes failing just leaves the default ring --
/// best-effort, never fatal.
pub(crate) const RING_SIZE_LADDER: [usize; 4] = [
    64 * 1024 * 1024,
    32 * 1024 * 1024,
    16 * 1024 * 1024,
    8 * 1024 * 1024,
];

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
///
/// This is the asm-generic layout, correct for the x86 and ARM targets
/// usbtop-ng ships on; powerpc, mips, and sparc pack `dir`/`size` into a
/// different bit layout and would need their own `ioc`.
const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (ty << 8) | nr
}

/// Returns the ring's size in bytes as the ioctl's own return value (it takes
/// no argument, so there is nothing to `_IOR`/`_IOW`).
pub(crate) const MON_IOCQ_RING_SIZE: u32 = ioc(IOC_NONE, USBMON_IOC_MAGIC, 5, 0);

/// Sets the ring's byte size, passed straight as the ioctl argument (a
/// directionless `_IO`, like [`MON_IOCH_MFLUSH`], so no size is packed into
/// the request number). The kernel page-aligns the request and clamps it to
/// its own `[BUFF_MIN, BUFF_MAX]`, so [`ring_size`] afterward reports the
/// value actually set. Must be issued before `mmap`, since the kernel
/// reallocates the ring on resize.
pub(crate) const MON_IOCT_RING_SIZE: u32 = ioc(IOC_NONE, USBMON_IOC_MAGIC, 4, 0);

/// Releases previously fetched events back to the ring by count.
/// [`MonBinMfetch::nflush`] does this same job between two fetches; this
/// stand-alone form is what [`read_packets`](super::mmap_ring::MmapReader::read_packets) calls
/// once at the end, to release the final batch a fetch already handed out but
/// no further fetch will ever flush.
pub(crate) const MON_IOCH_MFLUSH: u32 = ioc(IOC_NONE, USBMON_IOC_MAGIC, 8, 0);

/// Reads a [`MonBinStats`]: `queued` and kernel-side `dropped` counts.
pub(crate) const MON_IOCG_STATS: u32 = ioc(
    IOC_READ,
    USBMON_IOC_MAGIC,
    3,
    size_of::<MonBinStats>() as u32,
);

/// Fetches a batch of ring offsets via a [`MonBinMfetch`]. A `fn` rather than
/// a `const`, matching the verified spec: the struct's size (and therefore
/// this number) depends on the target's pointer width through the `offvec`
/// field, so the value must be computed per-build rather than pinned once.
pub(crate) fn mon_iocx_mfetch() -> u32 {
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
pub(crate) struct MonBinMfetch {
    pub(crate) offvec: *mut u32,
    pub(crate) nfetch: u32,
    pub(crate) nflush: u32,
}

/// Argument to `MON_IOCG_STATS`.
#[derive(Debug)]
#[repr(C)]
pub(crate) struct MonBinStats {
    pub(crate) queued: u32,
    pub(crate) dropped: u32,
}

// --- syscall helpers --------------------------------------------------

/// The type `libc::ioctl` declares for its request parameter, which differs
/// by libc: glibc takes `c_ulong`, musl takes `c_int` (both wrap the same
/// kernel `unsigned int`, so the `as` casts below are bit-identical either
/// way). Static armv6 builds for the Pi Zero link musl, so the request casts
/// go through this alias rather than hard-coding glibc's `c_ulong`.
#[cfg(target_env = "musl")]
pub(crate) type IoctlRequest = libc::c_int;
#[cfg(not(target_env = "musl"))]
pub(crate) type IoctlRequest = libc::c_ulong;

/// `MON_IOCQ_RING_SIZE`: the ring's size in bytes, returned as the ioctl's own
/// return value rather than through an output argument.
pub(crate) fn ring_size(fd: RawFd) -> io::Result<usize> {
    // SAFETY: `fd` is a valid, open descriptor for the duration of this call
    // (owned by the caller's `File`). `MON_IOCQ_RING_SIZE` takes no argument,
    // so the last parameter is inert; the kernel reports the size through the
    // call's return value, which is checked for the negative-on-error
    // convention before being trusted.
    let ret = unsafe { libc::ioctl(fd, MON_IOCQ_RING_SIZE as IoctlRequest, 0) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret as usize)
}

/// `MON_IOCT_RING_SIZE`: request `bytes` for this fd's ring, best-effort. On
/// success the kernel page-aligns `bytes` and reallocates the ring; read
/// [`ring_size`] afterward for the exact value it set. A request above the
/// kernel's `BUFF_MAX` is rejected with `EINVAL` and leaves the ring at its
/// previous size (the kernel does not clamp) -- see [`RING_SIZE_LADDER`] for
/// why the caller steps down rather than trusting one large request. Returns
/// the ioctl error (e.g. `ENOTTY` on a kernel without this request, or
/// `EINVAL`) so the caller can carry on with the default ring rather than fail
/// the whole reader.
pub(crate) fn set_ring_size(fd: RawFd, bytes: usize) -> io::Result<()> {
    // SAFETY: `fd` is a valid, open descriptor for the duration of this call.
    // `MON_IOCT_RING_SIZE` is a directionless `_IO` request that reads its size
    // straight from the argument (the same shape as `MON_IOCH_MFLUSH`), so
    // passing `bytes` as the argument is correct and nothing is copied to or
    // from user memory; the return is checked for the negative-on-error
    // convention before being treated as success.
    let ret = unsafe {
        libc::ioctl(
            fd,
            MON_IOCT_RING_SIZE as IoctlRequest,
            bytes as libc::c_ulong,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Step [`RING_SIZE_LADDER`] down largest-first on `fd`, stopping at the
/// first size this kernel accepts. Best-effort: each refusal is debug-logged
/// and the ring is left at whatever size it had -- the default, on a kernel
/// without the ioctl (`ENOTTY`, which is also what a regular fixture file
/// answers) or one that denies the request. Must run before `mmap` on a
/// reader that maps the ring and before the first `read(2)` on one that does
/// not: the kernel reallocates the ring on an accepted request.
pub(crate) fn request_ring_ladder(fd: RawFd, path: &Path) {
    for &target in &RING_SIZE_LADDER {
        match set_ring_size(fd, target) {
            Ok(()) => return,
            Err(e) => debug!("MON_IOCT_RING_SIZE({target}) on {}: {e}", path.display()),
        }
    }
}

/// `MON_IOCG_STATS`: the kernel's queued count and dropped count for this
/// ring. `dropped` is read-and-clear, not cumulative: `mon_bin_ioctl`
/// (`drivers/usb/mon/mon_bin.c`) copies `cnt_lost` out and zeroes it under
/// the same lock, every call, so each call's `dropped` is the count of
/// events lost since the *previous* call on this fd (or since the fd was
/// opened, on the first call) — never a running total to diff against a
/// remembered value.
pub(crate) fn stats(fd: RawFd) -> io::Result<MonBinStats> {
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
            MON_IOCG_STATS as IoctlRequest,
            std::ptr::addr_of_mut!(stats),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stats)
}

/// Adds one `MON_IOCG_STATS` read's `dropped` count into the shared
/// `counter`.
///
/// `dropped` is read-and-clear (see [`stats`]'s doc): each call's value is
/// already just the drops since the previous read on that fd, not a
/// cumulative total, so summing every read's value over the session — never
/// diffing it against a remembered value — gives the exact total lost.
/// `cnt_lost` is per-fd (per-open) kernel-side, so multiple per-bus readers
/// each summing their own reads into this one shared `counter` cannot
/// double-count each other's traffic.
pub(crate) fn add_kernel_drops(counter: &AtomicU64, dropped: u32) {
    if dropped != 0 {
        counter.fetch_add(u64::from(dropped), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    /// A regular file has no usbmon ring: every request in the ladder fails
    /// with ENOTTY, the helper swallows each one, and the size query fails
    /// the same way afterward. This is the fixture-file path every
    /// read()-based test exercises, so it must be silent and harmless.
    #[test]
    fn request_ring_ladder_is_a_no_op_on_a_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("not-usbmon");
        std::fs::write(&path, []).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd();

        request_ring_ladder(fd, &path);

        let err = ring_size(fd).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
        let err = stats(fd).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
    }

    /// `MON_IOCG_STATS` is read-and-clear (see `stats`'s doc), so repeated
    /// equal reads are not "no change" — each one is its own independent
    /// count of drops since the previous read, and must add every time. A
    /// delta-based fold (treating `dropped` as cumulative) would wrongly
    /// publish only the first 5 here; summing gets the true total, 10.
    #[test]
    fn add_kernel_drops_sums_every_read_even_when_the_value_repeats() {
        let counter = AtomicU64::new(0);

        add_kernel_drops(&counter, 0);
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "a zero read adds nothing"
        );

        add_kernel_drops(&counter, 5);
        assert_eq!(counter.load(Ordering::Relaxed), 5, "first read of 5 adds 5");

        add_kernel_drops(&counter, 5);
        assert_eq!(
            counter.load(Ordering::Relaxed),
            10,
            "a second, equal read of 5 must add 5 more, not 0 — each read is \
             independent, not a running total to diff against"
        );
    }

    /// A read-and-clear counter has no notion of "decreasing" — 7 then 3 are
    /// two unrelated counts, not a rollback. Treating them as cumulative and
    /// diffing (`3u32.wrapping_sub(7)`) would wrap to a multi-billion
    /// overcount; summing gets the true total, 10.
    #[test]
    fn add_kernel_drops_handles_a_lower_read_without_wrapping() {
        let counter = AtomicU64::new(0);

        add_kernel_drops(&counter, 7);
        add_kernel_drops(&counter, 3);

        assert_eq!(
            counter.load(Ordering::Relaxed),
            10,
            "7 then 3 are two independent counts (10 total), not a decrease \
             that would underflow a delta"
        );
    }

    /// Each bus's `MmapReader` owns its own fd, and `cnt_lost` is per-fd
    /// kernel-side (see `stats`'s doc), so two readers summing their own
    /// reads into one shared counter must not clobber or double-count each
    /// other's contribution.
    #[test]
    fn add_kernel_drops_from_multiple_readers_sums_into_one_counter() {
        let counter = AtomicU64::new(0);

        // Reader A's fd and reader B's fd each report their own drops.
        add_kernel_drops(&counter, 3); // reader A's first read
        add_kernel_drops(&counter, 2); // reader B's first read
        assert_eq!(
            counter.load(Ordering::Relaxed),
            5,
            "two readers' first reads must sum, not overwrite"
        );

        add_kernel_drops(&counter, 3); // reader A's second read
        add_kernel_drops(&counter, 6); // reader B's second read
        assert_eq!(
            counter.load(Ordering::Relaxed),
            14,
            "further reads from both readers keep summing"
        );
    }

    #[test]
    fn ioctl_numbers_match_the_verified_constants() {
        assert_eq!(MON_IOCQ_RING_SIZE, 0x9205);
        assert_eq!(MON_IOCT_RING_SIZE, 0x9204);
        assert_eq!(MON_IOCH_MFLUSH, 0x9208);
        assert_eq!(MON_IOCG_STATS, 0x8008_9203);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(mon_iocx_mfetch(), 0xc010_9207);
    }
}

//! The opt-in eBPF capture backend (feature `ebpf`).
//!
//! `build.rs` compiles `src/bpf/usbrate.bpf.c` (via
//! `libbpf_cargo::SkeletonBuilder`) into a generated `usbrate.skel.rs` in
//! `OUT_DIR`, and the `include!` below pulls it into this module: everything
//! from `pub use self::imp::*;` on (`UsbrateSkelBuilder`, `OpenUsbrateSkel`,
//! `UsbrateSkel`, the `bytes` map, the `on_giveback` kprobe program, ...) is
//! generated, not hand-written here.
//!
//! [`EbpfSource`] opens, loads, and attaches that skeleton, then turns its
//! monotonic cumulative-bytes map into [`TrafficDelta`]s a poll at a time.
//! `usbmon::monitor` decides whether to use it at all (see
//! `monitor::start_capture`) and, when it does, owns the poller thread and
//! the shutdown/channel plumbing -- this module only has to load the
//! program and answer one poll at a time.

use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapFlags, OpenObject};
use log::warn;

use crate::device::manager::TrafficDelta;
use crate::usbmon::parser::TransferType;
use crate::usbmon::POLL_INTERVAL;

include!(concat!(env!("OUT_DIR"), "/usbrate.skel.rs"));

/// Mirrors `src/bpf/usbrate.bpf.c`'s `struct key_t` field for field:
/// `busnum: u16, devnum: u8, epnum: u8, dir_in: u8, xfer: u8`.
///
/// `#[repr(C)]` with this exact field order needs no padding: the leading
/// `u16` is 2-aligned, every field after it is a `u8` (1-aligned), and the
/// struct's total size (6 bytes) already lands on the 2-byte boundary its
/// own alignment demands -- so there is no gap for the compiler to insert
/// anywhere in the layout, on either side (see
/// `key_size_matches_the_kernel_struct_with_no_padding` below, which pins
/// `size_of::<Key>()` at exactly [`KEY_BYTES`]). That is what makes decoding
/// the raw bytes libbpf-rs's `keys()`/`lookup()` hand back a plain
/// field-by-field read (see [`decode_key`]) rather than needing any
/// `unsafe` reinterpretation of the bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub busnum: u16,
    pub devnum: u8,
    pub epnum: u8,
    pub dir_in: u8,
    pub xfer: u8,
}

/// `size_of::<Key>()`, and the byte count the map's `keys()`/`lookup()` are
/// expected to hand back for this map's key. A named constant rather than a
/// bare `6` so [`decode_key`]'s length guard and the padding-pinning test
/// below read as the same claim.
const KEY_BYTES: usize = 6;

/// `size_of::<u64>()`, the map's value width.
const VALUE_BYTES: usize = 8;

/// Decode one map key's raw bytes into a [`Key`]. `None` when the byte count
/// doesn't match [`KEY_BYTES`] -- defensive against a skeleton/struct
/// mismatch rather than an index panic on live kernel data; every field is
/// read directly off known offsets, so no `unsafe` reinterpretation of the
/// bytes is needed either way.
fn decode_key(bytes: &[u8]) -> Option<Key> {
    if bytes.len() != KEY_BYTES {
        return None;
    }
    Some(Key {
        busnum: u16::from_ne_bytes([bytes[0], bytes[1]]),
        devnum: bytes[2],
        epnum: bytes[3],
        dir_in: bytes[4],
        xfer: bytes[5],
    })
}

/// Decode one map value's raw bytes into the cumulative byte count. `None`
/// on a byte-count mismatch, same defensive contract as [`decode_key`].
fn decode_value(bytes: &[u8]) -> Option<u64> {
    let array: [u8; VALUE_BYTES] = bytes.try_into().ok()?;
    Some(u64::from_ne_bytes(array))
}

/// The map's cumulative bytes for `key` since the last time this was called
/// for it: `Some(current - previous)` (and `last` is updated to `current`)
/// only when the map strictly increased since the last reading; a steady or
/// *decreasing* reading returns `None` and leaves `last` at its old, higher
/// value.
///
/// Leaving `last` untouched on a decrease is what makes a counter reset or
/// wrap safe rather than merely detected: a later reading has to climb back
/// past everything already accounted for before deltas resume, instead of
/// the reset itself briefly reading as one giant (or, if `last` had been
/// lowered instead, `current - 0`) delta. Same "never trust an
/// assumed-monotonic counter's raw value" lesson as the mmap ring's
/// read-and-clear `kdropped` handling.
pub(crate) fn delta_since(last: &mut HashMap<Key, u64>, key: Key, current: u64) -> Option<u64> {
    let previous = last.entry(key).or_insert(0);
    if current > *previous {
        let delta = current - *previous;
        *previous = current;
        Some(delta)
    } else {
        None
    }
}

/// The kernel `dropped` counter's new cumulative total when it has grown since
/// the last reading (updating `last_reported` to it), or `None` otherwise.
/// Mirrors [`delta_since`]'s "report only the change" shape so a full map
/// warns as it worsens, not once every poll -- and, like it, never lowers
/// `last_reported`, so a counter that somehow read backwards cannot re-arm a
/// spurious warning.
pub(crate) fn dropped_growth(last_reported: &mut u64, current: u64) -> Option<u64> {
    if current > *last_reported {
        *last_reported = current;
        Some(current)
    } else {
        None
    }
}

/// `xfer`'s `usb_pipetype` encoding (`(pipe >> 30) & 0x3`; see
/// `src/bpf/usbrate.bpf.c`): 0 Isochronous, 1 Interrupt, 2 Control, 3 Bulk.
/// Any other value -- impossible from a real `pipe` field, but the map
/// stores a plain integer with nothing to validate it against -- maps to
/// `None`, mirroring usbmon's own "transfer type unknown" case rather than
/// dropping the traffic.
pub(crate) fn xfer_to_transfer_type(xfer: u8) -> Option<TransferType> {
    match xfer {
        0 => Some(TransferType::Isochronous),
        1 => Some(TransferType::Interrupt),
        2 => Some(TransferType::Control),
        3 => Some(TransferType::Bulk),
        _ => None,
    }
}

/// Build the [`TrafficDelta`] one map key's fresh delta describes.
fn traffic_delta(key: Key, bytes: u64) -> TrafficDelta {
    TrafficDelta {
        // Every bus number this tool otherwise deals with is a `u8` (see
        // `UsbPacket::bus_id`, `UsbBus::bus_id`); the map key carries a `u16`
        // busnum, so narrow it the same way the usbmon reader does its own
        // u16->u8 busnum (see `binary.rs`): fall back to 0 (an obviously-
        // unknown id) for an out-of-range bus rather than silently wrapping
        // it onto some other real bus's low byte. Realistic hosts never have
        // >255 buses, so this is defensive, not expected.
        bus_id: u8::try_from(key.busnum).unwrap_or(0),
        device_id: key.devnum,
        endpoint: key.epnum,
        dir_in: key.dir_in != 0,
        transfer_type: xfer_to_transfer_type(key.xfer),
        bytes,
    }
}

/// The loaded, attached `usbrate` kprobe program, plus the per-key snapshot
/// [`delta_since`] needs to turn its monotonic map into deltas.
pub struct EbpfSource {
    skel: UsbrateSkel<'static>,
    last: HashMap<Key, u64>,
    /// Last-warned value of the kernel `dropped` map-full counter (see
    /// [`dropped_growth`]), so the warning fires as the loss grows rather than
    /// every poll while a full map keeps losing URBs.
    last_dropped: u64,
}

impl EbpfSource {
    /// Open, load, and attach the `usbrate` skeleton. `Err` on any failure
    /// (missing BTF, the `__usb_hcd_giveback_urb` kprobe symbol
    /// unresolvable, insufficient privilege, ...) -- the caller (see
    /// `monitor::start_capture`) treats that as "eBPF unavailable" and falls
    /// back to the usbmon chain rather than failing the program.
    pub fn load_and_attach() -> Result<Self> {
        // The generated skeleton ties its `'obj` lifetime to this storage:
        // `SkelBuilder::open` takes `&'obj mut MaybeUninit<OpenObject>`, and
        // every type it hands back (`OpenUsbrateSkel<'obj>`,
        // `UsbrateSkel<'obj>`) borrows from it. An `EbpfSource` has to move
        // the loaded, attached skeleton into the poller thread and hold it
        // there until shutdown -- there is no scope short of the process
        // itself to lend it a borrow from. Leaking this one small
        // (one-pointer-ish) `MaybeUninit<OpenObject>` container, once per
        // process run, is the standard way libbpf-rs skeleton consumers
        // give a long-lived skeleton a `'static` lifetime it can cross a
        // thread boundary with (`UsbrateSkel` is generated `Send + Sync`).
        // The real kernel resources -- the loaded program, the map, the
        // attached kprobe link -- are still released normally when `skel`
        // (and so this `EbpfSource`) drops; only this bookkeeping container
        // leaks, and only once.
        let open_object: &'static mut MaybeUninit<OpenObject> =
            Box::leak(Box::new(MaybeUninit::uninit()));
        let open_skel = UsbrateSkelBuilder::default()
            .open(open_object)
            .context("failed to open the usbrate eBPF skeleton")?;
        let mut skel = open_skel
            .load()
            .context("failed to load the usbrate eBPF program (BTF or kernel support missing?)")?;
        skel.attach()
            .context("failed to attach the usbrate kprobe (needs root or CAP_BPF)")?;
        Ok(Self {
            skel,
            last: HashMap::new(),
            last_dropped: 0,
        })
    }

    /// The kernel `dropped` counter's current cumulative total: the number of
    /// URBs whose bytes were lost because the `bytes` map was full (see
    /// `src/bpf/usbrate.bpf.c`). Slot 0 of the single-entry array map. Any read
    /// failure yields `0` -- surfacing the drop count must never crash the
    /// poller, and a map that cannot be read simply reports no growth.
    fn read_dropped(&self) -> u64 {
        let key = 0u32.to_ne_bytes();
        match libbpf_rs::MapCore::lookup(&self.skel.maps.dropped, &key, MapFlags::ANY) {
            Ok(Some(raw)) => decode_value(&raw).unwrap_or(0),
            _ => 0,
        }
    }

    /// One poll pass: read every key currently in the `bytes` map, and hand
    /// [`delta_since`]'s `Some` results to `on_delta` as [`TrafficDelta`]s.
    ///
    /// `keys()`/`lookup()` are called through fully qualified
    /// `MapCore::method(...)` syntax rather than `self.skel.maps.bytes.keys()`
    /// method-call sugar, deliberately: that sugar needs `MapCore` brought
    /// into this module's scope with a plain `use libbpf_rs::MapCore;`, and
    /// the generated skeleton (see the `include!` above) already imports the
    /// same trait *privately* inside its own `imp` module for its own
    /// purposes. With both imports live, rustc's `unused_imports` lint
    /// flags the skeleton's -- generated, not ours to edit -- import as
    /// dead, which fails `-D warnings`. Not importing the trait at all here
    /// and calling it fully qualified sidesteps that without needing an
    /// `#[allow]` anywhere.
    fn poll_once(&mut self, mut on_delta: impl FnMut(TrafficDelta)) {
        // Collected up front rather than iterated lazily: `keys()` borrows
        // the map, and the loop body below needs its own borrow (`lookup`)
        // plus a mutable borrow of `self.last` -- collecting first keeps
        // those borrows from overlapping. `keys()` snapshots the live key
        // set at this instant; the kprobe keeps running concurrently, so a
        // key it adds after this snapshot is simply picked up on the next
        // poll instead.
        let raw_keys: Vec<Vec<u8>> = libbpf_rs::MapCore::keys(&self.skel.maps.bytes).collect();
        for raw_key in raw_keys {
            let Some(key) = decode_key(&raw_key) else {
                continue;
            };
            // A key can vanish between `keys()` and `lookup` in principle
            // (the map API allows concurrent deletes); this program never
            // deletes a key, so in practice this arm is defensive rather
            // than expected, and either way there is nothing to report for
            // it this pass.
            let Ok(Some(raw_value)) =
                libbpf_rs::MapCore::lookup(&self.skel.maps.bytes, &raw_key, MapFlags::ANY)
            else {
                continue;
            };
            let Some(current) = decode_value(&raw_value) else {
                continue;
            };
            if let Some(bytes) = delta_since(&mut self.last, key, current) {
                on_delta(traffic_delta(key, bytes));
            }
        }

        // Surface the bounded map-full loss the kprobe records: when the
        // `bytes` map is full, a new key's URB cannot be accounted, and the
        // rate silently under-reports. Warn as the drop count grows so a too-
        // small map is visible instead of masquerading as low traffic.
        let current_dropped = self.read_dropped();
        if let Some(total) = dropped_growth(&mut self.last_dropped, current_dropped) {
            warn!(
                "eBPF aggregation map full: {total} URB(s) unattributed and undercounted \
                 (the bytes map holds 4096 device/endpoint keys). Traffic is under-reported \
                 until keys free up."
            );
        }
    }

    /// Poll loop mirroring the other readers' shutdown/park contract (see
    /// e.g. `usbmon::binary::BinaryReader::read_packets`): `shutdown` is
    /// checked at least once per [`POLL_INTERVAL`], and the loop returns
    /// promptly once it is set, bounding the caller's join at one interval.
    /// A map lookup never blocks the way a `read(2)` can, so unlike the
    /// file-backed readers this parks unconditionally between passes rather
    /// than only on `WouldBlock`.
    pub fn run(&mut self, shutdown: &AtomicBool, mut on_delta: impl FnMut(TrafficDelta)) {
        loop {
            self.poll_once(&mut on_delta);
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_size_matches_the_kernel_struct_with_no_padding() {
        // Pins the layout claim `Key`'s doc comment makes: a leading `u16`
        // plus four trailing `u8`s needs no padding, so this must be
        // exactly `KEY_BYTES` (6), not 8 (which a naive "round up to the
        // widest field" padding scheme would produce).
        assert_eq!(std::mem::size_of::<Key>(), KEY_BYTES);
    }

    #[test]
    fn decode_key_reads_every_field_from_its_own_byte() {
        // busnum = 0x0102 (little-endian on this host: bytes [0x02, 0x01]),
        // devnum = 4, epnum = 1, dir_in = 1, xfer = 3 (bulk).
        let bytes = [0x02, 0x01, 4, 1, 1, 3];
        let key = decode_key(&bytes).expect("6 bytes decodes");
        assert_eq!(
            key,
            Key {
                busnum: u16::from_ne_bytes([0x02, 0x01]),
                devnum: 4,
                epnum: 1,
                dir_in: 1,
                xfer: 3,
            }
        );
    }

    #[test]
    fn decode_key_rejects_the_wrong_byte_count() {
        assert!(decode_key(&[0u8; 5]).is_none());
        assert!(decode_key(&[0u8; 7]).is_none());
    }

    #[test]
    fn decode_value_round_trips_a_native_endian_u64() {
        let bytes = 0x1122_3344_5566_7788u64.to_ne_bytes();
        assert_eq!(decode_value(&bytes), Some(0x1122_3344_5566_7788));
    }

    #[test]
    fn decode_value_rejects_the_wrong_byte_count() {
        assert!(decode_value(&[0u8; 7]).is_none());
        assert!(decode_value(&[0u8; 9]).is_none());
    }

    #[test]
    fn dropped_growth_reports_only_when_the_counter_climbs() {
        let mut last = 0u64;
        // The first drop warns with the cumulative total.
        assert_eq!(dropped_growth(&mut last, 3), Some(3));
        // A steady reading does not warn again.
        assert_eq!(dropped_growth(&mut last, 3), None);
        // Further loss warns with the new total.
        assert_eq!(dropped_growth(&mut last, 5), Some(5));
        // A backward reading -- never expected from a monotonic counter --
        // neither warns nor lowers the watermark.
        assert_eq!(dropped_growth(&mut last, 4), None);
        assert_eq!(last, 5);
        // The steady-state no-drops case (a zero counter) never warns.
        let mut fresh = 0u64;
        assert_eq!(dropped_growth(&mut fresh, 0), None);
    }

    fn key(devnum: u8) -> Key {
        Key {
            busnum: 1,
            devnum,
            epnum: 1,
            dir_in: true as u8,
            xfer: 3,
        }
    }

    #[test]
    fn delta_since_reports_the_full_value_on_first_sight() {
        let mut last = HashMap::new();
        assert_eq!(delta_since(&mut last, key(1), 500), Some(500));
    }

    #[test]
    fn delta_since_reports_the_delta_when_the_counter_increases() {
        let mut last = HashMap::new();
        assert_eq!(delta_since(&mut last, key(1), 500), Some(500));
        assert_eq!(delta_since(&mut last, key(1), 900), Some(400));
    }

    #[test]
    fn delta_since_reports_none_when_the_counter_is_steady() {
        let mut last = HashMap::new();
        assert_eq!(delta_since(&mut last, key(1), 500), Some(500));
        assert_eq!(
            delta_since(&mut last, key(1), 500),
            None,
            "no new bytes moved, nothing to report"
        );
    }

    /// The critical case: a counter that goes backwards (a reset or wrap)
    /// must never be read as a huge delta, and must not poison future
    /// readings either -- see [`delta_since`]'s doc comment for why leaving
    /// `last` at its old, higher value is what makes that true.
    #[test]
    fn delta_since_reports_none_on_a_decreasing_counter_and_does_not_lower_the_baseline() {
        let mut last = HashMap::new();
        assert_eq!(delta_since(&mut last, key(1), 1_000_000), Some(1_000_000));

        // The map reset (e.g. reloaded) and is climbing back up from zero.
        assert_eq!(
            delta_since(&mut last, key(1), 0),
            None,
            "a decrease must never emit a delta"
        );
        assert_eq!(
            delta_since(&mut last, key(1), 500_000),
            None,
            "still below the old high-water mark: not yet new traffic"
        );
        assert_eq!(
            delta_since(&mut last, key(1), 1_000_000),
            None,
            "back exactly to the old high-water mark: still not a new delta"
        );
        assert_eq!(
            delta_since(&mut last, key(1), 1_000_100),
            Some(100),
            "only bytes genuinely beyond the old high-water mark count"
        );
    }

    #[test]
    fn delta_since_tracks_independent_keys_separately() {
        let mut last = HashMap::new();
        assert_eq!(delta_since(&mut last, key(1), 100), Some(100));
        assert_eq!(
            delta_since(&mut last, key(2), 50),
            Some(50),
            "a different key starts from its own zero baseline"
        );
        assert_eq!(delta_since(&mut last, key(1), 150), Some(50));
    }

    #[test]
    fn xfer_to_transfer_type_maps_every_known_value() {
        assert_eq!(xfer_to_transfer_type(0), Some(TransferType::Isochronous));
        assert_eq!(xfer_to_transfer_type(1), Some(TransferType::Interrupt));
        assert_eq!(xfer_to_transfer_type(2), Some(TransferType::Control));
        assert_eq!(xfer_to_transfer_type(3), Some(TransferType::Bulk));
    }

    #[test]
    fn xfer_to_transfer_type_rejects_an_invalid_value() {
        assert_eq!(xfer_to_transfer_type(4), None);
        assert_eq!(xfer_to_transfer_type(255), None);
    }

    #[test]
    fn traffic_delta_decodes_direction_and_transfer_type() {
        let delta = traffic_delta(
            Key {
                busnum: 1,
                devnum: 4,
                epnum: 2,
                dir_in: 1,
                xfer: 0,
            },
            67_583_256,
        );
        assert_eq!(delta.bus_id, 1);
        assert_eq!(delta.device_id, 4);
        assert_eq!(delta.endpoint, 2);
        assert!(delta.dir_in);
        assert_eq!(delta.transfer_type, Some(TransferType::Isochronous));
        assert_eq!(delta.bytes, 67_583_256);
    }

    /// Mirrors `binary.rs`'s `oversized_busnum_falls_back_to_zero`: a busnum
    /// past `u8::MAX` becomes bus 0 (obviously-unknown), never a wrapped low
    /// byte that would misattribute traffic onto a different real bus.
    #[test]
    fn traffic_delta_falls_back_to_bus_zero_for_an_oversized_busnum() {
        let delta = traffic_delta(
            Key {
                busnum: 257, // low byte 1 -- a naive `as u8` would say bus 1
                devnum: 4,
                epnum: 2,
                dir_in: 1,
                xfer: 0,
            },
            1024,
        );
        assert_eq!(delta.bus_id, 0);
    }

    /// The one path that needs a live kernel: BTF, the
    /// `__usb_hcd_giveback_urb` kprobe symbol, and (in practice) root or
    /// CAP_BPF. Skips gracefully -- rather than failing the suite -- when
    /// any of that is missing, which is the normal case for
    /// `cargo test --features ebpf` run as a plain user. Run as root on a
    /// BTF-enabled kernel (as the controller verified live for the
    /// spec this backend implements), this also exercises that the loaded
    /// program's map is actually readable through the same `poll_once` path
    /// `run` uses.
    #[test]
    fn loads_and_attaches_when_root_and_btf_are_available() {
        let mut source = match EbpfSource::load_and_attach() {
            Ok(source) => source,
            Err(e) => {
                eprintln!("eBPF backend unavailable (expected without root/BTF/kprobe): {e}");
                return;
            }
        };

        // A fixture channel stands in for the real delta channel
        // `monitor::start_capture` wires up: the point here is only that a
        // poll pass over the live, attached map does not panic and that
        // whatever it produces is readable on the other end, not that any
        // particular traffic occurred during the test.
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        source.poll_once(|delta| {
            let _ = tx.try_send(delta);
        });
        drop(tx);
        let _ = rx.try_iter().count();
    }
}

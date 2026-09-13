//! The BOS (Binary device Object Store): the device's own statement of the
//! link rates it supports, from the sysfs `bos_descriptors` attribute
//! (Linux 6.9 and later; `drivers/usb/core/sysfs.c` `bos_descriptors_read`
//! returns the BOS block to its `wTotalLength`, the file is absent when the
//! device has no BOS, and the kernel does not request one from a device
//! whose bcdUSB is below 2.01).
//!
//! Layouts verified against `include/uapi/linux/usb/ch9.h`:
//! - `USB_DT_BOS` 0x0F, `struct usb_bos_descriptor` (bLength 5): bLength,
//!   bDescriptorType, wTotalLength (little-endian), bNumDeviceCaps.
//! - `USB_DT_DEVICE_CAPABILITY` 0x10, `struct usb_dev_cap_header`: bLength,
//!   bDescriptorType, bDevCapabilityType.
//! - `USB_SS_CAP_TYPE` 3, `struct usb_ss_cap_descriptor` (bLength 10):
//!   wSpeedSupported at offset 4; `USB_5GBPS_OPERATION` is bit 3.
//! - `USB_SSP_CAP_TYPE` 0xA, `struct usb_ssp_cap_descriptor`: bmAttributes
//!   u32 at offset 4, its low five bits (`USB_SSP_SUBLINK_SPEED_ATTRIBS`)
//!   the sublink attribute count minus one; the u32 sublink speed
//!   attributes start at offset 12: `USB_SSP_SUBLINK_SPEED_LSE` bits 4-5
//!   (0 b/s, 1 Kb/s, 2 Mb/s, 3 Gb/s), `USB_SSP_SUBLINK_SPEED_LSM` from bit
//!   16 (the header masks eight bits, the USB 3.2 spec sixteen; they agree
//!   for every mantissa below 256).
//!
//! The BOS states lane rates, not lane counts, so the value here is the
//! per-lane rate: a dual-lane (20 Gb/s) device linked single-lane at
//! 10 Gb/s is not below its capability as far as this module can tell.

use std::path::Path;

use crate::usbmon::parser::UsbSpeed;

/// Where a device's capability figure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySource {
    /// Decoded from the device's BOS.
    Bos,
    /// Inferred from bcdUSB (sysfs `version`) 3.x on a kernel or device
    /// without a BOS file: a floor of 5 Gb/s, never more.
    BcdUsb,
}

impl CapabilitySource {
    /// The JSON value.
    pub fn as_str(self) -> &'static str {
        match self {
            CapabilitySource::Bos => "bos",
            CapabilitySource::BcdUsb => "bcd_usb",
        }
    }
}

/// The highest link rate a device says it supports, and how we know.
#[derive(Debug, Clone, PartialEq)]
pub struct Capability {
    pub speed: UsbSpeed,
    pub source: CapabilitySource,
}

const DT_BOS: u8 = 0x0f;
const DT_DEVICE_CAPABILITY: u8 = 0x10;
const SS_CAP_TYPE: u8 = 0x03;
const SSP_CAP_TYPE: u8 = 0x0a;
const SS_5GBPS_OPERATION: u16 = 1 << 3;
const SSP_SUBLINK_ATTRIBS_MASK: u32 = 0x1f;

/// The device capability descriptors of a BOS, each as its own bytes
/// (`bLength` first), in order. `None` when `bytes` do not start with a
/// BOS header. A malformed descriptor (a `bLength` below the three-byte
/// header, or one running past the block) stops the walk; the block is
/// read to `wTotalLength` at most, so nothing here indexes past the end.
fn capabilities(bytes: &[u8]) -> Option<impl Iterator<Item = &[u8]>> {
    let header_len = usize::from(*bytes.first()?);
    if header_len < 5 || bytes.len() < 5 || bytes[1] != DT_BOS {
        return None;
    }
    let total = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
    let block = &bytes[..total.min(bytes.len())];
    let mut at = header_len;
    Some(std::iter::from_fn(move || {
        if at + 3 > block.len() {
            return None;
        }
        let len = usize::from(block[at]);
        if len < 3 || at + len > block.len() {
            return None;
        }
        let cap = &block[at..at + len];
        at += len;
        Some(cap)
    }))
}

/// Whether a capability descriptor states a link rate: the SuperSpeed
/// (type 3) and SuperSpeedPlus (type 0x0A) capabilities.
fn states_a_rate(cap: &[u8]) -> bool {
    cap[1] == DT_DEVICE_CAPABILITY && matches!(cap[2], SS_CAP_TYPE | SSP_CAP_TYPE)
}

/// The largest link rate the BOS advertises: every SuperSpeedPlus sublink
/// rate and, when the SuperSpeed capability claims 5 Gb/s operation, 5000.
/// `None` when the bytes are not a BOS or carry neither capability. A
/// malformed descriptor stops the walk; nothing here panics on any input.
pub fn capability_from_bos(bytes: &[u8]) -> Option<UsbSpeed> {
    let mut best: Option<f64> = None;
    for cap in capabilities(bytes)?.filter(|cap| states_a_rate(cap)) {
        let len = cap.len();
        match cap[2] {
            SS_CAP_TYPE if len >= 6 => {
                let speeds = u16::from_le_bytes([cap[4], cap[5]]);
                if speeds & SS_5GBPS_OPERATION != 0 {
                    best = Some(best.map_or(5000.0, |b: f64| b.max(5000.0)));
                }
            }
            SSP_CAP_TYPE if len >= 12 => {
                let attrs = u32::from_le_bytes([cap[4], cap[5], cap[6], cap[7]]);
                let count = (attrs & SSP_SUBLINK_ATTRIBS_MASK) as usize + 1;
                for i in 0..count {
                    let off = 12 + i * 4;
                    if off + 4 > len {
                        break;
                    }
                    let v =
                        u32::from_le_bytes([cap[off], cap[off + 1], cap[off + 2], cap[off + 3]]);
                    let mantissa = f64::from(v >> 16);
                    let mbps = match (v >> 4) & 0x3 {
                        0 => mantissa / 1_000_000.0,
                        1 => mantissa / 1_000.0,
                        2 => mantissa,
                        _ => mantissa * 1_000.0,
                    };
                    if mbps > 0.0 {
                        best = Some(best.map_or(mbps, |b: f64| b.max(mbps)));
                    }
                }
            }
            _ => {}
        }
    }
    best.map(UsbSpeed::from_mbps)
}

/// The BOS reduced to what a replay reads: the SuperSpeed and
/// SuperSpeedPlus capability descriptors under a rebuilt header. Everything
/// else is dropped: the USB 2.0 extension, the Container ID (a 128-bit
/// UUID that identifies the unit the way a serial does), the billboard and
/// platform capabilities. A published fixture thus carries a device's rate
/// statement and nothing that names the unit; `capability_from_bos` reads
/// the reduced block exactly as it reads the original, so a replay decides
/// what the live tool decided. Bytes that are not a BOS, and a BOS with no
/// rate capability, both reduce to a bare header, which reads as no
/// capability, the same verdict the original bytes gave live.
pub fn rate_capabilities_only(bytes: &[u8]) -> Vec<u8> {
    let kept: Vec<&[u8]> = capabilities(bytes)
        .into_iter()
        .flatten()
        .filter(|cap| states_a_rate(cap))
        .collect();
    // A BOS is at most 65535 bytes and holds at most 255 capabilities, and
    // the walk never yields more than the original held.
    let total = 5 + kept.iter().map(|cap| cap.len()).sum::<usize>();
    let total = u16::try_from(total).unwrap_or(5);
    let count = u8::try_from(kept.len()).unwrap_or(0);
    let mut out = Vec::with_capacity(usize::from(total));
    out.extend_from_slice(&[5, DT_BOS]);
    out.extend_from_slice(&total.to_le_bytes());
    out.push(count);
    if total > 5 {
        for cap in kept {
            out.extend_from_slice(cap);
        }
    }
    out
}

/// The capability of the device whose sysfs directory is `dir`. A readable
/// `bos_descriptors` decides alone (read to EOF: sysfs declares a size it
/// does not deliver); without one, bcdUSB 3.x (sysfs `version`) is a 5 Gb/s
/// floor. A device linked below its capability usually reports bcdUSB 2.10
/// on the USB 2 bus, so on a kernel without the attribute the absence of a
/// capability proves nothing. Only an absent file falls back: a file that
/// is there but cannot be read is a BOS the tool cannot see, not evidence
/// of a device without one.
pub(super) fn read_capability(dir: &Path) -> Option<Capability> {
    match std::fs::read(dir.join("bos_descriptors")) {
        Ok(bytes) => capability_from_bos(&bytes).map(|speed| Capability {
            speed,
            source: CapabilitySource::Bos,
        }),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => None,
        Err(_) => {
            let raw = std::fs::read_to_string(dir.join("version")).ok()?;
            let major: u32 = raw.trim().split('.').next()?.parse().ok()?;
            (major >= 3).then_some(Capability {
                speed: UsbSpeed::from_mbps(5000.0),
                source: CapabilitySource::BcdUsb,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The NVMe adapter's shape: USB 2.0 extension, SuperSpeed (full, high
    /// and 5 Gb/s), SuperSpeedPlus with two 10 Gb/s sublink attributes
    /// (0x000a4030 rx, 0x000a40b0 tx: mantissa 10, exponent Gb/s).
    const ADAPTER: &[u8] = &[
        0x05, 0x0f, 0x2a, 0x00, 0x03, // BOS: 42 bytes, 3 capabilities
        0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, // USB 2.0 extension
        0x0a, 0x10, 0x03, 0x00, 0x0e, 0x00, 0x03, 0x0a, 0xff, 0x07, // SuperSpeed
        0x14, 0x10, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, // SSP head
        0x30, 0x40, 0x0a, 0x00, // sublink 0: 10 Gb/s
        0xb0, 0x40, 0x0a, 0x00, // sublink 1: 10 Gb/s
    ];

    /// The camera's shape: USB 2.0 extension plus SuperSpeed (high and
    /// 5 Gb/s), no SuperSpeedPlus.
    const CAMERA: &[u8] = &[
        0x05, 0x0f, 0x16, 0x00, 0x02, // BOS: 22 bytes, 2 capabilities
        0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, // USB 2.0 extension
        0x0a, 0x10, 0x03, 0x00, 0x0c, 0x00, 0x03, 0x0a, 0xff, 0x07, // SuperSpeed
    ];

    /// The dock billboard's shape: USB 2.0 extension, container ID and a
    /// billboard capability; nothing SuperSpeed.
    const BILLBOARD: &[u8] = &[
        0x05, 0x0f, 0x28, 0x00, 0x03, // BOS: 40 bytes, 3 capabilities
        0x07, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, // USB 2.0 extension
        0x14, 0x10, 0x04, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, // container ID
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x08, 0x10, 0x0d, 0x00, 0x00, 0x00, 0x00,
        0x00, // billboard (truncated shape)
    ];

    #[test]
    fn superspeedplus_sublink_attributes_give_the_lane_rate() {
        assert_eq!(
            capability_from_bos(ADAPTER),
            Some(UsbSpeed::from_mbps(10000.0))
        );
    }

    #[test]
    fn superspeed_alone_gives_five_gbps() {
        assert_eq!(
            capability_from_bos(CAMERA),
            Some(UsbSpeed::from_mbps(5000.0))
        );
    }

    #[test]
    fn a_bos_without_superspeed_gives_no_capability() {
        assert_eq!(capability_from_bos(BILLBOARD), None);
    }

    #[test]
    fn a_twenty_gbps_mantissa_decodes() {
        let mut bos = ADAPTER.to_vec();
        // Sublink 0's mantissa byte (index 36 of the array: cap offset 12 +
        // byte 2 of the little-endian u32) becomes 0x00144030: mantissa 20,
        // exponent Gb/s.
        bos[36] = 0x14;
        assert_eq!(
            capability_from_bos(&bos),
            Some(UsbSpeed::from_mbps(20000.0))
        );
    }

    #[test]
    fn a_kilobit_exponent_decodes_below_a_megabit() {
        let mut bos = ADAPTER.to_vec();
        // Both sublinks' low byte (indices 34 and 38): mantissa 10, exponent
        // Kb/s (0x000a4010 each): 0.01 Mb/s, so the SuperSpeed 5 Gb/s wins
        // the maximum.
        bos[34] = 0x10;
        bos[38] = 0x10;
        assert_eq!(capability_from_bos(&bos), Some(UsbSpeed::from_mbps(5000.0)));
    }

    /// The other two exponents: a sublink stated in b/s or in Mb/s. The
    /// SuperSpeed capability is dropped from the copy so the sublink
    /// value is the whole answer.
    #[test]
    fn bit_and_megabit_exponents_decode() {
        // Strip the SuperSpeed capability: header + USB 2 extension + SSP.
        let mut without_ss = ADAPTER[..12].to_vec();
        without_ss.extend_from_slice(&ADAPTER[22..]);
        without_ss[2] = 0x20; // wTotalLength 32
        without_ss[4] = 0x02; // two capabilities
                              // Sublink 0 in b/s (exponent 0): mantissa 10 is 0.00001 Mb/s;
                              // sublink 1 in Mb/s (exponent 2): 0x000a4020 is 10 Mb/s.
        without_ss[24] = 0x00;
        without_ss[28] = 0x20;
        assert_eq!(
            capability_from_bos(&without_ss),
            Some(UsbSpeed::from_mbps(10.0))
        );
        // Only the b/s sublink left: the capability is that tiny rate, which
        // can never exceed a link and so never fires a finding.
        without_ss[28] = 0x00;
        assert_eq!(
            capability_from_bos(&without_ss),
            Some(UsbSpeed::from_mbps(0.00001))
        );
    }

    #[test]
    fn malformed_input_never_panics_and_reads_what_it_can() {
        assert_eq!(capability_from_bos(&[]), None);
        assert_eq!(capability_from_bos(&[0x00]), None);
        assert_eq!(
            capability_from_bos(&[0x05, 0x10, 0x05, 0x00, 0x00]),
            None,
            "not a BOS"
        );
        assert_eq!(
            capability_from_bos(&[0x05, 0x0f, 0x05, 0x00, 0x00]),
            None,
            "no capabilities"
        );
        // Cut inside the SSP capability: the SuperSpeed one before it still counts.
        assert_eq!(
            capability_from_bos(&ADAPTER[..30]),
            Some(UsbSpeed::from_mbps(5000.0))
        );
        // A zero bLength stops the walk instead of looping.
        let mut zero = CAMERA.to_vec();
        zero[12] = 0x00;
        assert_eq!(capability_from_bos(&zero), None);
        // wTotalLength beyond the bytes read: clamp, do not index past the end.
        let mut long = CAMERA.to_vec();
        long[2] = 0xff;
        long[3] = 0x7f;
        assert_eq!(
            capability_from_bos(&long),
            Some(UsbSpeed::from_mbps(5000.0))
        );
    }

    /// A dock hub's shape: USB 2.0 extension, SuperSpeed, SuperSpeedPlus
    /// with four sublinks, then a Container ID whose UUID names the unit.
    const HUB_WITH_CONTAINER_ID: &[u8] = &[
        0x05, 0x0f, 0x3d, 0x00, 0x04, // BOS: 61 bytes, 4 capabilities
        0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, // USB 2.0 extension
        0x0a, 0x10, 0x03, 0x00, 0x0e, 0x00, 0x01, 0x08, 0xbe, 0x00, // SuperSpeed
        0x1c, 0x10, 0x0a, 0x00, 0x23, 0x00, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, // SSP head
        0x30, 0x00, 0x05, 0x00, 0xb0, 0x00, 0x05, 0x00, // 5 Gb/s rx, tx
        0x31, 0x40, 0x0a, 0x00, 0xb1, 0x40, 0x0a, 0x00, // 10 Gb/s rx, tx
        0x14, 0x10, 0x04, 0x00, 0xc0, 0x7b, 0x0c, 0x8d, 0xd4, 0x1c, 0x45,
        0x73, // Container ID
        0xa5, 0x4f, 0x7d, 0xfa, 0xb6, 0x0a, 0xef, 0x18,
    ];

    #[test]
    fn rate_capabilities_only_keeps_the_rate_statement_and_drops_the_rest() {
        let reduced = rate_capabilities_only(HUB_WITH_CONTAINER_ID);
        // Header (5) + SuperSpeed (10) + SuperSpeedPlus (28): two capabilities.
        assert_eq!(&reduced[..5], &[0x05, 0x0f, 0x2b, 0x00, 0x02]);
        assert_eq!(reduced.len(), 43);
        assert_eq!(&reduced[5..15], &HUB_WITH_CONTAINER_ID[12..22]);
        assert_eq!(&reduced[15..], &HUB_WITH_CONTAINER_ID[22..50]);
        // Nothing of the UUID survives, and the rate reads the same.
        assert!(!reduced.windows(4).any(|w| w == [0xc0, 0x7b, 0x0c, 0x8d]));
        assert_eq!(
            capability_from_bos(&reduced),
            capability_from_bos(HUB_WITH_CONTAINER_ID)
        );
        assert_eq!(
            capability_from_bos(&reduced),
            Some(UsbSpeed::from_mbps(10000.0))
        );
        // Reducing twice is the identity.
        assert_eq!(rate_capabilities_only(&reduced), reduced);
    }

    #[test]
    fn rate_capabilities_only_leaves_a_bare_header_for_a_usb2_device_and_for_garbage() {
        let bare = [0x05, 0x0f, 0x05, 0x00, 0x00];
        assert_eq!(rate_capabilities_only(BILLBOARD), bare);
        assert_eq!(
            capability_from_bos(&bare),
            None,
            "a bare header reads as no capability, the same as the original"
        );
        assert_eq!(rate_capabilities_only(&[]), bare);
        assert_eq!(rate_capabilities_only(b"garbage"), bare);
        assert_eq!(
            rate_capabilities_only(&[0x05, 0x10, 0x05, 0x00, 0x00]),
            bare
        );
        // A truncated SSP capability is dropped with the rest of the walk.
        let reduced = rate_capabilities_only(&ADAPTER[..30]);
        assert_eq!(reduced.len(), 15, "header plus the SuperSpeed capability");
        assert_eq!(
            capability_from_bos(&reduced),
            Some(UsbSpeed::from_mbps(5000.0))
        );
    }

    #[test]
    fn read_capability_lets_the_bos_decide_and_falls_back_to_bcd_usb() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        // BOS present with SuperSpeed, bcdUSB 2.10 (a device linked at High Speed).
        std::fs::write(dir.join("bos_descriptors"), CAMERA).unwrap();
        std::fs::write(dir.join("version"), " 2.10\n").unwrap();
        assert_eq!(
            read_capability(dir),
            Some(Capability {
                speed: UsbSpeed::from_mbps(5000.0),
                source: CapabilitySource::Bos
            })
        );
        // BOS present without SuperSpeed beats a bcdUSB 3.x claim.
        std::fs::write(dir.join("bos_descriptors"), BILLBOARD).unwrap();
        std::fs::write(dir.join("version"), " 3.20\n").unwrap();
        assert_eq!(read_capability(dir), None);
        // No BOS file: bcdUSB 3.x is a 5 Gb/s floor, labelled.
        std::fs::remove_file(dir.join("bos_descriptors")).unwrap();
        assert_eq!(
            read_capability(dir),
            Some(Capability {
                speed: UsbSpeed::from_mbps(5000.0),
                source: CapabilitySource::BcdUsb
            })
        );
        std::fs::write(dir.join("version"), " 2.10\n").unwrap();
        assert_eq!(read_capability(dir), None);
        std::fs::write(dir.join("version"), "not-a-version\n").unwrap();
        assert_eq!(read_capability(dir), None);
        std::fs::remove_file(dir.join("version")).unwrap();
        assert_eq!(read_capability(dir), None, "no version file");
    }

    #[test]
    fn capability_source_names_are_the_json_values() {
        assert_eq!(CapabilitySource::Bos.as_str(), "bos");
        assert_eq!(CapabilitySource::BcdUsb.as_str(), "bcd_usb");
    }
}

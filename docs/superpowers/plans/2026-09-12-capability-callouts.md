# Capability Call-outs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Read each device's real link capability from the kernel's
`bos_descriptors` attribute (falling back to bcdUSB), decide per connector
why a device or hub is linked below it, and surface those findings in the
TUI, the text report and the JSON report, with the corpus regenerated and
the dock bundle recaptured to carry real BOS bytes.

**Architecture:** A pure BOS parser in `src/device/bos.rs` feeds
`UsbDevice::capability`. A pure engine in `src/findings/mod.rs` walks the
device manager's rows and the connector `PortIndex` (kernel `peer` links
plus its own hub-half matching) and returns `Vec<Finding>`. `ui::sync_from`
and `headless::build_report` both run the engine; the TUI attaches a
finding to its `DeviceRow`, the report gains a top-level `findings` list and
two per-device capability fields. The materializer copies the binary
attribute so replays carry it.

**Tech Stack:** Rust 1.88 (edition 2021), ratatui 0.30, serde, tempfile
(dev). No new crates.

**Spec:** `docs/superpowers/specs/2026-09-12-capability-callouts-design.md`

## Global Constraints

- MSRV 1.88; `cargo +1.88.0 check --all-targets` passes.
- Zero `#[allow(...)]` and zero `#[expect(...)]`. Binary crate: every item
  must be reachable from `main` or a test in every feature configuration;
  gate test-only helpers with `#[cfg(test)]`, never suppress.
- `cargo fmt --all -- --check` clean; clippy `-D warnings` clean on the
  default, `capture-fixture`, `integration`, and `ebpf` configs.
- Verify kernel layouts against `include/uapi/linux/usb/ch9.h` (installed
  at `/usr/include/linux/usb/ch9.h`), and cite them in the module doc.
- Binary test data is written as `&[u8]` byte arrays; the word NUL is
  always spelled out, never a backslash escape.
- Privacy unchanged: `serial` is never copied into a fixture; nothing
  identifying a host, a hostname, or an account name enters the repo.
- The private reference project is never named in the repo.
- The JSON report `version` stays 1; fields are only added.
- Every committed golden is regenerated exactly once (Task 2) and the diff
  is verified to be only the added keys; the dock bundle's goldens change
  again in Task 5 by recapture, as evidence.
- Commit trailers per `CLAUDE.md`.
- Every cargo command in this environment is prefixed with
  `export PATH="$HOME/.cargo/bin:$PATH"`.
- Reviewers use read-only git only.

## File structure

- Create `src/device/bos.rs`: `CapabilitySource`, `Capability`,
  `capability_from_bos`, `read_capability`. Pure over bytes plus one file
  read.
- Modify `src/device/mod.rs`: `pub mod bos;`, re-exports, the
  `capability` field, `get_speed_indicator(Option<&UsbSpeed>)`, tests.
- Modify `src/connector/mod.rs`: `PortIndex::get` and `ports_of` public,
  `device_name_of_port`, tests.
- Create `src/findings/mod.rs`: `Cause`, `Finding`, `short_speed`,
  `analyze`, tests. Modify `src/main.rs`: `mod findings;`.
- Modify `src/headless/mod.rs`: `FindingReport`, the two `DeviceReport`
  fields, `build_report` runs the engine, `render_text` section, tests;
  `src/fixture_replay.rs` and `src/headless/export.rs`: literal `Report`
  constructions.
- Modify `src/ui/mod.rs`: `DeviceRow.finding`, `sync_from`,
  `push_device_row`, `indicator_for`, header counter, help bullet, tests.
- Modify `src/capture/sysfs.rs`: `ATTRS`; `evals/run.sh` and
  `.claude/hooks/content-guard.sh`: NUL-scan allowlist.
- Modify `tests/fixtures/hosts/*/stage*/golden.*.json` (Task 2, all 18
  with goldens) and `tests/fixtures/hosts/tgl-tb4-2026-09-12/stage2/`
  (Task 5, recapture); `src/fixture_corpus.rs`: the findings tests.
- Modify docs: `README.md`, `docs/ARCHITECTURE.md`, `docs/SCRIPTING.md`,
  `docs/TESTING.md`, `docs/ROADMAP.md`, `CHANGELOG.md`.

---

### Task 1: Capability from the BOS

**Files:**
- Create: `src/device/bos.rs`
- Modify: `src/device/mod.rs` (module declaration and re-export near line
  7; the `max_capability` field at 41-43 and its `new()` initializer at
  103; `read_metadata_from` line 179; `check_speed_mismatch` and
  `get_speed_indicator` at 256-280; `read_max_capability` at 335-347;
  `SpeedIndicator` at 350-396; tests 613-745)
- Modify: `src/usbmon/parser.rs:40` (doc comment mentions `max_capability`)

**Interfaces:**
- Consumes: `crate::usbmon::parser::UsbSpeed` (`from_mbps`, `to_mbps`).
- Produces: `device::bos::{Capability, CapabilitySource}` re-exported as
  `crate::device::{Capability, CapabilitySource}`;
  `bos::capability_from_bos(&[u8]) -> Option<UsbSpeed>`;
  `bos::read_capability(&Path) -> Option<Capability>` (`pub(super)`);
  `UsbDevice::capability: Option<Capability>`;
  `UsbDevice::get_speed_indicator(&self, below_capability: Option<&UsbSpeed>) -> SpeedIndicator`;
  `SpeedIndicator::BelowCapability(UsbSpeed)` (renamed from `LimitedByBus`).
- `check_speed_mismatch` is deleted in this task: its only production
  caller was `get_speed_indicator`, whose new signature no longer needs it.

- [ ] **Step 1: Write the failing parser tests**

Create `src/device/bos.rs` with only the module doc, the types and the
function signatures returning `todo!()`? No: write the tests first in the
new file's `#[cfg(test)] mod tests`, with the functions present as stubs
that return `None`, so the tests compile and fail for the stated reason.

```rust
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
        0x14, 0x10, 0x04, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // container ID
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
        0x08, 0x10, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, // billboard (truncated shape)
    ];

    #[test]
    fn superspeedplus_sublink_attributes_give_the_lane_rate() {
        assert_eq!(capability_from_bos(ADAPTER), Some(UsbSpeed::from_mbps(10000.0)));
    }

    #[test]
    fn superspeed_alone_gives_five_gbps() {
        assert_eq!(capability_from_bos(CAMERA), Some(UsbSpeed::from_mbps(5000.0)));
    }

    #[test]
    fn a_bos_without_superspeed_gives_no_capability() {
        assert_eq!(capability_from_bos(BILLBOARD), None);
    }

    #[test]
    fn a_twenty_gbps_mantissa_decodes() {
        let mut bos = ADAPTER.to_vec();
        // Sublink 0 becomes 0x00144030: mantissa 20, exponent Gb/s.
        bos[34] = 0x14;
        assert_eq!(capability_from_bos(&bos), Some(UsbSpeed::from_mbps(20000.0)));
    }

    #[test]
    fn a_kilobit_exponent_decodes_below_a_megabit() {
        let mut bos = ADAPTER.to_vec();
        // Both sublinks: mantissa 10, exponent Kb/s (0x000a4010): 0.01 Mb/s,
        // so the SuperSpeed 5 Gb/s wins the maximum.
        bos[33] = 0x10;
        bos[37] = 0x10;
        assert_eq!(capability_from_bos(&bos), Some(UsbSpeed::from_mbps(5000.0)));
    }

    #[test]
    fn malformed_input_never_panics_and_reads_what_it_can() {
        assert_eq!(capability_from_bos(&[]), None);
        assert_eq!(capability_from_bos(&[0x00]), None);
        assert_eq!(capability_from_bos(&[0x05, 0x10, 0x05, 0x00, 0x00]), None, "not a BOS");
        assert_eq!(capability_from_bos(&[0x05, 0x0f, 0x05, 0x00, 0x00]), None, "no capabilities");
        // Cut inside the SSP capability: the SuperSpeed one before it still counts.
        assert_eq!(capability_from_bos(&ADAPTER[..30]), Some(UsbSpeed::from_mbps(5000.0)));
        // A zero bLength stops the walk instead of looping.
        let mut zero = CAMERA.to_vec();
        zero[12] = 0x00;
        assert_eq!(capability_from_bos(&zero), None);
        // wTotalLength beyond the bytes read: clamp, do not index past the end.
        let mut long = CAMERA.to_vec();
        long[2] = 0xff;
        long[3] = 0x7f;
        assert_eq!(capability_from_bos(&long), Some(UsbSpeed::from_mbps(5000.0)));
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
            Some(Capability { speed: UsbSpeed::from_mbps(5000.0), source: CapabilitySource::Bos })
        );
        // BOS present without SuperSpeed beats a bcdUSB 3.x claim.
        std::fs::write(dir.join("bos_descriptors"), BILLBOARD).unwrap();
        std::fs::write(dir.join("version"), " 3.20\n").unwrap();
        assert_eq!(read_capability(dir), None);
        // No BOS file: bcdUSB 3.x is a 5 Gb/s floor, labelled.
        std::fs::remove_file(dir.join("bos_descriptors")).unwrap();
        assert_eq!(
            read_capability(dir),
            Some(Capability { speed: UsbSpeed::from_mbps(5000.0), source: CapabilitySource::BcdUsb })
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test device::bos`
Expected: compile errors (the items do not exist yet). Add the stubs from
Step 3 with bodies `None` first if you want to see red assertions; either
way the tests must fail before Step 3 is complete.

- [ ] **Step 3: Write the module**

```rust
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
//!   wSpeedsSupported at offset 4; `USB_5GBPS_OPERATION` is bit 3.
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

/// The largest link rate the BOS advertises: every SuperSpeedPlus sublink
/// rate and, when the SuperSpeed capability claims 5 Gb/s operation, 5000.
/// `None` when the bytes are not a BOS or carry neither capability. A
/// malformed descriptor stops the walk; nothing here panics on any input.
pub fn capability_from_bos(bytes: &[u8]) -> Option<UsbSpeed> {
    let header_len = usize::from(*bytes.first()?);
    if header_len < 5 || bytes.len() < 5 || bytes[1] != DT_BOS {
        return None;
    }
    let total = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
    let bytes = &bytes[..total.min(bytes.len())];
    let mut best: Option<f64> = None;
    let mut at = header_len;
    while at + 3 <= bytes.len() {
        let len = usize::from(bytes[at]);
        if len < 3 || at + len > bytes.len() {
            break;
        }
        let cap = &bytes[at..at + len];
        if cap[1] == DT_DEVICE_CAPABILITY {
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
                        let v = u32::from_le_bytes([cap[off], cap[off + 1], cap[off + 2], cap[off + 3]]);
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
        at += len;
    }
    best.map(UsbSpeed::from_mbps)
}

/// The capability of the device whose sysfs directory is `dir`. A readable
/// `bos_descriptors` decides alone (read to EOF: sysfs declares a size it
/// does not deliver); without one, bcdUSB 3.x (sysfs `version`) is a 5 Gb/s
/// floor. A device linked below its capability usually reports bcdUSB 2.10
/// on the USB 2 bus, so on a kernel without the attribute the absence of a
/// capability proves nothing.
pub(super) fn read_capability(dir: &Path) -> Option<Capability> {
    match std::fs::read(dir.join("bos_descriptors")) {
        Ok(bytes) => capability_from_bos(&bytes).map(|speed| Capability {
            speed,
            source: CapabilitySource::Bos,
        }),
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
```

- [ ] **Step 4: Wire the device model**

In `src/device/mod.rs`:

```rust
pub mod bos;
pub mod manager;

pub use bos::{Capability, CapabilitySource};
```

Replace the `max_capability` field (and its doc) with:

```rust
    /// The highest link rate this device says it supports (see
    /// [`bos::read_capability`]), independent of how fast it is linked.
    /// Read once from sysfs; the findings engine compares it with the link
    /// and the topology, never the filesystem.
    pub capability: Option<Capability>,
```

`new()`: `capability: None,`. `read_metadata_from`:
`self.capability = bos::read_capability(sysfs_path);`. Delete
`check_speed_mismatch` and `read_max_capability` (and its doc). Replace
`get_speed_indicator`:

```rust
    /// Visual indicator for the `!` column. `below_capability` is the
    /// capability the findings engine found this device linked below, when
    /// it did (see `findings::analyze`); it takes precedence over
    /// `HighUtilization`.
    pub fn get_speed_indicator(&self, below_capability: Option<&UsbSpeed>) -> SpeedIndicator {
        if let Some(capable_speed) = below_capability {
            SpeedIndicator::BelowCapability(capable_speed.clone())
        } else if self.speed.to_mbps() > 0.0 && self.get_busy_percentage() > 80.0 {
            SpeedIndicator::HighUtilization
        } else {
            SpeedIndicator::Normal
        }
    }
```

Rename `SpeedIndicator::LimitedByBus` to `BelowCapability` everywhere in
the file (symbol 🔺 and colour unchanged); `get_description` says
`"Device capable of {} but linked slower"`. Fix the `parser.rs:40` doc
comment to say `capability` instead of `max_capability`.

Then the `ui/mod.rs:1680` call must still compile: change it to
`device.get_speed_indicator(None)` for now (Task 3 wires the finding in).
This keeps the intermediate state honest: no 🔺 until the engine exists,
which is no worse than today for every device below 5 Gb/s on a USB 2 bus
except the bcdUSB 3.x case, restored in Task 3.

- [ ] **Step 5: Rewrite the device tests**

Replace `max_capability_reads_declared_bcd_usb_version` (613-635):

```rust
    #[test]
    fn capability_falls_back_to_declared_bcd_usb_version() {
        let temp = tempfile::tempdir().unwrap();
        write_device(
            &temp.path().join("1-2"),
            1,
            5,
            &[("speed", "480"), ("version", "3.20")],
        );
        let mut d = UsbDevice::new(1, 5);
        d.populate_from_sysfs(Some(temp.path()));
        assert_eq!(
            d.capability,
            Some(Capability {
                speed: UsbSpeed::from_mbps(5000.0),
                source: CapabilitySource::BcdUsb
            })
        );
    }

    #[test]
    fn capability_prefers_the_bos_over_bcd_usb() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("1-2");
        write_device(&dir, 1, 5, &[("speed", "480"), ("version", "2.10")]);
        // SuperSpeed capability only (the camera's shape).
        std::fs::write(
            dir.join("bos_descriptors"),
            [
                0x05u8, 0x0f, 0x16, 0x00, 0x02, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a,
                0x10, 0x03, 0x00, 0x0c, 0x00, 0x03, 0x0a, 0xff, 0x07,
            ],
        )
        .unwrap();
        let mut d = UsbDevice::new(1, 5);
        d.populate_from_sysfs(Some(temp.path()));
        assert_eq!(
            d.capability,
            Some(Capability {
                speed: UsbSpeed::from_mbps(5000.0),
                source: CapabilitySource::Bos
            })
        );
    }
```

`high_utilization_indicator_above_80_percent`: call
`d.get_speed_indicator(None)`. `limited_by_bus_takes_precedence_over_high_utilization`
becomes `below_capability_takes_precedence_over_high_utilization`: same
setup, assert `d.get_speed_indicator(Some(&UsbSpeed::from_mbps(5000.0))) ==
SpeedIndicator::BelowCapability(UsbSpeed::from_mbps(5000.0))`.
`normal_indicator_when_no_mismatch_and_low_utilization`: `None` argument.
Delete `read_max_capability_signals_only_on_declared_usb_3` and
`read_max_capability_none_when_version_is_missing_or_unparsable` (covered
in `bos.rs`). `no_capability_signal_falls_through_to_utilization_indicators`:
assert `d.capability == None`, drop the `check_speed_mismatch` line, pass
`None` to both indicator calls. `speed_indicator_symbols_and_colors`: the
`BelowCapability` variant.

- [ ] **Step 6: Run the gates**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features capture-fixture -- -D warnings
cargo clippy --all-targets --features integration -- -D warnings
cargo clippy --all-targets --features ebpf -- -D warnings
cargo test --all-targets
```
Expected: all green; the corpus test still passes (no report field moved).

- [ ] **Step 7: Commit**

```bash
git add src/device/bos.rs src/device/mod.rs src/usbmon/parser.rs src/ui/mod.rs
git commit -m "feat(device): read the link capability from the BOS, bcdUSB as the floor"
```
(with the trailers from `CLAUDE.md`).

---

### Task 2: The findings engine and the headless surface

**Files:**
- Modify: `src/connector/mod.rs` (`get` at 177-180 loses `#[cfg(test)]`;
  new `ports_of`; new `device_name_of_port` after `port_of_device` at
  ~226; tests)
- Create: `src/findings/mod.rs`
- Modify: `src/main.rs` (`mod findings;` in the alphabetical list)
- Modify: `src/headless/mod.rs` (`Report` 35-54, `DeviceReport` 66-86,
  `build_report` 156-293 and its doc comment 147-155, `render_text`
  310-361, tests)
- Modify: `src/fixture_replay.rs:304-320`, `src/headless/export.rs:244-254`
  (literal `Report { .. }` gain `findings: Vec::new()`)
- Modify: every `tests/fixtures/hosts/*/stage*/golden.*.json` (re-bless)

**Interfaces:**
- Consumes: `device::{Capability, CapabilitySource}`,
  `device::manager::DeviceManager` (`buses`, `UsbBus::speed`, `UsbDevice`
  fields `sysfs_path`, `bus_id`, `device_id`, `speed`, `capability`,
  `is_disconnected`), `connector::{PortIndex, port_of_device, port_name}`.
- Produces: `connector::PortIndex::get(&self, &str) -> Option<&PortInfo>`
  (now public), `connector::PortIndex::ports_of<'a>(&'a self, hub: &'a str)
  -> impl Iterator<Item = (&'a str, &'a PortInfo)>`,
  `connector::device_name_of_port(hub: &str, number: u32) -> Option<String>`,
  `findings::{Cause, Finding, analyze, short_speed}` with
  `Finding::message(&self) -> String` and `Cause::kind(&self) -> &'static str`,
  `headless::FindingReport`, `Report::findings: Vec<FindingReport>`,
  `DeviceReport::{capability_mbps: Option<f64>, capability_source: Option<&'static str>}`.

- [ ] **Step 1: Connector additions, test first**

In the connector tests add:

```rust
    #[test]
    fn device_name_of_port_inverts_port_of_device() {
        for name in ["3-1", "3-1.4", "3-1.4.2", "12-10.3"] {
            let (hub, number) = port_of_device(name).unwrap();
            assert_eq!(device_name_of_port(&hub, number).as_deref(), Some(name));
        }
        assert_eq!(device_name_of_port("usb6", 2).as_deref(), Some("6-2"));
        assert_eq!(device_name_of_port("6-1", 2).as_deref(), Some("6-1.2"));
        assert_eq!(device_name_of_port("garbage", 1), None);
    }

    #[test]
    fn ports_of_lists_a_hubs_ports_and_nothing_else() {
        let t = paired_tree();
        let index = PortIndex::scan(&t.base());
        let mut names: Vec<&str> = index.ports_of("3-1").map(|(name, _)| name).collect();
        names.sort();
        assert_eq!(names, ["3-1-port1", "3-1-port2"]);
        assert_eq!(index.ports_of("3-2").count(), 0, "a device without ports");
        assert_eq!(
            index.get("usb3-port1").map(|info| info.number),
            Some(1),
            "get is the public lookup the findings engine uses"
        );
    }
```

Implement: drop `#[cfg(test)]` from `get` (doc it: "The port named
`name`, when the scan found it."); add after `get`:

```rust
    /// The ports `hub` owns, by name.
    pub fn ports_of<'a>(&'a self, hub: &'a str) -> impl Iterator<Item = (&'a str, &'a PortInfo)> {
        self.ports
            .iter()
            .filter(move |(_, info)| info.hub == hub)
            .map(|(name, info)| (name.as_str(), info))
    }
```

and after `port_of_device`:

```rust
/// The sysfs name of the device on port `number` of `hub`, the inverse of
/// [`port_of_device`]: port 2 of `usb6` is `6-2`, port 2 of `6-1` is
/// `6-1.2`. `None` when `hub` is not a device name.
pub fn device_name_of_port(hub: &str, number: u32) -> Option<String> {
    let (bus, mut chain) = parse_device_name(hub)?;
    chain.push(number);
    Some(format!(
        "{bus}-{}",
        chain.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
    ))
}
```

Run `cargo test connector` : green.

- [ ] **Step 2: The engine's tests, written first**

Create `src/findings/mod.rs` with the types and an `analyze` stub returning
`Vec::new()`, plus these tests. The test tree helper mirrors the
connector tests' `Tree` and adds device attributes and BOS bytes.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// SuperSpeed only: 5 Gb/s (the camera's shape).
    const SS: &[u8] = &[
        0x05, 0x0f, 0x16, 0x00, 0x02, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10,
        0x03, 0x00, 0x0c, 0x00, 0x03, 0x0a, 0xff, 0x07,
    ];
    /// SuperSpeed plus SuperSpeedPlus at 10 Gb/s (the adapter's shape).
    const SSP: &[u8] = &[
        0x05, 0x0f, 0x2a, 0x00, 0x03, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10,
        0x03, 0x00, 0x0e, 0x00, 0x03, 0x0a, 0xff, 0x07, 0x14, 0x10, 0x0a, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x30, 0x40, 0x0a, 0x00, 0xb0, 0x40, 0x0a, 0x00,
    ];

    /// A fake `/sys/bus/usb/devices`: root hubs as symlinks into a
    /// controller directory, devices as directories with `busnum`,
    /// `devnum`, `speed`, optional `version` and `bos_descriptors`, hub
    /// ports under `<hub>/<hub>:1.0/<hub>-port<N>/` with `peer` links.
    struct Tree {
        root: tempfile::TempDir,
        next_devnum: std::cell::Cell<u8>,
    }

    impl Tree {
        fn new() -> Tree {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("devices")).unwrap();
            Tree { root, next_devnum: std::cell::Cell::new(2) }
        }

        fn base(&self) -> PathBuf {
            self.root.path().join("devices")
        }

        fn write(dir: &Path, bus: u8, devnum: u8, speed: &str, version: Option<&str>, bos: Option<&[u8]>) {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("busnum"), format!("{bus}\n")).unwrap();
            std::fs::write(dir.join("devnum"), format!("{devnum}\n")).unwrap();
            std::fs::write(dir.join("speed"), format!("{speed}\n")).unwrap();
            if let Some(version) = version {
                std::fs::write(dir.join("version"), format!("{version}\n")).unwrap();
            }
            if let Some(bos) = bos {
                std::fs::write(dir.join("bos_descriptors"), bos).unwrap();
            }
        }

        /// A root hub `usbN` at `speed`, devnum 1, under a controller dir.
        fn root_hub(&self, bus: u8, speed: &str) -> PathBuf {
            let real = self.root.path().join("0000:00:14.0").join(format!("usb{bus}"));
            Self::write(&real, bus, 1, speed, None, None);
            std::os::unix::fs::symlink(&real, self.base().join(format!("usb{bus}"))).unwrap();
            real
        }

        /// A device with the next devnum on its bus (taken from the name).
        fn device(&self, name: &str, speed: &str, version: Option<&str>, bos: Option<&[u8]>) -> PathBuf {
            let (bus, _) = crate::connector::parse_device_name(name).unwrap();
            let devnum = self.next_devnum.get();
            self.next_devnum.set(devnum + 1);
            let dir = self.base().join(name);
            Self::write(&dir, bus, devnum, speed, version, bos);
            dir
        }

        fn port(&self, hub_dir: &Path, hub: &str, number: u32) -> PathBuf {
            let dir = hub_dir.join(format!("{hub}:1.0")).join(port_name(hub, number));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn pair(&self, a: &Path, b: &Path) {
            std::os::unix::fs::symlink(b, a.join("peer")).unwrap();
            std::os::unix::fs::symlink(a, b.join("peer")).unwrap();
        }

        fn analyze(&self) -> Vec<Finding> {
            let mut manager = DeviceManager::with_sysfs_base(self.base());
            manager.enumerate_present_devices();
            manager.update_bus_speeds();
            let index = PortIndex::scan_devices(
                manager
                    .buses
                    .values()
                    .flat_map(|bus| bus.devices.values())
                    .filter_map(|device| device.sysfs_path.as_deref()),
            );
            super::analyze(&manager, &index)
        }
    }

    /// Paired root hubs 3 (480) and 4 (5000) with `n` root ports paired by
    /// number.
    fn paired_roots(t: &Tree, n: u32) -> (PathBuf, PathBuf) {
        let usb3 = t.root_hub(3, "480");
        let usb4 = t.root_hub(4, "5000");
        for i in 1..=n {
            t.pair(&t.port(&usb3, "usb3", i), &t.port(&usb4, "usb4", i));
        }
        (usb3, usb4)
    }

    fn causes(findings: &[Finding]) -> Vec<(&str, Option<&Cause>)> {
        findings.iter().map(|f| (f.path.as_str(), f.cause.as_ref())).collect()
    }

    #[test]
    fn a_superspeed_device_on_a_usb2_port_with_an_empty_superspeed_side() {
        let t = Tree::new();
        paired_roots(&t, 2);
        t.device("3-1", "480", Some("2.10"), Some(SSP));
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![("3-1", Some(&Cause::SuperSpeedSideEmpty { peer_port: "usb4-port1".into() }))]
        );
        let f = &findings[0];
        assert_eq!((f.bus, f.port.as_deref()), (3, Some("usb3-port1")));
        assert_eq!(f.capability.speed, UsbSpeed::from_mbps(10000.0));
        assert_eq!(f.capability.source, CapabilitySource::Bos);
        assert_eq!(
            f.message(),
            "linked at 480M, supports 10G: the SuperSpeed side of this connector (usb4-port1) is empty, so the link came up at USB 2 speed; check the cable or the port"
        );
    }

    #[test]
    fn a_hub_whose_superspeed_half_is_up_is_not_a_finding_nor_its_paired_child() {
        let t = Tree::new();
        paired_roots(&t, 1);
        let hub3 = t.device("3-1", "480", Some("2.10"), Some(SS));
        let hub4 = t.device("4-1", "5000", Some("3.00"), Some(SS));
        for n in 1..=2 {
            t.pair(&t.port(&hub3, "3-1", n), &t.port(&hub4, "4-1", n));
        }
        // A camera whose SuperSpeed side is up: only its USB 3 device enumerates.
        t.device("4-1.1", "5000", Some("3.00"), Some(SS));
        // A camera stuck at High Speed on hub port 2: its SuperSpeed side is empty.
        t.device("3-1.2", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![("3-1.2", Some(&Cause::SuperSpeedSideEmpty { peer_port: "4-1-port2".into() }))]
        );
    }

    #[test]
    fn a_usb2_only_hub_holds_its_child_and_is_named() {
        let t = Tree::new();
        let (usb3, usb4) = paired_roots(&t, 1);
        let hub3 = t.device("3-1", "480", Some("2.10"), Some(SS));
        let hub4 = t.device("4-1", "5000", Some("3.00"), Some(SS));
        for n in 1..=4 {
            t.pair(&t.port(&hub3, "3-1", n), &t.port(&hub4, "4-1", n));
        }
        // A USB 2 only hub (bcdUSB 2.00, no BOS) on hub port 4, its own ports unpaired.
        let terminus = t.device("3-1.4", "480", Some("2.00"), None);
        t.port(&terminus, "3-1.4", 5);
        t.device("3-1.4.5", "480", Some("2.10"), Some(SS));
        let _ = (usb3, usb4);
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![("3-1.4.5", Some(&Cause::UpstreamHubLink { hub: "3-1.4".into(), hub_link: UsbSpeed::from_mbps(480.0) }))]
        );
        assert_eq!(
            findings[0].message(),
            "linked at 480M, supports 5G: the hub above it (3-1.4) is linked at 480M; move it to a USB 3 port"
        );
    }

    /// The dock's shape: the inner hub's halves sit on different port
    /// numbers, the kernel pairs by number, so its halves are matched by
    /// elimination and neither half nor the billboard is a finding; the
    /// adapter on the outer hub's port 2 is.
    #[test]
    fn hub_halves_on_different_port_numbers_are_matched_by_elimination() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
        let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let inner2 = t.device("5-1.1", "480", Some("2.10"), Some(SS));
        let inner3 = t.device("6-1.4", "10000", Some("3.20"), Some(SSP));
        for n in 1..=8 {
            t.port(&inner2, "5-1.1", n);
        }
        for n in 1..=4 {
            t.port(&inner3, "6-1.4", n);
        }
        t.device("5-1.1.8", "12", Some("2.01"), Some(&[0x05, 0x0f, 0x0c, 0x00, 0x01, 0x07, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00]));
        t.device("5-1.2", "480", Some("2.10"), Some(SSP));
        // A 10 Gb/s SSD stuck at High Speed on the inner hub's port 2: its
        // SuperSpeed port is the matched half's port 2.
        t.device("5-1.1.2", "480", Some("2.10"), Some(SSP));
        // A device on the inner hub's port 8, which has no SuperSpeed twin.
        t.device("5-1.1.7", "480", Some("2.10"), Some(SS));
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![
                ("5-1.2", Some(&Cause::SuperSpeedSideEmpty { peer_port: "6-1-port2".into() })),
                ("5-1.1.2", Some(&Cause::SuperSpeedSideEmpty { peer_port: "6-1.4-port2".into() })),
                ("5-1.1.7", Some(&Cause::UpstreamHubLink { hub: "5-1.1".into(), hub_link: UsbSpeed::from_mbps(480.0) })),
            ]
        );
    }

    #[test]
    fn two_unclaimed_hubs_a_side_are_ambiguous_and_say_nothing_about_the_hubs() {
        let t = Tree::new();
        let usb5 = t.root_hub(5, "480");
        let usb6 = t.root_hub(6, "10000");
        t.pair(&t.port(&usb5, "usb5", 1), &t.port(&usb6, "usb6", 1));
        let outer2 = t.device("5-1", "480", Some("2.10"), Some(SS));
        let outer3 = t.device("6-1", "10000", Some("3.20"), Some(SSP));
        for n in 1..=4 {
            t.pair(&t.port(&outer2, "5-1", n), &t.port(&outer3, "6-1", n));
        }
        let a = t.device("5-1.1", "480", Some("2.10"), Some(SS));
        let b = t.device("5-1.2", "480", Some("2.10"), Some(SS));
        t.device("6-1.3", "5000", Some("3.00"), Some(SS));
        t.device("6-1.4", "5000", Some("3.00"), Some(SS));
        t.port(&a, "5-1.1", 1);
        t.port(&b, "5-1.2", 1);
        t.device("5-1.1.1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![("5-1.1.1", Some(&Cause::UpstreamHubLink { hub: "5-1.1".into(), hub_link: UsbSpeed::from_mbps(480.0) }))],
            "the hubs are ambiguous; the child's hub link is still a true statement"
        );
    }

    #[test]
    fn a_faster_hub_on_a_slower_root_port_and_a_faster_device_under_a_slower_hub() {
        let t = Tree::new();
        let (_, usb4) = paired_roots(&t, 1);
        let _ = usb4;
        // A 10 Gb/s hub on the 5 Gb/s root port.
        let hub = t.device("4-1", "5000", Some("3.20"), Some(SSP));
        t.port(&hub, "4-1", 1);
        t.port(&hub, "4-1", 2);
        // A 10 Gb/s device under it, and a 5 Gb/s device that is fine.
        t.device("4-1.1", "5000", Some("3.20"), Some(SSP));
        t.device("4-1.2", "5000", Some("3.00"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![
                ("4-1", Some(&Cause::HostPortMax { max: UsbSpeed::from_mbps(5000.0) })),
                ("4-1.1", Some(&Cause::UpstreamHubLink { hub: "4-1".into(), hub_link: UsbSpeed::from_mbps(5000.0) })),
            ]
        );
    }

    #[test]
    fn a_link_below_capability_under_a_permitting_port_blames_neither_upstream() {
        let t = Tree::new();
        let usb3 = t.root_hub(3, "480");
        let usb4 = t.root_hub(4, "10000");
        t.pair(&t.port(&usb3, "usb3", 1), &t.port(&usb4, "usb4", 1));
        t.device("4-1", "5000", Some("3.20"), Some(SSP));
        let findings = t.analyze();
        assert_eq!(causes(&findings), vec![("4-1", Some(&Cause::UpstreamPermits))]);
        assert_eq!(
            findings[0].message(),
            "linked at 5G, supports 10G: the port above it allows 10G; check the cable or the device"
        );
    }

    #[test]
    fn a_superspeed_hub_on_a_usb2_only_root_port_and_its_child() {
        let t = Tree::new();
        let usb1 = t.root_hub(1, "480");
        t.port(&usb1, "usb1", 1);
        let hub = t.device("1-1", "480", Some("2.10"), Some(SS));
        t.port(&hub, "1-1", 1);
        t.device("1-1.1", "480", Some("2.10"), Some(SS));
        assert_eq!(
            causes(&t.analyze()),
            vec![
                ("1-1", Some(&Cause::Usb2OnlyHostPort)),
                ("1-1.1", Some(&Cause::UpstreamHubLink { hub: "1-1".into(), hub_link: UsbSpeed::from_mbps(480.0) })),
            ]
        );
    }

    #[test]
    fn a_usb2_root_port_without_a_superspeed_twin_is_usb2_only() {
        let t = Tree::new();
        let (usb3, _) = paired_roots(&t, 1);
        t.port(&usb3, "usb3", 5);
        t.device("3-5", "480", Some("2.10"), Some(SS));
        assert_eq!(causes(&t.analyze()), vec![("3-5", Some(&Cause::Usb2OnlyHostPort))]);
    }

    #[test]
    fn the_bcd_usb_fallback_is_labelled_and_never_claims_more_than_five_gbps() {
        let t = Tree::new();
        let (usb3, usb4) = paired_roots(&t, 2);
        let _ = (usb3, usb4);
        t.device("3-1", "480", Some("3.20"), None);
        t.device("4-2", "5000", Some("3.20"), None);
        let findings = t.analyze();
        assert_eq!(
            causes(&findings),
            vec![("3-1", Some(&Cause::SuperSpeedSideEmpty { peer_port: "usb4-port1".into() }))],
            "the 5 Gb/s device with bcdUSB 3.20 and no BOS is not called out"
        );
        assert_eq!(findings[0].capability.source, CapabilitySource::BcdUsb);
        assert!(findings[0].message().starts_with("linked at 480M, supports 5G (from bcdUSB): "));
    }

    #[test]
    fn a_disconnected_device_is_ignored_and_a_present_twin_counts() {
        let t = Tree::new();
        paired_roots(&t, 1);
        t.device("3-1", "480", Some("2.10"), Some(SS));
        let mut manager = DeviceManager::with_sysfs_base(t.base());
        manager.enumerate_present_devices();
        manager.update_bus_speeds();
        let index = PortIndex::scan_devices(
            manager
                .buses
                .values()
                .flat_map(|bus| bus.devices.values())
                .filter_map(|device| device.sysfs_path.as_deref()),
        );
        assert_eq!(analyze(&manager, &index).len(), 1);
        manager.buses.get_mut(&3).unwrap().devices.get_mut(&2).unwrap().is_disconnected = true;
        assert!(analyze(&manager, &index).is_empty());
    }

    #[test]
    fn a_device_whose_port_is_unknown_has_no_cause() {
        let t = Tree::new();
        t.root_hub(3, "480");
        // No port objects at all (an old kernel or a bare fixture).
        t.device("3-1", "480", Some("3.00"), None);
        let findings = t.analyze();
        assert_eq!(causes(&findings), vec![("3-1", None)]);
        assert_eq!(findings[0].port, None);
        assert!(findings[0].message().ends_with(": its port is unknown to the connector index"));
    }

    #[test]
    fn findings_are_ordered_by_bus_then_address() {
        let t = Tree::new();
        let (usb3, _) = paired_roots(&t, 3);
        let _ = usb3;
        t.device("3-3", "480", Some("2.10"), Some(SS));
        t.device("3-1", "480", Some("2.10"), Some(SS));
        t.device("3-2", "480", Some("2.10"), Some(SS));
        let order: Vec<u8> = t.analyze().iter().map(|f| f.address).collect();
        assert_eq!(order, vec![2, 3, 4]);
    }

    #[test]
    fn short_speed_is_compact() {
        assert_eq!(short_speed(&UsbSpeed::from_mbps(480.0)), "480M");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(12.0)), "12M");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(1.5)), "1.5M");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(5000.0)), "5G");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(10000.0)), "10G");
        assert_eq!(short_speed(&UsbSpeed::from_mbps(20000.0)), "20G");
        assert_eq!(short_speed(&UsbSpeed::UNKNOWN), "?");
    }
}
```

Note the devnum bookkeeping: `Tree::device` hands out devnums from 2
upward in call order across all buses, so in `findings_are_ordered_by_bus_then_address`
the devices are created in the order 3-3 (2), 3-1 (3), 3-2 (4) and the
expected order is by address.

Run: `cargo test findings` : every test fails on the empty `Vec` (or on
compile until the stub exists). Confirm the failures are assertion
failures, not tree-building panics, before Step 3.

- [ ] **Step 3: The engine**

```rust
//! Call-outs: devices and hubs linked below the speed they support, and
//! why. Pure over the device manager's rows and the connector index; no
//! sysfs reads. Rules and the verdict doctrine they follow are in
//! `docs/superpowers/specs/2026-09-12-capability-callouts-design.md`.
//!
//! The kernel pairs a hub's ports by port number with the ports of the hub
//! on its upstream port's peer (`drivers/usb/core/port.c`
//! `find_and_link_peer`); a hub whose two halves sit on different port
//! numbers gets its own ports left unpaired and its halves paired with
//! empty ports. `Topology::ss_half` therefore matches such halves by
//! elimination and says nothing when the match is ambiguous.

use std::collections::HashMap;

use crate::connector::{device_name_of_port, port_name, port_of_device, PortIndex};
use crate::device::manager::DeviceManager;
use crate::device::{Capability, CapabilitySource};
use crate::usbmon::parser::UsbSpeed;

/// Why a device is linked below its capability, when the topology proves it.
#[derive(Debug, Clone, PartialEq)]
pub enum Cause {
    /// The connector's SuperSpeed port is empty: the link came up at USB 2
    /// speed (a cable or port problem, or a device that never trained).
    SuperSpeedSideEmpty { peer_port: String },
    /// The root port has no SuperSpeed twin.
    Usb2OnlyHostPort,
    /// The hub above the device is itself linked below the capability.
    UpstreamHubLink { hub: String, hub_link: UsbSpeed },
    /// The root hub's own rate is below the capability.
    HostPortMax { max: UsbSpeed },
    /// The upstream allows the capability; the link still came up slower.
    UpstreamPermits,
}

impl Cause {
    /// The JSON value.
    pub fn kind(&self) -> &'static str {
        match self {
            Cause::SuperSpeedSideEmpty { .. } => "superspeed_side_empty",
            Cause::Usb2OnlyHostPort => "usb2_only_host_port",
            Cause::UpstreamHubLink { .. } => "upstream_hub_link",
            Cause::HostPortMax { .. } => "host_port_max",
            Cause::UpstreamPermits => "upstream_permits",
        }
    }

    fn reason(&self, capability: &UsbSpeed) -> String {
        match self {
            Cause::SuperSpeedSideEmpty { peer_port } => format!(
                "the SuperSpeed side of this connector ({peer_port}) is empty, so the link came up at USB 2 speed; check the cable or the port"
            ),
            Cause::Usb2OnlyHostPort => "this host port is USB 2 only; move it to a USB 3 port".to_string(),
            Cause::UpstreamHubLink { hub, hub_link } => {
                let mut text = format!("the hub above it ({hub}) is linked at {}", short_speed(hub_link));
                if hub_link.to_mbps() <= HIGH_SPEED_MBPS {
                    text.push_str("; move it to a USB 3 port");
                }
                text
            }
            Cause::HostPortMax { max } => format!("this host port tops out at {}", short_speed(max)),
            Cause::UpstreamPermits => format!(
                "the port above it allows {}; check the cable or the device",
                short_speed(capability)
            ),
        }
    }
}

/// One device linked below the speed it supports.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub bus: u8,
    pub address: u8,
    /// The sysfs name, `5-1.2`.
    pub path: String,
    /// The kernel port name the device is on, `5-1-port2`; `None` when the
    /// index does not know it.
    pub port: Option<String>,
    pub link: UsbSpeed,
    pub capability: Capability,
    pub cause: Option<Cause>,
}

impl Finding {
    /// The sentence every surface shows: `linked at 480M, supports 10G:
    /// <reason>`, with `(from bcdUSB)` after the capability when it is the
    /// fallback floor.
    pub fn message(&self) -> String {
        let source = match self.capability.source {
            CapabilitySource::Bos => "",
            CapabilitySource::BcdUsb => " (from bcdUSB)",
        };
        let reason = match &self.cause {
            Some(cause) => cause.reason(&self.capability.speed),
            None => "its port is unknown to the connector index".to_string(),
        };
        format!(
            "linked at {}, supports {}{source}: {reason}",
            short_speed(&self.link),
            short_speed(&self.capability.speed)
        )
    }
}

/// `480M`, `5G`, `10G`; `?` for an unknown rate.
pub fn short_speed(speed: &UsbSpeed) -> String {
    let mbps = speed.to_mbps();
    if mbps <= 0.0 {
        "?".to_string()
    } else if mbps >= 1000.0 {
        format!("{}G", mbps / 1000.0)
    } else {
        format!("{mbps}M")
    }
}

const HIGH_SPEED_MBPS: f64 = 480.0;

struct Row<'a> {
    name: &'a str,
    bus: u8,
    address: u8,
    link: &'a UsbSpeed,
    capability: Option<&'a Capability>,
}

impl Row<'_> {
    fn capable_above_high(&self) -> bool {
        self.capability
            .is_some_and(|c| c.speed.to_mbps() > HIGH_SPEED_MBPS)
    }
}

struct Topology<'a> {
    rows: &'a [Row<'a>],
    by_name: HashMap<&'a str, &'a Row<'a>>,
    ports: &'a PortIndex,
    ss_half: HashMap<String, Option<String>>,
}

impl<'a> Topology<'a> {
    fn present(&self, name: &str) -> Option<&'a Row<'a>> {
        self.by_name.get(name).copied()
    }

    /// The reciprocal peer of `device`'s own port.
    fn peer_port_of(&self, device: &str) -> Option<String> {
        self.ports.connector_of(device)?.1.map(|peer| peer.name)
    }

    /// The present device on the port named `port`.
    fn device_on_port(&self, port: &str) -> Option<&'a Row<'a>> {
        let info = self.ports.get(port)?;
        let name = device_name_of_port(&info.hub, info.number)?;
        self.present(&name)
    }

    /// Whether `hub`'s own port has a reciprocal peer holding a present device.
    fn peer_holds_a_device(&self, hub: &str) -> bool {
        self.peer_port_of(hub)
            .is_some_and(|peer| self.device_on_port(&peer).is_some())
    }

    /// Present hubs attached to `parent`'s ports.
    fn hubs_on(&self, parent: &str) -> impl Iterator<Item = &'a Row<'a>> + '_ {
        let parent = parent.to_string();
        self.rows.iter().filter(move |row| {
            self.ports.is_hub(row.name)
                && port_of_device(row.name).is_some_and(|(hub, _)| hub == parent)
        })
    }

    /// The SuperSpeed half of `hub` (rule H in the spec), memoized. `None`
    /// for a SuperSpeed hub, a USB 2 only hub, or an ambiguous match.
    fn ss_half(&mut self, hub: &str) -> Option<String> {
        if let Some(known) = self.ss_half.get(hub) {
            return known.clone();
        }
        // Unknown while computing: a cycle in a corrupt tree ends here.
        self.ss_half.insert(hub.to_string(), None);
        let half = self.find_ss_half(hub);
        self.ss_half.insert(hub.to_string(), half.clone());
        half
    }

    fn find_ss_half(&mut self, hub: &str) -> Option<String> {
        let row = self.present(hub)?;
        if row.link.to_mbps() > HIGH_SPEED_MBPS {
            return None;
        }
        let Some((parent, _)) = port_of_device(hub) else {
            // A root hub: the hub owning the reciprocal peer of any of its ports.
            let peer_hubs: Vec<String> = self
                .ports
                .ports_of(hub)
                .filter_map(|(name, info)| {
                    let peer = info.peer.as_deref()?;
                    let peer_info = self.ports.get(peer)?;
                    (peer_info.peer.as_deref() == Some(name)).then(|| peer_info.hub.clone())
                })
                .collect();
            return peer_hubs.into_iter().find(|h| self.present(h).is_some());
        };
        // Step 2: the kernel's own pairing.
        if let Some(peer) = self.peer_port_of(hub) {
            if let Some(dev) = self.device_on_port(&peer) {
                if self.ports.is_hub(dev.name) {
                    return Some(dev.name.to_string());
                }
            }
        }
        // Step 3: match by elimination under the parent pair.
        if !row.capable_above_high() {
            return None;
        }
        let parent_ss = self.ss_half(&parent)?;
        let unclaimed_usb2: Vec<&str> = self
            .hubs_on(&parent)
            .filter(|h| h.link.to_mbps() <= HIGH_SPEED_MBPS && h.capable_above_high())
            .filter(|h| !self.peer_holds_a_device(h.name))
            .map(|h| h.name)
            .collect();
        let unclaimed_ss: Vec<&str> = self
            .hubs_on(&parent_ss)
            .filter(|h| h.link.to_mbps() > HIGH_SPEED_MBPS)
            .filter(|h| !self.peer_holds_a_device(h.name))
            .map(|h| h.name)
            .collect();
        match (unclaimed_usb2.as_slice(), unclaimed_ss.as_slice()) {
            ([one], [half]) if *one == hub => Some((*half).to_string()),
            _ => None,
        }
    }
}

/// Every present, non-root device linked below a known capability, with
/// the cause the topology proves, ordered by (bus, address).
pub fn analyze(manager: &DeviceManager, ports: &PortIndex) -> Vec<Finding> {
    let mut rows: Vec<Row> = manager
        .buses
        .values()
        .flat_map(|bus| bus.devices.values())
        .filter(|device| !device.is_disconnected)
        .filter_map(|device| {
            let name = device.sysfs_path.as_deref()?.file_name()?.to_str()?;
            Some(Row {
                name,
                bus: device.bus_id,
                address: device.device_id,
                link: &device.speed,
                capability: device.capability.as_ref(),
            })
        })
        .collect();
    rows.sort_by_key(|row| (row.bus, row.address));
    let by_name = rows.iter().map(|row| (row.name, row)).collect();
    let mut topology = Topology {
        rows: &rows,
        by_name,
        ports,
        ss_half: HashMap::new(),
    };
    let bus_speed = |bus: u8| manager.buses.get(&bus).map(|b| b.speed.clone());
    rows.iter()
        .filter_map(|row| finding_for(row, &mut topology, &bus_speed))
        .collect()
}

fn finding_for(
    row: &Row,
    topology: &mut Topology,
    bus_speed: &impl Fn(u8) -> Option<UsbSpeed>,
) -> Option<Finding> {
    let capability = row.capability?;
    let link = row.link.to_mbps();
    if link <= 0.0 || capability.speed.to_mbps() <= link {
        return None;
    }
    // A root hub is on no port and is never a finding.
    let (parent, number) = port_of_device(row.name)?;
    let port = port_name(&parent, number);
    let finding = |port: Option<String>, cause: Option<Cause>| Finding {
        bus: row.bus,
        address: row.address,
        path: row.name.to_string(),
        port,
        link: row.link.clone(),
        capability: capability.clone(),
        cause,
    };
    if topology.ports.get(&port).is_none() {
        return Some(finding(None, None));
    }
    let root_parent = port_of_device(&parent).is_none();
    if link <= HIGH_SPEED_MBPS {
        if topology.ports.is_hub(row.name) && topology.ss_half(row.name).is_some() {
            return None;
        }
        let ss_port = topology
            .ss_half(&parent)
            .map(|half| port_name(&half, number))
            .filter(|name| topology.ports.get(name).is_some());
        let cause = match ss_port {
            Some(ss_port) if topology.device_on_port(&ss_port).is_some() => return None,
            Some(ss_port) => Cause::SuperSpeedSideEmpty { peer_port: ss_port },
            None if root_parent => Cause::Usb2OnlyHostPort,
            None => Cause::UpstreamHubLink {
                hub_link: topology
                    .present(&parent)
                    .map_or(UsbSpeed::UNKNOWN, |p| p.link.clone()),
                hub: parent,
            },
        };
        return Some(finding(Some(port), Some(cause)));
    }
    let limit = if root_parent {
        bus_speed(row.bus)
    } else {
        topology.present(&parent).map(|p| p.link.clone())
    };
    let cause = match limit {
        Some(max) if max.to_mbps() > 0.0 && max.to_mbps() < capability.speed.to_mbps() => {
            if root_parent {
                Cause::HostPortMax { max }
            } else {
                Cause::UpstreamHubLink { hub: parent, hub_link: max }
            }
        }
        _ => Cause::UpstreamPermits,
    };
    Some(finding(Some(port), Some(cause)))
}
```

Add `mod findings;` to `src/main.rs` between `filter` and
`fixture_corpus`. Run `cargo test findings connector` : green. If a
borrow-checker complaint arises from `hubs_on` capturing `self` while
`find_ss_half` holds `&mut self`, collect the candidates into `Vec<String>`
before the recursive `ss_half` call (the recursion happens before the
filters, as written).

- [ ] **Step 4: The headless surface, tests first**

In `src/headless/mod.rs` tests add:

```rust
    /// A USB 3 device (bcdUSB 3.20, no BOS) at 480 on a paired root port
    /// whose SuperSpeed twin is empty, and a plain device: the report
    /// carries one finding and per-device capability fields.
    fn tree_with_a_finding() -> (tempfile::TempDir, DeviceManager) {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("devices");
        let ctrl = temp.path().join("0000:00:14.0");
        std::fs::create_dir_all(&base).unwrap();
        let write = |dir: &std::path::Path, attrs: &[(&str, &str)]| {
            std::fs::create_dir_all(dir).unwrap();
            for (k, v) in attrs {
                std::fs::write(dir.join(k), format!("{v}\n")).unwrap();
            }
        };
        let usb3 = ctrl.join("usb3");
        let usb4 = ctrl.join("usb4");
        write(&usb3, &[("busnum", "3"), ("devnum", "1"), ("speed", "480")]);
        write(&usb4, &[("busnum", "4"), ("devnum", "1"), ("speed", "5000")]);
        symlink(&usb3, base.join("usb3")).unwrap();
        symlink(&usb4, base.join("usb4")).unwrap();
        let p3 = usb3.join("usb3:1.0").join("usb3-port1");
        let p4 = usb4.join("usb4:1.0").join("usb4-port1");
        std::fs::create_dir_all(&p3).unwrap();
        std::fs::create_dir_all(&p4).unwrap();
        symlink(&p4, p3.join("peer")).unwrap();
        symlink(&p3, p4.join("peer")).unwrap();
        write(
            &base.join("3-1"),
            &[("busnum", "3"), ("devnum", "2"), ("speed", "480"), ("version", "3.20"), ("idVendor", "0bda"), ("idProduct", "9210")],
        );
        let mut mgr = DeviceManager::with_sysfs_base(base);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        (temp, mgr)
    }

    #[test]
    fn json_report_carries_findings_and_per_device_capability() {
        let (_temp, mgr) = tree_with_a_finding();
        let baseline = Baseline::capture(&mgr);
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &FilterSet::default());
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["version"], 1, "additive fields do not bump the schema");
        let findings = v["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f["bus"], 3);
        assert_eq!(f["address"], 2);
        assert_eq!(f["path"], "3-1");
        assert_eq!(f["port"], "usb3-port1");
        assert_eq!(f["link_mbps"], 480.0);
        assert_eq!(f["capability_mbps"], 5000.0);
        assert_eq!(f["capability_source"], "bcd_usb");
        assert_eq!(f["cause"], "superspeed_side_empty");
        assert_eq!(f["peer_port"], "usb4-port1");
        assert!(f["upstream"].is_null());
        assert!(f["limit_mbps"].is_null());
        assert!(f["message"].as_str().unwrap().starts_with("linked at 480M, supports 5G (from bcdUSB): "));
        let devices = v["buses"][0]["devices"].as_array().unwrap();
        let root = devices.iter().find(|d| d["address"] == 1).unwrap();
        assert!(root["capability_mbps"].is_null());
        assert!(root["capability_source"].is_null());
        let dev = devices.iter().find(|d| d["address"] == 2).unwrap();
        assert_eq!(dev["capability_mbps"], 5000.0);
        assert_eq!(dev["capability_source"], "bcd_usb");
    }

    #[test]
    fn findings_follow_the_filter() {
        let (_temp, mgr) = tree_with_a_finding();
        let baseline = Baseline::capture(&mgr);
        let filter = FilterSet::parse(&["bus=4".to_string()]).unwrap();
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &filter);
        assert!(report.findings.is_empty(), "the flagged device is on bus 3, which the filter excludes");
    }

    #[test]
    fn render_text_ends_with_the_findings_section() {
        let (_temp, mgr) = tree_with_a_finding();
        let baseline = Baseline::capture(&mgr);
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &FilterSet::default());
        let text = render_text(&report);
        assert!(text.contains("\nfindings: 1\n  3:2  3-1  0bda:9210  linked at 480M, supports 5G (from bcdUSB): the SuperSpeed side of this connector (usb4-port1) is empty, so the link came up at USB 2 speed; check the cable or the port\n\n"), "{text}");
        assert!(text.ends_with("\n\n"), "the blank terminator still ends the report");
    }

    #[test]
    fn render_text_says_findings_none_when_there_are_none() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &FilterSet::default());
        let text = render_text(&report);
        assert!(text.ends_with("findings: none\n\n"), "{text}");
    }
```

Check how `FilterSet` is constructed in the existing headless tests (grep
`FilterSet::` in the file) and use that constructor for `bus=4`.

- [ ] **Step 5: Implement the headless surface**

Add to `Report` (after `buses`):

```rust
    /// Devices linked below the speed they support, with the cause the
    /// topology proves (see `findings::analyze`); only devices the report
    /// lists. Empty when there is nothing to call out.
    pub findings: Vec<FindingReport>,
```

Add to `DeviceReport` (after `internal`):

```rust
    /// The highest link rate the device says it supports, in Mbps, and
    /// where that came from (`"bos"` or `"bcd_usb"`); `null` when unknown.
    pub capability_mbps: Option<f64>,
    pub capability_source: Option<&'static str>,
```

New struct and conversion:

```rust
/// One finding as the JSON report carries it: the device, the numbers,
/// the cause as a `snake_case` tag, and the sentence the text report and
/// the TUI show. Fields a cause does not use are `null`.
#[derive(Serialize)]
pub struct FindingReport {
    pub bus: u8,
    pub address: u8,
    pub path: String,
    pub port: Option<String>,
    pub link_mbps: f64,
    pub capability_mbps: f64,
    pub capability_source: &'static str,
    pub cause: Option<&'static str>,
    pub peer_port: Option<String>,
    pub upstream: Option<String>,
    pub limit_mbps: Option<f64>,
    pub message: String,
}

impl From<&Finding> for FindingReport {
    fn from(finding: &Finding) -> Self {
        let (peer_port, upstream, limit_mbps) = match &finding.cause {
            Some(Cause::SuperSpeedSideEmpty { peer_port }) => (Some(peer_port.clone()), None, None),
            Some(Cause::UpstreamHubLink { hub, hub_link }) => (None, Some(hub.clone()), Some(hub_link.to_mbps())),
            Some(Cause::HostPortMax { max }) => (None, None, Some(max.to_mbps())),
            Some(Cause::Usb2OnlyHostPort) | Some(Cause::UpstreamPermits) | None => (None, None, None),
        };
        FindingReport {
            bus: finding.bus,
            address: finding.address,
            path: finding.path.clone(),
            port: finding.port.clone(),
            link_mbps: finding.link.to_mbps(),
            capability_mbps: finding.capability.speed.to_mbps(),
            capability_source: finding.capability.source.as_str(),
            cause: finding.cause.as_ref().map(Cause::kind),
            peer_port,
            upstream,
            limit_mbps,
            message: finding.message(),
        }
    }
}
```

In `build_report`, fill the two device fields:

```rust
                        capability_mbps: device.capability.as_ref().map(|c| c.speed.to_mbps()),
                        capability_source: device.capability.as_ref().map(|c| c.source.as_str()),
```

and after `bus_reports` is built:

```rust
    // The same scan `ui::sync_from` does: bounded to the manager's own
    // device directories, under the bundle's `sysfs/` in replay.
    let index = PortIndex::scan_devices(
        manager
            .buses
            .values()
            .flat_map(|bus| bus.devices.values())
            .filter_map(|device| device.sysfs_path.as_deref()),
    );
    let listed: HashSet<(u8, u8)> = bus_reports
        .iter()
        .flat_map(|bus| bus.devices.iter().map(|d| (d.bus, d.address)))
        .collect();
    let findings = crate::findings::analyze(manager, &index)
        .iter()
        .filter(|f| listed.contains(&(f.bus, f.address)))
        .map(FindingReport::from)
        .collect();
```

with `findings,` in the `Report` literal, `use std::collections::HashSet;`,
`use crate::connector::PortIndex;`, `use crate::findings::{Cause, Finding};`.
Reword the doc comment at 147-155: "Pure over the manager's state except
for one read-only scan of the manager's own device directories for their
port objects (the connector index the findings need), no clock reads other
than the `timestamp` field."

`render_text`, before `out.push('\n');`:

```rust
    if report.findings.is_empty() {
        out.push_str("findings: none\n");
    } else {
        out.push_str(&format!("findings: {}\n", report.findings.len()));
        for finding in &report.findings {
            let id = report
                .buses
                .iter()
                .flat_map(|bus| bus.devices.iter())
                .find(|d| d.bus == finding.bus && d.address == finding.address)
                .map_or_else(
                    || "----:----".to_string(),
                    |d| match (&d.vendor_id, &d.product_id) {
                        (Some(v), Some(p)) => format!("{v}:{p}"),
                        _ => "----:----".to_string(),
                    },
                );
            out.push_str(&format!(
                "  {}:{}  {}  {}  {}\n",
                finding.bus, finding.address, finding.path, id, finding.message
            ));
        }
    }
```

Update the doc comment of `render_text` to name the section. Add
`findings: Vec::new(),` to the two literal `Report` constructions
(`fixture_replay.rs:304-320`, `headless/export.rs:244-254`).

Run `cargo test headless` : green. `cargo test fixture_corpus` : the golden
comparison FAILS on every bundle (new keys). That is expected; Step 6 fixes it.

- [ ] **Step 6: Re-bless every golden and verify the diff is additive**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
for d in tests/fixtures/hosts/*/*/; do
  [ -f "$d/golden.binary.json" ] || continue
  b=${d#tests/fixtures/hosts/}; b=${b%/}
  USBTOP_NG_BLESS_BUNDLE="$b" cargo test bless_named_bundle -- --ignored --nocapture || break
done
for g in tests/fixtures/hosts/*/*/golden.*.json; do
  diff <(git show HEAD:"$g" | jq -S .) \
       <(jq -S 'del(.findings, .buses[].devices[].capability_mbps, .buses[].devices[].capability_source)' "$g") >/dev/null \
    && jq -e '(.findings == []) and ([.buses[].devices[]|has("capability_mbps") and has("capability_source")]|all)' "$g" >/dev/null \
    && echo "OK   $g" || echo "DRIFT $g"
done
```

Expected: 36 `OK` lines, no `DRIFT`. (No bundle in the corpus carries a
bcdUSB 3.x device at a lower link, so every `findings` list is empty until
Task 5; a non-empty one here is a defect to investigate, not to bless.)
Then `cargo test --all-targets` : green, including `fixture_corpus`.

- [ ] **Step 7: Gates and commit**

fmt, the four clippy configs, `cargo test --all-targets`, `cargo test
--features capture-fixture`, `cargo test --features integration`. Commit:

```bash
git add src/connector/mod.rs src/findings/mod.rs src/main.rs src/headless/mod.rs src/headless/export.rs src/fixture_replay.rs tests/fixtures/hosts
git commit -m "feat(findings): call out devices linked below capability, with the cause; findings and capability in the reports"
```

---

### Task 3: The TUI

**Files:**
- Modify: `src/ui/mod.rs` (`DeviceRow` 57-61; `sync_from` 390-425;
  `device_list_lines_with_selection` 1564-1608; `push_device_row`
  1663-1729; `header_lines` 1084-1165; help overlay line 1854; the seven
  literal `DeviceRow { .. }` in tests at 4926-5006; new tests)

**Interfaces:**
- Consumes: `findings::{analyze, Finding}`, `SpeedIndicator::BelowCapability`,
  `UsbDevice::get_speed_indicator(Option<&UsbSpeed>)`.
- Produces: `DeviceRow::finding: Option<Finding>`,
  `UsbTopApp::findings_count(&self) -> usize`.

- [ ] **Step 1: Tests first**

Add to the ui tests, next to `connector_fixture`:

```rust
    /// `topology_fixture` plus a root port pair whose USB 2 side holds a
    /// USB 3 device (bcdUSB 3.20, no BOS) linked at 480 with the
    /// SuperSpeed side empty: one finding, on `3-1` (dev 2).
    fn flagged_fixture() -> (tempfile::TempDir, DeviceManager) {
        use std::os::unix::fs::symlink;
        let (temp, _) = topology_fixture();
        let base = temp.path().join("devices");
        let ctrl = temp.path().join("0000:00:14.0");
        let p3 = ctrl.join("usb3").join("usb3:1.0").join("usb3-port1");
        let p4 = ctrl.join("usb4").join("usb4:1.0").join("usb4-port1");
        std::fs::create_dir_all(&p3).unwrap();
        std::fs::create_dir_all(&p4).unwrap();
        symlink(&p4, p3.join("peer")).unwrap();
        symlink(&p3, p4.join("peer")).unwrap();
        let d = base.join("3-1");
        std::fs::create_dir_all(&d).unwrap();
        for (k, v) in [("busnum", "3"), ("devnum", "2"), ("speed", "480"), ("version", "3.20")] {
            std::fs::write(d.join(k), format!("{v}\n")).unwrap();
        }
        let mut mgr = DeviceManager::with_sysfs_base(base);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        (temp, mgr)
    }

    #[test]
    fn a_flagged_row_carries_the_marker_and_its_reason_line() {
        let (_temp, mgr) = flagged_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        assert_eq!(app.findings_count(), 1);

        let (lines, _) = device_list_lines_with_selection(&app);
        let at = lines
            .iter()
            .position(|l| l.to_string().contains("003:002"))
            .expect("the flagged device's row");
        assert_eq!(lines[at].spans[INDICATOR_SPAN_INDEX].content.trim(), "🔺");
        let reason = lines[at + 1].to_string();
        assert!(reason.starts_with("          🔺 linked at 480M, supports 5G (from bcdUSB): the SuperSpeed side of this connector (usb4-port1) is empty"), "{reason}");
        assert_eq!(lines[at + 1].style.fg, Some(Color::Rgb(255, 255, 0)), "the reason line wears the indicator's colour");
        assert!(
            !lines.iter().any(|l| l.to_string().contains("supports") && !l.to_string().contains("🔺")),
            "no other row carries a reason line"
        );
    }

    #[test]
    fn a_reason_line_shifts_the_rows_below_it_but_not_the_selection_key() {
        let (_temp, mgr) = flagged_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        // Select the device rendered after the flagged one (3-2, dev 3).
        app.selected_device = Some("3:3".to_string());
        let (lines, selected_line) = device_list_lines_with_selection(&app);
        let flagged = lines.iter().position(|l| l.to_string().contains("003:002")).unwrap();
        let selected = selected_line.expect("the selected row's line");
        assert!(selected > flagged + 1, "the reason line sits between the two rows");
        assert!(lines[selected].to_string().contains("003:003"));
    }

    #[test]
    fn the_header_counts_findings_only_when_there_are_any() {
        let (_temp, mgr) = flagged_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        let stats_line = &header_lines(&app)[1];
        assert!(stats_line.spans.iter().any(|s| s.content == " | findings: "));
        let value = stats_line.spans.iter().find(|s| s.content == "1").expect("the count");
        assert_eq!(value.style.fg, Some(WARNING_COLOR));

        let (_temp, plain) = topology_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&plain);
        assert!(!header_lines(&app)[1].to_string().contains("findings"));
    }
```

Extend the help-overlay test near 3286 with
`assert!(screen.contains("the line beneath says why"), "{screen}");`.
Run `cargo test ui::` : the new tests fail to compile (`finding`,
`findings_count`) or fail on their assertions.

- [ ] **Step 2: Implement**

`DeviceRow`:

```rust
pub struct DeviceRow {
    pub port_chain: Option<Vec<u32>>,
    pub device: UsbDevice,
    /// The call-out the findings engine attached to this device, when it
    /// is linked below the speed it supports (see `findings::analyze`).
    pub finding: Option<Finding>,
}
```

`sync_from`, after the index is built:

```rust
        let mut findings: HashMap<(u8, u8), Finding> = analyze(manager, &index)
            .into_iter()
            .map(|finding| ((finding.bus, finding.address), finding))
            .collect();
```

and in the row construction `finding: findings.remove(&(device.bus_id,
device.device_id)),`. Imports: `use crate::findings::{analyze, Finding};`
(and `HashMap` if not already imported). Add:

```rust
    /// How many visible rows carry a finding.
    pub fn findings_count(&self) -> usize {
        self.controllers
            .iter()
            .flat_map(ControllerView::rows)
            .filter(|row| row.finding.is_some())
            .count()
    }
```

`device_list_lines_with_selection`: drop the bus-speed lookup for
connector rows (the whole `let speed = ...` block and the comment above
it) and call `push_device_row(&mut lines, app, row, &mut selected_line)`
at both sites. `push_device_row` loses its `bus_speed` parameter; its doc
comment drops the last sentence; the indicator becomes:

```rust
    let indicator = device.get_speed_indicator(row.finding.as_ref().map(|f| &f.capability.speed));
```

After `lines.push(Line::from(spans).style(status_style));`:

```rust
    // The call-out's reason, directly under the row it concerns, in the
    // indicator's colour. Always shown, not only when selected: the point
    // is to be noticed. Indented to the Device column like an endpoint row.
    if let Some(finding) = &row.finding {
        let (r, g, b) = indicator.get_color();
        lines.push(
            Line::from(format!("{}🔺 {}", " ".repeat(FINDING_INDENT), finding.message()))
                .style(Style::default().fg(Color::Rgb(r, g, b))),
        );
    }
```

with `const FINDING_INDENT: usize = DEVICE_COLUMNS[0] + 2;` next to
`DEVICE_COLUMNS` (verify against the endpoint test's blank Port cell of
8 spaces plus the 2-space separator: the Device cell starts at column 10).

`header_lines`, after the `shed` block:

```rust
    // Devices linked below the speed they support; the rows say why.
    let findings = app.findings_count();
    if findings > 0 {
        stats_line.push(Span::raw(" | findings: "));
        stats_line.push(Span::styled(
            findings.to_string(),
            Style::default()
                .fg(WARNING_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
    }
```

Help overlay line 1854: `"  • 🔺 linked below the speed it supports; the line beneath says why"`.
The seven test `DeviceRow { .. }` literals gain `finding: None,`.

- [ ] **Step 3: Gates and commit**

fmt, four clippy configs, `cargo test --all-targets`. Commit:

```bash
git add src/ui/mod.rs
git commit -m "feat(ui): the reason under a flagged row, and a findings count in the header"
```

---

### Task 4: Capture plumbing

**Files:**
- Modify: `src/capture/sysfs.rs:14-27` (`ATTRS`), `evals/run.sh:21-29`,
  `.claude/hooks/content-guard.sh:22-26`, `docs/TESTING.md:257-265`

- [ ] **Step 1: A failing materializer test**

Find the existing materializer tests in `src/capture/sysfs.rs` (grep
`fn materialize` in the test module) and add one that writes a device
directory with a 22-byte `bos_descriptors` (the camera bytes from Task 1)
and asserts the materialized copy is byte-identical, and that a device
without the file gets none:

```rust
    #[test]
    fn bos_descriptors_are_copied_as_bytes_and_only_when_present() {
        // Build the source tree the way the neighbouring tests do, with
        // one device carrying `bos_descriptors` and one without; run
        // `materialize_sysfs`; then:
        assert_eq!(std::fs::read(out.join("sysfs/3-1/bos_descriptors")).unwrap(), CAMERA_BOS);
        assert!(!out.join("sysfs/3-2/bos_descriptors").exists());
    }
```

Run it: fails (the file is not copied).

- [ ] **Step 2: Copy the attribute**

`ATTRS` becomes `[&str; 9]` with `"bos_descriptors"` last, and its doc
gains: "`bos_descriptors` is the one binary attribute: the device's own
capability statement (see `device::bos`), copied as bytes, absent on
kernels before 6.9 and on devices without a BOS." Test green.

- [ ] **Step 3: The scan allowlists**

`evals/run.sh`, the `case` at line 27:

```sh
  # The corpus's one binary attribute: each device's BOS, copied byte for
  # byte by the fixture capturer (see src/capture/sysfs.rs ATTRS).
  case "$f" in *.bin|tests/fixtures/*/sysfs/*/bos_descriptors) continue ;; esac
```

`.claude/hooks/content-guard.sh`, the `case` at line 22-26: add
`*tests/fixtures/*/sysfs/*/bos_descriptors) exit 0 ;;` with the same
comment. Verify the hook still flags a NUL elsewhere: the hook has its own
tests (grep `content-guard` under `evals/` or `.claude/hooks/`); run them
if present. Run `bash evals/run.sh` : passes (no BOS files exist yet; the
allowlist is exercised in Task 5).

- [ ] **Step 4: TESTING.md**

In the "sysfs snapshot copies" paragraph (257-265) add that the snapshot
also copies each device's `bos_descriptors` when the kernel exposes it
(Linux 6.9+), the one binary file in a bundle besides the traces, so a
replay decides capability the way the live tool does.

- [ ] **Step 5: Gates and commit**

fmt, the four clippy configs (the materializer is behind
`capture-fixture`; run that config's tests too), `bash evals/run.sh`.

```bash
git add src/capture/sysfs.rs evals/run.sh .claude/hooks/content-guard.sh docs/TESTING.md
git commit -m "feat(capture): snapshot each device's BOS so replays decide capability like the live tool"
```

---

### Task 5: Recapture the dock bundle and pin its findings

Run by the session owner (needs the laptop over ssh; host details live
outside the repo).

- [ ] **Step 1: Build and stage the capturer**

`cargo build --release --features capture-fixture`; copy the binary and
`tests/fixtures/hosts/tgl-tb4-2026-08-31/stage1/internal-devices.toml` to
a user-owned scratch directory on the laptop.

- [ ] **Step 2: Capture with the same recipe**

On the laptop, as root: `--capture-fixture <scratch>/tgl-tb4-2026-09-12/stage2
--window 20 --baseline <scratch>/internal-devices.toml`, with a 400 MiB
direct read of the USB 2 flash drive on the Terminus chain started 3 s
into the window (the recipe the current meta.toml note records).

- [ ] **Step 3: Pull, replace, annotate**

Remove `tests/fixtures/hosts/tgl-tb4-2026-09-12/stage2` locally, pull the
new bundle with tar over ssh into `tests/fixtures/hosts/`, then re-edit
the `[generator] note` in its meta.toml: the existing text plus "The sysfs
snapshot carries each device's bos_descriptors, so the replay reads the
adapter's 10 Gb/s and the camera's 5 Gb/s capability from the BOS and the
report carries both findings." Delete the scratch directory on the laptop.

- [ ] **Step 4: Corpus tests**

In `src/fixture_corpus.rs`, next to the host-specific pairing tests:

```rust
/// The Thunderbolt 4 laptop with the dock: the two deliberate mis-placements
/// are the corpus's two findings, and the dock's own hub halves, which the
/// kernel pairs by port number onto empty ports, are not.
#[test]
fn the_dock_bundle_pins_the_two_findings_and_nothing_else() {
    let dir = fixtures_root().join("tgl-tb4-2026-09-12").join("stage2");
    let report = replay_fixture(&dir, FixtureSource::Binary).unwrap();
    let summary: Vec<(String, Option<&str>, f64, &str)> = report
        .findings
        .iter()
        .map(|f| (f.path.clone(), f.cause, f.capability_mbps, f.capability_source))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("3-1.4.5".to_string(), Some("upstream_hub_link"), 5000.0, "bos"),
            ("5-1.2".to_string(), Some("superspeed_side_empty"), 10000.0, "bos"),
        ]
    );
    assert_eq!(report.findings[0].upstream.as_deref(), Some("3-1.4"));
    assert_eq!(report.findings[1].peer_port.as_deref(), Some("6-1-port2"));
}

/// Every other bundle predates the BOS in the snapshot and holds no bcdUSB
/// 3.x device at a lower link: zero findings, and a capability source only
/// from bcdUSB.
#[test]
fn every_other_bundle_has_no_findings() {
    for bundle in discover_bundles() {
        if bundle.dir.ends_with("tgl-tb4-2026-09-12/stage2") {
            continue;
        }
        for source in sources_of(&bundle) {
            let report = replay_fixture(&bundle.dir, source).unwrap();
            assert!(report.findings.is_empty(), "{}: {:?}", bundle.dir.display(), report.findings.iter().map(|f| &f.path).collect::<Vec<_>>());
            for device in report.buses.iter().flat_map(|b| &b.devices) {
                assert_ne!(device.capability_source, Some("bos"), "{}: {}", bundle.dir.display(), device.address);
            }
        }
    }
}
```

Then re-bless the dock bundle (the capturer wrote goldens with the branch
build already; the bless is a no-op check): `USBTOP_NG_BLESS_BUNDLE=tgl-tb4-2026-09-12/stage2
cargo test bless_named_bundle -- --ignored` and `git diff --stat` shows
no golden change after the bless. Review the golden diff against the
previous commit as evidence: `capability_source` flips to `bos` on the
BOS-bearing devices, `findings` holds the two entries.

- [ ] **Step 5: Gates and commit**

`cargo test --all-targets`, `bash evals/run.sh` (the NUL scan now meets
real `bos_descriptors` files and must pass).

```bash
git add tests/fixtures/hosts/tgl-tb4-2026-09-12 src/fixture_corpus.rs
git commit -m "test(corpus): the dock bundle recaptured with each device's BOS; its two findings pinned"
```

---

### Task 6: Docs

**Files:** `README.md` (166-169, 651-652, a findings paragraph near the
report samples), `docs/ARCHITECTURE.md` (104-114, 123-127, 129-133,
316-359, a `findings/` entry), `docs/SCRIPTING.md` (report table 73-84,
device table 97-115, the example document 131-186, a "The findings list"
section before "The estimated field", a version sentence at 76 and 241),
`docs/TESTING.md` (164-165), `docs/ROADMAP.md` (cable and port
diagnostics 27-83; the connector-field idea at 7-8 stays), `CHANGELOG.md`
(Unreleased), plus the spec's message wording (the message no longer
starts with the path; the surfaces prefix it).

- [ ] **Step 1: Write them**

Follow each file's existing voice. README: the 🔺 rule now reads "🔺 marks
a device linked below the speed it supports, read from its BOS on Linux
6.9+ (bcdUSB 3.x as a floor elsewhere); the line beneath says why"; the
caveat says a device without a BOS and with bcdUSB below 3 is never
marked, so a missing 🔺 proves nothing; a short paragraph shows the text
report's `findings:` section with the two dock lines. SCRIPTING: the two
new device fields, the top-level `findings` array with its own field table
(every `FindingReport` field, the six cause tags and `null`), the example
document regenerated from the dock bundle's `golden.binary.json`
(trimmed), and "additive fields do not bump `version`". ARCHITECTURE:
`device/bos.rs` in the device section, the doctrine paragraph rewritten
(the capability is the device's own statement, decided per connector by
the findings engine; the fallback is a floor), a `#### 4b. Findings
(findings/)` entry, `ui/connectors.rs` named. TESTING: the two on-hand
rows say the call-out now fires (`superspeed_side_empty`,
`upstream_hub_link`) and that only the 2026-09-12 dock bundle carries BOS
blobs. ROADMAP: under cable and port diagnostics, a dated line on what
shipped (capability from the BOS, per-connector findings, the hub-half
matching) and a bullet for part 3, "shared uplink / bottleneck ranking
under load". CHANGELOG Unreleased, Added: "Capability call-outs: …";
Changed: "`--capture-fixture` snapshots copy `bos_descriptors`; every
corpus golden regenerated for the added report fields (`findings`,
`capability_mbps`, `capability_source`)".

- [ ] **Step 2: Commit**

```bash
git add README.md docs CHANGELOG.md
git commit -m "docs: capability call-outs in the README, architecture, scripting and testing docs"
```

## Self-review notes

- Spec coverage: capability (Task 1), engine rules H and D and messages
  (Task 2), JSON and text surfaces (Task 2), TUI (Task 3), capture and
  scan (Task 4), recapture and corpus pins (Task 5), docs (Task 6).
- Type consistency: `Capability { speed, source }`,
  `Cause::{SuperSpeedSideEmpty{peer_port}, Usb2OnlyHostPort,
  UpstreamHubLink{hub, hub_link}, HostPortMax{max}, UpstreamPermits}`,
  `Finding { bus, address, path, port, link, capability, cause }`,
  `analyze(&DeviceManager, &PortIndex) -> Vec<Finding>`,
  `FindingReport` field names match the JSON test and the corpus test.
- The message starts with `linked at`, not the path; the text report
  prefixes `bus:address path vid:pid`, the TUI line sits under its row,
  JSON carries `path` separately. The spec's wording is corrected in Task 6.

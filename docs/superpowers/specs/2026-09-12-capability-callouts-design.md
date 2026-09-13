# Capability call-outs — design

**Date:** 2026-09-12
**Status:** approved for implementation (design challenged in the planning
session; adversarial three-engine review before merge per REVIEW.md)

## Goal

Call out every device and hub that is linked below the speed it supports,
and say why, in the TUI and in the `--once`/`--batch` reports. The three
shapes this covers, all present on the corpus's Thunderbolt 4 laptop bundle
`tgl-tb4-2026-09-12/stage2`:

- **device attachment**: a device linked below its own capability (the
  NVMe adapter at `5-1.2`, a 10 Gb/s device linked at 480 Mb/s; the USB 3
  camera at `3-1.4.5`, held at 480 Mb/s on a USB 2 only hub chain);
- **a hub above the host port**: a hub whose capability exceeds the port it
  is on (a 10 Gb/s hub on a 5 Gb/s root port; a USB 3 hub on a USB 2 only
  root port);
- **a hub chained through a slower hub**: a 10 Gb/s hub below a 5 Gb/s hub,
  or a USB 3 hub below a USB 2 only hub.

Today nothing is called out: the capability signal is sysfs `version`
(bcdUSB) alone, a SuperSpeed device linked at High Speed reports bcdUSB
2.10, so the TUI's 🔺 never fires, and the reports carry no capability or
finding at all.

## The signal

Linux 6.9 and later expose `/sys/bus/usb/devices/<dev>/bos_descriptors`:
the device's Binary device Object Store, a header followed by device
capability descriptors, readable without privilege
(`drivers/usb/core/sysfs.c` `bos_descriptors_read`; the file is hidden when
the device has no BOS, and the kernel does not request a BOS from a device
whose bcdUSB is below 2.01). The layouts are in the kernel's uapi header
`include/uapi/linux/usb/ch9.h`:

- BOS header: `bLength` 5, `bDescriptorType` 0x0F (`USB_DT_BOS`),
  `wTotalLength` little-endian, `bNumDeviceCaps`.
- Each capability: `bLength`, `bDescriptorType` 0x10
  (`USB_DT_DEVICE_CAPABILITY`), `bDevCapabilityType`.
- SuperSpeed capability, type 3 (`USB_SS_CAP_TYPE`, length 10):
  `wSpeedsSupported` at offset 4; bit 3 (`USB_5GBPS_OPERATION`) means
  5 Gb/s operation.
- SuperSpeedPlus capability, type 0x0A (`USB_SSP_CAP_TYPE`, length at
  least 12): `bmAttributes` u32 at offset 4, whose low five bits
  (`USB_SSP_SUBLINK_SPEED_ATTRIBS`) hold the sublink attribute count minus
  one; the u32 sublink speed attributes start at offset 12. In each, bits
  4-5 are the lane speed exponent (`USB_SSP_SUBLINK_SPEED_LSE`: 0 b/s,
  1 Kb/s, 2 Mb/s, 3 Gb/s) and the mantissa is in the high bits
  (`USB_SSP_SUBLINK_SPEED_LSM`, bits 16 and up).

Decoded on the laptop: the NVMe adapter (0bda:9210, linked 480) carries a
SuperSpeed capability and a SuperSpeedPlus capability with two 10 Gb/s
sublink attributes; the mis-placed camera (1409:3270, linked 480) carries a
SuperSpeed capability byte-identical to its two siblings that link at
5 Gb/s; the dock's billboard device carries USB 2 extension, container ID
and billboard capabilities and nothing SuperSpeed; the Realtek hub's USB 2
half (`3-1`) carries a SuperSpeed capability, which is normal: its USB 3 half
is `4-1` on the peer port.

Fleet coverage: the x86 hosts (7.0 kernels), pi-4 and pi-400 (6.18) and
pi-zero (6.12) have the attribute; pi-5 (6.6), rock5c (6.1) and bm1684x (5.4)
do not. Every corpus bundle captured before this change lacks it, because
the sysfs snapshot copied eight attributes and not this one.

## Decisions

- **Capability, per device.** If `bos_descriptors` is readable, the BOS
  decides alone: SuperSpeedPlus gives the largest sublink rate (10 or
  20 Gb/s), SuperSpeed alone gives 5 Gb/s, neither gives no capability (a
  USB 2 device that happens to have a BOS, such as the billboard). If the
  file is absent, today's rule stands: bcdUSB 3.x gives 5 Gb/s. The
  capability carries its source, `bos` or `bcd_usb`.
- **The fallback is a floor, not a guess.** bcdUSB 3.10 without a BOS gives
  5 Gb/s, so a device linked at 5 Gb/s on such a host is never called out
  for a 10 Gb/s ability the tool cannot see. No wording anywhere calls the
  fallback a guess; the finding text says "(from bcdUSB)" and the JSON
  carries the source. On the three fleet hosts without the attribute this
  is exactly today's behaviour, labelled.
- **Per-lane rate.** The BOS states lane rates, not lane counts, so the
  capability is the per-lane rate: a dual-lane (20 Gb/s) device linked
  single-lane at 10 Gb/s is not called out. Documented, not worked around.
- **Findings are decided per connector, from topology, not from capability
  alone.** A hub's USB 2 half always advertises SuperSpeed; it is only a
  finding when its USB 3 half is missing. So the engine consults the
  connector pairing (`connector::PortIndex`, the kernel's reciprocal `peer`
  links) and the device tree.
- **The kernel's port pairing is not trusted blindly for hubs.**
  `drivers/usb/core/port.c` `find_and_link_peer` pairs a hub's ports by
  port number with the ports of the hub on its upstream port's peer. The
  laptop's dock has an inner hub whose USB 2 half is `5-1.1` (port 1 of
  `5-1`) and whose USB 3 half is `6-1.4` (port 4 of `6-1`): the kernel
  pairs `5-1-port1` with the empty `6-1-port1` and `6-1-port4` with the
  empty `5-1-port4`, and gives neither half's own ports any peer. The
  engine therefore matches hub halves itself (rule H below) and, where the
  match is ambiguous, says nothing.
- **Verdict doctrine** (docs/ROADMAP.md, cable and port diagnostics):
  convict only a uniquely limiting party, exonerate confidently, say
  nothing where attribution is ambiguous. Every finding states the
  symptom (linked at X, supports Y); the cause is optional and named only
  when the topology proves it.
- **Where findings appear.** The TUI keeps 🔺 in the `!` column and adds
  one line under the flagged row with the reason, plus a `findings: N`
  counter in the header when N is nonzero. The JSON report gains a
  top-level `findings` list and two per-device fields, `capability_mbps`
  and `capability_source`; the text report gains a `findings` section at
  the end. Findings are top-level, not nested per device, so a script can
  check `.findings | length` without walking the tree and so a later
  topology-level finding needs no device to hang from.
- **Report version stays 1.** The fields are additive; the scripting doc
  says so. Every committed golden is regenerated once and the diff is
  verified to be exactly the new keys.
- **The dock bundle is recaptured in place** (`tgl-tb4-2026-09-12/stage2`,
  same day, same topology, same traffic recipe) once the sysfs snapshot
  copies `bos_descriptors`, so the corpus holds real BOS bytes and pins the
  two expected findings. No other bundle is recaptured: they exercise the
  fallback path and must produce zero findings.
- **Part 3 is deferred.** Bottleneck ranking under load (several devices
  sharing one uplink) waits until the dock carries traffic; the finding
  model leaves it room (an open `cause` enum).

## Architecture

### `device::bos` (new file `src/device/bos.rs`)

```rust
pub enum CapabilitySource { Bos, BcdUsb }
pub struct Capability { pub speed: UsbSpeed, pub source: CapabilitySource }
pub fn capability_from_bos(bytes: &[u8]) -> Option<UsbSpeed>
pub(super) fn read_capability(dir: &Path) -> Option<Capability>
```

`capability_from_bos` walks the descriptors as laid out above and returns
the maximum of the SuperSpeed 5 Gb/s and every SuperSpeedPlus sublink rate,
or `None` when neither capability is present. Malformed input (a short or
mistyped header, a zero `bLength`, a descriptor running past the buffer, a
`wTotalLength` beyond the bytes read) stops the walk; it never panics.
`read_capability` reads the file to EOF with `std::fs::read` (sysfs
declares a size it does not deliver) and falls back to bcdUSB only when the
file is absent. `UsbDevice::max_capability` becomes
`capability: Option<Capability>`; `check_speed_mismatch` and the bus-speed
argument of the indicator go away, because the engine decides.

### `connector` additions

`PortIndex::get(name) -> Option<&PortInfo>` becomes available outside tests,
and `device_name_of_port(hub, number)` is the inverse of `port_of_device`:
`usb6` port 2 is `6-2`, `6-1` port 2 is `6-1.2`.

### `findings` (new module `src/findings/mod.rs`)

```rust
pub struct Finding {
    pub bus: u8, pub address: u8, pub path: String, pub port: Option<String>,
    pub link: UsbSpeed, pub capability: Capability, pub cause: Option<Cause>,
}
pub enum Cause {
    SuperSpeedSideEmpty { peer_port: String },
    Usb2OnlyHostPort,
    Usb2OnlyPort { hub: String, number: u32 },
    UpstreamHubLink { hub: String, hub_link: UsbSpeed },
    HostPortMax { max: UsbSpeed },
    UpstreamPermits,
}
pub fn analyze(manager: &DeviceManager, ports: &PortIndex) -> Vec<Finding>
```

Pure over the manager's devices (name from the sysfs path, bus, address,
link speed, capability; disconnected devices are ignored) and the port
index; no I/O. Output sorted by (bus, address). Only a present, non-root
device whose capability exceeds a known link speed can produce a finding.

**Rule H, the SuperSpeed half of a hub** (`ss_half(hub)`, memoized):

1. A root hub `usbN`: the hub owning the peer of any of its ports
   (`usb5-port1` peers `usb6-port1`, so `usb6`), if present.
2. Another hub at 480 Mb/s or below: the device on the kernel peer of its
   own port, if present.
3. Otherwise, only when the hub's capability is known to exceed 480 Mb/s:
   with P2 the hub owning its port and P3 = `ss_half(P2)`, let A be the
   present hubs on P2 at or below 480 Mb/s with a known capability above
   it whose kernel peer port holds no present **hub**, and B the present
   hubs on P3 at 5 Gb/s or more whose kernel peer port holds no present
   **hub** — only a hub can be the other half of a hub (kernel-paired
   ports are one receptacle), so a plain device on that peer port claims
   nothing. B empty means every SuperSpeed hub under P3 is spoken for, or
   there is none: this hub's half never enumerated and is *missing*. When
   both sets have exactly one member and the two agree on `idVendor` where
   both are known, they are halves of one hub. Otherwise the half is
   *ambiguous*.

The verdict is three-valued — `Known` (and whether it was found by the
kernel's `peer` or by elimination), `Missing`, `Ambiguous` — because
`Missing` is proof that a SuperSpeed half never enumerated, while
`Ambiguous` is ignorance.

**Rule D, a device D at link L with capability C > L**, own port (P, n)
from `port_of_device`; when the index does not know that port, the finding
has no cause:

- L at or below 480 Mb/s:
  - D is a hub whose `ss_half` is `Known` (its USB 3 half is up) or
    `Ambiguous` (nothing is provable): no finding.
  - The SuperSpeed port is the reciprocal kernel `peer` of D's own port
    when there is one; failing that, and only when `ss_half(P)` is `Known`
    *by elimination* (where the kernel left the ports unpaired by
    construction), it is (P3, n) if the index knows that port. The two
    halves of a controller number their root ports independently — the
    corpus's `tgl-x360` bundle pairs `usb3-port1` with `usb4-port2` — so
    the number is never carried across on its own.
  - A present device on that SuperSpeed port means no finding; an empty one
    means `SuperSpeedSideEmpty { peer_port }`.
  - No SuperSpeed port: P a root hub gives `Usb2OnlyHostPort`; P a hub
    whose half is `Known` gives `Usb2OnlyPort { hub: P, number: n }` (the
    hub is fine, this receptacle of it is USB 2 only); P a hub whose half
    is `Missing` gives `UpstreamHubLink { hub: P, hub_link: P.link }`,
    which is true both for a USB 2 only hub and for a USB 3 hub whose own
    link fell to 480; P a hub whose half is `Ambiguous` gives no cause.
- L at 5 Gb/s or more, with the limit the bus's root-hub speed when P is a
  root hub and P's own link otherwise:
  - No limit, or a limit at or below zero (P absent, disconnected, or of
    unknown rate): no cause. Ignorance is not permission.
  - The limit below C: `HostPortMax { max }` under a root hub,
    `UpstreamHubLink` under a hub.
  - Otherwise `UpstreamPermits`.

**Messages** (`Finding::message()`, one string shared by the text report,
the TUI line and the JSON `message` field): `"linked at 480M, supports
10G"` with `" (from bcdUSB)"` appended for the fallback, then `": "` and the
cause. The message carries no path of its own; each surface prefixes what it
needs — the text report `bus:address path vid:pid`, the TUI line sits under
its own row, and the JSON finding carries `path` as a field. The causes:
SuperSpeedSideEmpty "the SuperSpeed side of this connector
(<peer_port>) is empty, so the link came up at USB 2 speed; check the cable
or the port"; Usb2OnlyHostPort "this host port is USB 2 only; move it to a
USB 3 port"; Usb2OnlyPort "port <n> of the hub above it (<hub>) is USB 2
only; move it to a USB 3 port"; UpstreamHubLink "the hub above it (<hub>)
is linked at <hub_link>" plus "; move it to a USB 3 port" when that link is
a known rate at 480 Mb/s or below; HostPortMax "this host port tops out at
<max>"; UpstreamPermits "the port above it allows <C>; check the cable or
the device"; no cause "why is not attributable from the connector
topology" — which covers a port the index does not know, an ambiguous hub
pairing, and an upstream of unknown rate alike.

Expected on the recaptured dock bundle: exactly two findings, `5-1.2`
(10 Gb/s via bos, SuperSpeedSideEmpty at `6-1-port2`) and `3-1.4.5` (5 Gb/s
via bos, UpstreamHubLink at `3-1.4`, 480 Mb/s). Nothing for `5-1`, `5-1.1`,
`3-1`, `5-1.1.8`, the Terminus hubs or the root hubs. Zero findings on every
other bundle.

### Surfaces

- **TUI.** `sync_from` already builds a `PortIndex` per tick; it runs
  `analyze` and attaches `finding: Option<Finding>` to each `DeviceRow`.
  🔺 (`SpeedIndicator::BelowCapability`) fires when the row has a finding;
  the indicator choice moves into the UI. A flagged row gets one extra line
  beneath it, indented like an endpoint line, in the indicator's yellow:
  `🔺 <message>`. Selection is keyed by device, not line, so scrolling and
  selection are untouched. The help overlay bullet reads "🔺 linked below
  the speed it supports; the line beneath says why". The header shows
  ` | findings: N` in the warning colour when N is nonzero.
- **Headless.** `build_report` (signature unchanged) builds the same
  `PortIndex` and runs `analyze`. `Report` gains `findings`, `DeviceReport`
  gains `capability_mbps` and `capability_source` (null when unknown). A
  finding serializes as `bus`, `address`, `path`, `port`, `link_mbps`,
  `capability_mbps`, `capability_source`, `cause` (snake_case or null),
  `peer_port`, `upstream`, `limit_mbps` and `message`, with null for the
  fields a cause does not use. The text report ends with `findings: N` and
  one indented line per finding (`  5:7  5-1.2  0bda:9210  <message>`), or
  `findings: none`.
- **Capture.** The sysfs snapshot copies `bos_descriptors` (binary, copied
  as bytes, skipped when absent). The NUL-byte scan in `evals/run.sh` and
  the advisory hook exempt files named `bos_descriptors` under
  `tests/fixtures/`, the one binary attribute the corpus carries by design.

## Testing

- `device::bos`: synthetic byte arrays written as `&[u8]` for the adapter
  shape (10 Gb/s), the camera shape (5 Gb/s), the billboard shape (none), a
  20 Gb/s mantissa, and truncated or garbage inputs; `read_capability`
  precedence (a BOS without SuperSpeed beats bcdUSB 3.x; no file falls
  back, source `BcdUsb`).
- `connector`: `device_name_of_port` round-trips with `port_of_device` for
  root and nested hubs.
- `findings`: synthetic sysfs trees in a tempdir, read through the real
  `DeviceManager` and `PortIndex`: the dock shape (paired root ports,
  `5-1`/`6-1` by kernel peers, `5-1.1`/`6-1.4` by rule H step 3, `5-1.2`
  with an empty peer, `3-1.4.5` under a USB 2 only hub); the ambiguous
  case (two unclaimed hubs each side: no finding for those hubs, their
  children get `UpstreamHubLink`); a 10 Gb/s hub on a 5 Gb/s root port
  (`HostPortMax`); a 10 Gb/s device under a 5 Gb/s hub (`UpstreamHubLink`);
  a 10 Gb/s device at 5 Gb/s under a 10 Gb/s hub (`UpstreamPermits`); a USB 3
  hub on a USB 2 only root port (the hub `Usb2OnlyHostPort`, its child
  `UpstreamHubLink`); the bcdUSB fallback source; a disconnected device
  ignored; deterministic order.
- `ui`: a flagged row renders 🔺 and the extra line, the next row's
  selected line shifts by one, the header counter appears only when
  nonzero and in the warning colour, the help bullet text.
- `headless`: the JSON shape of a finding and of the per-device fields,
  `version` still 1, the text section with findings and with none.
- Corpus: every bundle replays to its regenerated golden; the dock bundle
  pins its two findings; every other bundle has zero findings and a null
  capability source except for bcdUSB 3.x devices.
- Live: the laptop shows the two findings in all three surfaces; an
  old-kernel host shows no false findings.

## Non-goals

- Bottleneck ranking under load (part 3).
- A `connector` field on device rows in the reports (findings carry the
  kernel port name; the row-level field stays a roadmap item).
- Lane-count awareness (dual-lane 20 Gb/s devices linked single-lane).
- Any change to the capture privacy rules: `serial` is still never copied.

## Docs

README (the 🔺 rule and its caveat, a findings paragraph with a text
sample), docs/ARCHITECTURE.md (device section, the capability doctrine, a
`findings/` entry, `ui/connectors.rs`), docs/SCRIPTING.md (field tables,
the example document, a findings explainer, the additive-version sentence),
docs/TESTING.md (the snapshot paragraph, the two on-hand rows, which bundle
carries BOS blobs), CHANGELOG.md, docs/ROADMAP.md (what shipped under cable
and port diagnostics; part 3 as the next step).

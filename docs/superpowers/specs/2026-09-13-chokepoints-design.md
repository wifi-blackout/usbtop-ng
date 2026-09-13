# Choke points — design

**Date:** 2026-09-13
**Status:** approved in the design session (sections 1-4); spec under review;
adversarial three-engine review before merge per REVIEW.md

## Goal

Part 3 of the capability work: name the uplinks that would choke if every
device below them pushed what it can. The model is theoretical, as the
owner asked: it aggregates each end device's rate up through the hubs it
sits under and compares the sum with each hub's own link, without looking
at measured traffic. It answers "which hub is oversubscribed, and by how
much", ranked worst first, in the TUI header, on the choked hub's connector
heading, and in the `--once`/`--batch` reports, at one of two bases the
user can switch: the rates the devices have now, or the rates they are
capable of.

This first cut stops at the root hub. The stages above it (a controller's
PCIe uplink, and the Thunderbolt link a tunneled controller shares with
every other tunnel and with a host-to-host network peer) are the second
step; see Deferred.

## Decisions

- **A stage is a hub's own link.** Everything below a hub crosses that
  link. A root port is not a separate stage: a hub on a root port has that
  port as its link (the two rows would always be identical), and a leaf on
  a root port shares nothing. The USB 2 and USB 3 halves of one hub are two
  sysfs hubs with two links, so their sums are separate without any pairing
  logic; a USB 2 mouse under a USB 3 hub crosses the 480M half, a camera
  the 5G half. A hub is a device that owns port objects or has children; a
  hub with nothing below it asks nothing. A hub captured without its port
  objects and without children is indistinguishable from a leaf and counts
  as one; the two older `devhost` snapshots hold the same empty hub that
  way.
- **Demand.** A leaf's demand is its rate times the class efficiency factor
  the `%busy` denominators already use (`UsbSpeed::class().efficiency()`:
  0.7 low, 0.8 full and high, 0.85 SuperSpeed and SuperSpeedPlus). A hub's
  demand is the sum over its children. A device of unknown rate contributes
  nothing; a device the manager lists as disconnected is excluded; internal
  devices count, they are traffic sources.
- **Capacity.** A hub's capacity is its rate times the same factor.
- **A choke point** is a hub whose demand divided by its capacity is at or
  above the **breathing room** of 1.25. Below that, an uplink is not
  listed: a 480M hub carrying one flash drive and a mouse reads 1.03x, and
  the owner does not want to hear about it. The list is ordered worst
  first; each entry names how many devices sit below the hub and its three
  largest contributors.
- **Two bases, `link` and `capability`.** `link` (the default) uses every
  device's current rate on both sides. `capability` is the topology as it
  could link, given the hubs it has: a leaf's rate is the larger of its BOS
  capability (bcdUSB floor where that is all the tool has) and its link,
  bounded by the capacity of every hub above it; a hub's capacity is its
  link when it is a USB 2 half (a link at or below 480 never becomes more:
  the SuperSpeed capacity its BOS may advertise belongs to its other half,
  a different sysfs hub), and the larger of its link and its capability
  when it is a SuperSpeed half (a 10G hub linked at 5G counts as 10G).
  Bounding the leaf keeps the view honest: the empty NVMe adapter on the
  dock's USB 2 half asks 480 of it, not 10G, because nothing under a USB 2
  half can push more, and its real fix (moving to the USB 3 half) is a
  call-out the findings already make. On the dock bundle the capability
  view therefore equals the link view; it differs where a SuperSpeed device
  is linked below its capability under a SuperSpeed hub that has the room,
  the cable cases.
- **Recomputed every tick.** The TUI rebuilds its render model from the
  device manager every tick and the findings ride on that; the choke model
  is one walk over the same rows and rides on it too, so any device change
  shows on the next tick with no thread and no cache.
- **Where it shows.** The header, after `findings: N`: ` | choke: 3.05x`
  in the warning colour, bold, only when the worst ratio is at or above the
  breathing room; ` | choke: 3.05x (cap)` at the capability basis. The
  choked hub's connector heading, on the connector the hub sits on (that
  connector is its link): `▶ Port 1 · bus 03 + 04 · hub · choke 3.05x
  (1.17G asked of 384M)`. The `c` key toggles the basis; the controls line
  gains `c Capacity basis`; the help overlay explains the counter, the
  breathing room and the key.
- **Reports.** `--demand link|capability` (default `link`) picks the basis
  for `--once` and `--batch`. JSON (version stays 1, additive): top-level
  `demand_basis` (`"link"` or `"capability"`), `choke_floor` (`1.25`, the
  breathing room, so a script sees the floor that was applied) and
  `chokepoints`, worst first: `bus`, `address`, `path` (the hub),
  `port` (the kernel port name of the hub's link, the same key the findings
  carry), `capacity_mbps`, `demand_mbps`, `ratio`, `devices`, `top` (up to
  three `{path, demand_mbps}`), `message`. Text, after the findings
  section: `chokepoints: N` and one line per entry, `  hub 3-1 (3:2,
  0bda:5411) 384M carries 9 devices asking 1.17G: 3.05x`, or
  `chokepoints: none`.

## Expected on the dock bundle (`tgl-tb4-2026-09-12/stage2`)

Both bases, practical Mb/s:

| Hub | Capacity | Demand | Ratio | Listed |
| --- | --- | --- | --- | --- |
| `3-1.4` Terminus, 8 devices below | 384 | 1172 | 3.05 | yes |
| `3-1` Realtek USB 2 half, 9 below | 384 | 1172 | 3.05 | yes |
| `4-1` Realtek USB 3 half, two 5G cameras | 4250 | 8500 | 2.00 | yes |
| `3-1.4.7` second Terminus | 384 | 404 | 1.05 | no |
| `5-1` dock USB 2 half, adapter and billboard | 384 | 394 | 1.03 | no |
| `6-1` dock USB 3 half | 8500 | 8500 | 1.00 | no |

Order: `3-1.4` and `3-1` tie at 3.05; ties break by path, so `3-1` first,
then `3-1.4`, then `4-1`. The worst ratio the header shows is 3.05x.

## Architecture

### `capacity` (new module `src/capacity/mod.rs`)

```rust
pub const CHOKE_FLOOR: f64 = 1.25;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Basis { Link, Capability }   // as_str: "link" | "capability"

pub struct Contributor { pub path: String, pub demand_mbps: f64 }
pub struct Chokepoint {
    pub bus: u8, pub address: u8, pub path: String, pub port: Option<String>,
    pub capacity_mbps: f64, pub demand_mbps: f64, pub ratio: f64,
    pub devices: usize, pub top: Vec<Contributor>,
}
impl Chokepoint { pub fn message(&self) -> String }
pub fn analyze(manager: &DeviceManager, ports: &PortIndex, basis: Basis)
    -> Vec<Chokepoint>
```

Pure over the manager's rows: name from the sysfs path, bus, address, rate,
capability, disconnected flag. The tree comes from the names
(`connector::port_of_device` gives every device's hub). The connector index
says which devices own port objects, so a hub is any device that owns one or
has children: an empty hub is a hub, not a leaf asking its link rate. Both
callers already build that index for the findings. Root hubs are walked for
their children but are never a stage. The walk is depth-first from each root
hub, memoizing each hub's demand, so a nested hub's subtree is summed once.
Output sorted by ratio descending, then path; only entries at or above
`CHOKE_FLOOR`. `message()` renders `"384M carries 9 devices asking 1.17G:
3.05x"` with `findings::short_speed`, which moves to a small shared place
(`usbmon::parser::short_mbps`) so neither module depends on the other.

Basis rules, exactly:

- `rate(device, Link)` = its link rate.
- `rate(device, Capability)` = the larger of its capability (when known)
  and its link, then bounded: `min(rate, capacity_rate(hub))` for every hub
  above it, applied as the walk descends. The `max` mirrors the hub rule: a
  bcdUSB 3.x floor of 5 Gb/s never makes a device linked at 10 Gb/s ask for
  less than it already asks.
- `capacity_rate(hub, Link)` = its link rate.
- `capacity_rate(hub, Capability)` = its link rate when the link is at or
  below 480; else `max(link, capability)` when a capability is known, else
  the link.
- demand or capacity in Mb/s = rate times `class().efficiency()` of that
  rate; a rate of zero yields zero and a hub of zero capacity is never a
  choke point (division is guarded).

### Surfaces

- **TUI** (`src/ui/mod.rs`): `UsbTopApp` gains `demand_basis: Basis`
  (default `Link`) and, per tick in `sync_from`, `chokepoints:
  Vec<Chokepoint>`; `ConnectorView` gains `choke: Option<Chokepoint>`,
  attached to the connector the hub sits on (matched by the hub's port
  name, which `connector_placement` already knows); `header_lines`
  renders the counter from the worst ratio; `connector_line` appends the
  suffix; the `c` key flips the basis (`KeyOutcome::Redraw`; the next tick
  recomputes); the help overlay and the controls line gain their text.
- **Headless** (`src/headless/mod.rs`, `src/main.rs`): `HeadlessOptions`
  gains `demand: Basis`; `build_report` gains the basis as a parameter (its
  24 call sites pass `Basis::Link`, the replay included, so goldens are
  the link view); `Report` gains `demand_basis: &'static str`,
  `choke_floor: f64`, `chokepoints: Vec<ChokepointReport>`; `render_text`
  appends the section after the findings one, before the blank terminator.
  The `--demand` flag is a clap `ValueEnum` with `link` and `capability`.

## Testing

- `capacity`: synthetic trees through the findings tests' tempdir helper
  (moved to a shared `#[cfg(test)]` module so both use one): two 480M
  devices under a 480M hub (2.0x, listed, both named as contributors); a
  480M flash drive and a 1.5M mouse under one (1.03x, not listed); a mouse
  and a keyboard alone (0.03x); nested hubs (the parent's demand includes
  the child hub's whole subtree; the child hub is also its own entry when
  choked); the two halves of a USB 3 hub summed separately; an unknown-rate
  device contributing nothing; a disconnected device excluded; internal
  devices counted; the capability basis: a 10G device linked at 5G under a
  10G hub (link 0.5x, capability 1.0x, neither listed), two of them (link
  1.0x, capability 2.0x listed only at capability), the adapter shape (a
  10G device under a 480 half stays bounded, 1.03x at both bases), a 10G
  hub linked at 5G with two 10G devices (2.0x at both); top three of five
  contributors; worst-first order with the path tie-break; the floor
  exactly at 1.25 (listed) and just below (not).
- Corpus: the dock bundle pins the table above at both bases; every golden
  is re-blessed once for the three additive keys and the diff verified
  additive with the jq check; bundles that gain entries (any hub with two
  480M devices) are read as model output and listed in the commit message.
- `ui`: the header counter absent below the floor, present in the warning
  colour, with `(cap)` after `c`; `c` flips `demand_basis` and redraws; the
  connector heading suffix on the choked hub's connector and nowhere else;
  the help overlay and controls texts.
- `headless`: `--demand` parses both values and rejects others; the JSON
  shape with nulls where due; the text section with entries and with
  `none`; `demand_basis` follows the flag.
- Live: the laptop shows the three entries in the TUI header and reports.

## Docs

README (a Choke points paragraph after the findings one, the `c` key, the
breathing room); docs/SCRIPTING.md (the three fields, "The chokepoints
list" explainer with the breathing room and the two bases, `--demand`);
docs/ARCHITECTURE.md (the `capacity` module); CHANGELOG (Added);
docs/ROADMAP.md (part 3 shipped for the USB tree; the second step named).

## Deferred

- The controller's PCIe uplink as a stage (sysfs `current_link_speed` times
  `current_link_width`; integrated controllers expose none).
- The Thunderbolt link as a stage shared by every tunnel through a router
  (the dock: 2 lanes at 20 Gb/s) and by a host-to-host network peer when one
  is attached; the kernel gives the link rate and no per-tunnel allocation.
- Any measured-traffic overlay on the theoretical ratios.

## Non-goals

- Relocating a device to the hub half it would land on if a call-out were
  fixed; the findings name that move, this model does not simulate it.
- Per-endpoint or per-transfer-type modelling (isochronous reservations).

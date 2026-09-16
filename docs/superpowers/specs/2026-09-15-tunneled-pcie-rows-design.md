# Tunneled PCIe devices in the device table — design

**Date:** 2026-09-15
**Status:** approved in the design session (sections 1-5); spec under review;
adversarial three-engine review before merge per REVIEW.md

## Goal

The ROADMAP's deferred row: a tunneled PCIe device that is not a USB
controller, a 10G NIC behind a USB4 adapter say, shown as a sibling
consumer of the Thunderbolt link in the device table, and carried by the
`--once`/`--batch` reports. usbmon never sees such a device, so today the
TUI has no place for it; the kernel marks it `removable` and puts it under
the same root port as the tunneled xHCI, and that root port is the join.
The support bundle's `inventory/pci-removable.toml` already collects the
same devices; this design gives the live views the same facts.

The Thunderbolt link as a choke stage, the controller's PCIe uplink as a
stage, and a pci.ids lookup for names stay out (see Non-goals).

## What the hardware shows

Verified on the Thunderbolt 4 laptop with the CalDigit dock attached
(2026-09-15, live sysfs), and against the kernel source where the rule
depends on it:

- The dock's xHCI `0000:2e:00.0` hangs under root port `0000:00:07.1`
  through two bridges, `0000:2c:00.0` and `0000:2d:00.0`; all three read
  `removable`, the root port does not (`pci_set_removable` in
  drivers/pci/probe.c, v5.16 and v7.0: only a device below an
  external-facing or removable parent gets the attribute). Buses `usb5` and
  `usb6` name `0000:2e:00.0` as their controller.
- The router `0-3` reports `generation` 4, `rx_speed` and `tx_speed`
  `20.0 Gb/s`, `rx_lanes` and `tx_lanes` 2, `device_name` `Element Hub`.
  Routers are named `%u-%llx` (domain, route; drivers/thunderbolt/switch.c
  v6.12) and a route's depth is `tb_route_length` (tb.h v6.12: bits above
  each 8-bit hop), so a depth-one router's route is 1..=255 and `D-0` is
  the host router.
- Nothing in that sysfs maps a root port to a router: `/sys/class/devlink`
  has no entry for the host interface or the root ports, and the host router
  has no `usb4_port*` objects. The kernel's ABI file documents no such
  mapping either.
- `current_link_speed`, `current_link_width` and `max_link_width` are read
  through `pci_config_pm_runtime_get` (drivers/pci/pci-sysfs.c v7.0), which
  resumes a device in D3cold; `max_link_speed` is not. The bundle already
  reads the three only while `power/runtime_status` is `active`.
- No host on the fleet has a tunneled PCIe device that is not a USB
  controller. The dock's own bridges are the only other removable devices,
  and bridges are not rows.

## Decisions

- **The join is the root port.** Every removable, non-bridge PCI function
  is grouped by the first PCI address above it (root-first chain), and a
  bus whose controller is such a function belongs to that group instead of
  to its controller's own group. Nothing pairs a root port with a router
  except the one case that cannot be wrong: exactly one depth-one router
  in `/sys/bus/thunderbolt/devices` and exactly one root port with
  removable devices below it. Otherwise every group shows its root port
  alone. No guessed pairing, ever; the doctrine is the findings engine's.
- **Rows are topology, not traffic.** A PCIe row carries what sysfs says
  about the function and its link, never a bandwidth figure: usbmon does not
  see it, and nothing else measures it here. The traffic columns show `--`.
- **The link is read only from an awake device.** `current_link_speed` and
  `current_link_width` are opened only when `power/runtime_status` reads
  `active`; a sleeping function shows the passive `max_link_speed` and says
  `asleep`. The TUI refreshes every second, so a reader that woke devices
  would keep every tunneled device out of D3cold for as long as usbtop-ng
  ran.
- **Names from sysfs alone.** Class name from a built-in table over the PCI
  class code, the bound driver, vendor and device IDs, the network interface
  name when the function has one. A hidden USB controller is a row too: a
  tunneled xHCI with no bus (its driver gone, the adapter hung) appears as
  `USB controller · no driver`, which is the failure the user report
  described.
- **One reader, both bases, both surfaces.** `tunnel::read_tunnels` feeds
  the TUI each tick and `build_report_at` each window. The bundle keeps its
  own walker and file format; the two share the address check, the
  parent-chain walk and the wake rule through a new `pci` module, so they
  agree by construction.
- **Additive JSON.** `tunnels` is a new top-level list, empty when there is
  none; `version` stays 1, as it did for `findings` and `chokepoints`.

## 1. The reader (`src/tunnel/mod.rs`, new) and the shared helpers (`src/pci/mod.rs`, new)

`src/pci/mod.rs` takes over from `diag/inventory.rs`, unchanged in
behaviour: `is_address` (the former `is_pci_address`: a domain of four to
eight hex digits, a two-digit bus, a two-digit slot, a one-digit function),
`chain` (the former `pci_chain`: the PCI addresses above a resolved
directory, root-first), `LINK_ATTRS` and the citation that explains them,
plus `is_awake(real: &Path) -> bool` (`power/runtime_status` reads
`active`). The inventory's test
`pci_addresses_allow_wide_domains_and_nothing_else` moves with them. The
inventory keeps `read_pci`, `read_pci_attrs` and its notes.

```rust
pub struct TunnelRoots { pub pci: PathBuf, pub thunderbolt: PathBuf }
impl TunnelRoots {
    /// `/sys/bus/pci/devices` and `/sys/bus/thunderbolt/devices`.
    pub fn live() -> Self
}

pub struct Router {
    pub name: String,                 // "0-3"
    pub vendor_name: Option<String>,
    pub device_name: Option<String>,
    pub generation: Option<u32>,
    pub rx_gbps: Option<f64>,         // "20.0 Gb/s" -> 20.0
    pub tx_gbps: Option<f64>,
    pub rx_lanes: Option<u32>,
    pub tx_lanes: Option<u32>,
}

pub struct PciLink { pub gts: f64, pub width: u32 }

pub struct PciFunction {
    pub address: String,
    pub class: u32,                   // 0x020000
    pub class_name: &'static str,     // "Ethernet controller"
    pub vendor_id: u16,
    pub device_id: u16,
    pub driver: Option<String>,       // basename of the `driver` link
    pub interface: Option<String>,    // the one entry of `net/`, when present
    pub runtime_status: Option<String>, // `power/runtime_status`, as read
    pub awake: bool,                  // runtime_status == "active"
    pub link: Option<PciLink>,        // only when awake and both attrs parse
    pub max_gts: Option<f64>,         // `max_link_speed`, passive
}

pub struct Tunnel {
    pub root_port: String,
    pub router: Option<Router>,
    /// Every removable non-bridge function under the root port, USB
    /// controllers included, by address.
    pub functions: Vec<PciFunction>,
}

impl PciFunction {
    pub fn is_usb_controller(&self) -> bool  // class >> 8 == 0x0c03
}

pub fn read_tunnels(roots: &TunnelRoots) -> Vec<Tunnel>
```

Reading rules:

- Entries of `roots.pci` whose name passes `is_address` are resolved with
  `canonicalize`; an entry that fails to resolve or whose `removable` file
  is absent or does not read `removable` is skipped. A bridge (class byte
  `0x06`) is skipped. A function with an empty chain is skipped. The root
  port is `chain[0]`.
- Parsing: `vendor`, `device` and `class` are `0x`-prefixed hex; a value
  that does not parse skips the function (identity is the row).
  `max_link_speed` and `current_link_speed` are `"8.0 GT/s PCIe"`: the
  leading float. `current_link_width` is a decimal. A value that does not
  parse leaves the field `None`.
- `driver` is the basename of the `driver` symlink when it resolves.
  `interface` is the single directory entry of `net/` when there is exactly
  one; two or more leave it `None`.
- `runtime_status` is `power/runtime_status` trimmed, `None` when absent;
  `awake` is true only for `active`. `link` is read only when awake; when
  awake and either attribute is absent or unparsable it is `None`. Neither
  attribute is opened otherwise.
- Routers: entries of `roots.thunderbolt` of the form `<decimal>-<hex>`
  whose route parses and lies in 1..=255. Attributes are read as
  documented above; each is `None` when absent or unparsable. The join
  applies when exactly one router and exactly one tunnel were found.
- Tunnels sort by root port, functions by address, and the function list is
  never empty (a root port with only bridges below it is not a tunnel).
- An unreadable `roots.pci` yields an empty list; an unreadable
  `roots.thunderbolt` yields tunnels without routers. The reader has no
  notes channel; the bundle's walker keeps its own.

Class table (`class_name`), by the top byte and, where named, the top two
bytes; anything else prints `class 0x......`:

| code | name |
|---|---|
| `0x0108` | NVMe controller |
| `0x01` | mass storage controller |
| `0x0200` | Ethernet controller |
| `0x0280` | wireless controller |
| `0x02` | network controller |
| `0x03` | display controller |
| `0x0403` | audio device |
| `0x04` | multimedia controller |
| `0x0c03` | USB controller |
| `0x0c0a` | USB4 host interface |
| `0x0c` | serial bus controller |
| `0x08` | system peripheral |
| `0x0d` | wireless controller |
| `0x12` | processing accelerator |

## 2. The TUI (`src/ui/mod.rs`, `src/tui/mod.rs`)

- `UsbTopApp` gains `tunnels: Vec<Tunnel>` and `set_tunnels(&mut self,
  Vec<Tunnel>)`. The tick in `tui::run_app` calls
  `app.set_tunnels(tunnel::read_tunnels(&roots))` before `sync_from` on
  every tick (not on a search resync), with `roots = TunnelRoots::live()`
  owned by the loop. Tests set the list directly.
- `ControllerView` gains `tunnel: Option<TunnelView>` and `pcie:
  Vec<PcieRow>`. In `sync_from`, a bus whose `controller` address matches a
  function of some tunnel is grouped under that tunnel's root port; every
  tunnel gets a group even when no bus lands in it. `TunnelView { label:
  String }` is `Thunderbolt 0-3 Element Hub · 2×20 Gb/s` when the router
  is joined (name, then `device_name` when present, then
  `{rx_lanes}×{rx_gbps} Gb/s` when both are known, `{rx_gbps} Gb/s` when
  only the rate is), and `external PCIe port` otherwise.
- The heading prints `═ {id} · {label} ═` for a tunnel group and `═ {id} ═`
  for a controller group as today. Root ports sort among controller ids
  alphabetically, as ids do today.
- `PcieRow { address: String, text: String }`, one per function of the
  tunnel that no bus of the manager names as its controller, in address
  order, rendered after the connectors as `▶ {text}` in the connector
  heading style. `text` is:

  ```
  PCIe 0000:2d:00.1 · Ethernet controller · atlantic · 1d6a:14c0 · enp45s0 · 8 GT/s ×1
  PCIe 0000:2d:00.1 · Ethernet controller · atlantic · 1d6a:14c0 · asleep, ≤ 16 GT/s per lane
  PCIe 0000:2e:00.0 · USB controller · no driver · 8086:15ec · asleep
  ```

  The pieces in order: `PCIe {address}`, the class name, the driver or `no
  driver`, `{vendor:04x}:{device:04x}`, the interface when present, then
  the link: `{gts} GT/s ×{width}` when read; `asleep` when
  `runtime_status` is `suspended`; `link not read` otherwise (awake but
  unparsable, or any other status, `unsupported` included); the two
  unread forms append `, ≤ {max} GT/s per lane` when `max_gts` is known. A
  rate prints without decimals when it is integral and with one decimal
  otherwise (`8 GT/s`, `2.5 GT/s`, `20 Gb/s`).
- The rows are not selectable: `device_keys` and the selection logic are
  untouched. `hide_idle_devices` does not apply to them. A non-empty search
  query keeps a row only when the lower-cased text contains the lower-cased
  query. `prune_empty_groups` keeps a group that has a surviving PCIe row.
  `keep_chokepoints_on_screen` and `recompute_rates` are untouched.
- `help_lines` gains one line, under 76 columns: `PCIe rows are tunneled
  devices usbmon never sees: their link, not their traffic`.

## 3. The reports (`src/headless/mod.rs`, `src/fixture_replay.rs`, `src/diag/support.rs`)

- `build_report_at` gains `tunnels: &[Tunnel]` as its last parameter.
  `headless::run` reads `tunnel::read_tunnels(&TunnelRoots::live())` once
  per window and passes it; `fixture_replay::Replayed::report` and the
  support bundle's replay pass `&[]` (a bundle has no PCI tree); the
  in-crate `build_report` test helper passes `&[]`.
- `Report` gains `tunnels: Vec<TunnelReport>` after `buses`:

  ```rust
  pub struct TunnelReport {
      pub root_port: String,
      pub router: Option<RouterReport>,
      /// Addresses among `functions` that are USB controllers; join with
      /// `buses[].controller`.
      pub controllers: Vec<String>,
      pub functions: Vec<PciFunctionReport>,
  }
  pub struct RouterReport {
      pub name: String, pub vendor_name: Option<String>, pub device_name: Option<String>,
      pub generation: Option<u32>, pub rx_gbps: Option<f64>, pub tx_gbps: Option<f64>,
      pub rx_lanes: Option<u32>, pub tx_lanes: Option<u32>,
  }
  pub struct PciFunctionReport {
      pub address: String, pub class: String /* "0x020000" */, pub class_name: String,
      pub vendor_id: String /* "1d6a" */, pub device_id: String, pub driver: Option<String>,
      pub interface: Option<String>, pub runtime_status: Option<String>, pub awake: bool,
      pub link_gts: Option<f64>, pub link_width: Option<u32>, pub max_link_gts: Option<f64>,
  }
  ```

- `render_text` prints, after the last bus's devices and before
  `findings:`, `tunnels: none` or `tunnels: {n}` followed by one line per
  tunnel, `  {root_port}  {label}  controllers {a, b}` (`controllers none`
  when there are none; `label` as the TUI's), and one line per function,
  `    {address}  {class_name}  {driver|no driver}  {vv}:{dd}  {iface|-}  {link}`
  with `{link}` as the TUI's link text. The pinned order of `findings:`
  then `chokepoints:` at the end holds.
- The two literal `Report { .. }` constructions (`fixture_replay.rs`,
  `headless/export.rs`) gain `tunnels: Vec::new()`.
- The 18 goldens are re-blessed once; the review evidence is a jq diff
  showing the only change is the added empty `tunnels` key. A corpus test
  asserts every bundle replays to an empty list.
- `docs/SCRIPTING.md`: `tunnels` in the field list, the example document,
  and a "The tunnels list" section after the chokepoints one; the additive
  version sentence covers it.

## 4. Docs

README (the device table: the tunnel heading and the PCIe row; scriptable
output: `tunnels`), ARCHITECTURE (module map: `pci/`, `tunnel/`; the UI
paragraph; the diagnostics paragraph now points at the shared helpers),
CONTRIBUTING (nothing new to capture: fixtures carry no PCI tree, said in
TESTING), TESTING (no fleet host has a tunneled non-USB PCIe device; the
row is proven by synthetic trees; the dock shows the heading), CHANGELOG
(Added), ROADMAP (the row shipped; the two stages stay deferred with their
text).

## 5. Tests and the live check

- `pci`: the moved address test; `chain` root-first over a fake tree;
  `is_awake`.
- `tunnel`: a fake tree builder (root port, two bridges, an xHCI, a NIC
  with `net/enp45s0`, a router dir) and tests for grouping by root port,
  the bridge and USB-controller classification, the wake gate (a NIC with
  `power/runtime_status` `suspended` and a present `current_link_speed`
  yields `link: None`, `max_gts: Some` and `awake: false`; a status of
  `unsupported` yields the same with the row saying `link not read`), the
  interface rule (two
  entries under `net/` give `None`), the join for one router and one
  tunnel, no join for two routers, a depth-two router (`0-301`) ignored,
  `D-0` ignored, parse failures, an unreadable root.
- `ui`: a tunneled controller's buses land under the root-port heading
  with the router label; a bare tunnel (no bus) renders its PCIe rows and
  survives `prune_empty_groups`; the row text for awake, asleep and
  driverless functions; a search hides a non-matching row and keeps a
  matching one; `hide_idle_devices` leaves rows alone; the help line fits
  (the existing 76-column pin covers it).
- `headless`: JSON carries a tunnel with its router and functions; text
  prints the section in place; `tunnels: none` otherwise; the section
  order test still holds.
- `fixture_corpus`: every bundle's `tunnels` is empty.
- Live: on the dock laptop the TUI shows `═ 0000:00:07.1 · Thunderbolt 0-3
  Element Hub · 2×20 Gb/s ═` over usb5 and usb6 with no PCIe row, and
  `--once --force --json | jq .tunnels` shows the tunnel with
  `controllers: ["0000:2e:00.0"]` and functions holding only that xHCI;
  on devhost both are empty. Recorded in the ledger.

## Non-goals

- The Thunderbolt link and the PCIe uplink as choke stages (ROADMAP,
  unchanged).
- pci.ids names.
- Measured traffic for a tunneled device (netdev counters).
- Fixture capture of PCI or Thunderbolt trees.

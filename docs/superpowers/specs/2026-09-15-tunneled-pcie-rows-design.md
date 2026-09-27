# Tunneled PCIe devices in the device table — design

**Date:** 2026-09-15
**Status:** approved in the design session (sections 1-5); the 2026-09-16
two-axis review's five points folded in on 2026-09-23; the 2026-09-23
three-engine design challenge (Codex adversarial, Antigravity, Claude
reviewer) folded in the same day, rulings in the SDD ledger; adversarial
three-engine review of the build before merge per REVIEW.md

## Goal

The ROADMAP's deferred row: a tunneled PCIe device that is not a USB
controller, a 10G NIC behind a USB4 adapter say, shown as a sibling
consumer of the Thunderbolt link in the device table, and carried by the
`--once`/`--batch` reports and the support bundle. usbmon never sees such
a device, so today the TUI has no place for it; the kernel marks it
`removable` and puts it under the same root port as the tunneled xHCI, and
that root port is the join. The support bundle's
`inventory/pci-removable.toml` already collects the same devices; this
design gives the live views the same facts.

The Thunderbolt link as a choke stage, the controller's PCIe uplink as a
stage, and a pci.ids lookup for names stay out (see Non-goals).

## What the hardware shows

Verified on the Thunderbolt 4 laptop with the CalDigit dock attached
(2026-09-15 and 2026-09-23, live sysfs), and against the kernel source
where a rule depends on it:

- The dock's xHCI `0000:2e:00.0` hangs under root port `0000:00:07.1`
  through two bridges, `0000:2c:00.0` and `0000:2d:00.0`; all three read
  `removable`, the root port does not. Buses `usb5` and `usb6` name
  `0000:2e:00.0` as their controller.
- What `removable` means depends on the kernel. From v5.16 to v6.12,
  `pci_set_removable` (drivers/pci/probe.c) marks a device whose parent is
  external-facing or removable: everything below a root port the firmware
  marks `ExternalFacingPort`. On a laptop with a discrete Thunderbolt
  controller (Alpine, Titan or Maple Ridge between that root port and the
  connector) that includes the controller's own switch, its host interface
  (NHI) and its own xHCI, which serves the host's ports directly. From
  v6.13 (`arch_pci_dev_is_removable`, arch/x86/pci/acpi.c, x86 with ACPI
  only; every other architecture returns false and marks nothing) the rule
  is: below a removable parent, or directly behind a port that carries the
  `usb4-host-interface` firmware property or is one of the Ice Lake and
  Tiger Lake root ports listed by ID, or below the external-facing root
  port but not in the switch directly under it (`pcie_switch_directly_under`:
  the upstream port under the root port, that port's downstream ports, and
  the endpoints directly under those; "those are not behind a PCIe
  tunnel"). The reader applies that last exclusion itself, so it reads the
  same on both rule sets.
- Each Thunderbolt domain hangs off its host interface: the parent
  directory of `/sys/bus/thunderbolt/devices/domainN` (resolved) is the
  NHI's PCI device. The bundle already uses this. On the laptop the NHI is
  `0000:00:0d.2`, class `0x0c0340`, on bus 0; a discrete controller's NHI
  sits under the external-facing root port instead, which is how the reader
  tells the two apart without a kernel version.
- The router `0-3` reports `generation` 4, `rx_speed` and `tx_speed`
  `20.0 Gb/s`, `rx_lanes` and `tx_lanes` 2, `device_name` `Element Hub`,
  `vendor_name` the dock's maker, `authorized` 1; `domain0/security` reads
  the platform's level. The formats, from drivers/thunderbolt/switch.c
  v6.12: `speed_show` prints `%u.0 Gb/s` for both speeds, `rx_lanes_show`
  and `tx_lanes_show` print 1, 2 or 3 (3 is the wider side of an asymmetric
  link), `generation_show` prints `%u`, and `device_name_show` and
  `vendor_name_show` print the DROM string or an empty line when the DROM
  has none. `security` is one of `none`, `user`, `secure`, `dponly`,
  `usbonly`, `nopcie`; the last three create no PCIe tunnel, and an
  `authorized` of 0 means "no PCIe devices are available to the system".
  All of these are in Documentation/ABI/testing/sysfs-bus-thunderbolt.
  Routers are named `%u-%llx` (domain, route; `dev_set_name` in switch.c)
  and a route's depth is `tb_route_length` (tb.h v6.12: bits above each
  8-bit hop), so a depth-one router's route is 1..=255 and `D-0` is the
  host router.
- Host-to-host peers (XDomain, Thunderbolt networking) sit on the same bus
  with the same `%u-%llx` name and the same speed and lane attributes
  (drivers/thunderbolt/xdomain.c v6.12), and their `device_name` is the
  other computer's `utsname()->nodename`, sent as the `deviceid` property.
  The device type tells them apart: a router's `uevent` carries
  `DEVTYPE=thunderbolt_device`, a peer's `DEVTYPE=thunderbolt_xdomain`.
  Retimers (`0-0:1.1`), services (`0-1.1`), `domainN` and `usb4_portN` do
  not match the name shape.
- Nothing in sysfs maps a root port to a router on this laptop:
  `/sys/class/devlink` has no entry for the host interface or the root
  ports, and the host router has no `usb4_port*` objects. Tiger Lake root
  ports lack the `usb4-host-interface` property (which is why the kernel
  lists their IDs); on platforms whose firmware has it, `tb_acpi_add_link`
  (drivers/thunderbolt/acpi.c) links each tunneled root or downstream port
  to its NHI and the link shows as `/sys/class/devlink/pci:<nhi>--pci:<port>`.
  That maps a port to a domain, never to a router, so it is left for a later
  per-domain refinement (Non-goals).
- `current_link_speed` and `current_link_width` are read through
  `pci_config_pm_runtime_get` (drivers/pci/pci-sysfs.c v7.0), which takes a
  runtime-PM reference on the parent, waits for a suspend in progress and
  resumes a device in D3cold (drivers/pci/pci.c v7.0). The bundle already
  reads them (and `max_link_width`, which a row has no use for) only while
  `power/runtime_status` is `active`. `max_link_speed` takes no reference;
  from v6.13 it prints the `supported_speeds` cached at probe
  (`pcie_get_speed_cap`, pci.c v7.0), but up to v6.12 it reads Link
  Capabilities 2 from config space, which for a function in D3cold is a
  read of a powered-down device: no wake, but a meaningless value (all-ones
  decodes as `64.0 GT/s PCIe`). So the reader opens it under the same gate.
  Both speed attributes print `pci_speed_string` (drivers/pci/probe.c
  v7.0): `2.5 GT/s PCIe`, `5.0 GT/s PCIe`, `8.0 GT/s PCIe`,
  `16.0 GT/s PCIe`, `32.0 GT/s PCIe`, `64.0 GT/s PCIe`, or `Unknown` for a
  speed outside its table; `current_link_width_show` prints `%u`. The other
  attributes the reader opens are passive: `vendor`, `device` and `class`
  print cached fields, `removable`, `power/runtime_status`, `uevent` and
  the `driver` link come from the driver core, listing `net/` reads nothing
  from the device.
- No host on the fleet has a tunneled PCIe device that is not a USB
  controller. The dock's own bridges are the only other removable devices,
  and bridges are not rows.

## Decisions

- **The join is the root port.** Every removable, non-bridge PCI function
  that is not a discrete controller's own (see the reading rules) is grouped
  by the top of its parent chain, the root port, and a bus whose controller
  is such a function belongs to that group instead of to its controller's
  own group. A tunnel is a root port with at least one such function below
  it; a root port with only removable bridges below it is no tunnel and
  counts for nothing.
- **The router is named only when nothing else fits.** Nothing pairs a
  root port with a router except one case: exactly one depth-one router,
  and it can carry PCIe (`DEVTYPE=thunderbolt_device`, its domain's
  `security` one of `none`, `user`, `secure`, its `authorized` 1 or 2) and
  exactly one tunnel, the router list read before and after the PCI walk
  and identical both times, and the PCI walk complete (no function skipped
  for an unreadable identity). Otherwise every group shows its root port
  alone. The residual case the rule cannot exclude is a device in a
  non-USB4 external-facing slot beside a single authorized router whose
  tunnel carries only bridges (an empty enclosure); it is accepted and
  documented. The label names the first hop: a hub or a daisy chain puts
  the depth-one router's name and link over every device behind it, which
  is the link they all share.
- **Rows are topology, not traffic.** A PCIe row carries what sysfs says
  about the function and its link, never a bandwidth figure: usbmon does not
  see it, and nothing else measures it here. The row is one line of text,
  not a row of the traffic columns.
- **The link is read from an awake device, best-effort.** `max_link_speed`,
  `current_link_speed` and `current_link_width` are opened only when
  `power/runtime_status` reads `active`; a sleeping function says `asleep`.
  The check and the reads are not atomic: a function that suspends in the
  microseconds between them is resumed once by the kernel and suspends
  again on its own. The gate is stated as that, not as a guarantee; a
  reader without it would keep every tunneled device out of D3cold for as
  long as usbtop-ng ran. The reader runs at most once a second whatever
  `--refresh` says (its floor is 100 ms).
- **Names from sysfs alone.** Class name from a built-in table over the PCI
  class code, the bound driver, vendor and device IDs, the network interface
  name when the function has one. A hidden USB controller is a row too: a
  tunneled xHCI with no bus and no bound driver appears as
  `USB controller · no driver`; one whose driver is still bound after the
  controller died shows its driver and its link like any other row.
- **PCIe rows are selectable.** They join the selection cycle so every row
  can be reached at the 80x24 floor, where the list scrolls only to follow
  the selection. Selecting one highlights it and nothing more: no endpoint
  lines, no finding line. The idle filter hides a suspended function (the
  kernel's own notion of idle for a device usbmon cannot see); the search
  matches the row text; `--filter`, a USB vendor:product filter, hides
  every PCIe row in the TUI and leaves the JSON `tunnels` in place.
- **One reader, both bases, every surface.** `tunnel::read_tunnels` feeds
  the TUI each tick, `headless::run` each window, and the support bundle
  once per run; fixtures replay with an empty list. The bundle keeps its own
  walker and file format; the two share the address check, the parent-chain
  walk and the wake rule through a new `pci` module.
- **Additive JSON.** `tunnels` is a new top-level list, empty when there is
  none; `version` stays 1, as it did for `findings` and `chokepoints`
  (docs/SCRIPTING.md: additive fields do not bump it).
- **Privacy.** PCI addresses, class codes, IDs, driver names and DROM names
  are device topology and stay. The interface name is the kernel's device
  name; the bundle already carries it in the kernel log's driver lines. A
  name that embeds a MAC (`enx`, `wlx` or `wwx` + twelve hex digits,
  systemd's MAC naming policy) is masked by the redactor wherever the
  bundle prints it, the kernel log and the bundle's tunnels alike, and
  counted with the colon-form MACs it already masks. A peer computer's
  hostname never enters: XDomains are not routers.

## 1. The reader and the shared helpers

The reader is `src/tunnel/mod.rs` (new); the helpers are `src/pci/mod.rs`
(new), which takes over from `diag/inventory.rs`, unchanged in behaviour:
`is_address` (the former `is_pci_address`: a domain of four to eight hex
digits, a two-digit bus, a two-digit slot, a one-digit function), `chain`
(the former `pci_chain`: the PCI addresses above a resolved directory,
root-first), and `is_awake(real: &Path) -> bool` (`power/runtime_status`
reads `active`) with the citation that explains the wake gate. The
inventory's test `pci_addresses_allow_wide_domains_and_nothing_else` moves
with them. The inventory keeps `read_pci`, `read_pci_attrs`, its
`PCI_LINK_ATTRS` list (the row has no use for `max_link_width`) and its
notes; its doc comment on `removable` (inventory.rs, the "v5.16 and v7.0"
sentence) is corrected to the two rule sets above.

```rust
/// `/sys/bus/pci/devices`; `diag::support::Roots::live` uses the same.
pub const PCI_DEVICES: &str = "/sys/bus/pci/devices";
/// `/sys/bus/thunderbolt/devices`; likewise.
pub const THUNDERBOLT_DEVICES: &str = "/sys/bus/thunderbolt/devices";

pub struct Router {
    pub name: String,                 // "0-3"
    pub vendor_name: Option<String>,
    pub device_name: Option<String>,
    pub generation: Option<u32>,
    pub rx_gbps: Option<f64>,         // "20.0 Gb/s" -> 20.0
    pub tx_gbps: Option<f64>,
    pub rx_lanes: Option<u32>,
    pub tx_lanes: Option<u32>,
    pub authorized: Option<u32>,      // the router's `authorized`
    pub security: Option<String>,     // its domain's `security`
}

impl Router {
    /// `security` is none, user or secure and `authorized` is 1 or 2.
    pub fn can_tunnel_pcie(&self) -> bool
}

pub struct PciLink { pub gts: f64, pub width: u32 }

pub struct PciFunction {
    pub address: String,
    pub class: u32,                   // 0x020000
    pub class_name: String,           // table name, or "class 0x......"
    pub vendor_id: u16,
    pub device_id: u16,
    pub driver: Option<String>,       // basename of the `driver` link
    pub interface: Option<String>,    // the one entry of `net/`, when present
    pub runtime_status: Option<String>, // `power/runtime_status`, as read
    pub awake: bool,                  // runtime_status == "active"
    pub link: Option<PciLink>,        // only when awake and both attrs parse
    pub max_gts: Option<f64>,         // `max_link_speed`, only when awake
}

pub struct Tunnel {
    pub root_port: String,
    pub router: Option<Router>,
    /// Every removable non-bridge function under the root port that is not
    /// a discrete controller's own, USB controllers included, by address.
    pub functions: Vec<PciFunction>,
}

impl PciFunction {
    /// `class >> 8 == 0x0c03`, except the USB4 host interface `0x0c0340`
    /// and the USB device controller `0x0c03fe`, which host no bus.
    pub fn is_usb_controller(&self) -> bool
}

/// The class table below; `None` for a code it does not name.
fn class_name(class: u32) -> Option<&'static str>

/// The tunnels under `pci` with the routers under `thunderbolt`, joined
/// by the rule in Decisions.
pub fn read_tunnels(pci: &Path, thunderbolt: &Path) -> Vec<Tunnel>

/// The join, pure: `Some(index of the one router)` only when `before` and
/// `after` hold the same names, exactly one router, and it can carry PCIe,
/// exactly one tunnel, and `complete`.
fn join(before: &[Router], after: &[Router], tunnels: &[Tunnel], complete: bool)
    -> Option<usize>

```

The bundle's masking lives with the redactor, not here (`diag::redact`):
`pub fn embeds_mac(name: &str) -> bool` (`enx`, `wlx` or `wwx` + twelve
hex digits), `Redactor::mac_addresses` masking such tokens in text beside
the colon form, and `Redactor::mask_interface_names(&self, &mut [Tunnel])`
for the bundle's tunnels, all counted under `mac_address`.

Reading rules, in the order `read_tunnels` runs them:

- Routers, first pass: entries of `thunderbolt` of the form
  `<decimal>-<hex>` whose route parses and lies in 1..=255 and whose
  `uevent` contains the line `DEVTYPE=thunderbolt_device`. Attributes are
  read in the formats documented above; each is `None` when absent or
  unparsable, and an empty `device_name` or `vendor_name` (a DROM without
  one) is `None` too. `security` is read from `thunderbolt/domain<D>/
  security` for the router's domain `D`.
- Discrete controllers: for each `thunderbolt/domain<D>` that resolves, the
  parent directory of the resolved path is the NHI; when `pci::chain` of
  that directory is non-empty its top is a discrete controller's root port.
  The set of those is `discrete_ports`.
- Functions: entries of `pci` whose name passes `is_address` are resolved
  with `canonicalize`; an entry that fails to resolve or whose `removable`
  file is absent or does not read `removable` is skipped. A bridge (class
  byte `0x06`) is skipped. A function with an empty chain is skipped. A
  function whose root port is in `discrete_ports` and whose chain has
  exactly three entries (root port, upstream port, downstream port) is the
  controller's own NHI or xHCI and is skipped; deeper functions under that
  root port are tunneled. The root port is `chain[0]`, the top of the
  chain.
- Parsing: `vendor`, `device` and `class` are `0x`-prefixed hex; a value
  that does not parse skips the function and marks the walk incomplete
  (identity is the row; a half-read tree must not decide the join). The
  earlier skips, an entry that does not resolve or a `removable` that is
  absent or unreadable, are skips only: completeness is about identity,
  and a tunnel that vanished mid-walk, its router with it, is what the
  second router listing catches.
  `class_name` is the table's name for `class`, or `class 0x{class:06x}`
  when the table has none. `max_link_speed` and `current_link_speed` are
  `pci_speed_string` text (`"8.0 GT/s PCIe"`, see above): the leading
  float, so `Unknown` does not parse. `current_link_width` is a decimal. A
  value that does not parse leaves the field `None`.
- `driver` is the basename of the `driver` symlink when it resolves.
  `interface` is the single directory entry of `net/` when there is exactly
  one; two or more leave it `None`. It is read whatever the power state.
- `runtime_status` is `power/runtime_status` trimmed, `None` when absent;
  `awake` is true only for `active`. `max_gts` and `link` are read only
  when awake; when awake and an attribute is absent or unparsable the field
  is `None`. None of the three is opened otherwise.
- Routers, second pass: the same listing again, names only. `join` runs
  over the two lists, the tunnels and the completeness flag; on `Some(i)`
  the one tunnel takes router `i`.
- Tunnels sort by root port, functions by address, and the function list is
  never empty (a root port with only bridges, or only a discrete
  controller's own functions, below it is not a tunnel).
- An unreadable `pci` yields an empty list; an unreadable `thunderbolt`
  yields tunnels without routers. The reader has no notes channel; the
  bundle's walker keeps its own.

Class table (`class_name`). The lookup tries the whole 24-bit code, then
the top two bytes, then the top byte; anything else prints
`class 0x......`. The names are short forms of the class section of pci.ids
(its `C` records: base class, subclass, programming interface; the copy at
`/usr/share/misc/pci.ids`, read 2026-09-23), shortened so a row fits the
78 columns inside the list's border at the 80-column floor; the pci.ids
name is beside each. The codes match include/linux/pci_ids.h v7.0
(`PCI_CLASS_STORAGE_EXPRESS 0x010802`, `PCI_CLASS_STORAGE_SATA 0x0106`,
`PCI_CLASS_NETWORK_ETHERNET 0x0200`, `PCI_CLASS_MULTIMEDIA_HD_AUDIO 0x0403`,
`PCI_CLASS_SERIAL_FIREWIRE 0x0c00`, `PCI_CLASS_SERIAL_USB 0x0c03`,
`PCI_CLASS_SERIAL_USB_DEVICE 0x0c03fe`, the `PCI_BASE_CLASS_*` bytes). The
USB4 host interface is pci.ids' programming interface `40` under `0c03`,
read live as `0x0c0340` on the laptop's host interface; pci_ids.h v7.0 has
no name for it. `0x0280` is pci.ids' "Network controller", the same as its
base class, so it takes the `0x02` row.

| code | name | pci.ids |
|---|---|---|
| `0x010802` | NVMe | NVM Express |
| `0x0106` | SATA controller | SATA controller |
| `0x0108` | non-volatile memory | Non-Volatile memory controller |
| `0x01` | mass storage | Mass storage controller |
| `0x0200` | Ethernet | Ethernet controller |
| `0x02` | network | Network controller |
| `0x03` | display | Display controller |
| `0x0403` | audio | Audio device |
| `0x04` | multimedia | Multimedia controller |
| `0x08` | system peripheral | Generic system peripheral |
| `0x0c00` | FireWire | FireWire (IEEE 1394) |
| `0x0c0340` | USB4 host interface | USB4 Host Interface |
| `0x0c03` | USB controller | USB controller |
| `0x0c` | serial bus | Serial bus controller |
| `0x0d` | wireless | Wireless controller |
| `0x12` | accelerator | Processing accelerators |

## 2. The TUI (`src/ui/mod.rs`, `src/tui/mod.rs`)

- `UsbTopApp` gains `tunnels: Vec<Tunnel>` and `set_tunnels(&mut self,
  Vec<Tunnel>)`. The loop in `tui::run_app` keeps an `Instant` of the last
  read and calls `app.set_tunnels(tunnel::read_tunnels(Path::new(
  tunnel::PCI_DEVICES), Path::new(tunnel::THUNDERBOLT_DEVICES)))` before
  `sync_from` on the first tick and then on any tick at least one second
  after the last read (not on a search resync). Tests set the list
  directly.
- `ControllerView` gains `tunnel: Option<TunnelView>` and `pcie:
  Vec<PcieRow>`. In `sync_from`, a bus whose `controller` address matches a
  function of some tunnel is grouped under that tunnel's root port; every
  tunnel gets a group even when no bus lands in it. `TunnelView { label:
  String }` is `Thunderbolt 0-3 Element Hub · 2×20 Gb/s` when the router
  is joined (name, then `device_name` when present, then
  `{rx_lanes}×{rx_gbps} Gb/s` when both are known, `{rx_gbps} Gb/s` when
  only the rate is; the lane count is the kernel's, 3 included), and
  `external PCIe port` otherwise. "Thunderbolt" is the bus's name in the
  kernel and on every fleet device; the generation is in the JSON for
  anyone who wants USB4 spelled out.
- The heading prints `═ {id} · {label} ═` for a tunnel group and `═ {id} ═`
  for a controller group as today. Root ports sort among controller ids
  alphabetically, as ids do today.
- `PcieRow { address: String, text: String }`, one per function of the
  tunnel that no bus of the manager names as its controller, in address
  order, rendered after the connectors as `▶ {text}` in the list's text
  colour, the selected style when selected. `text` is:

  ```
  PCIe 0000:2d:00.1 · Ethernet · 8 GT/s ×1 · atlantic · enp45s0 · 1d6a:14c0
  PCIe 0000:2d:00.1 · Ethernet · asleep · atlantic · enp45s0 · 1d6a:14c0
  PCIe 0000:2e:00.0 · USB controller · asleep · no driver · 8086:15ec
  ```

  The pieces in order: `PCIe {address}`, the class name, the link, the
  driver or `no driver`, the interface when present (read whatever the
  power state, so an asleep NIC keeps its name), then
  `{vendor:04x}:{device:04x}`. The link is `{gts} GT/s ×{width}` when read;
  `asleep` when `runtime_status` is `suspended`; `link unread` otherwise
  (awake but unparsable, or any other status, `unsupported` included). A
  rate prints without decimals when it is integral and with one decimal
  otherwise (`8 GT/s`, `2.5 GT/s`, `20 Gb/s`). The list is a bordered
  `Paragraph` without wrap, 78 columns inside at `MIN_COLS`; the order
  puts the link before the identity so the floor clips the IDs, never the
  link, and the three examples above measure 75, 72 and 69 columns with
  the `▶ ` prefix (the widest realistic rows, `wireless · 2.5 GT/s ×4 ·
  iwlwifi · wlp45s0` and an Ethernet `link unread`, measure 76 and 77). A
  test renders the three at 80 columns.
- The rows are selectable: `device_keys` lists a PCIe row as
  `pcie:{address}` in render order after its group's device rows, so `j`/`k`
  reach it and `follow_selection_in_list` scrolls to it; a selected PCIe
  row is drawn in the selected style; `find_selected_device` finds no
  device for such a key, so `selected_row_trailing_lines` is 0 and no
  endpoint or finding line follows. The selection is cleared as today when
  its key leaves `device_keys`.
- `hide_idle_devices` drops a PCIe row whose `runtime_status` is
  `suspended`. A non-empty search query keeps a row only when the
  lower-cased text contains the lower-cased query. `retain_filtered_devices`
  (a non-empty `--filter`) drops every PCIe row. `prune_empty_groups` keeps
  a group that has a surviving PCIe row. `keep_chokepoints_on_screen` and
  `recompute_rates` are untouched.
- `help_lines` gains one line: `PCIe rows: tunneled devices usbmon never
  sees; their link, not traffic` (70 columns bare, 74 with the list's
  `  • ` prefix; the pin is 76).

## 3. The reports

The files: `src/headless/mod.rs`, `src/headless/export.rs`,
`src/fixture_replay.rs`, `src/diag/support.rs`.

- `build_report_at` gains `tunnels: &[Tunnel]` as its last parameter (six
  parameters become seven). `headless::run` reads `read_tunnels` once per
  window over the two live paths and passes it. `fixture_replay::Replayed`
  gains a `tunnels: Vec<Tunnel>` field that `Replayed::report` passes;
  fixture replay leaves it empty, and `run_support` fills it with
  `read_tunnels(&roots.pci, &roots.thunderbolt)` run through the
  redactor's `mask_interface_names`, so `report.json` and `report.capability.json`
  agree with `inventory/pci-removable.toml` from the same run. The test in
  support.rs that calls `build_report_at` directly and the in-crate
  `build_report` helper pass `&[]`.
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
      pub authorized: Option<u32>, pub security: Option<String>,
  }
  pub struct PciFunctionReport {
      pub address: String, pub class: String /* "020000", no prefix, as the IDs */,
      pub class_name: String, pub vendor_id: String /* "1d6a" */, pub device_id: String,
      pub driver: Option<String>, pub interface: Option<String>,
      pub runtime_status: Option<String>, pub awake: bool,
      pub link_gts: Option<f64>, pub link_width: Option<u32>, pub max_link_gts: Option<f64>,
  }
  ```

- `render_text` prints, after the last bus's devices and before
  `findings:`, `tunnels: none` or `tunnels: {n}` followed by one line per
  tunnel, `  {root_port}  {label}  controllers {a, b}` (`controllers none`
  when there are none; `label` as the TUI's), and one line per function,
  `    {address}  {class_name}  {link}  {driver|no driver}  {iface|-}  {vv}:{dd}`
  with `{link}` as the TUI's link text, in the TUI's order. The pinned
  order of `findings:` then `chokepoints:` at the end holds.
- The two literal `Report { .. }` constructions (`fixture_replay.rs`,
  `headless/export.rs`, both in tests) gain `tunnels: Vec::new()`.
- The 18 golden bundles (36 files: `golden.binary.json` and
  `golden.text.json` each) are re-blessed once, in one commit with the
  field: `cargo test bless_seed_goldens -- --ignored` for the seed bundles
  (it also rewrites their `trace.bin` from the text trace; `git status`
  must show no trace changed), then `USBTOP_NG_BLESS_BUNDLE=<host>/<stage>
  cargo test bless_named_bundle -- --ignored` for each real bundle. The
  review evidence is a jq diff over every golden showing the only change is
  the added empty `tunnels` key.
- `docs/SCRIPTING.md`: `tunnels` in the field list, the example document,
  and a "The tunnels list" section after the chokepoints one; the additive
  version sentence covers it.

## 4. Docs

README (the device table: the tunnel heading, the PCIe row and its
selection; scriptable output: `tunnels`), ARCHITECTURE (module map: `pci/`,
`tunnel/`; the UI paragraph; the diagnostics paragraph now points at the
shared helpers), CONTRIBUTING (nothing new to capture: fixtures carry no
PCI tree, said in TESTING), TESTING (no fleet host has a tunneled non-USB
PCIe device; the row is proven by synthetic trees; the dock shows the
heading; coverage: nothing is `removable` before v5.16, nor on any
architecture but x86 with ACPI from v6.13, so the row is silent there; a
USB4 dock's USB 3 goes to the host's own xHCI over a USB3 tunnel, so its
devices stay in the host controller's group and no heading appears for
that link), CHANGELOG (Added), ROADMAP (the row shipped; the two stages
stay deferred with their text; the per-domain refinement noted).

## 5. Tests and the live check

- `pci`: the moved address test; `chain` root-first over a fake tree;
  `is_awake`.
- `tunnel`: a fake tree builder (root port, two bridges, an xHCI, a NIC
  with `net/enp45s0`, a router dir with `uevent`, a `domain0` dir whose
  resolved parent is a fake NHI) and tests for grouping by root port; the
  bridge and USB-controller classification (`0x0c0330` is a controller,
  `0x0c0340` and `0x0c03fe` are not); the class name at each tier and the
  `class 0x......` fallback; the wake gate (a NIC with
  `power/runtime_status` `suspended` and a present, valid
  `current_link_speed` and `max_link_speed` yields `link: None`,
  `max_gts: None` and `awake: false`, which a reader that opened them
  could not produce; a status of `unsupported` yields the same with the row
  saying `link unread`); the interface rule (two entries under `net/` give
  `None`); the discrete controller (an NHI under `root · U · D` beside an
  xHCI under `root · U · D'`, and a dock's NIC under `root · U · D'' · U2
  · D2`: only the NIC is a function, the root port is one tunnel); the join
  for one router and one tunnel; no join for two routers, for one router
  and two tunnels, for a router whose `authorized` is 0 or whose domain's
  `security` is `nopcie`, `usbonly` or `dponly`, for an XDomain (`uevent`
  `DEVTYPE=thunderbolt_xdomain`) as the only entry, for an incomplete walk
  (a function with an unparsable `class`), and for router lists that differ
  between the two passes (`join` called directly); a depth-two router
  (`0-301`) ignored, `D-0` ignored, a retimer `0-0:1.1` and a service
  `0-1.1` ignored; a root port with only bridges below it counts for
  nothing; parse failures (`Unknown` speed, empty `device_name`); a
  `removable` that exists but cannot be read; an unreadable root.
- `diag::redact`: `embeds_mac` and `mask_interface_names` on `enx`, `wlx`
  and `wwx` names beside `enp45s0`, eleven- and thirteen-digit names and a
  non-hex character, with the count; the kernel-log line
  `atlantic 0000:2d:00.1 enx001122334455: renamed from eth0` reaching
  `dmesg-usb.txt` as `enx<redacted>: renamed from eth0`.
- `ui`: a tunneled controller's buses land under the root-port heading
  with the router label; a bare tunnel (no bus) renders its PCIe rows and
  survives `prune_empty_groups`; the row text for awake, asleep and
  driverless functions rendered at 80 columns with the link intact; `j`
  from the last device row selects the PCIe row and `follow_selection_in_list`
  brings it on screen, with no trailing lines; a search hides a
  non-matching row and keeps a matching one; `hide_idle_devices` hides a
  suspended row and keeps an awake one; `--filter` hides every row; the
  help line fits (the existing 76-column pin covers it).
- `headless`: JSON carries a tunnel with its router and functions, `class`
  without a prefix; text prints the section in place; `tunnels: none`
  otherwise; the section order test still holds; a `Replayed` with a
  non-empty `tunnels` reports it (the seam, not a constant).
- Live: on the dock laptop the TUI shows `═ 0000:00:07.1 · Thunderbolt 0-3
  Element Hub · 2×20 Gb/s ═` over usb5 and usb6 with no PCIe row, and
  `--once --force --json | jq .tunnels` shows the tunnel with
  `controllers: ["0000:2e:00.0"]`, functions holding only that xHCI, and
  the router's `authorized` and `security`; `power/runtime_status` of every
  removable function reads the same before and after sixty seconds of the
  TUI; on devhost both are empty. Recorded in the ledger.

## Non-goals

- The Thunderbolt link and the PCIe uplink as choke stages (ROADMAP,
  unchanged).
- pci.ids names.
- Measured traffic for a tunneled device (netdev counters).
- Fixture capture of PCI or Thunderbolt trees.
- A per-domain join through the `usb4-host-interface` device links, which
  would pair a dock and an eGPU on two USB4 domains (AMD) where the
  host-wide rule stays silent; noted on the ROADMAP.
- Caching a function's link between ticks.

# Choke Points Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Name the hubs whose links would choke if every device below them
pushed what it can, at the link or the capability basis, in the TUI header
and connector headings and in the JSON and text reports.

**Architecture:** A pure module `src/capacity/mod.rs` walks the device
tree the manager already holds (parents from sysfs names), sums each
hub's subtree demand in practical Mb/s, compares it with the hub's own
link, and returns the entries at or above the breathing room, worst first.
`ui::sync_from` and `headless::build_report_at` both call it; a `c` key
and a `--demand` flag pick the basis. The findings engine is untouched;
its test tree helper moves to a shared test module both engines use.

**Tech Stack:** Rust 1.88 (edition 2021), ratatui 0.30, clap 4 derive
(`ValueEnum`), serde, tempfile (dev). No new crates.

**Spec:** `docs/superpowers/specs/2026-09-13-chokepoints-design.md`

## Global Constraints

- MSRV 1.88; `cargo +1.88.0 check --all-targets` passes.
- Zero `#[allow(...)]` and zero `#[expect(...)]`. Binary crate: every item
  must be reachable from `main` or a test in every feature configuration
  (`ui`, `headless`, `capacity`, `findings`, `connector` are compiled in
  all four); gate test-only items with `#[cfg(test)]`, never suppress.
- `cargo fmt --all -- --check` clean; clippy `-D warnings` clean on the
  default, `capture-fixture`, `integration`, and `ebpf` configs.
- The breathing room is exactly `1.25`, named `CHOKE_FLOOR`, carried in
  every JSON report as `choke_floor`; ratios at or above it are listed.
- The JSON report `version` stays 1; the three new keys are additive; every
  committed golden is regenerated once (Task 3) and the diff verified to be
  only the added keys.
- Demand and capacity are practical Mb/s: rate times
  `UsbSpeed::class().efficiency()` of that rate.
- Capability basis: a leaf's rate is its capability (else its link) bounded
  by the capacity rate of every hub above it; a hub's capacity rate is its
  link when the link is at or below 480, else the larger of link and
  capability; a root hub is never a stage but does bound its subtree.
- Never write a hostname, account name, email, or IP address into any
  file; hosts appear only as public labels. Write the word NUL in words,
  never as an escape. The private reference project is never named.
- Commit trailers per `CLAUDE.md`: `Co-Authored-By: Claude Fable 5.1
  <noreply@anthropic.com>` and `Claude-Session:
  https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t`. Commits are
  signed by the repo's configured SSH signer; if `git commit` reports
  "Couldn't find key in agent", stop and tell the controller.
- Every cargo command is prefixed with `export PATH="$HOME/.cargo/bin:$PATH"`.
- Reviewers use read-only git only.

## File structure

- Create `src/test_tree.rs` (`#[cfg(test)]`): the fake-sysfs `Tree` helper
  moved out of `src/findings/mod.rs`, plus `manager()` and `port_index()`,
  and the `SS`/`SSP` BOS byte constants.
- Modify `src/usbmon/parser.rs`: `short_mbps` (moved from
  `findings::short_speed`).
- Modify `src/findings/mod.rs`: use `short_mbps` and the shared tree.
- Create `src/capacity/mod.rs`: `CHOKE_FLOOR`, `Basis`, `Contributor`,
  `Chokepoint`, `analyze`, tests. Modify `src/main.rs`: `mod capacity;`,
  `#[cfg(test)] mod test_tree;`, the `--demand` flag.
- Modify `src/headless/mod.rs`: `HeadlessOptions.demand`,
  `build_report_at`, `Report` fields, `ChokepointReport`, `render_text`.
  Modify `src/fixture_replay.rs` (basis through replay), `src/headless/export.rs`
  and `src/fixture_replay.rs` test literals, `src/fixture_corpus.rs`
  (the dock pins), every `tests/fixtures/hosts/*/stage*/golden.*.json`.
- Modify `src/ui/mod.rs`: `demand_basis`, `chokepoints`, `ConnectorView.choke`,
  header, heading, `c` key, controls and help text, tests.
- Modify docs: `README.md`, `docs/SCRIPTING.md`, `docs/ARCHITECTURE.md`,
  `docs/ROADMAP.md`, `CHANGELOG.md`.

---

### Task 1: The shared test tree and the compact rate formatter

**Files:**
- Create: `src/test_tree.rs`
- Modify: `src/main.rs` (module list, lines 20-35), `src/usbmon/parser.rs`
  (after `format_mbps`, ~line 105), `src/findings/mod.rs` (`short_speed`
  at ~129-139 and its five callers; the test module's `Tree`, `SS`, `SSP`
  and `t.analyze()` call sites)

**Interfaces:**
- Consumes: `connector::{parse_device_name, port_name}`,
  `device::manager::DeviceManager`, `connector::PortIndex`.
- Produces: `crate::usbmon::parser::short_mbps(mbps: f64) -> String`;
  `crate::test_tree::{Tree, SS, SSP}` with `Tree::new()`, `base()`,
  `root_hub(bus, speed)`, `device(name, speed, version, bos)`,
  `device_of_vendor(..., vendor)`, `port(hub_dir, hub, number)`,
  `pair(a, b)`, `locate(port_dir, location)`, `manager() -> DeviceManager`
  (enumerated, bus speeds resolved), `Tree::port_index(&DeviceManager) ->
  PortIndex`; in findings tests a free `analyze_tree(&Tree) -> Vec<Finding>`.

- [ ] **Step 1: Move `short_speed` to the parser as `short_mbps`, test first**

In `src/usbmon/parser.rs` tests add:

```rust
    #[test]
    fn short_mbps_is_compact() {
        assert_eq!(short_mbps(480.0), "480M");
        assert_eq!(short_mbps(12.0), "12M");
        assert_eq!(short_mbps(1.5), "1.5M");
        assert_eq!(short_mbps(5000.0), "5G");
        assert_eq!(short_mbps(8500.0), "8.5G");
        assert_eq!(short_mbps(10000.0), "10G");
        assert_eq!(short_mbps(0.0), "?");
    }
```

Run `cargo test -- parser::tests::short_mbps` : fails to compile. Add
after `format_mbps`:

```rust
/// `480M`, `5G`, `10G`; `?` for an unknown rate. The compact form the
/// findings and choke-point messages use.
pub fn short_mbps(mbps: f64) -> String {
    if mbps <= 0.0 {
        "?".to_string()
    } else if mbps >= 1000.0 {
        format!("{}G", mbps / 1000.0)
    } else {
        format!("{mbps}M")
    }
}
```

In `src/findings/mod.rs` delete `short_speed` and its test
`short_speed_is_compact`, import `short_mbps` from `crate::usbmon::parser`,
and replace each `short_speed(x)` with `short_mbps(x.to_mbps())` (five
sites: `Cause::reason` three times, `Finding::message` twice). Run
`cargo test -- findings parser` : green.

- [ ] **Step 2: Move the tree helper**

Create `src/test_tree.rs`:

```rust
//! A fake `/sys/bus/usb/devices` for the engine tests: root hubs as
//! symlinks into a controller directory, devices as directories with
//! `busnum`, `devnum`, `speed`, optional `version`, `bos_descriptors` and
//! `idVendor`, hub ports under `<hub>/<hub>:1.0/<hub>-port<N>/` with `peer`
//! links and an optional `location`. Shared by the findings and capacity
//! tests; read back through the real `DeviceManager` and `PortIndex`.

use std::path::{Path, PathBuf};

use crate::connector::{parse_device_name, port_name, PortIndex};
use crate::device::manager::DeviceManager;

/// SuperSpeed only: 5 Gb/s (the camera's shape).
pub(crate) const SS: &[u8] = &[
    0x05, 0x0f, 0x16, 0x00, 0x02, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10, 0x03,
    0x00, 0x0c, 0x00, 0x03, 0x0a, 0xff, 0x07,
];
/// SuperSpeed plus SuperSpeedPlus at 10 Gb/s (the adapter's shape).
pub(crate) const SSP: &[u8] = &[
    0x05, 0x0f, 0x2a, 0x00, 0x03, 0x07, 0x10, 0x02, 0x06, 0x00, 0x00, 0x00, 0x0a, 0x10, 0x03,
    0x00, 0x0e, 0x00, 0x03, 0x0a, 0xff, 0x07, 0x14, 0x10, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x11, 0x00, 0x00, 0x30, 0x40, 0x0a, 0x00, 0xb0, 0x40, 0x0a, 0x00,
];

pub(crate) struct Tree {
    root: tempfile::TempDir,
    next_devnum: std::cell::Cell<u8>,
}

impl Tree {
    pub(crate) fn new() -> Tree { /* the body from findings, unchanged */ }
    pub(crate) fn base(&self) -> PathBuf { /* unchanged */ }
    fn write(dir: &Path, bus: u8, devnum: u8, speed: &str, version: Option<&str>, bos: Option<&[u8]>) { /* unchanged */ }
    pub(crate) fn root_hub(&self, bus: u8, speed: &str) -> PathBuf { /* unchanged */ }
    pub(crate) fn device(&self, name: &str, speed: &str, version: Option<&str>, bos: Option<&[u8]>) -> PathBuf { /* unchanged */ }
    pub(crate) fn device_of_vendor(&self, name: &str, speed: &str, version: Option<&str>, bos: Option<&[u8]>, vendor: u16) -> PathBuf { /* unchanged */ }
    pub(crate) fn port(&self, hub_dir: &Path, hub: &str, number: u32) -> PathBuf { /* unchanged */ }
    pub(crate) fn pair(&self, a: &Path, b: &Path) { /* unchanged */ }
    pub(crate) fn locate(&self, port_dir: &Path, location: u32) { /* unchanged */ }

    /// The manager as the live tool builds it: every device enumerated,
    /// bus speeds resolved.
    pub(crate) fn manager(&self) -> DeviceManager {
        let mut manager = DeviceManager::with_sysfs_base(self.base());
        manager.enumerate_present_devices();
        manager.update_bus_speeds();
        manager
    }

    /// The connector index over the manager's devices, as `ui::sync_from`
    /// and `headless::build_report` build it.
    pub(crate) fn port_index(manager: &DeviceManager) -> PortIndex {
        PortIndex::scan_devices(
            manager
                .buses
                .values()
                .flat_map(|bus| bus.devices.values())
                .filter_map(|device| device.sysfs_path.as_deref()),
        )
    }
}
```

The bodies marked unchanged are the ones now at
`src/findings/mod.rs` lines 535-635 (`Tree`, `new`, `base`, `write`,
`root_hub`, `device`, `device_of_vendor`, `port`, `pair`, `locate`): copy
them verbatim, making the methods `pub(crate)`. Delete the `analyze`
method (it belongs to findings). In `src/main.rs` add, next to
`#[cfg(test)] mod fixture_corpus;`:

```rust
#[cfg(test)]
mod test_tree;
```

In the findings test module: delete the moved `Tree` impl and the `SS`/
`SSP` constants, add `use crate::test_tree::{Tree, SS, SSP};`, keep
`paired_roots` and `causes`, and add

```rust
    fn analyze_tree(t: &Tree) -> Vec<Finding> {
        let manager = t.manager();
        let index = Tree::port_index(&manager);
        analyze(&manager, &index)
    }
```

then replace every `t.analyze()` with `analyze_tree(&t)` (about twenty
sites; `grep -n 't.analyze()' src/findings/mod.rs` lists them). The two
tests that build a manager by hand to mark a device disconnected
(`a_device_that_went_disconnected_is_ignored`,
`a_child_of_a_disconnected_hub_gets_no_advice_it_cannot_earn`,
`a_device_under_a_disconnected_hub...`) switch to `t.manager()` and
`Tree::port_index(&manager)`.

- [ ] **Step 3: Gates and commit**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features capture-fixture -- -D warnings
cargo clippy --all-targets --features integration -- -D warnings
cargo clippy --all-targets --features ebpf -- -D warnings
cargo test --all-targets
```
The findings test count is unchanged. Commit:

```bash
git add src/test_tree.rs src/main.rs src/usbmon/parser.rs src/findings/mod.rs
git commit -m "refactor(test): share the fake-sysfs tree between engines; the compact rate formatter moves to the parser"
```

---

### Task 2: The capacity engine

**Files:**
- Create: `src/capacity/mod.rs`
- Modify: `src/main.rs` (`mod capacity;` in the alphabetical list, before
  `mod capture;`)

**Interfaces:**
- Consumes: `connector::{port_name, port_of_device}`,
  `device::manager::DeviceManager` (`buses`, `UsbDevice::{sysfs_path,
  bus_id, device_id, speed, capability, is_disconnected}`),
  `usbmon::parser::{short_mbps, UsbSpeed}`, `test_tree::{Tree, SS, SSP}`.
- Produces: `capacity::CHOKE_FLOOR: f64`, `capacity::Basis {Link,
  Capability}` with `as_str(self) -> &'static str` and `toggled(self) ->
  Basis`, `capacity::Contributor { path: String, demand_mbps: f64 }`,
  `capacity::Chokepoint { bus: u8, address: u8, path: String, port:
  Option<String>, capacity_mbps: f64, demand_mbps: f64, ratio: f64,
  devices: usize, top: Vec<Contributor> }` with `message(&self) -> String`,
  `capacity::analyze(manager: &DeviceManager, basis: Basis) ->
  Vec<Chokepoint>`.

- [ ] **Step 1: Write the failing tests**

Create `src/capacity/mod.rs` with the types below and an `analyze` stub
returning `Vec::new()` (so the tests compile and fail), then the tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_tree::{Tree, SS, SSP};
    use std::path::PathBuf;

    /// One root hub `usbN` at `speed` with `ports` ports; returns its dir.
    fn root(t: &Tree, bus: u8, speed: &str, ports: u32) -> PathBuf {
        let dir = t.root_hub(bus, speed);
        for n in 1..=ports {
            t.port(&dir, &format!("usb{bus}"), n);
        }
        dir
    }

    /// A hub `name` at `speed` with `ports` ports; returns its dir.
    fn hub(t: &Tree, name: &str, speed: &str, bos: Option<&[u8]>, ports: u32) -> PathBuf {
        let dir = t.device(name, speed, if bos.is_some() { Some("2.10") } else { Some("2.00") }, bos);
        for n in 1..=ports {
            t.port(&dir, name, n);
        }
        dir
    }

    fn summary(points: &[Chokepoint]) -> Vec<(&str, f64, f64, f64, usize)> {
        points
            .iter()
            .map(|c| (c.path.as_str(), c.capacity_mbps, c.demand_mbps, (c.ratio * 100.0).round() / 100.0, c.devices))
            .collect()
    }

    #[test]
    fn two_high_speed_devices_under_a_high_speed_hub_are_listed_at_two_x() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 2);
        t.device("1-1.1", "480", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        let points = analyze(&t.manager(), Basis::Link);
        assert_eq!(summary(&points), vec![("1-1", 384.0, 768.0, 2.0, 2)]);
        let c = &points[0];
        assert_eq!((c.bus, c.address, c.port.as_deref()), (1, 2, Some("usb1-port1")));
        assert_eq!(
            c.top,
            vec![
                Contributor { path: "1-1.1".into(), demand_mbps: 384.0 },
                Contributor { path: "1-1.2".into(), demand_mbps: 384.0 },
            ]
        );
        assert_eq!(c.message(), "384M carries 2 devices asking 768M: 2.00x");
    }

    #[test]
    fn a_flash_drive_and_a_mouse_stay_inside_the_breathing_room() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 2);
        t.device("1-1.1", "480", Some("2.00"), None);
        t.device("1-1.2", "1.5", Some("2.00"), None);
        assert!(analyze(&t.manager(), Basis::Link).is_empty(), "1.003x is not a choke point");
    }

    #[test]
    fn the_floor_is_inclusive_at_one_and_a_quarter() {
        // 384 + ten full-speed devices at 9.6 = 480 practical: exactly 1.25x.
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 12);
        t.device("1-1.1", "480", Some("2.00"), None);
        for n in 2..=11 {
            t.device(&format!("1-1.{n}"), "12", Some("2.00"), None);
        }
        let points = analyze(&t.manager(), Basis::Link);
        assert_eq!(points.len(), 1, "exactly 1.25x is listed");
        assert!((points[0].ratio - 1.25).abs() < 1e-9);
        // Nine of them: 470.4, below the floor.
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 12);
        t.device("1-1.1", "480", Some("2.00"), None);
        for n in 2..=10 {
            t.device(&format!("1-1.{n}"), "12", Some("2.00"), None);
        }
        assert!(analyze(&t.manager(), Basis::Link).is_empty());
    }

    #[test]
    fn nested_hubs_sum_the_whole_subtree() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 2);
        hub(&t, "1-1.1", "480", None, 2);
        t.device("1-1.1.1", "480", Some("2.00"), None);
        t.device("1-1.1.2", "480", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        let points = analyze(&t.manager(), Basis::Link);
        assert_eq!(
            summary(&points),
            vec![("1-1", 384.0, 1152.0, 3.0, 4), ("1-1.1", 384.0, 768.0, 2.0, 2)],
            "the parent counts the child hub and every device below it"
        );
    }

    #[test]
    fn the_two_halves_of_a_hub_are_summed_separately() {
        let t = Tree::new();
        let usb3 = root(&t, 3, "480", 1);
        let usb4 = root(&t, 4, "5000", 1);
        t.pair(&usb3.join("usb3:1.0/usb3-port1"), &usb4.join("usb4:1.0/usb4-port1"));
        hub(&t, "3-1", "480", Some(SS), 2);
        hub(&t, "4-1", "5000", Some(SS), 2);
        t.device("3-1.1", "480", Some("2.00"), None);
        t.device("3-1.2", "480", Some("2.00"), None);
        t.device("4-1.1", "5000", Some("3.00"), Some(SS));
        assert_eq!(
            summary(&analyze(&t.manager(), Basis::Link)),
            vec![("3-1", 384.0, 768.0, 2.0, 2)],
            "the USB 3 half carries one 5G device, 1.0x, and is not listed"
        );
    }

    #[test]
    fn unknown_rates_disconnected_devices_and_internal_devices() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 3);
        t.device("1-1.1", "480", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        t.device("1-1.3", "0", Some("2.00"), None); // rate unknown
        let mut manager = t.manager();
        let points = analyze(&manager, Basis::Link);
        assert_eq!(summary(&points), vec![("1-1", 384.0, 768.0, 2.0, 3)], "the unknown one counts as a device but asks nothing");
        assert_eq!(points[0].top.len(), 2, "a zero contributor is not in the top list");
        // Internal devices are traffic sources too.
        for device in manager.buses.get_mut(&1).unwrap().devices.values_mut() {
            device.is_internal = true;
        }
        assert_eq!(analyze(&manager, Basis::Link).len(), 1);
        // A device the manager lists as disconnected is out of the sum.
        manager.buses.get_mut(&1).unwrap().devices.get_mut(&3).unwrap().is_disconnected = true;
        assert!(analyze(&manager, Basis::Link).is_empty(), "one 480 device left: 1.0x");
    }

    #[test]
    fn the_capability_basis_moves_both_sides() {
        // A 10G hub linked at 10G with one 10G device linked at 5G: link
        // 0.5x, capability 1.0x, neither listed.
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "10000", Some(SSP), 4);
        t.device("2-1.1", "5000", Some("3.20"), Some(SSP));
        assert!(analyze(&t.manager(), Basis::Link).is_empty());
        assert!(analyze(&t.manager(), Basis::Capability).is_empty());
        // Two of them: link 1.0x (not listed), capability 2.0x (listed).
        t.device("2-1.2", "5000", Some("3.20"), Some(SSP));
        assert!(analyze(&t.manager(), Basis::Link).is_empty());
        assert_eq!(summary(&analyze(&t.manager(), Basis::Capability)), vec![("2-1", 8500.0, 17000.0, 2.0, 2)]);
    }

    #[test]
    fn a_leaf_is_bounded_by_every_hub_above_it_at_the_capability_basis() {
        // The adapter shape: a 10G device on a USB 2 half stays bounded to
        // 480 by that half, and the half never gains SuperSpeed capacity.
        let t = Tree::new();
        root(&t, 5, "480", 1);
        hub(&t, "5-1", "480", None, 2);
        t.device("5-1.1", "480", Some("2.10"), Some(SSP));
        t.device("5-1.2", "12", Some("2.01"), None);
        assert!(analyze(&t.manager(), Basis::Link).is_empty(), "1.03x at link");
        assert!(analyze(&t.manager(), Basis::Capability).is_empty(), "still 1.03x at capability: bounded");
        // A USB 2 half that advertises SuperSpeed (its other half's) keeps 480.
        let t = Tree::new();
        root(&t, 3, "480", 1);
        hub(&t, "3-1", "480", Some(SS), 2);
        t.device("3-1.1", "480", Some("2.10"), Some(SS));
        t.device("3-1.2", "480", Some("2.10"), Some(SS));
        assert_eq!(summary(&analyze(&t.manager(), Basis::Capability)), vec![("3-1", 384.0, 768.0, 2.0, 2)]);
    }

    #[test]
    fn a_ten_gig_hub_linked_at_five_counts_as_ten_at_the_capability_basis() {
        let t = Tree::new();
        root(&t, 2, "10000", 1);
        hub(&t, "2-1", "5000", Some(SSP), 4);
        t.device("2-1.1", "5000", Some("3.20"), Some(SSP));
        t.device("2-1.2", "5000", Some("3.20"), Some(SSP));
        assert_eq!(summary(&analyze(&t.manager(), Basis::Link)), vec![("2-1", 4250.0, 8500.0, 2.0, 2)]);
        assert_eq!(summary(&analyze(&t.manager(), Basis::Capability)), vec![("2-1", 8500.0, 17000.0, 2.0, 2)]);
    }

    #[test]
    fn top_three_of_five_contributors_by_demand_then_path() {
        let t = Tree::new();
        root(&t, 1, "480", 1);
        hub(&t, "1-1", "480", None, 5);
        t.device("1-1.1", "12", Some("2.00"), None);
        t.device("1-1.2", "480", Some("2.00"), None);
        t.device("1-1.3", "1.5", Some("2.00"), None);
        t.device("1-1.4", "12", Some("2.00"), None);
        t.device("1-1.5", "480", Some("2.00"), None);
        let points = analyze(&t.manager(), Basis::Link);
        let top: Vec<&str> = points[0].top.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(top, ["1-1.2", "1-1.5", "1-1.1"]);
    }

    #[test]
    fn worst_first_with_the_path_as_the_tie_break() {
        let t = Tree::new();
        root(&t, 1, "480", 3);
        for (name, kids) in [("1-2", 2), ("1-1", 2), ("1-3", 3)] {
            hub(&t, name, "480", None, kids);
            for n in 1..=kids {
                t.device(&format!("{name}.{n}"), "480", Some("2.00"), None);
            }
        }
        let order: Vec<&str> = analyze(&t.manager(), Basis::Link).iter().map(|c| c.path.as_str()).collect();
        assert_eq!(order, ["1-3", "1-1", "1-2"]);
    }

    #[test]
    fn basis_names_and_toggle() {
        assert_eq!(Basis::Link.as_str(), "link");
        assert_eq!(Basis::Capability.as_str(), "capability");
        assert_eq!(Basis::Link.toggled(), Basis::Capability);
        assert_eq!(Basis::Capability.toggled(), Basis::Link);
        assert_eq!(Basis::default(), Basis::Link);
    }
}
```

Run `cargo test -- capacity` : the tests fail on the empty `Vec` (or on
compile until the stub exists).

- [ ] **Step 2: The module**

```rust
//! Choke points: the theoretical load every hub's link would carry if
//! every device below it pushed what it can, against that link's own
//! capacity. No measured traffic, no I/O: the tree comes from the
//! manager's rows (parents from sysfs names), rates from the rows. See
//! `docs/superpowers/specs/2026-09-13-chokepoints-design.md`.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::connector::{port_name, port_of_device};
use crate::device::manager::DeviceManager;
use crate::usbmon::parser::{short_mbps, UsbSpeed};

/// The breathing room: a hub is listed only when its subtree asks at least
/// this many times its link's practical capacity. Below it, a 480M hub
/// carrying a flash drive and a mouse (1.03x) stays quiet.
pub const CHOKE_FLOOR: f64 = 1.25;

const HIGH_SPEED_MBPS: f64 = 480.0;

/// Which rate every device is assumed to push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Basis {
    /// The rate each device has now, on both sides.
    #[default]
    Link,
    /// The topology as it could link: a leaf's capability (bounded by the
    /// hubs above it), a SuperSpeed hub's capability as its capacity.
    Capability,
}

impl Basis {
    /// The JSON value and the `--demand` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::Link => "link",
            Basis::Capability => "capability",
        }
    }

    pub fn toggled(self) -> Basis {
        match self {
            Basis::Link => Basis::Capability,
            Basis::Capability => Basis::Link,
        }
    }
}

/// One device below a choked hub and what it asks, practical Mb/s.
#[derive(Debug, Clone, PartialEq)]
pub struct Contributor {
    pub path: String,
    pub demand_mbps: f64,
}

/// A hub whose subtree asks more of its link than the link can carry.
#[derive(Debug, Clone, PartialEq)]
pub struct Chokepoint {
    pub bus: u8,
    pub address: u8,
    /// The hub's sysfs name, `3-1`.
    pub path: String,
    /// The kernel port name of the hub's link, `usb3-port1`: the same key
    /// the findings carry, so a script can join the two lists.
    pub port: Option<String>,
    pub capacity_mbps: f64,
    pub demand_mbps: f64,
    pub ratio: f64,
    /// Every device below the hub, hubs included.
    pub devices: usize,
    /// The largest contributors, at most three, demand descending then path.
    pub top: Vec<Contributor>,
}

impl Chokepoint {
    /// `384M carries 9 devices asking 1172M: 3.05x`.
    pub fn message(&self) -> String {
        format!(
            "{} carries {} devices asking {}: {:.2}x",
            short_mbps(self.capacity_mbps),
            self.devices,
            short_mbps(self.demand_mbps),
            self.ratio
        )
    }
}

struct Node<'a> {
    name: &'a str,
    bus: u8,
    address: u8,
    link: f64,
    capability: Option<f64>,
    children: Vec<usize>,
}

/// A rate in Mb/s to what the class's efficiency leaves of it; zero and
/// unknown rates give zero.
fn practical(rate_mbps: f64) -> f64 {
    let speed = UsbSpeed::from_mbps(rate_mbps);
    rate_mbps * speed.class().efficiency()
}

/// The rate a hub's link is taken to carry, in Mb/s, before efficiency.
fn capacity_rate(node: &Node, basis: Basis) -> f64 {
    match basis {
        Basis::Link => node.link,
        // A USB 2 half is 480 whatever its BOS says (the SuperSpeed
        // capability it advertises belongs to its other half, a different
        // sysfs hub); a SuperSpeed half linked below its capability counts
        // at that capability.
        Basis::Capability if node.link <= HIGH_SPEED_MBPS => node.link,
        Basis::Capability => node.capability.map_or(node.link, |c| c.max(node.link)),
    }
}

/// The rate a leaf is taken to push, before efficiency: at the capability
/// basis its capability bounded by `bound`, the tightest capacity rate of
/// the hubs above it.
fn leaf_rate(node: &Node, basis: Basis, bound: f64) -> f64 {
    match basis {
        Basis::Link => node.link,
        Basis::Capability => node.capability.unwrap_or(node.link).min(bound),
    }
}

/// Walk `at`'s subtree: returns (demand in practical Mb/s, devices below,
/// every leaf's contribution), and pushes a `Chokepoint` for `at` when it
/// is a hub at or above the floor.
fn walk(nodes: &[Node], at: usize, basis: Basis, bound: f64, out: &mut Vec<Chokepoint>) -> (f64, usize, Vec<Contributor>) {
    let node = &nodes[at];
    if node.children.is_empty() {
        let demand = practical(leaf_rate(node, basis, bound));
        let contributors = if demand > 0.0 {
            vec![Contributor { path: node.name.to_string(), demand_mbps: demand }]
        } else {
            Vec::new()
        };
        return (demand, 0, contributors);
    }
    let rate = capacity_rate(node, basis);
    let inner_bound = match basis {
        Basis::Link => bound,
        Basis::Capability => bound.min(rate),
    };
    let mut demand = 0.0;
    let mut devices = 0;
    let mut contributors = Vec::new();
    for &child in &node.children {
        let (d, n, c) = walk(nodes, child, basis, inner_bound, out);
        demand += d;
        devices += n + 1;
        contributors.extend(c);
    }
    // A root hub is a bound for its subtree but never a stage of its own.
    if let Some((parent, number)) = port_of_device(node.name) {
        let capacity = practical(rate);
        if capacity > 0.0 {
            let ratio = demand / capacity;
            if ratio >= CHOKE_FLOOR {
                let mut top = contributors.clone();
                top.sort_by(|a, b| {
                    b.demand_mbps
                        .partial_cmp(&a.demand_mbps)
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| a.path.cmp(&b.path))
                });
                top.truncate(3);
                out.push(Chokepoint {
                    bus: node.bus,
                    address: node.address,
                    path: node.name.to_string(),
                    port: Some(port_name(&parent, number)),
                    capacity_mbps: capacity,
                    demand_mbps: demand,
                    ratio,
                    devices,
                    top,
                });
            }
        }
    }
    (demand, devices, contributors)
}

/// Every hub whose subtree asks at least `CHOKE_FLOOR` times its link's
/// practical capacity, worst first, ties by path.
pub fn analyze(manager: &DeviceManager, basis: Basis) -> Vec<Chokepoint> {
    let mut nodes: Vec<Node> = manager
        .buses
        .values()
        .flat_map(|bus| bus.devices.values())
        .filter(|device| !device.is_disconnected)
        .filter_map(|device| {
            let name = device.sysfs_path.as_deref()?.file_name()?.to_str()?;
            Some(Node {
                name,
                bus: device.bus_id,
                address: device.device_id,
                link: device.speed.to_mbps(),
                capability: device.capability.as_ref().map(|c| c.speed.to_mbps()),
                children: Vec::new(),
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(b.name));
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.name, i)).collect();
    let mut parents = vec![None; nodes.len()];
    for i in 0..nodes.len() {
        if let Some(parent) = port_of_device(nodes[i].name).and_then(|(hub, _)| index.get(hub.as_str()).copied()) {
            nodes[parent].children.push(i);
            parents[i] = Some(parent);
        }
    }
    let mut out = Vec::new();
    for i in 0..nodes.len() {
        if parents[i].is_none() {
            walk(&nodes, i, basis, f64::INFINITY, &mut out);
        }
    }
    out.sort_by(|a, b| {
        b.ratio
            .partial_cmp(&a.ratio)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}
```

Add `mod capacity;` to `src/main.rs`. The module is dead until Task 3
wires it, so this task's commit is folded into Task 3's (see below); run
`cargo test -- capacity` now and keep the tree uncommitted, or commit
with `#[cfg(test)]` only on nothing: the `-D warnings` gate refuses an
unused module. **Rule: Task 2 and Task 3 land as one commit.**

---

### Task 3: The headless surface, the flag, and the goldens

**Files:**
- Modify: `src/headless/mod.rs` (`HeadlessOptions` 22-35, `Report` 37-60,
  `FindingReport` ~102, `build_report` 220-380, `render_text` ~400-473,
  `run` ~548-600, tests), `src/main.rs` (flag near the `--json` flag at
  ~94; `HeadlessOptions` literal at ~730; the `UpdateUsbidsMode` enum at
  ~183 as the pattern), `src/fixture_replay.rs` (`replay_fixture_with_elapsed`
  ~195-245, the literal `Report` in its tests ~304), `src/headless/export.rs`
  (literal `Report` in tests ~244), `src/fixture_corpus.rs` (the dock test
  at ~337), every `tests/fixtures/hosts/*/stage*/golden.*.json`.

**Interfaces:**
- Consumes: `capacity::{analyze, Basis, Chokepoint, Contributor, CHOKE_FLOOR}`.
- Produces: `headless::build_report_at(basis: Basis, manager, baseline,
  elapsed, source, dropped, text_active, filter) -> Report` (`build_report`
  keeps its signature and passes `Basis::Link`); `HeadlessOptions.demand:
  Basis`; `Report.{demand_basis: &'static str, choke_floor: f64,
  chokepoints: Vec<ChokepointReport>}`; `ChokepointReport { bus, address,
  path, port, capacity_mbps, demand_mbps, ratio, devices, top:
  Vec<ContributorReport>, message }`, `ContributorReport { path,
  demand_mbps }`; `fixture_replay::replay_fixture_at(dir, source, basis)`;
  the `--demand link|capability` flag.

- [ ] **Step 1: Failing headless tests**

Next to `json_report_carries_findings_and_per_device_capability` add:

```rust
    /// A 480 hub on a root port with two 480 devices: one choke point.
    fn tree_with_a_choke() -> (tempfile::TempDir, DeviceManager) {
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
        let usb1 = ctrl.join("usb1");
        write(&usb1, &[("busnum", "1"), ("devnum", "1"), ("speed", "480")]);
        std::os::unix::fs::symlink(&usb1, base.join("usb1")).unwrap();
        std::fs::create_dir_all(usb1.join("usb1:1.0").join("usb1-port1")).unwrap();
        let hub = base.join("1-1");
        write(&hub, &[("busnum", "1"), ("devnum", "2"), ("speed", "480"), ("idVendor", "1a40"), ("idProduct", "0201")]);
        for n in 1..=2 {
            std::fs::create_dir_all(hub.join("1-1:1.0").join(format!("1-1-port{n}"))).unwrap();
            write(&base.join(format!("1-1.{n}")), &[("busnum", "1"), ("devnum", &(n + 2).to_string()), ("speed", "480")]);
        }
        let mut mgr = DeviceManager::with_sysfs_base(base);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        (temp, mgr)
    }

    #[test]
    fn json_report_carries_the_choke_points_and_the_basis() {
        let (_temp, mgr) = tree_with_a_choke();
        let baseline = Baseline::capture(&mgr);
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &FilterSet::default());
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["demand_basis"], "link");
        assert_eq!(v["choke_floor"], 1.25);
        let points = v["chokepoints"].as_array().unwrap();
        assert_eq!(points.len(), 1);
        let c = &points[0];
        assert_eq!((c["bus"].as_u64(), c["address"].as_u64()), (Some(1), Some(2)));
        assert_eq!(c["path"], "1-1");
        assert_eq!(c["port"], "usb1-port1");
        assert_eq!(c["capacity_mbps"], 384.0);
        assert_eq!(c["demand_mbps"], 768.0);
        assert_eq!(c["ratio"], 2.0);
        assert_eq!(c["devices"], 2);
        assert_eq!(c["top"].as_array().unwrap().len(), 2);
        assert_eq!(c["top"][0]["path"], "1-1.1");
        assert_eq!(c["top"][0]["demand_mbps"], 384.0);
        assert_eq!(c["message"], "384M carries 2 devices asking 768M: 2.00x");

        let report = build_report_at(crate::capacity::Basis::Capability, &mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &FilterSet::default());
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["demand_basis"], "capability");
    }

    #[test]
    fn choke_points_follow_the_filter() {
        let (_temp, mgr) = tree_with_a_choke();
        let baseline = Baseline::capture(&mgr);
        let filter = FilterSet::parse(&["bus=2".to_string()]).unwrap();
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &filter);
        assert!(report.chokepoints.is_empty(), "the hub is on bus 1, which the filter excludes");
    }

    #[test]
    fn render_text_lists_the_choke_points_after_the_findings() {
        let (_temp, mgr) = tree_with_a_choke();
        let baseline = Baseline::capture(&mgr);
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &FilterSet::default());
        let text = render_text(&report);
        assert!(text.contains("findings: none\nchokepoints: 1\n  hub 1-1 (1:2, 1a40:0201) 384M carries 2 devices asking 768M: 2.00x\n\n"), "{text}");
    }

    #[test]
    fn render_text_says_chokepoints_none_when_there_are_none() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let report = build_report(&mgr, &baseline, Duration::from_secs(1), "binary", 0, false, &FilterSet::default());
        assert!(render_text(&report).ends_with("findings: none\nchokepoints: none\n\n"));
    }
```

Copy the `FilterSet` constructor from `findings_follow_the_filter` in the
same file. Run `cargo test -- headless` : compile errors.

- [ ] **Step 2: Implement**

`HeadlessOptions` gains

```rust
    /// `--demand`: which rate the choke-point model assumes every device
    /// pushes (see `capacity::Basis`).
    pub demand: Basis,
```

`Report` gains, after `findings`:

```rust
    /// The basis the choke points were computed at, `"link"` or
    /// `"capability"` (see `capacity::Basis`).
    pub demand_basis: &'static str,
    /// The breathing room applied: a hub is listed only when its subtree
    /// asks at least this many times its link's capacity.
    pub choke_floor: f64,
    /// Hubs whose links are asked more than they can carry, worst first
    /// (see `capacity::analyze`); only hubs the report lists.
    pub chokepoints: Vec<ChokepointReport>,
```

New structs after `FindingReport`:

```rust
#[derive(Serialize)]
pub struct ContributorReport {
    pub path: String,
    pub demand_mbps: f64,
}

/// One choke point as the JSON report carries it (see `capacity::Chokepoint`).
#[derive(Serialize)]
pub struct ChokepointReport {
    pub bus: u8,
    pub address: u8,
    pub path: String,
    pub port: Option<String>,
    pub capacity_mbps: f64,
    pub demand_mbps: f64,
    pub ratio: f64,
    pub devices: usize,
    pub top: Vec<ContributorReport>,
    pub message: String,
}

impl From<&Chokepoint> for ChokepointReport {
    fn from(c: &Chokepoint) -> Self {
        ChokepointReport {
            bus: c.bus,
            address: c.address,
            path: c.path.clone(),
            port: c.port.clone(),
            capacity_mbps: c.capacity_mbps,
            demand_mbps: c.demand_mbps,
            ratio: c.ratio,
            devices: c.devices,
            top: c.top.iter().map(|t| ContributorReport { path: t.path.clone(), demand_mbps: t.demand_mbps }).collect(),
            message: c.message(),
        }
    }
}
```

Rename the existing `build_report` to `build_report_at` with a leading
`basis: Basis` parameter, and add

```rust
/// [`build_report_at`] at the link basis: what every replay and every
/// test uses, and therefore what the committed goldens hold.
pub fn build_report(
    manager: &DeviceManager,
    baseline: &Baseline,
    elapsed: Duration,
    source: &'static str,
    dropped: u64,
    text_active: bool,
    filter: &FilterSet,
) -> Report {
    build_report_at(Basis::Link, manager, baseline, elapsed, source, dropped, text_active, filter)
}
```

In `build_report_at`, after the findings are collected:

```rust
    let chokepoints = crate::capacity::analyze(manager, basis)
        .iter()
        .filter(|c| listed.contains(&(c.bus, c.address)))
        .map(ChokepointReport::from)
        .collect();
```

and in the `Report` literal `demand_basis: basis.as_str(), choke_floor:
CHOKE_FLOOR, chokepoints,`. `run` calls `build_report_at(opts.demand, ...)`.
Imports: `use crate::capacity::{Basis, Chokepoint, CHOKE_FLOOR};`. The
doc comment of `build_report_at` keeps the existing text plus one
sentence: "the choke points are computed at `basis`".

`render_text`, after the findings block and before `out.push('\n')`:

```rust
    if report.chokepoints.is_empty() {
        out.push_str("chokepoints: none\n");
    } else {
        out.push_str(&format!("chokepoints: {}\n", report.chokepoints.len()));
        for point in &report.chokepoints {
            let id = device_id_cell(report, point.bus, point.address);
            out.push_str(&format!(
                "  hub {} ({}:{}, {}) {}\n",
                point.path, point.bus, point.address, id, point.message
            ));
        }
    }
```

(one space before the message: `  hub 3-1 (3:2, 0bda:5411) 384M carries ...`, the spec's line). Extract the findings block's vendor:product lookup into

```rust
/// `vid:pid` of the listed device, or `----:----`.
fn device_id_cell(report: &Report, bus: u8, address: u8) -> String {
    report
        .buses
        .iter()
        .flat_map(|b| b.devices.iter())
        .find(|d| d.bus == bus && d.address == address)
        .map_or_else(
            || "----:----".to_string(),
            |d| match (&d.vendor_id, &d.product_id) {
                (Some(v), Some(p)) => format!("{v}:{p}"),
                _ => "----:----".to_string(),
            },
        )
}
```

and use it in the findings block too. Update `render_text`'s doc comment.

`src/main.rs`: next to `UpdateUsbidsMode`:

```rust
/// `--demand`'s two bases (see `capacity::Basis`).
#[derive(Clone, clap::ValueEnum)]
enum DemandBasis {
    Link,
    Capability,
}

impl From<DemandBasis> for capacity::Basis {
    fn from(basis: DemandBasis) -> Self {
        match basis {
            DemandBasis::Link => capacity::Basis::Link,
            DemandBasis::Capability => capacity::Basis::Capability,
        }
    }
}
```

The flag, after `--json`:

```rust
    /// Which rate the choke-point model assumes every device pushes:
    /// `link` (its current link) or `capability` (what it could link at)
    #[arg(long, value_enum, default_value = "link", value_name = "BASIS")]
    demand: DemandBasis,
```

and `demand: cli.demand.clone().into(),` in the `HeadlessOptions`
literal. If the file has a clap parsing test (grep `try_parse_from`), add
one asserting `--demand capability` parses and `--demand both` is
rejected; otherwise add:

```rust
    #[test]
    fn demand_flag_parses_its_two_bases_and_nothing_else() {
        let cli = Cli::try_parse_from(["usbtop-ng", "--once", "--demand", "capability"]).unwrap();
        assert!(matches!(cli.demand, DemandBasis::Capability));
        let cli = Cli::try_parse_from(["usbtop-ng", "--once"]).unwrap();
        assert!(matches!(cli.demand, DemandBasis::Link));
        assert!(Cli::try_parse_from(["usbtop-ng", "--once", "--demand", "both"]).is_err());
    }
```

`src/fixture_replay.rs`: `replay_fixture_with_elapsed` gains a trailing
`basis: Basis` parameter and calls `build_report_at(basis, ...)`;
`replay_fixture` passes `Basis::Link`; add

```rust
/// `replay_fixture` at a chosen basis, for the corpus tests that pin the
/// capability view.
#[cfg(test)]
pub fn replay_fixture_at(bundle_dir: &Path, source: FixtureSource, basis: Basis) -> anyhow::Result<Report> {
    replay_fixture_with_elapsed(bundle_dir, Some(source), FIXED_ELAPSED, basis)
}
```

(match how `replay_fixture` calls `replay_fixture_with_elapsed`; every
other caller, the capturer included, passes `Basis::Link`). The literal
`Report { .. }` in `fixture_replay.rs` tests and `headless/export.rs`
tests gain `demand_basis: "link", choke_floor: 1.25, chokepoints:
Vec::new()`.

Run `cargo test -- headless main fixture_replay` : green; `cargo test
fixture_corpus` : the goldens fail (three new keys). Expected.

- [ ] **Step 3: Corpus pins, then re-bless**

In `src/fixture_corpus.rs`, extend `the_dock_bundle_pins_the_two_findings_and_nothing_else`
(inside its per-source loop, after the capability assertions):

```rust
        // The theoretical load on every hub link, worst first: the Realtek
        // USB 2 half and the Terminus below it tie at 3.05x (path breaks the
        // tie), the Realtek USB 3 half with two 5G cameras is 2.00x; the
        // second Terminus (1.05x) and the dock's USB 2 half (1.03x) sit
        // inside the breathing room.
        // Practical figures rounded to a tenth and the ratio to a hundredth:
        // the sums of 0.8- and 0.7-scaled rates are not exact in f64.
        let tenth = |x: f64| (x * 10.0).round() / 10.0;
        let points: Vec<(&str, f64, f64, f64, usize)> = report
            .chokepoints
            .iter()
            .map(|c| (c.path.as_str(), tenth(c.capacity_mbps), tenth(c.demand_mbps), (c.ratio * 100.0).round() / 100.0, c.devices))
            .collect();
        assert_eq!(
            points,
            vec![
                ("3-1", 384.0, 1172.0, 3.05, 9),
                ("3-1.4", 384.0, 1172.0, 3.05, 8),
                ("4-1", 4250.0, 8500.0, 2.0, 2),
            ],
            "{source:?}"
        );
        assert_eq!(report.demand_basis, "link");
        assert_eq!(report.choke_floor, 1.25);
        assert_eq!(report.chokepoints[0].port.as_deref(), Some("usb3-port1"));
        // The capability view equals the link view here: every leaf below
        // a USB 2 half is bounded to 480 by it, and the USB 3 half's
        // cameras already link at their 5G capability.
        let at_capability = replay_fixture_at(&dir, source, Basis::Capability).unwrap();
        let cap_points: Vec<(&str, f64)> = at_capability.chokepoints.iter().map(|c| (c.path.as_str(), (c.ratio * 100.0).round() / 100.0)).collect();
        assert_eq!(cap_points, vec![("3-1", 3.05), ("3-1.4", 3.05), ("4-1", 2.0)]);
        assert_eq!(at_capability.demand_basis, "capability");
```

with `use crate::capacity::Basis;` and `replay_fixture_at` imported. If a
demand or capacity figure differs by rounding (e.g. 1172.4), replace the
expected value with the printed one only after checking it against the
spec's table by hand and noting the difference in the commit message.

Re-bless every golden and verify the diff is purely the three keys:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
for d in tests/fixtures/hosts/*/*/; do
  [ -f "$d/golden.binary.json" ] || continue
  b=${d#tests/fixtures/hosts/}; b=${b%/}
  USBTOP_NG_BLESS_BUNDLE="$b" cargo test bless_named_bundle -- --ignored --nocapture || break
done
for g in tests/fixtures/hosts/*/*/golden.*.json; do
  diff <(git show HEAD:"$g" | jq -S .) \
       <(jq -S 'del(.chokepoints, .demand_basis, .choke_floor)' "$g") >/dev/null \
    && jq -e '.demand_basis == "link" and .choke_floor == 1.25 and (.chokepoints | type == "array")' "$g" >/dev/null \
    && echo "OK   $g" || echo "DRIFT $g"
done
echo "--- bundles with choke points (model output, list them in the commit message):"
for g in tests/fixtures/hosts/*/*/golden.binary.json; do n=$(jq '.chokepoints | length' "$g"); [ "$n" -gt 0 ] && echo "$n $g $(jq -c '[.chokepoints[] | [.path, (.ratio*100|round)/100]]' "$g")"; done
```

Expected: 36 `OK`, no `DRIFT`. Bundles other than the dock may list
entries (any hub with two 480M devices); read each against its sysfs
snapshot once and list them in the commit message. `cargo test --all-targets`
: green.

- [ ] **Step 4: Gates and one commit for Tasks 2 and 3**

fmt, the four clippy configs, `cargo test --all-targets`, `cargo test
--features capture-fixture`, `cargo test --features integration`.

```bash
git add src/capacity/mod.rs src/main.rs src/headless/mod.rs src/headless/export.rs src/fixture_replay.rs src/fixture_corpus.rs tests/fixtures/hosts
git commit -m "feat(capacity): choke points, the theoretical load on every hub link, in the reports at the link or the capability basis"
```

---

### Task 4: The TUI

**Files:**
- Modify: `src/ui/mod.rs` (`UsbTopApp` struct ~131 and `new()`;
  `ConnectorView` ~90-110; `sync_from` ~395-460; `header_lines` findings
  block ~1204-1214; `connector_line` ~1699-1721; `apply_key` bare-letter
  arms ~905-930; the controls spans ~1845-1856; the help overlay key list
  ~1896 and features list ~1926; tests)

**Interfaces:**
- Consumes: `capacity::{analyze, Basis, Chokepoint}`,
  `usbmon::parser::short_mbps`, `connector::{port_of_device, port_name}`.
- Produces: `UsbTopApp::demand_basis: Basis`, `UsbTopApp::chokepoints:
  Vec<Chokepoint>`, `ConnectorView::choke: Option<Chokepoint>`, the `c`
  binding, `UsbTopApp::worst_choke(&self) -> Option<f64>`.

- [ ] **Step 1: Tests first**

```rust
    /// `flagged_fixture`'s topology plus a 480 hub on root port 2 of usb3
    /// carrying two 480 devices: one choke point at 2.00x on that hub.
    fn choked_fixture() -> (tempfile::TempDir, DeviceManager) {
        let (temp, _) = topology_fixture();
        let base = temp.path().join("devices");
        let ctrl = temp.path().join("0000:00:14.0");
        std::fs::create_dir_all(ctrl.join("usb3").join("usb3:1.0").join("usb3-port2")).unwrap();
        let write = |dir: &std::path::Path, attrs: &[(&str, &str)]| {
            std::fs::create_dir_all(dir).unwrap();
            for (k, v) in attrs {
                std::fs::write(dir.join(k), format!("{v}\n")).unwrap();
            }
        };
        // topology_fixture already holds 3-2 (dev 3, 480) as a plain device
        // on root port 2; turn it into a hub with two devices below.
        for n in 1..=2 {
            std::fs::create_dir_all(base.join("3-2").join("3-2:1.0").join(format!("3-2-port{n}"))).unwrap();
            write(&base.join(format!("3-2.{n}")), &[("busnum", "3"), ("devnum", &(20 + n).to_string()), ("speed", "480")]);
        }
        let mut mgr = DeviceManager::with_sysfs_base(base);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        (temp, mgr)
    }

    #[test]
    fn the_header_shows_the_worst_choke_ratio_only_above_the_floor() {
        let (_temp, mgr) = choked_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        assert_eq!(app.chokepoints.len(), 1);
        let stats_line = &header_lines(&app)[1];
        assert!(stats_line.spans.iter().any(|s| s.content == " | choke: "));
        let value = stats_line.spans.iter().find(|s| s.content == "2.00x").expect("the ratio");
        assert_eq!(value.style.fg, Some(WARNING_COLOR));

        let (_temp, plain) = topology_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&plain);
        assert!(!header_lines(&app)[1].to_string().contains("choke"));
    }

    #[test]
    fn the_c_key_toggles_the_basis_and_the_header_says_so() {
        let (_temp, mgr) = choked_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        assert_eq!(app.demand_basis, Basis::Link);
        let outcome = apply_key(&mut app, KeyEvent::from(KeyCode::Char('c')));
        assert_eq!(outcome, KeyOutcome::Resync, "the model is rebuilt at the new basis before the repaint");
        assert_eq!(app.demand_basis, Basis::Capability);
        app.sync_from(&mgr);
        assert!(header_lines(&app)[1].to_string().contains("2.00x (cap)"));
        apply_key(&mut app, KeyEvent::from(KeyCode::Char('c')));
        assert_eq!(app.demand_basis, Basis::Link);
    }

    #[test]
    fn the_choked_hubs_connector_heading_carries_the_ratio() {
        let (_temp, mgr) = choked_fixture();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        let (lines, _) = device_list_lines_with_selection(&app);
        let headings: Vec<String> = lines.iter().map(|l| l.to_string()).filter(|l| l.starts_with("▶ Port")).collect();
        let choked: Vec<&String> = headings.iter().filter(|h| h.contains("choke")).collect();
        assert_eq!(choked.len(), 1, "{headings:?}");
        assert!(choked[0].contains("· hub · choke 2.00x (768M asked of 384M)  rx"), "{}", choked[0]);
    }
```

Extend the help-overlay test near line 3470 with
`assert!(screen.contains("Toggle the choke basis"), "{screen}");` and the
controls test (the one that asserts `"Controls: "`) with a check that
`"Capacity basis"` is present. Check how `apply_key` is invoked in the
existing key tests (name and argument shape) and match it. Run `cargo test
-- ui::` : compile errors.

- [ ] **Step 2: Implement**

`UsbTopApp` gains

```rust
    /// Which rate the choke-point model assumes every device pushes; the
    /// `c` key toggles it.
    pub demand_basis: Basis,
    /// The hubs whose links are asked more than they can carry, worst first,
    /// recomputed from the manager every tick (see `capacity::analyze`).
    pub chokepoints: Vec<Chokepoint>,
```

initialised `demand_basis: Basis::default(), chokepoints: Vec::new()` in
`new()`. `ConnectorView` gains

```rust
    /// The choke point of a hub sitting on this connector (that connector
    /// is the hub's link); the worse one when both halves of a hub choke.
    pub choke: Option<Chokepoint>,
```

In `sync_from`, after the findings map:

```rust
        self.chokepoints = analyze_capacity(manager, self.demand_basis);
        // By the port name of the hub's link, the same names the placement
        // key joins with `+`.
        let chokes_by_port: HashMap<&str, &Chokepoint> = self
            .chokepoints
            .iter()
            .filter_map(|c| c.port.as_deref().map(|p| (p, c)))
            .collect();
```

(import `crate::capacity::{analyze as analyze_capacity, Basis, Chokepoint}`).
When a `ConnectorView` is created:

```rust
                            choke: placement
                                .key
                                .split('+')
                                .filter_map(|port| chokes_by_port.get(port))
                                .max_by(|a, b| a.ratio.partial_cmp(&b.ratio).unwrap_or(std::cmp::Ordering::Equal))
                                .map(|c| (*c).clone()),
```

(`placement.key` is `usb3-port1+usb4-port1` or `device:<name>`; the
latter never matches a port name). Add to `UsbTopApp`:

```rust
    /// The worst choke ratio on screen, when any hub is at or above the
    /// breathing room; the list is worst first.
    pub fn worst_choke(&self) -> Option<f64> {
        self.chokepoints.first().map(|c| c.ratio)
    }
```

`header_lines`, after the findings block:

```rust
    // The worst hub link: asked this many times its capacity by the devices
    // below it (theoretical, see `capacity`); `(cap)` at the capability basis.
    if let Some(ratio) = app.worst_choke() {
        stats_line.push(Span::raw(" | choke: "));
        stats_line.push(Span::styled(
            format!("{ratio:.2}x"),
            Style::default()
                .fg(WARNING_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
        if app.demand_basis == Basis::Capability {
            stats_line.push(Span::raw(" (cap)"));
        }
    }
```

`connector_line`: build `let choke = match &connector.choke { Some(c) =>
format!(" · choke {:.2}x ({} asked of {})", c.ratio, short_mbps(c.demand_mbps),
short_mbps(c.capacity_mbps)), None => String::new() };` and render
`format!("▶ {heading} · bus {span}{hub}{choke}  ")`. `apply_key`, after
the `KeyCode::Char('i')` arm:

```rust
        KeyCode::Char('c') => {
            app.demand_basis = app.demand_basis.toggled();
            KeyOutcome::Resync
        }
```

(the Ctrl-C arm above it still matches first). Controls spans: after
`" Idle devices  "` add `Span::styled("c", accent_bold), Span::raw("
Capacity basis  ")`. Help overlay key list, after the `i` line:

```rust
        Line::from(vec![
            Span::styled("  c", Style::default().fg(ACCENT_COLOR)),
            Span::raw("        Toggle the choke basis: link rates (default) or capabilities"),
        ]),
```

and in the features list after the 🔺 bullet:

```rust
        Line::from("  • Header shows 'choke: N.NNx' when a hub's link is asked at least 1.25x its capacity"),
        Line::from("    (the breathing room) by the devices below it; '(cap)' marks the capability basis"),
```

Adjust the help overlay's height if its test asserts a line count. Run
`cargo test -- ui::` : green.

- [ ] **Step 3: Gates and commit**

fmt, four clippy configs, `cargo test --all-targets`.

```bash
git add src/ui/mod.rs
git commit -m "feat(ui): the worst choke ratio in the header, the ratio on the choked hub's connector, c toggles the basis"
```

---

### Task 5: Docs

**Files:** `README.md` (after the Findings paragraph; the keys list; the
caveats), `docs/SCRIPTING.md` (report table, a `chokepoints[]` table, the
example document, "The chokepoints list" section after "The findings
list", `--demand` where `--once`/`--batch` flags are described),
`docs/ARCHITECTURE.md` (a `#### 4c. Capacity (capacity/)` entry after the
findings one; the user-interface paragraph naming the `c` key),
`docs/ROADMAP.md` (cable and port diagnostics: part 3 shipped for the USB
tree; the next bullet: the controller's PCIe uplink and the Thunderbolt
link as stages shared by every tunnel and a host-to-host network peer),
`CHANGELOG.md` (Unreleased, Added).

- [ ] **Step 1: Write them**

README: a "Choke points" paragraph: what the model is (theoretical:
every device below a hub pushing its rate, summed against the hub's own
link), the breathing room of 1.25 by name, the `c` key and the two bases,
and a two-line text sample from the dock bundle (`chokepoints: 3` then
the `hub 3-1 (3:2, 0bda:5411) 384M carries 9 devices asking 1172M: 3.05x`
line); the keys list gains `c`; the caveats gain "choke points are
theoretical: two 480M devices under one 480M hub read 2.00x whether or
not they ever transfer together". SCRIPTING: the three report fields in
the report table (`demand_basis`, `choke_floor`, `chokepoints`), a
`chokepoints[]` table with every `ChokepointReport` field and the `top[]`
pair, the example document regenerated from the dock bundle's
`golden.binary.json` (trimmed like the existing one), the explainer
section defining the breathing room and the two bases with the bounded
capability rule, and `--demand link|capability` (default `link`); a jq
line: `jq -c '.chokepoints[] | [.path, .ratio]'`. ARCHITECTURE: the module
entry (pure over the manager's rows, parents from names, one walk, the
floor). ROADMAP and CHANGELOG as described. Verify every number and
string against the code and the golden (`jq '.chokepoints' tests/fixtures/hosts/tgl-tb4-2026-09-12/stage2/golden.binary.json`).

- [ ] **Step 2: Commit**

```bash
git add README.md docs CHANGELOG.md
git commit -m "docs: choke points in the README, scripting, architecture and roadmap docs"
```

## Self-review notes

- Spec coverage: stages and demand (Task 2), breathing room (Task 2, carried
  in JSON in Task 3), bases with the bounded capability rule (Task 2),
  per-tick recompute and the `c` key (Task 4), header and heading (Task 4),
  `--demand` and the JSON/text surfaces (Task 3), corpus pins at both bases
  and the additive re-bless (Task 3), docs (Task 5), deferred stages
  untouched.
- Type consistency: `Basis::{Link, Capability}` with `as_str`/`toggled`;
  `Chokepoint { bus, address, path, port, capacity_mbps, demand_mbps,
  ratio, devices, top }`; `Contributor { path, demand_mbps }`;
  `analyze(&DeviceManager, Basis)`; `build_report_at(Basis, ...)`;
  `replay_fixture_at(dir, source, Basis)`; `ChokepointReport` mirrors
  `Chokepoint` plus `message`; `ConnectorView.choke: Option<Chokepoint>`.
- Dead code: `capacity` lands with its callers (Tasks 2+3 one commit);
  `Basis::toggled` is used by the `c` key (Task 4) and tested in Task 2;
  `replay_fixture_at` is `#[cfg(test)]`.

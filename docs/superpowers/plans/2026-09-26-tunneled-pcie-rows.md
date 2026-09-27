# Tunneled PCIe Rows Implementation Plan

> **Outcome (2026-09-27):** executed and merged to main as 9f96faf..e7ba978.
> The deploy review's fix wave moved the MAC-name masking that Task 3
> describes here (`tunnel::mask_mac_interfaces`) into the redactor
> (`diag::redact::embeds_mac`, `Redactor::mask_interface_names`, and
> `Redactor::mac_addresses` masking such tokens in the kernel log). The
> task text below is the plan as executed; the spec is the authority for
> what shipped.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the PCIe devices tunneled over Thunderbolt or USB4 as rows
of the device table, grouped under their root port with the router named
when the pairing cannot be wrong, and carry the same facts in the JSON and
text reports and the support bundle.

**Architecture:** A new `pci` module holds what the tunneled-device reader
and the support bundle's PCI inventory agree on (the address shape, the
parent chain, the wake gate). A new `tunnel` module reads
`/sys/bus/pci/devices` and `/sys/bus/thunderbolt/devices` into `Tunnel`
values with a pure join rule, and lands with its first callers: the report
builder (`headless::build_report_at` gains a `tunnels` parameter, the JSON
gains a top-level `tunnels` list, the text a `tunnels:` section). The
bundle's `Replayed` state carries live tunnels; the TUI groups a tunneled
xHCI's buses under the root port, renders one selectable row per function
no bus represents, and reads tunnels at most once a second.

**Tech Stack:** Rust 1.88 (edition 2021), ratatui 0.30, serde, tempfile
(dev). No new crates.

**Spec:** `docs/superpowers/specs/2026-09-15-tunneled-pcie-rows-design.md`

## Global Constraints

- MSRV 1.88; `cargo +1.88.0 check --all-targets` passes.
- Zero `#[allow(...)]` and zero `#[expect(...)]`. Binary crate: every item
  must be reachable from `main` or a test in every feature configuration
  (`pci`, `tunnel`, `ui`, `tui`, `headless`, `fixture_replay`, `diag` are
  compiled in all four); gate test-only items with `#[cfg(test)]`, never
  suppress. A module lands in the same task as its first non-test caller.
- `cargo fmt --all -- --check` clean; clippy `-D warnings` clean on the
  default, `capture-fixture`, `integration`, and `ebpf` configs.
- Every cargo command is prefixed with `export PATH="$HOME/.cargo/bin:$PATH"`,
  and a step that pipes cargo through `tail` runs `set -o pipefail` first so
  a failed build fails the step. Several test filters go after `--`
  (`cargo test -- a b`); cargo takes only one before it.
- The gate, run at the end of every task before its commit:

  ```bash
  export PATH="$HOME/.cargo/bin:$PATH"
  cargo fmt --all
  cargo clippy --all-targets -- -D warnings
  cargo clippy --all-targets --features capture-fixture -- -D warnings
  cargo clippy --all-targets --features integration -- -D warnings
  cargo clippy --all-targets --features ebpf -- -D warnings
  cargo test --all-targets
  ```

  Tasks 4 and 5 also run `cargo test --features capture-fixture`,
  `cargo test --features integration`, `cargo +1.88.0 check --all-targets`
  and `bash evals/run.sh`.
- The join: exactly one depth-one router whose `uevent` reads
  `DEVTYPE=thunderbolt_device`, whose domain's `security` is `none`,
  `user` or `secure` and whose `authorized` is 1 or 2; exactly one tunnel;
  the router name list read before and after the PCI walk identical; the
  walk complete (no function skipped for an unparsable `vendor`, `device`
  or `class`). Otherwise no router is named.
- A tunnel is a root port with at least one removable, non-bridge function
  below it that is not a discrete controller's own. A discrete controller's
  root port is the top of `pci::chain` of a domain's host interface (the
  resolved parent directory of `thunderbolt/domainN`); under such a port a
  function whose chain has exactly three entries (root port, upstream
  port, downstream port) is skipped.
- `max_link_speed`, `current_link_speed` and `current_link_width` are
  opened only when `power/runtime_status` reads `active`; nothing else
  the reader opens can resume a device.
- The class table, the row text order (`PCIe {address} · {class} · {link}
  · {driver|no driver} · {interface}? · {vvvv}:{dddd}`), `asleep` and
  `link unread`, the label `Thunderbolt {name} {device_name} · {lanes}×{gbps}
  Gb/s` or `external PCIe port`, and the help line `PCIe rows: tunneled
  devices usbmon never sees; their link, not traffic` are exactly as the
  spec states them; rates print without decimals when integral, else one.
- The JSON report `version` stays 1; `tunnels` is additive; the JSON
  `class` is six hex digits with no prefix, `vendor_id`/`device_id` four;
  every committed golden is regenerated once (Task 2) and the diff
  verified to be only the added empty `tunnels` key.
- PCIe rows are selectable by the key `pcie:{address}`; a selected row has
  no trailing lines; the idle filter hides a `suspended` function; the
  search matches the row text; `--filter` hides every PCIe row in the TUI
  and leaves the JSON `tunnels` alone.
- The support bundle's `report.json` carries the live tunnels with any
  `enx`/`wlx` + twelve-hex-digit interface name masked as `enx<redacted>`
  or `wlx<redacted>`; fixtures replay with an empty list.
- Never write a hostname, account name, email, or IP address into any
  file; hosts appear only as public labels (`devhost`, `tgl-tb4`). Write
  the word NUL in words, never as an escape. The private reference project
  is never named.
- Commit trailers per `CLAUDE.md`: `Co-Authored-By: Claude Fable 5.1
  <noreply@anthropic.com>` and `Claude-Session:
  https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t`. Commits are
  signed by the repo's configured SSH signer; if `git commit` reports
  "Couldn't get agent socket" or "Couldn't find key in agent", stop and
  tell the controller (it runs `~/.local/bin/op-agent-load`).
- Reviewers use read-only git only.

## Review Focus

1. An awake function whose `current_link_width` reads `0` (link down): the
   row must say `link unread`, never `8 GT/s ×0`. Pinned in Task 2
   (`a_zero_width_link_is_unread`).
2. A `net` entry that is a file, or absent: `interface` is `None` and the
   function is still a row. Pinned in Task 2
   (`the_interface_is_the_single_net_entry_or_none`).
3. The selected PCIe row's tunnel vanishes on the next tick: the selection
   is cleared, nothing panics. Pinned in Task 4
   (`down_from_the_last_device_row_selects_the_pcie_row_and_scrolls_to_it`).
4. `--filter` keeps `tunnels` in the JSON and hides every PCIe row in the
   TUI. Pinned in Task 2 (`tunnels_ignore_the_filter`) and Task 4
   (`search_idle_and_filter_apply_to_pcie_rows`).
5. A `removable` that exists but cannot be read (a directory in its place,
   which fails the same way an unreadable file does for root): the entry
   is skipped and counts for nothing. Pinned in Task 2
   (`an_unreadable_removable_is_skipped`).

---

## File structure

- Create `src/pci/mod.rs`: `is_address`, `chain`, `runtime_status`,
  `is_awake`, the wake-gate citation, the moved address test.
- Modify `src/diag/inventory.rs`: use the `pci` helpers; drop its own
  copies; correct the `removable` doc comment.
- Modify `src/test_tree.rs` (`#[cfg(test)]`): `PciTree`, a fake PCI and
  Thunderbolt tree shared by the tunnel, support and replay tests.
- Create `src/tunnel/mod.rs`: `Router`, `PciLink`, `PciFunction`,
  `Tunnel`, `read_tunnels`, the join, the class table, the label and link
  texts, `mask_mac_interfaces`, the two live path constants.
- Modify `src/main.rs`: `mod pci;` and `mod tunnel;`.
- Modify `src/headless/mod.rs`: `Report.tunnels`, `TunnelReport`,
  `RouterReport`, `PciFunctionReport`, the `tunnels` parameter of
  `build_report_at`, the text section, `run` reading live tunnels.
- Modify `src/headless/export.rs` (a test's `Report` literal),
  `src/fixture_replay.rs` (`Replayed.tunnels`, a test literal),
  `src/diag/support.rs` (`Roots::live` constants, `run_support` filling
  `Replayed.tunnels`, a test's `build_report_at` call).
- Modify `src/ui/mod.rs`: `UsbTopApp.tunnels`, `set_tunnels`,
  `ControllerView.tunnel`/`pcie`, `TunnelView`, `PcieRow`, grouping,
  keys, filters, pruning, rendering, the help line; `src/tui/mod.rs`: the
  once-a-second read.
- Re-bless `tests/fixtures/hosts/*/*/golden.{binary,text}.json` (36 files)
  inside Task 2's commit.
- Docs: `README.md`, `docs/ARCHITECTURE.md`, `docs/SCRIPTING.md`,
  `docs/TESTING.md`, `CHANGELOG.md`, `docs/ROADMAP.md`.

---

### Task 1: The shared `pci` module

**Files:**
- Create: `src/pci/mod.rs`
- Modify: `src/main.rs` (the `mod` list), `src/diag/inventory.rs`
  (`is_pci_address`, `pci_chain`, `read_pci_attrs`, the `PciEntry` doc
  comment, the moved test)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn pci::is_address(name: &str) -> bool`,
  `pub fn pci::chain(real: &Path) -> Vec<String>`,
  `pub fn pci::is_awake(status: Option<&str>) -> bool`. (The reader of
  `power/runtime_status` itself, `pci::runtime_status`, arrives in Task 2
  with its first caller.)

- [ ] **Step 1: Write the failing tests**

Create `src/pci/mod.rs` with only the tests module and `use` line for now:

```rust
//! What the two readers of the PCI side of a Thunderbolt or USB4 tunnel
//! share: the tunneled-device rows (`tunnel`) and the support bundle's
//! `inventory/pci-removable.toml` (`diag::inventory`). The shape of a PCI
//! address, the bridges above a device, and the rule for reading its link.

use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_lists_the_addresses_above_a_device_root_first() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp
            .path()
            .join("sys/devices/pci0000:00/0000:00:07.1/0000:2c:00.0/0000:2d:00.0/0000:2e:00.0");
        std::fs::create_dir_all(&real).unwrap();
        assert_eq!(
            chain(&real),
            ["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0"]
        );
        let on_the_root_bus = temp.path().join("sys/devices/pci0000:00/0000:00:0d.2");
        std::fs::create_dir_all(&on_the_root_bus).unwrap();
        assert!(chain(&on_the_root_bus).is_empty());
    }

    #[test]
    fn only_active_is_awake() {
        assert!(is_awake(Some("active")));
        assert!(!is_awake(Some("suspended")));
        assert!(!is_awake(Some("unsupported")));
        assert!(!is_awake(Some("active\n")), "callers trim before asking");
        assert!(!is_awake(None));
    }
}
```

Then move the address test: cut the whole `#[test] fn
pci_addresses_allow_wide_domains_and_nothing_else()` function out of
`src/diag/inventory.rs`'s tests module (find it with `grep -n
pci_addresses_allow_wide_domains src/diag/inventory.rs`; it runs from the
`#[test]` line to the closing `}` of the function) and paste it into the
tests module above, replacing every `is_pci_address(` in it with
`is_address(`.

Add `mod pci;` to `src/main.rs` directly after the line `mod headless;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test pci::tests 2>&1 | tail -20`
Expected: compile errors, `cannot find function `chain``, `is_address`,
`is_awake` in this scope.

- [ ] **Step 3: Write the implementation**

Insert between the `use` line and `#[cfg(test)]` in `src/pci/mod.rs`:

```rust
/// `0000:2e:00.0`: a domain of four hex digits or more (VMD and other
/// synthetic domains go past `ffff`), then a bus, a slot and a function.
pub fn is_address(name: &str) -> bool {
    let hex = |s: &str, min: usize, max: usize| {
        (min..=max).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
    };
    let mut parts = name.split(':');
    let (Some(domain), Some(bus), Some(rest), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Some((slot, func)) = rest.split_once('.') else {
        return false;
    };
    hex(domain, 4, 8) && hex(bus, 2, 2) && hex(slot, 2, 2) && hex(func, 1, 1)
}

/// The PCI addresses above `real` (a resolved device directory) in the
/// device tree, root-first: for a dock's xHCI, its root port and then the
/// dock's two bridges. Empty for a device on the root bus.
pub fn chain(real: &Path) -> Vec<String> {
    let mut chain: Vec<String> = real
        .ancestors()
        .skip(1)
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| is_address(n))
        .collect();
    chain.reverse();
    chain
}

/// The wake gate over a `power/runtime_status` value: only `active` is
/// awake; `suspended`, `unsupported`, anything else and an absent file are
/// not. Callers trim the value first.
///
/// `current_link_speed`, `current_link_width` and `max_link_width` are
/// read by the kernel through `pci_config_pm_runtime_get`
/// (drivers/pci/pci.c v7.0: a runtime-PM reference on the parent, a
/// barrier for a suspend in progress, and a resume for a device in
/// D3cold; the sysfs readers in drivers/pci/pci-sysfs.c call it), and
/// `max_link_speed` reads Link Capabilities 2 from config space with no
/// reference at all up to v6.12, which for a device in D3cold is a read of
/// a powered-down device and a meaningless value (v6.13 caches the
/// supported speeds at probe). Both readers open those attributes only
/// while this holds. The check and the read are not atomic: a device that
/// suspends between them is resumed once by the kernel and suspends again
/// on its own. The gate is best-effort, and a reader without it would keep
/// every device it described out of D3cold for as long as it ran.
pub fn is_awake(status: Option<&str>) -> bool {
    status == Some("active")
}
```

Now `src/diag/inventory.rs`:

1. Delete the functions `is_pci_address` and `pci_chain` (each with its
   doc comment).
2. Replace every remaining `is_pci_address(` with `crate::pci::is_address(`
   and every `pci_chain(` with `crate::pci::chain(` (check with `grep -n
   'is_pci_address\|pci_chain' src/diag/inventory.rs`; the result must be
   empty afterwards).
3. In `read_pci_attrs`, replace the line
   `if attrs.get("power/runtime_status").map(String::as_str) == Some("active") {`
   with
   `if crate::pci::is_awake(attrs.get("power/runtime_status").map(String::as_str)) {`.
4. Replace the doc comment on `PCI_LINK_ATTRS` with:

```rust
/// The attributes whose sysfs readers take a runtime-PM reference on the
/// device and its parent and resume a device in D3cold; read only while
/// the device is awake, the rule and its citation being
/// `crate::pci::is_awake`. The window between the check and the read is
/// accepted.
```

5. In the doc comment on `pub struct PciEntry`, replace the parenthetical
   `(behind a port the firmware flags as externally facing, which is what a
   Thunderbolt or USB4 port is, so a tunnel's devices are the removable
   ones: `pci_set_removable` in drivers/pci/probe.c, verified against
   v5.16 and v7.0, marks a device only below an external-facing or
   removable parent, and leaves every other device without the attribute)`
   with `(`pci_set_removable` in drivers/pci/probe.c: from v5.16 to v6.12
   everything below a port the firmware flags as externally facing, which
   a Thunderbolt or USB4 port is; from v6.13, on x86 with ACPI, only what
   sits behind a tunnel, a discrete controller's own switch excluded
   (`arch_pci_dev_is_removable`); the bundle lists what the running kernel
   marks and says nothing more)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && set -o pipefail && cargo test -- pci:: inventory 2>&1 | tail -15`
Expected: the three `pci::tests` pass; every `inventory` test still passes;
`grep -c is_pci_address src/diag/inventory.rs` prints `0`.

- [ ] **Step 5: Run the gate and commit**

Run the gate from Global Constraints. Then:

```bash
git add src/pci/mod.rs src/main.rs src/diag/inventory.rs
git commit -F - <<'EOF'
refactor(pci): the address check, the parent chain and the wake gate move to a shared module

The tunneled-device reader and the support bundle's PCI inventory must
agree on what a PCI address looks like, which bridges sit above a device,
and when its link may be read; `pci` holds those three, with the
runtime-PM citation on the gate, and the inventory calls them. Its doc comment on
`removable` now states both kernel rule sets (v5.16 to v6.12: everything
below an external-facing port; v6.13 and later: only behind a tunnel).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t
EOF
```

---

### Task 2: The `tunnel` reader and the reports

The reader lands with its first callers: `headless::run` reads live
tunnels each window, `build_report_at` carries them into the JSON and
text reports. Every other caller passes an empty list for now (Task 3
gives the bundle its live read).

**Files:**
- Modify: `src/pci/mod.rs` (`runtime_status` and its test)
- Modify: `src/test_tree.rs` (append `PciTree`)
- Create: `src/tunnel/mod.rs`
- Modify: `src/main.rs` (the `mod` list), `src/headless/mod.rs`
  (`Report`, the three report structs, `build_report_at`, `build_report`,
  `render_text`, `run`, tests), `src/headless/export.rs` (the test's
  `Report` literal), `src/fixture_replay.rs` (`Replayed::report`, the
  test's `Report` literal), `src/diag/support.rs` (the test's
  `build_report_at` call)

**Interfaces:**
- Consumes: `pci::{is_address, chain, is_awake}` from Task 1.
- Produces: `pci::runtime_status(real: &Path) -> Option<String>`;
  `tunnel::{PCI_DEVICES, THUNDERBOLT_DEVICES}` (`&str`),
  `tunnel::Router`, `tunnel::PciLink`, `tunnel::PciFunction` (with
  `is_usb_controller(&self) -> bool`), `tunnel::Tunnel`,
  `tunnel::read_tunnels(pci: &Path, thunderbolt: &Path) -> Vec<Tunnel>`,
  `tunnel::router_label(name: &str, device_name: Option<&str>, rx_lanes:
  Option<u32>, rx_gbps: Option<f64>) -> String`,
  `tunnel::NO_ROUTER_LABEL: &str`, `tunnel::link_text(gts: Option<f64>,
  width: Option<u32>, runtime_status: Option<&str>) -> String`,
  `tunnel::format_rate(rate: f64) -> String`;
  `headless::build_report_at(basis, manager, baseline, elapsed, facts,
  filter, tunnels: &[Tunnel]) -> Report`, `headless::{TunnelReport,
  RouterReport, PciFunctionReport}`, `Report.tunnels: Vec<TunnelReport>`;
  `test_tree::PciTree`.

- [ ] **Step 1: Add `PciTree` to the shared test helper**

Append to `src/test_tree.rs` (after the `impl Tree` block and before any
trailing tests):

```rust
/// A fake `/sys/bus/pci/devices` and `/sys/bus/thunderbolt/devices` for the
/// tunnel tests: device directories nested under `sys/devices/pci0000:00`
/// in their parent chain and symlinked from the flat `pci/` directory, so
/// `canonicalize` and `pci::chain` read them like real sysfs; routers and
/// domains as directories under `thunderbolt/`.
pub(crate) struct PciTree {
    root: tempfile::TempDir,
}

impl PciTree {
    pub(crate) fn new() -> PciTree {
        let root = tempfile::tempdir().unwrap();
        for dir in ["pci", "thunderbolt", "sys/devices/pci0000:00"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        PciTree { root }
    }

    /// The flat devices directory the reader lists.
    pub(crate) fn pci(&self) -> PathBuf {
        self.root.path().join("pci")
    }

    /// The Thunderbolt bus directory the reader lists.
    pub(crate) fn thunderbolt(&self) -> PathBuf {
        self.root.path().join("thunderbolt")
    }

    /// A PCI device: `chain` is root-first and ends with the device's own
    /// address. Its real directory nests under the addresses above it, the
    /// flat `pci/` directory gets a symlink to it, and each of `attrs` is
    /// written as a file with a trailing newline (a name with a `/` makes
    /// its directory, so `power/runtime_status` works). Returns the real
    /// directory.
    pub(crate) fn device(&self, chain: &[&str], attrs: &[(&str, &str)]) -> PathBuf {
        let mut real = self.root.path().join("sys/devices/pci0000:00");
        for address in chain {
            real = real.join(address);
        }
        std::fs::create_dir_all(&real).unwrap();
        let own = chain.last().expect("a chain ends with the device");
        let link = self.pci().join(own);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(&real, &link).unwrap();
        }
        for (name, value) in attrs {
            let path = real.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, format!("{value}\n")).unwrap();
        }
        real
    }

    /// The device's `net/<name>` directory: its network interface.
    pub(crate) fn net(&self, real: &Path, name: &str) {
        std::fs::create_dir_all(real.join("net").join(name)).unwrap();
    }

    /// A `driver` symlink in `real` to a directory named `name`.
    pub(crate) fn driver(&self, real: &Path, name: &str) {
        let target = self.root.path().join("drivers").join(name);
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, real.join("driver")).unwrap();
    }

    /// An entry `name` on the Thunderbolt bus whose `uevent` names
    /// `devtype` (`thunderbolt_device` for a router, `thunderbolt_xdomain`
    /// for a host-to-host peer), plus attrs. Returns the directory.
    pub(crate) fn router(&self, name: &str, devtype: &str, attrs: &[(&str, &str)]) -> PathBuf {
        let dir = self.thunderbolt().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("uevent"), format!("DEVTYPE={devtype}\n")).unwrap();
        for (name, value) in attrs {
            std::fs::write(dir.join(name), format!("{value}\n")).unwrap();
        }
        dir
    }

    /// `domain<n>` on the Thunderbolt bus: a symlink to `<nhi>/domain<n>`,
    /// the way the kernel hangs a domain off its host interface's PCI
    /// device directory, holding `security`.
    pub(crate) fn domain(&self, n: u32, nhi: &Path, security: &str) {
        let real = nhi.join(format!("domain{n}"));
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("security"), format!("{security}\n")).unwrap();
        std::os::unix::fs::symlink(&real, self.thunderbolt().join(format!("domain{n}"))).unwrap();
    }
}
```

- [ ] **Step 2: Write the failing tests**

In `src/pci/mod.rs`'s tests module add:

```rust
    #[test]
    fn runtime_status_is_read_trimmed() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("dev");
        std::fs::create_dir_all(dev.join("power")).unwrap();
        assert_eq!(runtime_status(&dev), None);
        std::fs::write(dev.join("power/runtime_status"), "suspended\n").unwrap();
        assert_eq!(runtime_status(&dev).as_deref(), Some("suspended"));
        assert!(!is_awake(runtime_status(&dev).as_deref()));
        std::fs::write(dev.join("power/runtime_status"), "active\n").unwrap();
        assert!(is_awake(runtime_status(&dev).as_deref()));
    }
```

Create `src/tunnel/mod.rs` with the module doc, the `use` lines and the
tests module only:

```rust
//! The PCIe side of every Thunderbolt or USB4 tunnel, read from sysfs for
//! the device table and the reports: which removable PCI functions sit
//! under which root port, what each is (class, driver, IDs, interface) and
//! how it is linked, and, when the pairing cannot be wrong, the router the
//! tunnel runs through. The rules and their kernel citations are in
//! `docs/superpowers/specs/2026-09-15-tunneled-pcie-rows-design.md`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::pci;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_tree::PciTree;

    /// The laptop with the dock: root port 07.1, two bridges, the xHCI
    /// awake at 2.5 GT/s x4; router 0-3 authorized on a `none` domain
    /// whose NHI sits on the root bus.
    fn dock(t: &PciTree) {
        t.device(
            &["0000:00:07.1", "0000:2c:00.0"],
            &[("removable", "removable"), ("class", "0x060400"), ("vendor", "0x8086"), ("device", "0x0b26")],
        );
        t.device(
            &["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0"],
            &[("removable", "removable"), ("class", "0x060400"), ("vendor", "0x8086"), ("device", "0x0b26")],
        );
        let xhci = t.device(
            &["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0", "0000:2e:00.0"],
            &[
                ("removable", "removable"),
                ("class", "0x0c0330"),
                ("vendor", "0x8086"),
                ("device", "0x0b27"),
                ("power/runtime_status", "active"),
                ("current_link_speed", "2.5 GT/s PCIe"),
                ("current_link_width", "4"),
                ("max_link_speed", "2.5 GT/s PCIe"),
            ],
        );
        t.driver(&xhci, "xhci_hcd");
        let nhi = t.device(&["0000:00:0d.2"], &[("class", "0x0c0340")]);
        t.domain(0, &nhi, "none");
        t.router(
            "0-3",
            "thunderbolt_device",
            &[
                ("generation", "4"),
                ("rx_speed", "20.0 Gb/s"),
                ("tx_speed", "20.0 Gb/s"),
                ("rx_lanes", "2"),
                ("tx_lanes", "2"),
                ("device_name", "Element Hub"),
                ("vendor_name", "CalDigit, Inc."),
                ("authorized", "1"),
            ],
        );
    }

    /// A NIC at 2d:00.1 under the dock's second bridge, `runtime_status` as
    /// given, with link attributes present whatever the status says.
    fn nic(t: &PciTree, status: &str) -> std::path::PathBuf {
        let nic = t.device(
            &["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0", "0000:2d:00.1"],
            &[
                ("removable", "removable"),
                ("class", "0x020000"),
                ("vendor", "0x1d6a"),
                ("device", "0x14c0"),
                ("power/runtime_status", status),
                ("current_link_speed", "8.0 GT/s PCIe"),
                ("current_link_width", "1"),
                ("max_link_speed", "16.0 GT/s PCIe"),
            ],
        );
        t.driver(&nic, "atlantic");
        t.net(&nic, "enp45s0");
        nic
    }

    #[test]
    fn functions_group_under_their_root_port_bridges_excluded_and_the_router_joins() {
        let t = PciTree::new();
        dock(&t);
        nic(&t, "active");
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        let tunnel = &tunnels[0];
        assert_eq!(tunnel.root_port, "0000:00:07.1");
        let addresses: Vec<&str> = tunnel.functions.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(addresses, ["0000:2d:00.1", "0000:2e:00.0"]);
        let nic = &tunnel.functions[0];
        assert_eq!(nic.class, 0x020000);
        assert_eq!(nic.class_name, "Ethernet");
        assert_eq!((nic.vendor_id, nic.device_id), (0x1d6a, 0x14c0));
        assert_eq!(nic.driver.as_deref(), Some("atlantic"));
        assert_eq!(nic.interface.as_deref(), Some("enp45s0"));
        assert!(nic.awake);
        assert_eq!(nic.link, Some(PciLink { gts: 8.0, width: 1 }));
        assert_eq!(nic.max_gts, Some(16.0));
        assert!(!nic.is_usb_controller());
        let xhci = &tunnel.functions[1];
        assert!(xhci.is_usb_controller());
        assert_eq!(xhci.link, Some(PciLink { gts: 2.5, width: 4 }));
        let router = tunnel.router.as_ref().expect("one router, one tunnel: joined");
        assert_eq!(router.name, "0-3");
        assert_eq!(router.device_name.as_deref(), Some("Element Hub"));
        assert_eq!(router.vendor_name.as_deref(), Some("CalDigit, Inc."));
        assert_eq!((router.generation, router.rx_gbps, router.rx_lanes), (Some(4), Some(20.0), Some(2)));
        assert_eq!((router.authorized, router.security.as_deref()), (Some(1), Some("none")));
    }

    #[test]
    fn the_wake_gate_leaves_a_sleeping_functions_link_unread() {
        let t = PciTree::new();
        dock(&t);
        nic(&t, "suspended");
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let nic = &tunnels[0].functions[0];
        assert!(!nic.awake);
        assert_eq!(nic.runtime_status.as_deref(), Some("suspended"));
        // The attributes hold valid values: a reader that opened them could
        // not produce this.
        assert_eq!(nic.link, None);
        assert_eq!(nic.max_gts, None);
        assert_eq!(nic.interface.as_deref(), Some("enp45s0"), "read whatever the power state");
        assert_eq!(link_text(None, None, Some("suspended")), "asleep");
        assert_eq!(link_text(None, None, Some("unsupported")), "link unread");
        assert_eq!(link_text(None, None, None), "link unread");
        assert_eq!(link_text(Some(8.0), Some(1), Some("active")), "8 GT/s ×1");
        assert_eq!(link_text(Some(2.5), Some(4), Some("active")), "2.5 GT/s ×4");
    }

    #[test]
    fn a_zero_width_link_is_unread() {
        let t = PciTree::new();
        dock(&t);
        let nic = nic(&t, "active");
        std::fs::write(nic.join("current_link_width"), "0\n").unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let nic = &tunnels[0].functions[0];
        assert!(nic.awake);
        assert_eq!(nic.link, None);
        assert_eq!(nic.max_gts, Some(16.0));
        assert_eq!(link_text(None, None, Some("active")), "link unread");
    }

    #[test]
    fn the_interface_is_the_single_net_entry_or_none() {
        let t = PciTree::new();
        dock(&t);
        let nic = nic(&t, "active");
        t.net(&nic, "enp45s0v1");
        assert_eq!(read_tunnels(&t.pci(), &t.thunderbolt())[0].functions[0].interface, None);
        std::fs::remove_dir_all(nic.join("net")).unwrap();
        std::fs::write(nic.join("net"), "").unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels[0].functions[0].interface, None);
        assert_eq!(tunnels[0].functions.len(), 2, "still a row");
    }

    #[test]
    fn class_names_come_from_three_tiers_or_fall_back_to_the_code() {
        assert_eq!(class_name(0x010802), Some("NVMe"));
        assert_eq!(class_name(0x010801), Some("non-volatile memory"));
        assert_eq!(class_name(0x010601), Some("SATA controller"));
        assert_eq!(class_name(0x018000), Some("mass storage"));
        assert_eq!(class_name(0x020000), Some("Ethernet"));
        assert_eq!(class_name(0x028000), Some("network"));
        assert_eq!(class_name(0x030000), Some("display"));
        assert_eq!(class_name(0x040300), Some("audio"));
        assert_eq!(class_name(0x048000), Some("multimedia"));
        assert_eq!(class_name(0x088000), Some("system peripheral"));
        assert_eq!(class_name(0x0c0010), Some("FireWire"));
        assert_eq!(class_name(0x0c0340), Some("USB4 host interface"));
        assert_eq!(class_name(0x0c0330), Some("USB controller"));
        assert_eq!(class_name(0x0c0500), Some("serial bus"));
        assert_eq!(class_name(0x0d1100), Some("wireless"));
        assert_eq!(class_name(0x120000), Some("accelerator"));
        assert_eq!(class_name(0xff0000), None);
        let t = PciTree::new();
        dock(&t);
        t.device(
            &["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0", "0000:2d:00.2"],
            &[("removable", "removable"), ("class", "0xff0000"), ("vendor", "0x1234"), ("device", "0x5678")],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let odd = tunnels[0].functions.iter().find(|f| f.address == "0000:2d:00.2").unwrap();
        assert_eq!(odd.class_name, "class 0xff0000");
        assert_eq!(odd.driver, None);
        assert_eq!(odd.runtime_status, None);
        assert_eq!(link_text(None, None, odd.runtime_status.as_deref()), "link unread");
    }

    #[test]
    fn usb_controllers_are_the_0c03_class_less_the_host_interface_and_the_gadget() {
        let f = |class: u32| PciFunction {
            address: "0000:2e:00.0".into(),
            class,
            class_name: String::new(),
            vendor_id: 0,
            device_id: 0,
            driver: None,
            interface: None,
            runtime_status: None,
            awake: false,
            link: None,
            max_gts: None,
        };
        assert!(f(0x0c0330).is_usb_controller());
        assert!(f(0x0c0320).is_usb_controller());
        assert!(!f(0x0c0340).is_usb_controller(), "the USB4 host interface");
        assert!(!f(0x0c03fe).is_usb_controller(), "the USB device controller");
        assert!(!f(0x020000).is_usb_controller());
    }

    #[test]
    fn a_discrete_controllers_own_functions_are_not_tunneled() {
        // Root port 1c.4, the chip's switch 05:00.0 / 06:0x.0, its NHI at
        // 07:00.0 and its xHCI at 08:00.0 directly under the switch, a
        // dock's NIC behind the dock's own switch under 06:02.0.
        let t = PciTree::new();
        let switch = &["0000:00:1c.4", "0000:05:00.0"];
        t.device(switch, &[("removable", "removable"), ("class", "0x060400"), ("vendor", "0x8086"), ("device", "0x15da")]);
        for down in ["0000:06:00.0", "0000:06:01.0", "0000:06:02.0"] {
            t.device(&["0000:00:1c.4", "0000:05:00.0", down], &[("removable", "removable"), ("class", "0x060400"), ("vendor", "0x8086"), ("device", "0x15da")]);
        }
        let nhi = t.device(
            &["0000:00:1c.4", "0000:05:00.0", "0000:06:00.0", "0000:07:00.0"],
            &[("removable", "removable"), ("class", "0x0c0340"), ("vendor", "0x8086"), ("device", "0x15d9")],
        );
        t.device(
            &["0000:00:1c.4", "0000:05:00.0", "0000:06:01.0", "0000:08:00.0"],
            &[("removable", "removable"), ("class", "0x0c0330"), ("vendor", "0x8086"), ("device", "0x15db")],
        );
        t.device(
            &["0000:00:1c.4", "0000:05:00.0", "0000:06:02.0", "0000:09:00.0"],
            &[("removable", "removable"), ("class", "0x060400"), ("vendor", "0x8086"), ("device", "0x0b26")],
        );
        t.device(
            &["0000:00:1c.4", "0000:05:00.0", "0000:06:02.0", "0000:09:00.0", "0000:0a:01.0"],
            &[("removable", "removable"), ("class", "0x060400"), ("vendor", "0x8086"), ("device", "0x0b26")],
        );
        t.device(
            &["0000:00:1c.4", "0000:05:00.0", "0000:06:02.0", "0000:09:00.0", "0000:0a:01.0", "0000:0b:00.0"],
            &[("removable", "removable"), ("class", "0x020000"), ("vendor", "0x1d6a"), ("device", "0x14c0")],
        );
        t.domain(0, &nhi, "user");
        t.router("0-1", "thunderbolt_device", &[("authorized", "1"), ("generation", "3")]);
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].root_port, "0000:00:1c.4");
        let addresses: Vec<&str> = tunnels[0].functions.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(addresses, ["0000:0b:00.0"], "the NHI and the chip's xHCI are the host's own");
        assert!(tunnels[0].router.is_some(), "still one router and one tunnel");
    }

    #[test]
    fn the_join_stays_silent_unless_everything_lines_up() {
        // Two routers.
        let t = PciTree::new();
        dock(&t);
        t.router("0-1", "thunderbolt_device", &[("authorized", "1")]);
        assert!(read_tunnels(&t.pci(), &t.thunderbolt())[0].router.is_none());

        // One router, two tunnels.
        let t = PciTree::new();
        dock(&t);
        t.device(
            &["0000:00:07.0", "0000:01:00.0", "0000:02:00.0", "0000:03:00.0"],
            &[("removable", "removable"), ("class", "0x0c0330"), ("vendor", "0x8086"), ("device", "0x0b27")],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 2);
        assert!(tunnels.iter().all(|t| t.router.is_none()));
        assert_eq!(tunnels[0].root_port, "0000:00:07.0", "sorted by root port");

        // A router that cannot carry PCIe: unauthorized, or a domain with
        // no PCIe tunneling.
        for (security, authorized) in [("none", "0"), ("nopcie", "1"), ("usbonly", "1"), ("dponly", "1")] {
            let t = PciTree::new();
            dock(&t);
            std::fs::write(t.thunderbolt().join("0-3/authorized"), format!("{authorized}\n")).unwrap();
            std::fs::write(t.thunderbolt().join("domain0/security"), format!("{security}\n")).unwrap();
            assert!(
                read_tunnels(&t.pci(), &t.thunderbolt())[0].router.is_none(),
                "security {security}, authorized {authorized}"
            );
        }

        // A host-to-host peer is not a router, however it is named.
        let t = PciTree::new();
        dock(&t);
        std::fs::remove_dir_all(t.thunderbolt().join("0-3")).unwrap();
        t.router("0-3", "thunderbolt_xdomain", &[("device_name", "peer-hostname"), ("authorized", "1")]);
        assert!(read_tunnels(&t.pci(), &t.thunderbolt())[0].router.is_none());

        // An incomplete walk: a function whose class does not parse.
        let t = PciTree::new();
        dock(&t);
        t.device(
            &["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0", "0000:2d:00.3"],
            &[("removable", "removable"), ("class", "0x"), ("vendor", "0x1234"), ("device", "0x5678")],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels[0].functions.len(), 1, "the unreadable function is no row");
        assert!(tunnels[0].router.is_none(), "and a half-read tree decides no join");

        // Router lists that differ between the two passes, in `join` itself.
        let router = Router {
            name: "0-3".into(),
            vendor_name: None,
            device_name: None,
            generation: None,
            rx_gbps: None,
            tx_gbps: None,
            rx_lanes: None,
            tx_lanes: None,
            authorized: Some(1),
            security: Some("none".into()),
        };
        let one = std::slice::from_ref(&router);
        assert!(join(one, &["0-3".to_string()], 1, true));
        assert!(!join(one, &["0-1".to_string()], 1, true));
        assert!(!join(one, &[], 1, true));
        assert!(!join(one, &["0-3".to_string()], 2, true));
        assert!(!join(one, &["0-3".to_string()], 1, false));
        assert!(!join(&[], &[], 1, true));
    }

    #[test]
    fn only_depth_one_routers_count_and_only_bridges_make_no_tunnel() {
        let t = PciTree::new();
        dock(&t);
        // Ignored by name shape or depth: the host router, a depth-two
        // router, a retimer, a service, the domain itself.
        t.router("0-0", "thunderbolt_device", &[("authorized", "1")]);
        t.router("0-301", "thunderbolt_device", &[("authorized", "1")]);
        t.router("0-0:1.1", "thunderbolt_device", &[]);
        t.router("0-3.1", "thunderbolt_device", &[]);
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].router.as_ref().map(|r| r.name.as_str()), Some("0-3"));

        // A second root port with only removable bridges below it is no
        // tunnel and does not block the join.
        t.device(
            &["0000:00:07.0", "0000:01:00.0"],
            &[("removable", "removable"), ("class", "0x060400"), ("vendor", "0x8086"), ("device", "0x0b26")],
        );
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1);
        assert!(tunnels[0].router.is_some());
    }

    #[test]
    fn parse_failures_leave_fields_empty_and_names_empty_are_none() {
        let t = PciTree::new();
        dock(&t);
        let nic = nic(&t, "active");
        std::fs::write(nic.join("current_link_speed"), "Unknown\n").unwrap();
        std::fs::write(nic.join("max_link_speed"), "Unknown\n").unwrap();
        std::fs::write(t.thunderbolt().join("0-3/device_name"), "\n").unwrap();
        std::fs::write(t.thunderbolt().join("0-3/rx_lanes"), "many\n").unwrap();
        std::fs::remove_file(t.thunderbolt().join("0-3/tx_speed")).unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        let nic = &tunnels[0].functions[0];
        assert!(nic.awake);
        assert_eq!(nic.link, None);
        assert_eq!(nic.max_gts, None);
        let router = tunnels[0].router.as_ref().unwrap();
        assert_eq!(router.device_name, None);
        assert_eq!(router.rx_lanes, None);
        assert_eq!(router.tx_gbps, None);
        assert_eq!(router.rx_gbps, Some(20.0));
    }

    #[test]
    fn an_unreadable_removable_is_skipped() {
        let t = PciTree::new();
        dock(&t);
        let odd = t.device(
            &["0000:00:07.0", "0000:01:00.0", "0000:02:00.0", "0000:03:00.0"],
            &[("class", "0x0c0330"), ("vendor", "0x8086"), ("device", "0x0b27")],
        );
        std::fs::create_dir_all(odd.join("removable")).unwrap();
        let tunnels = read_tunnels(&t.pci(), &t.thunderbolt());
        assert_eq!(tunnels.len(), 1, "a `removable` that cannot be read counts for nothing");
        assert!(tunnels[0].router.is_some());
    }

    #[test]
    fn unreadable_roots_give_nothing_or_no_routers() {
        let t = PciTree::new();
        dock(&t);
        assert!(read_tunnels(Path::new("/nonexistent/pci"), &t.thunderbolt()).is_empty());
        let tunnels = read_tunnels(&t.pci(), Path::new("/nonexistent/thunderbolt"));
        assert_eq!(tunnels.len(), 1);
        assert!(tunnels[0].router.is_none());
    }

    #[test]
    fn the_label_and_the_rates_print_as_the_spec_says() {
        assert_eq!(format_rate(8.0), "8");
        assert_eq!(format_rate(2.5), "2.5");
        assert_eq!(format_rate(20.0), "20");
        assert_eq!(
            router_label("0-3", Some("Element Hub"), Some(2), Some(20.0)),
            "Thunderbolt 0-3 Element Hub · 2×20 Gb/s"
        );
        assert_eq!(router_label("0-3", None, None, Some(10.0)), "Thunderbolt 0-3 · 10 Gb/s");
        assert_eq!(router_label("0-1", Some("Dock"), Some(3), None), "Thunderbolt 0-1 Dock");
        assert_eq!(NO_ROUTER_LABEL, "external PCIe port");
    }
}
```

Add `mod tunnel;` to `src/main.rs` directly after the line `mod tui;`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && set -o pipefail && cargo test -- tunnel::tests pci::tests 2>&1 | tail -20`
Expected: compile errors naming `runtime_status`, `read_tunnels`,
`PciFunction`, `Router`, `PciLink`, `link_text`, `class_name`, `join`,
`format_rate`, `router_label`, `NO_ROUTER_LABEL`.

- [ ] **Step 4: Write the reader**

First, in `src/pci/mod.rs`, insert after `chain` and before `is_awake`:

```rust
/// `power/runtime_status` of `real`, trimmed; `None` when absent or
/// unreadable. The value [`is_awake`] gates the link reads on; the
/// citation is there.
pub fn runtime_status(real: &Path) -> Option<String> {
    std::fs::read_to_string(real.join("power/runtime_status"))
        .ok()
        .map(|s| s.trim().to_string())
}
```

Then insert between the `use crate::pci;` line and `#[cfg(test)]` in
`src/tunnel/mod.rs`:

```rust
/// `/sys/bus/pci/devices`; `diag::support::Roots::live` uses the same.
pub const PCI_DEVICES: &str = "/sys/bus/pci/devices";
/// `/sys/bus/thunderbolt/devices`; likewise.
pub const THUNDERBOLT_DEVICES: &str = "/sys/bus/thunderbolt/devices";

/// The heading's label for a tunnel whose router is not named.
pub const NO_ROUTER_LABEL: &str = "external PCIe port";

/// A depth-one router on the Thunderbolt bus, as sysfs describes it
/// (Documentation/ABI/testing/sysfs-bus-thunderbolt).
#[derive(Debug, Clone, PartialEq)]
pub struct Router {
    /// `0-3`: domain, then the route in hex.
    pub name: String,
    pub vendor_name: Option<String>,
    pub device_name: Option<String>,
    pub generation: Option<u32>,
    /// `rx_speed`, `20.0 Gb/s` read as 20.0: the rate per lane.
    pub rx_gbps: Option<f64>,
    pub tx_gbps: Option<f64>,
    /// 1, 2 or 3 (3 being the wider side of an asymmetric link).
    pub rx_lanes: Option<u32>,
    pub tx_lanes: Option<u32>,
    /// The router's `authorized`: 0 means no PCIe devices reach the host.
    pub authorized: Option<u32>,
    /// Its domain's `security`: `dponly`, `usbonly` and `nopcie` create no
    /// PCIe tunnel.
    pub security: Option<String>,
}

impl Router {
    /// Whether a PCIe tunnel can run through this router: `security` is
    /// `none`, `user` or `secure` and `authorized` is 1 or 2.
    pub fn can_tunnel_pcie(&self) -> bool {
        matches!(self.security.as_deref(), Some("none" | "user" | "secure"))
            && matches!(self.authorized, Some(1 | 2))
    }
}

/// A function's negotiated link, read only while it was awake.
#[derive(Debug, Clone, PartialEq)]
pub struct PciLink {
    /// `current_link_speed`, `8.0 GT/s PCIe` read as 8.0.
    pub gts: f64,
    /// `current_link_width`; never 0 (a link of no lanes is down, so it
    /// is left unread).
    pub width: u32,
}

/// One removable, non-bridge PCI function under a tunnel's root port.
#[derive(Debug, Clone, PartialEq)]
pub struct PciFunction {
    pub address: String,
    /// The 24-bit class code, `0x020000`.
    pub class: u32,
    /// The class table's name, or `class 0x......` when it has none.
    pub class_name: String,
    pub vendor_id: u16,
    pub device_id: u16,
    /// The basename of the `driver` link, when bound.
    pub driver: Option<String>,
    /// The one entry of `net/`, when there is exactly one; read whatever
    /// the power state.
    pub interface: Option<String>,
    /// `power/runtime_status` as read; `None` when absent.
    pub runtime_status: Option<String>,
    /// `runtime_status` is `active`: the link attributes were opened.
    pub awake: bool,
    /// Only when awake and both attributes parsed.
    pub link: Option<PciLink>,
    /// `max_link_speed`, only when awake (see `pci::runtime_status`).
    pub max_gts: Option<f64>,
}

impl PciFunction {
    /// `class >> 8 == 0x0c03`, except the USB4 host interface `0x0c0340`
    /// and the USB device controller `0x0c03fe`, which host no bus.
    pub fn is_usb_controller(&self) -> bool {
        self.class >> 8 == 0x0c03 && !matches!(self.class & 0xff, 0x40 | 0xfe)
    }
}

/// One tunnel: a root port with tunneled functions under it, and its
/// router when the join names one.
#[derive(Debug, Clone, PartialEq)]
pub struct Tunnel {
    pub root_port: String,
    pub router: Option<Router>,
    /// Every removable non-bridge function under the root port that is not
    /// a discrete controller's own, USB controllers included, by address.
    /// Never empty.
    pub functions: Vec<PciFunction>,
}

/// A rate without decimals when it is integral and with one otherwise:
/// `8`, `2.5`, `20`.
pub fn format_rate(rate: f64) -> String {
    if rate.fract() == 0.0 {
        format!("{rate:.0}")
    } else {
        format!("{rate:.1}")
    }
}

/// The heading's label for a joined router: the name, then the device name
/// when the DROM has one, then `{lanes}×{gbps} Gb/s` when both are known or
/// `{gbps} Gb/s` when only the rate is. `Thunderbolt 0-3 Element Hub ·
/// 2×20 Gb/s`.
pub fn router_label(
    name: &str,
    device_name: Option<&str>,
    rx_lanes: Option<u32>,
    rx_gbps: Option<f64>,
) -> String {
    let mut label = format!("Thunderbolt {name}");
    if let Some(device_name) = device_name {
        label.push(' ');
        label.push_str(device_name);
    }
    match (rx_lanes, rx_gbps) {
        (Some(lanes), Some(gbps)) => label.push_str(&format!(" · {lanes}×{} Gb/s", format_rate(gbps))),
        (None, Some(gbps)) => label.push_str(&format!(" · {} Gb/s", format_rate(gbps))),
        _ => {}
    }
    label
}

/// The link as a row prints it: `8 GT/s ×1` when it was read, `asleep`
/// when the function is `suspended`, `link unread` otherwise (awake but
/// unparsable, or any other status, `unsupported` included).
pub fn link_text(gts: Option<f64>, width: Option<u32>, runtime_status: Option<&str>) -> String {
    match (gts, width, runtime_status) {
        (Some(gts), Some(width), _) => format!("{} GT/s ×{width}", format_rate(gts)),
        (_, _, Some("suspended")) => "asleep".to_string(),
        _ => "link unread".to_string(),
    }
}

/// The class table: the whole 24-bit code first, then the top two bytes,
/// then the top byte. Short forms of pci.ids' class names; the codes match
/// include/linux/pci_ids.h (see the spec's table).
fn class_name(class: u32) -> Option<&'static str> {
    let exact = match class {
        0x010802 => Some("NVMe"),
        0x0c0340 => Some("USB4 host interface"),
        _ => None,
    };
    let two_bytes = || match class >> 8 {
        0x0106 => Some("SATA controller"),
        0x0108 => Some("non-volatile memory"),
        0x0200 => Some("Ethernet"),
        0x0403 => Some("audio"),
        0x0c00 => Some("FireWire"),
        0x0c03 => Some("USB controller"),
        _ => None,
    };
    let top_byte = || match class >> 16 {
        0x01 => Some("mass storage"),
        0x02 => Some("network"),
        0x03 => Some("display"),
        0x04 => Some("multimedia"),
        0x08 => Some("system peripheral"),
        0x0c => Some("serial bus"),
        0x0d => Some("wireless"),
        0x12 => Some("accelerator"),
        _ => None,
    };
    exact.or_else(two_bytes).or_else(top_byte)
}

/// A sysfs attribute, trimmed; `None` when absent or unreadable.
fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// `0x1d6a` -> 0x1d6a.
fn parse_hex(value: &str) -> Option<u32> {
    let digits = value.trim().strip_prefix("0x")?;
    u32::from_str_radix(digits, 16).ok()
}

/// `8.0 GT/s PCIe` -> 8.0, `20.0 Gb/s` -> 20.0; `Unknown` -> `None`.
fn leading_float(value: &str) -> Option<f64> {
    value.split_whitespace().next()?.parse().ok()
}

/// The basename of the symlink at `path`, when it resolves.
fn link_basename(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

/// Sorted entry names under `dir`; empty when it cannot be listed.
fn sorted_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `0-3` -> the domain when the route (hex) is a depth-one route, 1..=255
/// (`tb_route_length` in drivers/thunderbolt/tb.h: bits above each 8-bit
/// hop); `0-0` is the host router and `0-301` is depth two.
fn depth_one_domain(name: &str) -> Option<u32> {
    let (domain, route) = name.split_once('-')?;
    let domain: u32 = domain.parse().ok()?;
    if route.is_empty() || !route.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let route = u64::from_str_radix(route, 16).ok()?;
    (1..=255).contains(&route).then_some(domain)
}

/// The depth-one routers on the bus, by name with their domain: entries of
/// the `%u-%llx` shape whose `uevent` carries `DEVTYPE=thunderbolt_device`
/// (a host-to-host peer has the same name shape and
/// `DEVTYPE=thunderbolt_xdomain`; retimers, services and `domainN` do not
/// match the shape).
fn router_names(thunderbolt: &Path) -> Vec<(String, u32)> {
    sorted_entries(thunderbolt)
        .into_iter()
        .filter_map(|name| {
            let domain = depth_one_domain(&name)?;
            let uevent = std::fs::read_to_string(thunderbolt.join(&name).join("uevent")).ok()?;
            uevent
                .lines()
                .any(|line| line == "DEVTYPE=thunderbolt_device")
                .then_some((name, domain))
        })
        .collect()
}

/// The router's attributes in the formats sysfs prints them; each `None`
/// when absent or unparsable, and an empty `device_name` or `vendor_name`
/// (a DROM without one) `None` too.
fn read_router(thunderbolt: &Path, name: &str, domain: u32) -> Router {
    let dir = thunderbolt.join(name);
    let text = |attr: &str| read_trimmed(&dir.join(attr)).filter(|s| !s.is_empty());
    let gbps = |attr: &str| text(attr).and_then(|s| leading_float(&s));
    let number = |attr: &str| text(attr).and_then(|s| s.parse::<u32>().ok());
    Router {
        name: name.to_string(),
        vendor_name: text("vendor_name"),
        device_name: text("device_name"),
        generation: number("generation"),
        rx_gbps: gbps("rx_speed"),
        tx_gbps: gbps("tx_speed"),
        rx_lanes: number("rx_lanes"),
        tx_lanes: number("tx_lanes"),
        authorized: number("authorized"),
        security: read_trimmed(
            &thunderbolt
                .join(format!("domain{domain}"))
                .join("security"),
        )
        .filter(|s| !s.is_empty()),
    }
}

/// The root ports with a discrete Thunderbolt controller below them: for
/// each `domainN` on the bus, the resolved directory's parent is the host
/// interface's PCI device, and the top of its chain, when it has one, is
/// such a port. An integrated host interface sits on the root bus and has
/// no chain.
fn discrete_ports(thunderbolt: &Path) -> BTreeSet<String> {
    sorted_entries(thunderbolt)
        .into_iter()
        .filter(|name| name.starts_with("domain"))
        .filter_map(|name| std::fs::canonicalize(thunderbolt.join(name)).ok())
        .filter_map(|real| real.parent().map(Path::to_path_buf))
        .filter_map(|nhi| pci::chain(&nhi).into_iter().next())
        .collect()
}

/// Every removable, non-bridge function under `pci` with its root port,
/// in address order, and whether the walk was complete (no function
/// skipped for an unparsable identity). Under a discrete controller's
/// root port, a function whose chain is exactly root port, upstream port,
/// downstream port is the controller's own and is skipped.
fn read_functions(pci_root: &Path, discrete: &BTreeSet<String>) -> (Vec<(String, PciFunction)>, bool) {
    let mut functions = Vec::new();
    let mut complete = true;
    for name in sorted_entries(pci_root) {
        if !pci::is_address(&name) {
            continue;
        }
        let Ok(real) = std::fs::canonicalize(pci_root.join(&name)) else {
            continue;
        };
        if read_trimmed(&real.join("removable")).as_deref() != Some("removable") {
            continue;
        }
        let Some(class) = read_trimmed(&real.join("class")).and_then(|s| parse_hex(&s)) else {
            complete = false;
            continue;
        };
        if class >> 16 == 0x06 {
            continue;
        }
        let chain = pci::chain(&real);
        let Some(root_port) = chain.first().cloned() else {
            continue;
        };
        if discrete.contains(&root_port) && chain.len() == 3 {
            continue;
        }
        let id = |attr: &str| {
            read_trimmed(&real.join(attr))
                .and_then(|s| parse_hex(&s))
                .and_then(|v| u16::try_from(v).ok())
        };
        let (Some(vendor_id), Some(device_id)) = (id("vendor"), id("device")) else {
            complete = false;
            continue;
        };
        let runtime_status = pci::runtime_status(&real);
        let awake = pci::is_awake(runtime_status.as_deref());
        let (link, max_gts) = if awake {
            let gts = read_trimmed(&real.join("current_link_speed")).and_then(|s| leading_float(&s));
            let width = read_trimmed(&real.join("current_link_width"))
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|w| *w > 0);
            let link = match (gts, width) {
                (Some(gts), Some(width)) => Some(PciLink { gts, width }),
                _ => None,
            };
            let max_gts = read_trimmed(&real.join("max_link_speed")).and_then(|s| leading_float(&s));
            (link, max_gts)
        } else {
            (None, None)
        };
        let mut net = sorted_entries(&real.join("net"));
        let interface = if net.len() == 1 { net.pop() } else { None };
        functions.push((
            root_port,
            PciFunction {
                address: name,
                class,
                class_name: class_name(class)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("class 0x{class:06x}")),
                vendor_id,
                device_id,
                driver: link_basename(&real.join("driver")),
                interface,
                runtime_status,
                awake,
                link,
                max_gts,
            },
        ));
    }
    (functions, complete)
}

/// The join, pure: the one router of `routers` names the one tunnel only
/// when `names_after` (the bus listed again after the PCI walk) holds the
/// same names, there is exactly one router and it can carry PCIe, there is
/// exactly one tunnel, and the walk was complete.
fn join(routers: &[Router], names_after: &[String], tunnel_count: usize, complete: bool) -> bool {
    let unchanged = routers.len() == names_after.len()
        && routers
            .iter()
            .zip(names_after)
            .all(|(router, name)| router.name == *name);
    complete
        && unchanged
        && tunnel_count == 1
        && matches!(routers, [router] if router.can_tunnel_pcie())
}

/// The tunnels under `pci_root` (`/sys/bus/pci/devices`) with the routers
/// under `thunderbolt` (`/sys/bus/thunderbolt/devices`), joined by the rule
/// in the spec's Decisions. Sorted by root port, functions by address. An
/// unreadable `pci_root` yields nothing; an unreadable `thunderbolt` yields
/// tunnels without routers. Nothing here resumes a sleeping device (see
/// `pci::runtime_status`).
pub fn read_tunnels(pci_root: &Path, thunderbolt: &Path) -> Vec<Tunnel> {
    let first = router_names(thunderbolt);
    let routers: Vec<Router> = first
        .iter()
        .map(|(name, domain)| read_router(thunderbolt, name, *domain))
        .collect();
    let discrete = discrete_ports(thunderbolt);
    let (functions, complete) = read_functions(pci_root, &discrete);
    let second: Vec<String> = router_names(thunderbolt)
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    let mut tunnels: Vec<Tunnel> = Vec::new();
    for (root_port, function) in functions {
        match tunnels.iter_mut().find(|t| t.root_port == root_port) {
            Some(tunnel) => tunnel.functions.push(function),
            None => tunnels.push(Tunnel {
                root_port,
                router: None,
                functions: vec![function],
            }),
        }
    }
    tunnels.sort_by(|a, b| a.root_port.cmp(&b.root_port));
    for tunnel in &mut tunnels {
        tunnel.functions.sort_by(|a, b| a.address.cmp(&b.address));
    }
    if join(&routers, &second, tunnels.len(), complete) {
        tunnels[0].router = routers.into_iter().next();
    }
    tunnels
}
```

- [ ] **Step 5: Run the tunnel tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && set -o pipefail && cargo test -- tunnel::tests pci::tests 2>&1 | tail -20`
Expected: the 13 tunnel tests and the 4 pci tests pass. (Until Step 6 wires the callers, clippy will
report dead code; that is expected mid-task and is resolved by Step 6.)

- [ ] **Step 6: Write the failing headless tests**

In `src/headless/mod.rs`'s tests module, add after the existing
`render_text_says_findings_none_when_there_are_none` test:

```rust
    fn sample_tunnel() -> Tunnel {
        Tunnel {
            root_port: "0000:00:07.1".into(),
            router: Some(Router {
                name: "0-3".into(),
                vendor_name: Some("CalDigit, Inc.".into()),
                device_name: Some("Element Hub".into()),
                generation: Some(4),
                rx_gbps: Some(20.0),
                tx_gbps: Some(20.0),
                rx_lanes: Some(2),
                tx_lanes: Some(2),
                authorized: Some(1),
                security: Some("none".into()),
            }),
            functions: vec![
                PciFunction {
                    address: "0000:2d:00.1".into(),
                    class: 0x020000,
                    class_name: "Ethernet".into(),
                    vendor_id: 0x1d6a,
                    device_id: 0x14c0,
                    driver: Some("atlantic".into()),
                    interface: Some("enp45s0".into()),
                    runtime_status: Some("active".into()),
                    awake: true,
                    link: Some(PciLink { gts: 8.0, width: 1 }),
                    max_gts: Some(16.0),
                },
                PciFunction {
                    address: "0000:2e:00.0".into(),
                    class: 0x0c0330,
                    class_name: "USB controller".into(),
                    vendor_id: 0x8086,
                    device_id: 0x0b27,
                    driver: Some("xhci_hcd".into()),
                    interface: None,
                    runtime_status: Some("suspended".into()),
                    awake: false,
                    link: None,
                    max_gts: None,
                },
            ],
        }
    }

    fn report_with(tunnels: &[Tunnel], filter: &FilterSet) -> Report {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        build_report_at(
            Basis::Link,
            &mgr,
            &baseline,
            Duration::from_secs(1),
            WindowFacts {
                source: "none",
                dropped: 0,
                text_active: false,
            },
            filter,
            tunnels,
        )
    }

    #[test]
    fn json_report_carries_a_tunnel_with_its_router_and_functions() {
        let report = report_with(&[sample_tunnel()], &FilterSet::default());
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["version"], 1, "additive fields do not bump the schema");
        let tunnels = v["tunnels"].as_array().unwrap();
        assert_eq!(tunnels.len(), 1);
        let t = &tunnels[0];
        assert_eq!(t["root_port"], "0000:00:07.1");
        assert_eq!(t["router"]["name"], "0-3");
        assert_eq!(t["router"]["device_name"], "Element Hub");
        assert_eq!(t["router"]["rx_gbps"], 20.0);
        assert_eq!(t["router"]["rx_lanes"], 2);
        assert_eq!(t["router"]["authorized"], 1);
        assert_eq!(t["router"]["security"], "none");
        assert_eq!(t["controllers"], serde_json::json!(["0000:2e:00.0"]));
        let nic = &t["functions"][0];
        assert_eq!(nic["address"], "0000:2d:00.1");
        assert_eq!(nic["class"], "020000");
        assert_eq!(nic["class_name"], "Ethernet");
        assert_eq!(nic["vendor_id"], "1d6a");
        assert_eq!(nic["device_id"], "14c0");
        assert_eq!(nic["driver"], "atlantic");
        assert_eq!(nic["interface"], "enp45s0");
        assert_eq!(nic["runtime_status"], "active");
        assert_eq!(nic["awake"], true);
        assert_eq!(nic["link_gts"], 8.0);
        assert_eq!(nic["link_width"], 1);
        assert_eq!(nic["max_link_gts"], 16.0);
        let xhci = &t["functions"][1];
        assert_eq!(xhci["awake"], false);
        assert!(xhci["link_gts"].is_null());
        assert!(xhci["interface"].is_null());
    }

    #[test]
    fn tunnels_ignore_the_filter() {
        let filter = FilterSet::parse(&["bus=9".into()]).unwrap();
        let report = report_with(&[sample_tunnel()], &filter);
        assert_eq!(report.tunnels.len(), 1, "a USB filter narrows buses, never the tunnels");
    }

    #[test]
    fn render_text_prints_the_tunnels_section_after_the_buses_and_before_findings() {
        let text = render_text(&report_with(&[sample_tunnel()], &FilterSet::default()));
        assert!(
            text.contains(
                "tunnels: 1\n  0000:00:07.1  Thunderbolt 0-3 Element Hub · 2×20 Gb/s  controllers 0000:2e:00.0\n    0000:2d:00.1  Ethernet  8 GT/s ×1  atlantic  enp45s0  1d6a:14c0\n    0000:2e:00.0  USB controller  asleep  xhci_hcd  -  8086:0b27\nfindings: none\n"
            ),
            "{text}"
        );
        let mut bare = sample_tunnel();
        bare.router = None;
        bare.functions.truncate(1);
        let text = render_text(&report_with(&[bare], &FilterSet::default()));
        assert!(
            text.contains("tunnels: 1\n  0000:00:07.1  external PCIe port  controllers none\n"),
            "{text}"
        );
    }

    #[test]
    fn render_text_says_tunnels_none_when_there_are_none() {
        let text = render_text(&report_with(&[], &FilterSet::default()));
        assert!(text.contains("\ntunnels: none\nfindings: none\n"), "{text}");
    }
```

Add `use crate::tunnel::{PciFunction, PciLink, Router};` to the tests
module's `use` lines (`use super::*;` brings in `Tunnel` once Step 7
imports it at the top of the file, but not these three).

- [ ] **Step 7: Wire the reports**

In `src/headless/mod.rs`:

1. Add to the top-level `use` lines: `use std::path::Path;` and
   `use crate::tunnel::{self, Tunnel};`.

2. In `pub struct Report`, insert after the `pub buses: Vec<BusReport>,`
   line:

```rust
    /// The PCIe side of every Thunderbolt or USB4 tunnel: one entry per
    /// root port with tunneled functions under it (see
    /// `tunnel::read_tunnels`). Empty when there is none; always empty in
    /// a fixture replay, which carries no PCI tree.
    pub tunnels: Vec<TunnelReport>,
```

3. Insert after the `impl From<&Finding> for FindingReport { ... }` block:

```rust
/// One tunnel as the JSON report carries it (see `tunnel::Tunnel`).
#[derive(Serialize)]
pub struct TunnelReport {
    pub root_port: String,
    pub router: Option<RouterReport>,
    /// Addresses among `functions` that are USB controllers; join with
    /// `buses[].controller`.
    pub controllers: Vec<String>,
    pub functions: Vec<PciFunctionReport>,
}

/// The joined router (see `tunnel::Router`); `null` when the join named none.
#[derive(Serialize)]
pub struct RouterReport {
    pub name: String,
    pub vendor_name: Option<String>,
    pub device_name: Option<String>,
    pub generation: Option<u32>,
    pub rx_gbps: Option<f64>,
    pub tx_gbps: Option<f64>,
    pub rx_lanes: Option<u32>,
    pub tx_lanes: Option<u32>,
    pub authorized: Option<u32>,
    pub security: Option<String>,
}

/// One tunneled function (see `tunnel::PciFunction`).
#[derive(Serialize)]
pub struct PciFunctionReport {
    pub address: String,
    /// The 24-bit class code as six hex digits, `020000`, no prefix, as
    /// the IDs.
    pub class: String,
    pub class_name: String,
    pub vendor_id: String,
    pub device_id: String,
    pub driver: Option<String>,
    pub interface: Option<String>,
    pub runtime_status: Option<String>,
    pub awake: bool,
    pub link_gts: Option<f64>,
    pub link_width: Option<u32>,
    pub max_link_gts: Option<f64>,
}

impl From<&Tunnel> for TunnelReport {
    fn from(t: &Tunnel) -> Self {
        TunnelReport {
            root_port: t.root_port.clone(),
            router: t.router.as_ref().map(|r| RouterReport {
                name: r.name.clone(),
                vendor_name: r.vendor_name.clone(),
                device_name: r.device_name.clone(),
                generation: r.generation,
                rx_gbps: r.rx_gbps,
                tx_gbps: r.tx_gbps,
                rx_lanes: r.rx_lanes,
                tx_lanes: r.tx_lanes,
                authorized: r.authorized,
                security: r.security.clone(),
            }),
            controllers: t
                .functions
                .iter()
                .filter(|f| f.is_usb_controller())
                .map(|f| f.address.clone())
                .collect(),
            functions: t
                .functions
                .iter()
                .map(|f| PciFunctionReport {
                    address: f.address.clone(),
                    class: format!("{:06x}", f.class),
                    class_name: f.class_name.clone(),
                    vendor_id: format!("{:04x}", f.vendor_id),
                    device_id: format!("{:04x}", f.device_id),
                    driver: f.driver.clone(),
                    interface: f.interface.clone(),
                    runtime_status: f.runtime_status.clone(),
                    awake: f.awake,
                    link_gts: f.link.as_ref().map(|l| l.gts),
                    link_width: f.link.as_ref().map(|l| l.width),
                    max_link_gts: f.max_gts,
                })
                .collect(),
        }
    }
}

/// The label the text report prints for a tunnel: the TUI's.
fn tunnel_label(t: &TunnelReport) -> String {
    match &t.router {
        Some(r) => tunnel::router_label(&r.name, r.device_name.as_deref(), r.rx_lanes, r.rx_gbps),
        None => tunnel::NO_ROUTER_LABEL.to_string(),
    }
}
```

4. Change the signature of `build_report_at` to end
   `filter: &FilterSet, tunnels: &[Tunnel],) -> Report {` and, in its
   final `Report { ... }` literal, insert `tunnels: tunnels.iter().map(TunnelReport::from).collect(),`
   directly after `buses: bus_reports,`. Extend its doc comment with one
   sentence: `The tunnels are the caller's read of the PCI side (see
   `tunnel::read_tunnels`); a replay passes none.`

5. In the `#[cfg(test)] pub fn build_report(...)` helper, add `&[]` as the
   last argument of its `build_report_at` call, and likewise in the test
   `json_report_carries_the_choke_points_and_the_basis`, whose `report`
   closure calls `build_report_at` directly. Afterwards
   `grep -n 'build_report_at(' src` must list exactly six sites, each with
   the new argument: the definition, `build_report`, `run` and that test in
   `src/headless/mod.rs`; `Replayed::report` in `src/fixture_replay.rs`;
   the test closure in `src/diag/support.rs` (item 10 below).

6. In `render_text`, insert directly after the `for bus in &report.buses {
   ... }` loop and before `if report.findings.is_empty() {`:

```rust
    if report.tunnels.is_empty() {
        out.push_str("tunnels: none\n");
    } else {
        out.push_str(&format!("tunnels: {}\n", report.tunnels.len()));
        for tunnel in &report.tunnels {
            let controllers = if tunnel.controllers.is_empty() {
                "none".to_string()
            } else {
                tunnel.controllers.join(", ")
            };
            out.push_str(&format!(
                "  {}  {}  controllers {}\n",
                tunnel.root_port,
                tunnel_label(tunnel),
                controllers
            ));
            for f in &tunnel.functions {
                out.push_str(&format!(
                    "    {}  {}  {}  {}  {}  {}:{}\n",
                    f.address,
                    f.class_name,
                    tunnel::link_text(f.link_gts, f.link_width, f.runtime_status.as_deref()),
                    f.driver.as_deref().unwrap_or("no driver"),
                    f.interface.as_deref().unwrap_or("-"),
                    f.vendor_id,
                    f.device_id
                ));
            }
        }
    }
```

   Update `render_text`'s doc comment: after the sentence about the
   device lines, add `then `tunnels: none` or `tunnels: N` with one line
   per tunnel (root port, label, its USB controllers) and one indented line
   per function, then the `findings:` section`.

7. In `run`, directly before `let mut report = build_report_at(`, add:

```rust
        // The PCI side of every tunnel, read fresh per window: the report
        // is a sample of the host, and a dock can come and go between two.
        let tunnels = tunnel::read_tunnels(
            Path::new(tunnel::PCI_DEVICES),
            Path::new(tunnel::THUNDERBOLT_DEVICES),
        );
```

   and add `&tunnels,` as the last argument of that `build_report_at`
   call.

8. `src/fixture_replay.rs`: in `Replayed::report`, add `&[],` as the last
   argument of `build_report_at` (Task 3 replaces it with the field). In
   the test `report_to_golden_json_strips_timestamp_and_pretty_prints_with_trailing_newline`,
   add `tunnels: Vec::new(),` after `buses: Vec::new(),` in the `Report`
   literal.

9. `src/headless/export.rs`: in the tests' `fn report() -> Report`, add
   `tunnels: Vec::new(),` after `buses: Vec::<BusReport>::new(),`.

10. `src/diag/support.rs`: in the test whose closure `report` calls
    `build_report_at(...)` (find it with `grep -n 'build_report_at(' src/diag/support.rs`),
    add `&[],` as the last argument.

- [ ] **Step 8: Run the non-corpus tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test --all-targets -- --skip fixture_corpus 2>&1 | tail -25`
Expected: every test passes, including the four new headless tests; the
text-pinning tests `render_text_ends_with_the_findings_and_chokepoints_sections`,
`render_text_lists_the_choke_points_after_the_findings` and
`render_text_says_chokepoints_none_when_there_are_none` still pass since
`tunnels: none` precedes `findings:`. The corpus tests are skipped here:
Step 9 re-blesses the goldens they compare against.

- [ ] **Step 9: Regenerate every golden**

The corpus compares each bundle's replay with its committed golden, so
the added `tunnels` key fails the corpus tests until the goldens carry
it. They move once, in this commit.

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test bless_seed_goldens -- --ignored 2>&1 | tail -5
for dir in tests/fixtures/hosts/*/*/; do
  [ -f "$dir/golden.binary.json" ] || continue
  name=${dir#tests/fixtures/hosts/}; name=${name%/}
  USBTOP_NG_BLESS_BUNDLE="$name" cargo test bless_named_bundle -- --ignored 2>&1 | grep -E 'test result|panicked' | head -2
done
```

Expected: every invocation reports `test result: ok`.

- [ ] **Step 10: Verify only the goldens moved, and only by the added key**

```bash
git status --short tests/fixtures | grep -v 'golden\.\(binary\|text\)\.json$' && echo "UNEXPECTED FIXTURE CHANGES ABOVE (a trace moved)" || echo "only goldens changed"
git status --short tests/fixtures | grep -c 'golden\.'   # expected: 36
for g in tests/fixtures/hosts/*/*/golden.*.json; do
  diff <(git show HEAD:"$g" | jq -S .) <(jq -S 'del(.tunnels)' "$g") >/dev/null \
    && jq -e '.tunnels == []' "$g" >/dev/null \
    && echo "OK   $g" || echo "DRIFT $g"
done | sort | uniq -c -w 5
```

Expected: `only goldens changed`, `36`, and 36 lines starting `OK`, no
`DRIFT`. A `DRIFT` line means a replay change beyond the added key; stop
and report it rather than committing.

- [ ] **Step 11: Run the corpus tests**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test fixture_corpus 2>&1 | tail -5
```

Expected: pass.

- [ ] **Step 12: Run the gate and commit**

Run the gate from Global Constraints (its `cargo test --all-targets` now
includes the corpus). Then:

```bash
git add src/pci/mod.rs src/test_tree.rs src/tunnel/mod.rs src/main.rs src/headless/mod.rs src/headless/export.rs src/fixture_replay.rs src/diag/support.rs tests/fixtures/hosts
git commit -F - <<'EOF'
feat(tunnel): read the PCIe side of every tunnel and carry it in the reports

`tunnel::read_tunnels` lists the removable, non-bridge PCI functions
under each root port with what sysfs says about them (class, driver,
IDs, interface, and the link, read only while awake), the depth-one
routers on the Thunderbolt bus, and names the router only when exactly
one that can carry PCIe faces exactly one tunnel, the router list is
unchanged across the PCI walk, and the walk is complete. A discrete
controller's own host interface and xHCI are excluded by the shape v6.13
excludes them by, so kernels up to 6.12 read the same. `headless::run`
reads the tunnels each window; the JSON gains a top-level `tunnels` list
and the text a `tunnels:` section before `findings:`; the schema stays
version 1. Fixture replay passes none, and every committed golden
(eighteen bundles, thirty-six files) is regenerated once for the added
empty key, a jq diff over each showing no other change.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t
EOF
```

---

### Task 3: The support bundle carries the live tunnels

**Files:**
- Modify: `src/tunnel/mod.rs` (`mask_mac_interfaces` and its test),
  `src/fixture_replay.rs` (`Replayed.tunnels`, `Replayed::report`,
  `replay_fixture_prepared`, a test), `src/diag/support.rs`
  (`Roots::live`, `run_support`, a test)

**Interfaces:**
- Consumes: `tunnel::{read_tunnels, Tunnel, PCI_DEVICES,
  THUNDERBOLT_DEVICES}` and `test_tree::PciTree` from Task 2.
- Produces: `pub fn tunnel::mask_mac_interfaces(tunnels: &mut [Tunnel])`;
  `fixture_replay::Replayed { ..., pub tunnels: Vec<Tunnel> }`.

- [ ] **Step 1: Write the failing tests**

In `src/tunnel/mod.rs`'s tests module add:

```rust
    #[test]
    fn mask_mac_interfaces_masks_only_names_that_embed_a_mac() {
        let function = |interface: Option<&str>| PciFunction {
            address: "0000:2d:00.1".into(),
            class: 0x020000,
            class_name: "Ethernet".into(),
            vendor_id: 0x1d6a,
            device_id: 0x14c0,
            driver: None,
            interface: interface.map(str::to_string),
            runtime_status: None,
            awake: false,
            link: None,
            max_gts: None,
        };
        let mut tunnels = vec![Tunnel {
            root_port: "0000:00:07.1".into(),
            router: None,
            functions: vec![
                function(Some("enx001122334455")),
                function(Some("wlxAABBCCDDEEFF")),
                function(Some("enp45s0")),
                function(Some("enx00112233445")),
                function(None),
            ],
        }];
        mask_mac_interfaces(&mut tunnels);
        let names: Vec<Option<&str>> = tunnels[0]
            .functions
            .iter()
            .map(|f| f.interface.as_deref())
            .collect();
        assert_eq!(
            names,
            [
                Some("enx<redacted>"),
                Some("wlx<redacted>"),
                Some("enp45s0"),
                Some("enx00112233445"),
                None
            ]
        );
    }
```

In `src/fixture_replay.rs`'s tests module add:

```rust
    #[test]
    fn a_replayed_state_reports_the_tunnels_it_holds() {
        let temp = tempfile::tempdir().unwrap();
        let manager = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&manager);
        let replayed = Replayed {
            manager,
            baseline,
            elapsed: Duration::from_secs(1),
            source: None,
            tunnels: vec![crate::tunnel::Tunnel {
                root_port: "0000:00:07.1".into(),
                router: None,
                functions: vec![crate::tunnel::PciFunction {
                    address: "0000:2d:00.1".into(),
                    class: 0x020000,
                    class_name: "Ethernet".into(),
                    vendor_id: 0x1d6a,
                    device_id: 0x14c0,
                    driver: None,
                    interface: None,
                    runtime_status: None,
                    awake: false,
                    link: None,
                    max_gts: None,
                }],
            }],
        };
        let report = replayed.report(Basis::Link);
        assert_eq!(report.tunnels.len(), 1, "the seam, not a constant");
        assert_eq!(report.tunnels[0].root_port, "0000:00:07.1");
        assert_eq!(report.tunnels[0].functions[0].class, "020000");
    }
```

In `src/diag/support.rs`'s tests module add, after
`run_support_without_capture_writes_a_consistent_static_bundle`:

```rust
    /// The bundle runs live: its `report.json` carries the tunnels read
    /// from the same PCI tree `inventory/pci-removable.toml` lists, with an
    /// interface name that embeds a MAC masked the way every other MAC in
    /// the bundle is.
    #[test]
    fn the_bundle_report_carries_the_live_tunnels_with_mac_names_masked() {
        let temp = tempfile::tempdir().unwrap();
        let mut roots = fake_roots(temp.path());
        let tree = crate::test_tree::PciTree::new();
        let nic = tree.device(
            &["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0", "0000:2d:00.1"],
            &[
                ("removable", "removable"),
                ("class", "0x020000"),
                ("vendor", "0x1d6a"),
                ("device", "0x14c0"),
                ("power/runtime_status", "suspended"),
            ],
        );
        tree.net(&nic, "enx001122334455");
        roots.pci = tree.pci();
        roots.thunderbolt = tree.thunderbolt();
        let target = temp.path().join("out");
        private_target(&target);
        let prepared = prepare_dir(&target, 1_788_000_000).unwrap();
        std::fs::write(prepared.dir.join("usbtop-ng.log"), "[INFO] starting usbtop-ng\n").unwrap();
        let env = environment(1000, Ok(status(false)));
        let opts = SupportOpts {
            window: Duration::from_secs(1),
            no_capture: true,
            command: vec!["usbtop-ng".into(), "--support".into(), "--no-capture".into()],
        };
        run_support(&opts, &roots, &env, &prepared, 1_788_000_000).unwrap();
        let dir = &prepared.dir;

        let report = std::fs::read_to_string(dir.join("report.json")).unwrap();
        let lines: Vec<&str> = report.lines().collect();
        let doc: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        let tunnels = doc["tunnels"].as_array().unwrap();
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0]["root_port"], "0000:00:07.1");
        assert_eq!(tunnels[0]["functions"][0]["address"], "0000:2d:00.1");
        assert_eq!(tunnels[0]["functions"][0]["interface"], "enx<redacted>");
        assert_eq!(tunnels[0]["functions"][0]["awake"], false);
        let inventory = std::fs::read_to_string(dir.join("inventory/pci-removable.toml")).unwrap();
        assert!(inventory.contains("0000:2d:00.1"), "{inventory}");
        assert!(!report.contains("001122334455"), "no MAC in the report");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && set -o pipefail && cargo test -- mask_mac_interfaces a_replayed_state the_bundle_report_carries 2>&1 | tail -20`
Expected: compile errors: `mask_mac_interfaces` not found; `Replayed` has
no field `tunnels`.

- [ ] **Step 3: Implement the seam and the mask**

In `src/tunnel/mod.rs`, before `#[cfg(test)]`:

```rust
/// The bundle's copy of the tunnels: an `interface` of the form `enx` or
/// `wlx` followed by twelve hex digits embeds a MAC (systemd's
/// `NamePolicy=mac`), so it is masked as the bundle masks every other MAC,
/// keeping the three-letter prefix.
pub fn mask_mac_interfaces(tunnels: &mut [Tunnel]) {
    for function in tunnels.iter_mut().flat_map(|t| t.functions.iter_mut()) {
        if let Some(name) = &function.interface {
            if embeds_mac(name) {
                function.interface = Some(format!("{}<redacted>", &name[..3]));
            }
        }
    }
}

fn embeds_mac(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("enx")
        .or_else(|| name.strip_prefix("wlx"))
    else {
        return false;
    };
    rest.len() == 12 && rest.chars().all(|c| c.is_ascii_hexdigit())
}
```

In `src/fixture_replay.rs`:

1. Add `use crate::tunnel::Tunnel;` to the `use` lines.
2. In `pub struct Replayed`, add the field
   `/// The PCI side of the tunnels the report carries: what \`--support\`
   read live, nothing in a fixture replay.` then `pub tunnels: Vec<Tunnel>,`.
3. In `Replayed::report`, replace the `&[],` argument with
   `&self.tunnels,`.
4. In `replay_fixture_prepared`'s final `Ok(Replayed { ... })`, add
   `tunnels: Vec::new(),`.

In `src/diag/support.rs`:

1. In `Roots::live()`, replace
   `thunderbolt: PathBuf::from("/sys/bus/thunderbolt/devices"),` with
   `thunderbolt: PathBuf::from(crate::tunnel::THUNDERBOLT_DEVICES),` and
   `pci: PathBuf::from("/sys/bus/pci/devices"),` with
   `pci: PathBuf::from(crate::tunnel::PCI_DEVICES),`.
2. In `run_support`, change `Ok(replayed) => {` (the arm of `match
   replay_fixture_prepared(&fixture_base, source, opts.window)`) to
   `Ok(mut replayed) => {` and insert as its first statements:

```rust
                // The bundle runs live, so its report carries the PCI side
                // of every tunnel, read from the same roots as
                // inventory/pci-removable.toml; a name that embeds a MAC
                // is masked like every other MAC in the bundle.
                let mut tunnels = crate::tunnel::read_tunnels(&roots.pci, &roots.thunderbolt);
                crate::tunnel::mask_mac_interfaces(&mut tunnels);
                replayed.tunnels = tunnels;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test --all-targets 2>&1 | tail -20`
Expected: all pass, the three new ones included.

- [ ] **Step 5: Run the gate and commit**

Run the gate from Global Constraints. Then:

```bash
git add src/tunnel/mod.rs src/fixture_replay.rs src/diag/support.rs
git commit -F - <<'EOF'
feat(support): the bundle's report carries the tunnels read live, MAC-bearing interface names masked

`Replayed` gains a `tunnels` field that its report passes through: a
fixture replay leaves it empty, `--support` fills it from the same PCI and
Thunderbolt roots its `inventory/pci-removable.toml` walks, so the two
files agree. An interface named after its MAC (`enx`/`wlx` and twelve hex
digits) is masked as the bundle masks every other MAC. The live sysfs
paths now come from one place.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t
EOF
```

---

### Task 4: The TUI

**Files:**
- Modify: `src/tunnel/mod.rs` (`Tunnel::label`, `PciFunction::link_text`,
  `PciFunction::row_text`), `src/ui/mod.rs` (`ControllerView`,
  `UsbTopApp`, `sync_from`, `device_keys`, the retention passes,
  `prune_empty_groups`, `device_list_lines_with_selection`, `help_lines`,
  tests), `src/tui/mod.rs` (`run_app`)

**Interfaces:**
- Consumes: `tunnel::{Tunnel, PciFunction, PciLink, Router,
  read_tunnels, router_label, link_text, NO_ROUTER_LABEL, PCI_DEVICES,
  THUNDERBOLT_DEVICES}` from Task 2.
- Produces: `pub fn Tunnel::label(&self) -> String`,
  `pub fn PciFunction::link_text(&self) -> String`,
  `pub fn PciFunction::row_text(&self) -> String`;
  `pub fn UsbTopApp::set_tunnels(&mut self, tunnels: Vec<Tunnel>)`;
  `ui::ControllerView { ..., pub tunnel: Option<TunnelView>, pub pcie:
  Vec<PcieRow> }`, `ui::TunnelView { pub label: String }`,
  `ui::PcieRow { pub address: String, pub text: String, pub suspended: bool }`.

- [ ] **Step 1: Write the failing tunnel text tests**

In `src/tunnel/mod.rs`'s tests module add:

```rust
    #[test]
    fn a_tunnel_labels_itself_and_a_function_prints_its_row() {
        let nic = PciFunction {
            address: "0000:2d:00.1".into(),
            class: 0x020000,
            class_name: "Ethernet".into(),
            vendor_id: 0x1d6a,
            device_id: 0x14c0,
            driver: Some("atlantic".into()),
            interface: Some("enp45s0".into()),
            runtime_status: Some("active".into()),
            awake: true,
            link: Some(PciLink { gts: 8.0, width: 1 }),
            max_gts: Some(16.0),
        };
        assert_eq!(nic.link_text(), "8 GT/s ×1");
        assert_eq!(
            nic.row_text(),
            "PCIe 0000:2d:00.1 · Ethernet · 8 GT/s ×1 · atlantic · enp45s0 · 1d6a:14c0"
        );
        let asleep = PciFunction {
            runtime_status: Some("suspended".into()),
            awake: false,
            link: None,
            max_gts: None,
            ..nic.clone()
        };
        assert_eq!(
            asleep.row_text(),
            "PCIe 0000:2d:00.1 · Ethernet · asleep · atlantic · enp45s0 · 1d6a:14c0"
        );
        let bare_xhci = PciFunction {
            address: "0000:2e:00.0".into(),
            class: 0x0c0330,
            class_name: "USB controller".into(),
            vendor_id: 0x8086,
            device_id: 0x15ec,
            driver: None,
            interface: None,
            ..asleep.clone()
        };
        assert_eq!(
            bare_xhci.row_text(),
            "PCIe 0000:2e:00.0 · USB controller · asleep · no driver · 8086:15ec"
        );
        let mut tunnel = Tunnel {
            root_port: "0000:00:07.1".into(),
            router: Some(Router {
                name: "0-3".into(),
                vendor_name: Some("CalDigit, Inc.".into()),
                device_name: Some("Element Hub".into()),
                generation: Some(4),
                rx_gbps: Some(20.0),
                tx_gbps: Some(20.0),
                rx_lanes: Some(2),
                tx_lanes: Some(2),
                authorized: Some(1),
                security: Some("none".into()),
            }),
            functions: vec![nic],
        };
        assert_eq!(tunnel.label(), "Thunderbolt 0-3 Element Hub · 2×20 Gb/s");
        tunnel.router = None;
        assert_eq!(tunnel.label(), "external PCIe port");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test a_tunnel_labels_itself 2>&1 | tail -8`
Expected: compile errors: no method `link_text`, `row_text`, `label`.

- [ ] **Step 3: Add the three methods**

In `src/tunnel/mod.rs`, extend `impl PciFunction` with:

```rust
    /// The link as the row prints it (see [`link_text`]).
    pub fn link_text(&self) -> String {
        link_text(
            self.link.as_ref().map(|l| l.gts),
            self.link.as_ref().map(|l| l.width),
            self.runtime_status.as_deref(),
        )
    }

    /// The TUI row: `PCIe {address} · {class} · {link} · {driver|no
    /// driver} · {interface}? · {vvvv}:{dddd}`. The link comes before the
    /// identity so the 80-column floor clips the IDs, never the link.
    pub fn row_text(&self) -> String {
        let mut parts = vec![
            format!("PCIe {}", self.address),
            self.class_name.clone(),
            self.link_text(),
            self.driver.clone().unwrap_or_else(|| "no driver".to_string()),
        ];
        if let Some(interface) = &self.interface {
            parts.push(interface.clone());
        }
        parts.push(format!("{:04x}:{:04x}", self.vendor_id, self.device_id));
        parts.join(" · ")
    }
```

and add after `pub struct Tunnel { ... }`:

```rust
impl Tunnel {
    /// The heading's label: the joined router (see [`router_label`]) or
    /// [`NO_ROUTER_LABEL`].
    pub fn label(&self) -> String {
        match &self.router {
            Some(r) => router_label(&r.name, r.device_name.as_deref(), r.rx_lanes, r.rx_gbps),
            None => NO_ROUTER_LABEL.to_string(),
        }
    }
}
```

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test a_tunnel_labels_itself 2>&1 | tail -5`
Expected: PASS. (Clippy reports the three as unused until Step 6; expected mid-task.)

- [ ] **Step 4: Write the failing UI tests**

In `src/ui/mod.rs`'s tests module, add after
`device_list_renders_headings_above_port_ordered_rows`:

```rust
    fn sample_router() -> crate::tunnel::Router {
        crate::tunnel::Router {
            name: "0-3".into(),
            vendor_name: Some("CalDigit, Inc.".into()),
            device_name: Some("Element Hub".into()),
            generation: Some(4),
            rx_gbps: Some(20.0),
            tx_gbps: Some(20.0),
            rx_lanes: Some(2),
            tx_lanes: Some(2),
            authorized: Some(1),
            security: Some("none".into()),
        }
    }

    /// A NIC at `address` with `interface`, awake with an 8 GT/s x1 link
    /// or suspended with none.
    fn nic(address: &str, interface: &str, awake: bool) -> crate::tunnel::PciFunction {
        crate::tunnel::PciFunction {
            address: address.into(),
            class: 0x020000,
            class_name: "Ethernet".into(),
            vendor_id: 0x1d6a,
            device_id: 0x14c0,
            driver: Some("atlantic".into()),
            interface: Some(interface.into()),
            runtime_status: Some(if awake { "active" } else { "suspended" }.into()),
            awake,
            link: awake.then_some(crate::tunnel::PciLink { gts: 8.0, width: 1 }),
            max_gts: awake.then_some(16.0),
        }
    }

    /// The dock's xHCI as a function: awake at 2.5 GT/s x4, or asleep with
    /// no driver when `driver` is false.
    fn xhci_function(driver: bool) -> crate::tunnel::PciFunction {
        crate::tunnel::PciFunction {
            address: "0000:2e:00.0".into(),
            class: 0x0c0330,
            class_name: "USB controller".into(),
            vendor_id: 0x8086,
            device_id: 0x15ec,
            driver: driver.then(|| "xhci_hcd".to_string()),
            interface: None,
            runtime_status: Some(if driver { "active" } else { "suspended" }.into()),
            awake: driver,
            link: driver.then_some(crate::tunnel::PciLink { gts: 2.5, width: 4 }),
            max_gts: driver.then_some(2.5),
        }
    }

    fn tunnel(router: bool, functions: Vec<crate::tunnel::PciFunction>) -> crate::tunnel::Tunnel {
        crate::tunnel::Tunnel {
            root_port: "0000:00:07.1".into(),
            router: router.then(sample_router),
            functions,
        }
    }

    #[test]
    fn a_tunneled_controllers_buses_land_under_the_root_port_heading_with_the_router_label() {
        let (_t, mut mgr) = topology_fixture_named("0000:2e:00.0");
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.set_tunnels(vec![tunnel(true, vec![xhci_function(true)])]);
        app.sync_from(&mgr);
        assert_eq!(app.controllers.len(), 1);
        assert_eq!(app.controllers[0].id, "0000:00:07.1");
        assert_eq!(
            app.controllers[0].tunnel.as_ref().map(|t| t.label.as_str()),
            Some("Thunderbolt 0-3 Element Hub · 2×20 Gb/s")
        );
        let text = list_text(&app);
        assert!(
            text.contains("═ 0000:00:07.1 · Thunderbolt 0-3 Element Hub · 2×20 Gb/s ═"),
            "{text}"
        );
        assert!(text.contains("▶ Bus 03"), "the buses moved with their controller: {text}");
        assert!(!text.contains("▶ PCIe"), "the xHCI has buses, so it is no row: {text}");
    }

    #[test]
    fn a_bare_tunnel_renders_its_pcie_rows_and_survives_pruning() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.set_tunnels(vec![tunnel(false, vec![nic("0000:2d:00.1", "enp45s0", true)])]);
        app.sync_from(&mgr);
        assert_eq!(app.controllers.len(), 1);
        let text = list_text(&app);
        assert!(text.contains("═ 0000:00:07.1 · external PCIe port ═"), "{text}");
        assert!(
            text.contains("▶ PCIe 0000:2d:00.1 · Ethernet · 8 GT/s ×1 · atlantic · enp45s0 · 1d6a:14c0"),
            "{text}"
        );
        assert_eq!(app.device_keys(), ["pcie:0000:2d:00.1"].map(String::from));
    }

    #[test]
    fn pcie_rows_fit_the_eighty_column_floor_with_the_link_intact() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.set_tunnels(vec![tunnel(
            true,
            vec![
                nic("0000:2d:00.1", "enp45s0", true),
                nic("0000:2d:00.2", "enp45s1", false),
                xhci_function(false),
            ],
        )]);
        app.sync_from(&mgr);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_device_list(f, area, &mut app);
            })
            .unwrap();
        let screen = terminal.backend().to_string();
        for row in [
            "▶ PCIe 0000:2d:00.1 · Ethernet · 8 GT/s ×1 · atlantic · enp45s0 · 1d6a:14c0",
            "▶ PCIe 0000:2d:00.2 · Ethernet · asleep · atlantic · enp45s1 · 1d6a:14c0",
            "▶ PCIe 0000:2e:00.0 · USB controller · asleep · no driver · 8086:15ec",
        ] {
            assert!(screen.contains(row), "{row}\n{screen}");
        }
    }

    #[test]
    fn down_from_the_last_device_row_selects_the_pcie_row_and_scrolls_to_it() {
        let (_t, mut mgr) = topology_fixture_named("0000:2e:00.0");
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.set_tunnels(vec![tunnel(
            true,
            vec![xhci_function(true), nic("0000:2d:00.1", "enp45s0", true)],
        )]);
        app.sync_from(&mgr);
        let keys = app.device_keys();
        assert_eq!(keys.last().map(String::as_str), Some("pcie:0000:2d:00.1"));
        for _ in 0..keys.len() {
            app.select_next_device();
        }
        assert_eq!(app.selected_device.as_deref(), Some("pcie:0000:2d:00.1"));
        assert_eq!(app.selected_row_trailing_lines(), 0);
        // A window too short for the list: the scroll follows the selection.
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 6)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_device_list(f, area, &mut app);
            })
            .unwrap();
        let screen = terminal.backend().to_string();
        assert!(screen.contains("▶ PCIe 0000:2d:00.1"), "{screen}");
        assert!(app.list_scroll > 0, "the list scrolled down to the row");
        // Wrapping past the last row lands on the first device again.
        app.select_next_device();
        assert_eq!(app.selected_device.as_deref(), keys.first().map(String::as_str));
        // The row went away with its tunnel: the selection is cleared.
        app.selected_device = Some("pcie:0000:2d:00.1".into());
        app.set_tunnels(Vec::new());
        app.sync_from(&mgr);
        assert_eq!(app.selected_device, None);
    }

    #[test]
    fn search_idle_and_filter_apply_to_pcie_rows() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let rows = || {
            vec![tunnel(
                false,
                vec![
                    nic("0000:2d:00.1", "enp45s0", true),
                    nic("0000:2d:00.2", "enp45s1", false),
                ],
            )]
        };

        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.set_tunnels(rows());
        app.search = SearchState::Committed("enp45s1".into());
        app.sync_from(&mgr);
        let text = list_text(&app);
        assert!(text.contains("0000:2d:00.2") && !text.contains("0000:2d:00.1"), "{text}");

        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.set_tunnels(rows());
        app.hide_idle_devices = true;
        app.sync_from(&mgr);
        let text = list_text(&app);
        assert!(
            text.contains("0000:2d:00.1") && !text.contains("0000:2d:00.2"),
            "a suspended function is idle: {text}"
        );

        let mut app = UsbTopApp::new(Duration::from_millis(100))
            .with_filter(FilterSet::parse(&["bus=1".into()]).unwrap());
        app.set_tunnels(rows());
        app.sync_from(&mgr);
        assert!(
            app.controllers.is_empty(),
            "a USB filter hides every PCIe row, and the bare group with them"
        );
    }

    #[test]
    fn the_help_explains_pcie_rows_within_the_floor() {
        let line = "PCIe rows: tunneled devices usbmon never sees; their link, not traffic";
        assert!(help_lines().iter().any(|l| l.to_string().contains(line)));
    }
```

- [ ] **Step 5: Run them to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && set -o pipefail && cargo test -- ui::tests::a_tunneled ui::tests::a_bare ui::tests::pcie_rows ui::tests::down_from ui::tests::search_idle ui::tests::the_help_explains 2>&1 | tail -12`
Expected: compile errors: no method `set_tunnels`, no field `tunnel`.

- [ ] **Step 6: Implement the model, the filters and the rendering**

In `src/ui/mod.rs`:

1. Add `use crate::tunnel::Tunnel;` to the `use` lines (the UI calls
   methods only; no `tunnel::` path).

2. Replace `pub struct ControllerView { ... }` with:

```rust
/// One host controller, or one tunnel's root port: its bus lines in bus
/// order, then its connectors in chain order, then the tunneled PCIe
/// functions no bus represents.
pub struct ControllerView {
    pub id: String,
    pub buses: Vec<BusView>,
    pub connectors: Vec<ConnectorView>,
    /// Set when `id` is a tunnel's root port: the heading's label.
    pub tunnel: Option<TunnelView>,
    /// The tunnel's functions that no bus names as its controller, in
    /// address order (see `tunnel::PciFunction::row_text`).
    pub pcie: Vec<PcieRow>,
}

/// The heading of a tunnel group (see `tunnel::Tunnel::label`).
pub struct TunnelView {
    pub label: String,
}

/// One tunneled PCIe function as a row: selectable by `pcie:{address}`,
/// rendered as `▶ {text}`, hidden by the idle filter when suspended.
pub struct PcieRow {
    pub address: String,
    pub text: String,
    pub suspended: bool,
}
```

3. In `pub struct UsbTopApp`, add after `pub chokepoints: Vec<Chokepoint>,`:

```rust
    /// The PCI side of every tunnel, set by the loop before each tick's
    /// `sync_from` (see [`Self::set_tunnels`]); tests set it directly.
    pub tunnels: Vec<Tunnel>,
```

   and in `UsbTopApp::new`'s literal add `tunnels: Vec::new(),` after
   `chokepoints: Vec::new(),`. Add to `impl UsbTopApp`, next to
   `with_filter`:

```rust
    /// The tunnels the next `sync_from` groups by.
    pub fn set_tunnels(&mut self, tunnels: Vec<Tunnel>) {
        self.tunnels = tunnels;
    }
```

4. In `sync_from`, directly before `let mut buses: Vec<&UsbBus> = ...`:

```rust
        // A bus whose controller is a tunneled function belongs to the
        // tunnel's root port, not to its own controller id.
        let root_port_of: HashMap<&str, &str> = self
            .tunnels
            .iter()
            .flat_map(|t| {
                t.functions
                    .iter()
                    .map(move |f| (f.address.as_str(), t.root_port.as_str()))
            })
            .collect();
```

   Replace the three lines
   `let controller = bus\n                .controller\n                .clone()\n                .unwrap_or_else(|| UNKNOWN_CONTROLLER.to_string());`
   with:

```rust
            let controller = bus
                .controller
                .as_deref()
                .map(|c| root_port_of.get(c).copied().unwrap_or(c).to_string())
                .unwrap_or_else(|| UNKNOWN_CONTROLLER.to_string());
```

   In the `or_insert_with(|| ControllerView { ... })` literal add
   `tunnel: None,` and `pcie: Vec::new(),` after `connectors: Vec::new(),`.

   Directly after the `for view in grouped.values_mut() { ... }` block
   (the connector sorting) and before `// Named controllers first`, add:

```rust
        // Every tunnel gets a group even when no bus lands in it; its rows
        // are the functions no bus names as its controller.
        for tunnel in &self.tunnels {
            let view = grouped
                .entry(tunnel.root_port.clone())
                .or_insert_with(|| ControllerView {
                    id: tunnel.root_port.clone(),
                    buses: Vec::new(),
                    connectors: Vec::new(),
                    tunnel: None,
                    pcie: Vec::new(),
                });
            view.tunnel = Some(TunnelView {
                label: tunnel.label(),
            });
            view.pcie = tunnel
                .functions
                .iter()
                .filter(|f| {
                    !manager
                        .buses
                        .values()
                        .any(|bus| bus.controller.as_deref() == Some(f.address.as_str()))
                })
                .map(|f| PcieRow {
                    address: f.address.clone(),
                    text: f.row_text(),
                    suspended: f.runtime_status.as_deref() == Some("suspended"),
                })
                .collect();
        }
```

5. Replace `fn device_keys` with:

```rust
    /// Device keys (`bus:dev`) and PCIe row keys (`pcie:{address}`)
    /// flattened in render order: each group's device rows, then its
    /// PCIe rows.
    fn device_keys(&self) -> Vec<String> {
        self.controllers
            .iter()
            .flat_map(|controller| {
                controller
                    .rows()
                    .map(|row| format!("{}:{}", row.device.bus_id, row.device.device_id))
                    .chain(
                        controller
                            .pcie
                            .iter()
                            .map(|row| format!("pcie:{}", row.address)),
                    )
            })
            .collect()
    }
```

6. Add after `fn retain_rows(...) { ... }`:

```rust
    /// Keep only the PCIe rows `keep` accepts, in every group.
    fn retain_pcie(&mut self, keep: impl Fn(&PcieRow) -> bool) {
        for controller in &mut self.controllers {
            controller.pcie.retain(|row| keep(row));
        }
    }
```

   In `retain_active_devices`, add `self.retain_pcie(|row| !row.suspended);`
   after the `retain_rows` call and extend its doc: `A suspended PCIe
   function is idle by the kernel's own measure.`. In
   `retain_filtered_devices`, add `self.retain_pcie(|_| false);` after the
   `retain_rows` call and extend its doc: `A USB filter hides every PCIe
   row.`. In `retain_searched_devices`, add after the `retain_rows` call:
   `self.retain_pcie(|row| row.text.to_lowercase().contains(&query));`.

7. In `prune_empty_groups`, replace the final
   `self.controllers.retain(|c| !c.buses.is_empty() || !c.connectors.is_empty());`
   with
   `self.controllers.retain(|c| !c.buses.is_empty() || !c.connectors.is_empty() || !c.pcie.is_empty());`
   and add to the comment above it: `A tunnel group with a surviving PCIe
   row stays too.`

8. In `device_list_lines_with_selection`, replace
   `lines.push(Line::styled(format!("═ {} ═", controller.id), heading_style,));`
   with:

```rust
        let heading = match &controller.tunnel {
            Some(tunnel) => format!("═ {} · {} ═", controller.id, tunnel.label),
            None => format!("═ {} ═", controller.id),
        };
        lines.push(Line::styled(heading, heading_style));
```

   and, after the `for connector in &controller.connectors { ... }` loop
   inside the controller loop, add:

```rust
        for row in &controller.pcie {
            let key = format!("pcie:{}", row.address);
            let is_selected = app.selected_device.as_ref() == Some(&key);
            if is_selected {
                selected_line = Some(lines.len());
            }
            let line = Line::from(format!("▶ {}", row.text));
            lines.push(if is_selected {
                line.style(Style::default().bg(ACCENT_COLOR).fg(Color::Black))
            } else {
                line
            });
        }
```

   Update the function's doc comment: `... then per connector a heading and its
   device rows, then the group's PCIe rows` and `the selected device's or
   PCIe row's line`.

9. In `help_lines`, after the line
   `Line::from("  • 🔺 linked below the speed it supports; the line beneath says why"),`
   add
   `Line::from("  • PCIe rows: tunneled devices usbmon never sees; their link, not traffic"),`.

In `src/tui/mod.rs`:

1. Add `use std::path::Path;` and `use crate::tunnel;` to the `use` lines.
2. In `run_app`, after `let mut packet_backlog = false;`, add:

```rust
    // The PCI side of every tunnel, read at most once a second whatever
    // `--refresh` says: the reader walks every PCI device and opens the
    // link attributes of the awake tunneled ones.
    let pci_devices = Path::new(tunnel::PCI_DEVICES);
    let thunderbolt_devices = Path::new(tunnel::THUNDERBOLT_DEVICES);
    let mut last_tunnel_read: Option<Instant> = None;
```

3. In the `if now >= next_tick {` branch, after `let _ = manager.refresh();`
   and before `app.sync_from(manager);`, add:

```rust
            if last_tunnel_read.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1)) {
                app.set_tunnels(tunnel::read_tunnels(pci_devices, thunderbolt_devices));
                last_tunnel_read = Some(now);
            }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test --all-targets 2>&1 | tail -20`
Expected: all pass, the seven new ones included, and
`every_help_line_fits_the_overlay_at_the_floor` still passes.

- [ ] **Step 8: Run the full gate and commit**

Run the gate from Global Constraints plus:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --features capture-fixture 2>&1 | tail -3
cargo test --features integration 2>&1 | tail -3
cargo +1.88.0 check --all-targets 2>&1 | tail -2
bash evals/run.sh 2>&1 | tail -4
```

Expected: all green, `ALL EVALS PASS`. Then:

```bash
git add src/tunnel/mod.rs src/ui/mod.rs src/tui/mod.rs
git commit -F - <<'EOF'
feat(ui): tunneled PCIe devices in the device table

A tunneled xHCI's buses group under the tunnel's root port, headed
`═ 0000:00:07.1 · Thunderbolt 0-3 Element Hub · 2×20 Gb/s ═` when the
join names the router and `external PCIe port` otherwise; every tunnel
gets a group, and each function no bus represents is a row, `▶ PCIe
0000:2d:00.1 · Ethernet · 8 GT/s ×1 · atlantic · enp45s0 · 1d6a:14c0`,
the link before the identity so the 80-column floor never clips it. The
rows are selectable (the list scrolls only to follow the selection) with
no trailing lines, the idle filter hides a suspended function, the search
matches the row text, and a USB filter hides them. The loop reads the
tunnels at most once a second whatever `--refresh` says. The help gains
one line.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t
EOF
```

---

### Task 5: Docs, changelog, roadmap

**Files:**
- Modify: `README.md`, `docs/ARCHITECTURE.md`, `docs/SCRIPTING.md`,
  `docs/TESTING.md`, `CHANGELOG.md`, `docs/ROADMAP.md`

**Interfaces:** none; prose only. Every claim below is what Tasks 1 to 4
built; do not describe anything else.

- [ ] **Step 1: README**

In `### The device table`, add two bullets after the connector-heading
bullet (the one ending `Port 1 (USB3 side: 2)`.`):

```markdown
- A Thunderbolt or USB4 tunnel gets a group of its own, headed by the PCI
  root port it lands on and, when exactly one router faces exactly one
  tunnel, the router's name and link:
  `═ 0000:00:07.1 · Thunderbolt 0-3 Element Hub · 2×20 Gb/s ═`. The dock's
  own USB controller is tunneled too, so its buses sit under that heading
  rather than under their controller's address. With two docks attached,
  or a router that cannot carry PCIe, the heading says `external PCIe
  port` and names no router: usbtop-ng never guesses the pairing.
- A tunneled PCIe device that is not a USB controller, a NIC or an NVMe
  drive behind a USB4 adapter, is a row under that heading:
  `▶ PCIe 0000:2d:00.1 · Ethernet · 8 GT/s ×1 · atlantic · enp45s0 · 1d6a:14c0`,
  its address, class, negotiated link (`asleep` for a function in runtime
  suspend, whose link usbtop-ng does not wake it to read), driver,
  network interface and IDs. usbmon never sees such a device, so the row
  carries its link, not traffic. `↑`/`↓` select it like any row; the idle
  filter hides a suspended one; a `--filter` hides them all.
```

In `### Scriptable output`, extend the `--json` bullet with one sentence:
`The document also carries a top-level `tunnels` list, the PCIe side of
every Thunderbolt or USB4 tunnel, and the text report a `tunnels:`
section.`

- [ ] **Step 2: ARCHITECTURE**

After the `#### 4c. Capacity (`capacity/`)` section (before
`#### 5. TUI chassis`), add:

```markdown
#### 4d. Tunnels (`tunnel/`, `pci/`)

- `tunnel/mod.rs`: `read_tunnels(pci, thunderbolt) -> Vec<Tunnel>`, the
  PCIe side of every Thunderbolt or USB4 tunnel: the removable, non-bridge
  PCI functions grouped under their root port (class from a built-in table,
  driver, IDs, `net/` interface, and the link read only while the function
  is awake), the depth-one routers on the Thunderbolt bus, and the join
  that names a router only when exactly one that can carry PCIe faces
  exactly one tunnel, the router list is unchanged across the PCI walk and
  the walk is complete. A discrete Thunderbolt controller's own host
  interface and xHCI are excluded by the shape v6.13's
  `arch_pci_dev_is_removable` excludes them by, so kernels that mark
  everything below an external-facing port read the same. The TUI reads
  it once a second, `--once`/`--batch` once a window, `--support` once a
  run. The rules and citations are in
  `docs/superpowers/specs/2026-09-15-tunneled-pcie-rows-design.md`.
- `pci/mod.rs`: what that reader and `diag/inventory.rs` share, the PCI
  address shape, the parent chain and the wake gate with its runtime-PM
  citation.
```

In the `### User interface` section's `UsbTopApp` sketch, add a line
`    pub tunnels: Vec<Tunnel>,            // the PCI side of every tunnel, read once a second`
after the `controllers` line. In the `diag/collect.rs and
diag/inventory.rs` bullet of section 7, append the sentence: `The PCI
walk shares its address check, parent chain and wake gate with the
tunneled-device reader through `pci/`.`

- [ ] **Step 3: SCRIPTING**

In the top-level field table, add after the `buses` row:

```markdown
| `tunnels` | array | the PCIe side of every Thunderbolt or USB4 tunnel, one entry per root port with tunneled functions under it, sorted by root port; empty when there is none, and always empty in a fixture replay. See [The tunnels list](#the-tunnels-list) |
```

In the example document, add `"tunnels": [],` on its own line between the
closing `],` of `"buses"` and `"findings": [`.

Add a section before `## The chokepoints list`:

```markdown
## The tunnels list

`tunnels` is the top-level list of Thunderbolt or USB4 tunnels seen from
the PCI side: every PCI function the kernel marks `removable` (behind a
port the firmware flags as externally facing; Linux 5.16 and later, and
from 6.13 only behind a tunnel proper) that is not a bridge and not a
discrete Thunderbolt controller's own, grouped under the root port it
hangs from. The dock's own USB controller is one such function, which is
how a bus joins its tunnel: `tunnels[].controllers` holds the addresses
among the functions that are USB controllers, and `buses[].controller`
names one of them.

`tunnels[]`, one entry per root port:

| Field | Type | Meaning |
| --- | --- | --- |
| `root_port` | string | the PCI root port, `0000:00:07.1` |
| `router` | object or null | the Thunderbolt router the tunnel runs through, only when exactly one depth-one router that can carry PCIe faces exactly one tunnel; null otherwise, never a guess |
| `controllers` | array | the addresses among `functions` that are USB controllers |
| `functions` | array | the tunneled functions, sorted by address |

`tunnels[].router`:

| Field | Type | Meaning |
| --- | --- | --- |
| `name` | string | the bus name, `0-3` (domain, then the route) |
| `vendor_name`, `device_name` | string or null | from the device's DROM |
| `generation` | u32 or null | 3 for Thunderbolt 3, 4 for USB4 |
| `rx_gbps`, `tx_gbps` | f64 or null | the rate per lane, 20 for a 40 Gb/s link |
| `rx_lanes`, `tx_lanes` | u32 or null | 1, 2 or 3 (the wider side of an asymmetric link) |
| `authorized` | u32 or null | the router's `authorized`; 0 means no PCIe devices reach the host |
| `security` | string or null | the domain's security level: `none`, `user`, `secure`, `dponly`, `usbonly`, `nopcie` |

`tunnels[].functions[]`:

| Field | Type | Meaning |
| --- | --- | --- |
| `address` | string | `0000:2d:00.1` |
| `class` | string | the 24-bit class code, six hex digits, `020000` |
| `class_name` | string | a short name for the class, `Ethernet`, `NVMe`, `USB controller`, or `class 0x......` when the table has none |
| `vendor_id`, `device_id` | string | four hex digits |
| `driver` | string or null | the bound driver |
| `interface` | string or null | the network interface when the function has exactly one |
| `runtime_status` | string or null | the kernel's `power/runtime_status`, `active` or `suspended` |
| `awake` | bool | `runtime_status` was `active`, so the link was read |
| `link_gts`, `link_width` | f64, u32 or null | the negotiated link, `8` and `1` for 8 GT/s x1; null when the function was not awake or the link was down |
| `max_link_gts` | f64 or null | the fastest rate the function supports, read only when awake |

usbtop-ng never wakes a function to read its link: a device in runtime
suspend reports `awake: false` and null link fields. `--filter` does not
narrow `tunnels`; it is topology, not a device the filter names.

```bash
sudo usbtop-ng --once --json | jq -c '.tunnels[] | [.root_port, .router.name, (.functions | map(.address))]'
```
```

The text report prints, after the last bus and before `findings:`,
`tunnels: none` or `tunnels: N` with one line per tunnel (root port,
label, `controllers a, b` or `controllers none`) and one indented line per
function (address, class name, link, driver, interface or `-`, IDs); add
that sentence to the `--once` section's description of the text report
where it lists `findings:` then `chokepoints:`.

- [ ] **Step 4: TESTING**

In the test-host table, extend the `tgl-tb4` row's last cell with: `; a
second chain on its other Thunderbolt port since 2026-09-26: a Dell WD22TB4
dock (router 0-1, its xHCI 0000:03:00.0 as buses 7 and 8, a Realtek USB
NIC), an XYJ-LINK Thunderbolt 3 to PCIe bridge (router 0-301, Alpine Ridge
switch, a corrupt DROM so the kernel names no device) and an ASMedia
ASM2464PD NVMe enclosure (a Samsung NVMe at 0000:13:00.0, the live
non-USB PCIe row); the chain dropped seventy seconds after plug-in on its
first attachment, so survey before use`.

In the `### On hand` table add:

```markdown
| Dell WD22TB4 Thunderbolt 4 dock | Intel 8086:0b27 xHCI, Realtek 0bda:8153 NIC, Dell 413c:b06e | tunneled xHCI, 480 + 10000 | a second tunnel (the join goes silent, by design), a tunneled USB NIC |
| XYJ-LINK Thunderbolt 3 to PCIe bridge | Intel 8086:15da switch | tunneled, PCIe slot | a depth-two router, a corrupt DROM (no `device_name`), a tunnel of bridges only |
| ASMedia ASM2464PD NVMe enclosure | 1b21:2463 switch, Samsung 144d:a802 NVMe | tunneled, 2.5 GT/s x1 uplink | the live non-USB PCIe row, NVMe class 010802 |
```

Add a paragraph at the end of `### On hand`:

```markdown
The tunneled PCIe row is proven by synthetic trees (`tunnel::tests`); the
corpus carries no PCI tree, so every bundle replays to `tunnels: []`. On
the dock laptop the CalDigit hub gives the heading over buses 5 and 6 and,
when the second chain is up, the NVMe enclosure gives a real `PCIe` row.
Coverage: nothing is `removable` before Linux 5.16, nor on any
architecture but x86 with ACPI from 6.13, so the row is silent there; a
USB4 dock's USB 3 runs to the host's own xHCI over a USB 3 tunnel, so its
devices stay in the host controller's group and no heading appears for
that link.
```

- [ ] **Step 5: CHANGELOG and ROADMAP**

In `CHANGELOG.md` under `## [Unreleased]` / `### Added`, add a bullet:

```markdown
- Tunneled PCIe devices in the device table: a Thunderbolt or USB4 tunnel is a group headed by its PCI root port and, when exactly one router that can carry PCIe faces exactly one tunnel, the router's name and link (`═ 0000:00:07.1 · Thunderbolt 0-3 Element Hub · 2×20 Gb/s ═`; `external PCIe port` otherwise, never a guess); a tunneled USB controller's buses sit under it, and every tunneled function no bus represents, a NIC or an NVMe drive behind a USB4 adapter, is a selectable row carrying its class, negotiated link, driver, interface and IDs (`asleep` for a function in runtime suspend, which usbtop-ng never wakes to read). The JSON report gains a top-level `tunnels` list and the text report a `tunnels:` section; the support bundle's `report.json` carries the tunnels read live, an interface name that embeds a MAC masked; the schema stays version 1 and every committed golden was regenerated for the added key. A discrete Thunderbolt controller's own host interface and xHCI are excluded by the shape Linux 6.13 excludes them by, so kernels up to 6.12, which mark everything below an external-facing port removable, read the same. See the README's device table and [docs/SCRIPTING.md](docs/SCRIPTING.md#the-tunnels-list).
```

In `docs/ROADMAP.md`, replace the tunneled-PCIe bullet (from `- A
tunneled PCIe device that is not a USB controller` through `a `tunnels`
list in the reports.`) with:

```markdown
- A tunneled PCIe device that is not a USB controller, a 10G NIC behind a
  USB4 adapter say, as a sibling consumer of that Thunderbolt link in the
  controller grouping. Shipped: a tunnel group headed by its root port and
  the router's link, one row per tunneled function no bus represents, a
  `tunnels` list in the reports. Left for a later step: a per-domain join
  through the `usb4-host-interface` device links, which would pair a dock
  and an eGPU on two USB4 domains (AMD) where the host-wide rule stays
  silent by design.
```

- [ ] **Step 6: Check, gate, commit**

```bash
bash evals/run.sh 2>&1 | tail -4
```

Expected: `ALL EVALS PASS` (the tracked-file scan checks the untracked
local denylist, which names the private reference project; in a worktree
without that file, grep the six files for the name from the controller's
memory yourself and record the empty result). Run the gate from Global Constraints (the docs change no code,
but the commit must leave the tree green). Then:

```bash
git add README.md docs/ARCHITECTURE.md docs/SCRIPTING.md docs/TESTING.md CHANGELOG.md docs/ROADMAP.md
git commit -F - <<'EOF'
docs: tunneled PCIe devices in the device table, the tunnels list, the second Thunderbolt chain

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t
EOF
```

---

### Task 6: The live check

No code. The controller runs this after Task 5, over ssh to the dock
laptop (the alias is in the controller's memory, never in the repo), and
records the outcome in the SDD ledger, never in a tracked file.

- [ ] **Step 1: Survey what is attached**

Read `/sys/bus/thunderbolt/devices` and the removable PCI functions
first; the second chain has dropped before. Note which routers and
functions are present.

- [ ] **Step 2: The reports**

Copy a release build to a user-owned scratch directory on the host and
run `sudo <bin> --once --force --json | jq .tunnels`. Expected with the
CalDigit hub alone: one tunnel, `root_port` `0000:00:07.1`, `router.name`
`0-3` with `authorized` 1 and `security` `none`, `controllers`
`["0000:2e:00.0"]`, functions holding only that xHCI, awake at
`link_gts` 2.5 and `link_width` 4. With two routers present both tunnels
show `router: null`. With the NVMe enclosure up, a function of
`class_name` `NVMe` under the Dell dock's root port `0000:00:07.0`. Run
`sudo <bin> --once --force` and confirm the `tunnels:` section prints
the same in text.

- [ ] **Step 3: The TUI**

Run the TUI over ssh (`sudo <bin>`) and confirm the heading
`═ 0000:00:07.1 · Thunderbolt 0-3 Element Hub · 2×20 Gb/s ═` over buses 5
and 6 (or `external PCIe port` with two routers), a `▶ PCIe` row for any
non-USB function, `↓` reaching it, `i` hiding a suspended one. Before and
after sixty seconds of the TUI, read `power/runtime_status` of every
removable function: unchanged.

- [ ] **Step 4: Record**

Write the observed headings, rows, JSON excerpts and the runtime-status
readings to the ledger entry for this plan.

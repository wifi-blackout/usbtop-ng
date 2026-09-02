# Hardware fixture-capture & golden-replay harness — design

Date: 2026-08-30
Status: design, pending implementation plan
Wave: 2 (test infrastructure across the newly-provisioned fleet)

## Purpose

Turn the test fleet's real hardware diversity into **automated regression
coverage** of usbtop-ng's `capture → parse → aggregate → render` pipeline.

This is *not* a human-facing "supported hardware" compatibility matrix. It is a
corpus of captured real-host **fixtures** — a sysfs topology snapshot plus
usbmon trace samples — each with a **golden output**, replayed hermetically in
the default test suite. Each real host+topology becomes a regression case that
exercises the parser, the byte aggregation, the controller/bus/port topology
resolution, and the report rendering against hardware that synthetic fixtures do
not reach (real VL805/RP1/Rockchip/`dwc_otg`/Intel+AMD xHCI/`ehci`+`ohci-platform`
controllers; armv6l→aarch64→x86_64; kernels 5.4→7.0; real iso multipliers,
dual-personality hubs, deep chains).

Success: adding a captured host+stage to `tests/fixtures/hosts/` adds a test that
fails if the pipeline's deterministic output for that real input ever changes.

## Grounding: the seams this design builds on

Verified against the codebase (file:line):

- **Injection seams are `#[cfg(test)]`**: `DeviceManager::with_sysfs_base`
  (`src/device/manager.rs:131`), `UsbmonReader::with_path`
  (`src/usbmon/reader.rs:41`), `BinaryReader::with_path` (`src/usbmon/binary.rs:62`).
  So the replay harness lives **inside the crate** as `#[cfg(test)]` code. Because
  replay is fully hermetic (committed files, no root, no hardware), it belongs in
  the **default** test suite (CI runs it) — not behind the `integration`/`ebpf`
  features, which exist for live-hardware tests.
- **Report pipeline is pure and drivable**: `Baseline::capture(&DeviceManager)`
  (`src/headless/mod.rs:104`) and
  `build_report(&DeviceManager, &Baseline, elapsed, source, dropped, text_active, &FilterSet)`
  (`src/headless/mod.rs:151`). The only non-determinism is `Report.timestamp`
  (`SystemTime::now()`, `src/headless/mod.rs:165`). With a **fixed `elapsed`**, the
  entire report is reproducible except `timestamp`.
- **Canonical replay skeleton already exists** in a test
  (`report_rates_come_from_window_deltas`, `src/headless/mod.rs:589`):
  `with_sysfs_base` → feed packets via `apply_packet` → `enumerate_present_devices`
  → `build_report`.
- **sysfs read set is small and centralized** (`read_metadata_from`,
  `src/device/mod.rs:116-146`): per device dir, only `busnum, devnum, speed,
  idVendor, idProduct, manufacturer, product, version`. Topology comes from
  the **directory name** (`UsbDevice::port_chain`, `src/device/mod.rs:75`), not a
  file. The **controller** is resolved by `canonicalize(<base>/usbN)` then its
  parent's dir name (`src/device/manager.rs:40`), so `usbN` must be a **symlink**
  into a controller directory (see the fixture at `src/ui/mod.rs:1878`).
- **usbmon trace formats**: text `Nu` lines via `UsbmonReader` (grammar in
  `parse_usbmon_text_line`, `src/usbmon/parser.rs:210`); binary 48-byte
  native-endian headers + captured payload via `BinaryReader` (offsets pinned in
  `parse_binary_header`, `src/usbmon/binary.rs:211`). **mmap is not file-replayable**
  (it needs live `MON_IOCX_MFETCH`/`mmap` ioctls) and is excluded from the corpus.
- **`Snapshot::capture`** (`src/snapshot/mod.rs:33`) records a strict subset
  (`port_path, idVendor, idProduct`) → reuse it only for the internal-marking
  input (`set_internal_snapshot`), not as the general sysfs fixture.

## The fixture bundle

One committed directory per host **per ladder stage**:

```
tests/fixtures/hosts/<board>-<date>/stage<N>/
├── sysfs/                     # fixture-owned snapshot of /sys/bus/usb/devices — NO host symlinks
│   ├── <port-dir>/            # e.g. 3-1.4.2/ — a REAL materialized dir per device (not a symlink); no `*:*` interface dirs
│   │   ├── busnum devnum speed idVendor idProduct manufacturer product version
│   ├── <controller-id>/usb<bus>/              # fixture-local stand-in controller dir (name = PCI/platform id)
│   └── usb<bus> -> <controller-id>/usb<bus>   # the ONLY symlink: a controlled RELATIVE link so `controller` resolves
├── trace.bin                  # SANITIZED binary usbmon events: 48-byte headers only, len_cap forced to 0, NO payload
├── trace.txt                  # SANITIZED debugfs `Nu` text lines: length columns only, data field elided (`<`)
├── golden.binary.json         # deterministic Report replayed from trace.bin (timestamp masked)
├── golden.text.json           # deterministic Report replayed from trace.txt (timestamp masked)
├── internal-devices.toml      # the host BASELINE snapshot (bare board, no external devices) — reused across this host's stages
└── meta.toml                  # coverage tags + provenance
```

`meta.toml`:

```toml
board = "Raspberry Pi 4 Model B"
soc = "BCM2711"
arch = "aarch64"
kernel = "6.12.75+rpt-rpi-v8"
os = "Debian GNU/Linux 13"
usbtop_ng_version = "1.5.0"
stage_id = 7
stage_name = "Saturation"
captured_unix = 1787667796
controllers = ["0000:01:00.0", "..."]     # as resolved into BusReport.controller
speed_classes = ["480", "5000"]
transfer_types = ["bulk", "iso"]
sources = ["binary", "text"]
# optional ground-truth annotation (documentation only, NOT an assertion):
[generator]
kind = "bulk-dd"
ground_truth_bytes = 4293525472
note = "dd 4 GiB from the USB3 SSD; text golden reflects the known iso overcount"
```

The `generator` block is **documentation** so a human can see golden-vs-truth
divergence (e.g. the text iso overcount); it is never asserted against.

## The capture subcommand (`--capture-fixture`)

`usbtop-ng --capture-fixture <outdir> [--window <secs>] [--bus <n>]`

Needs root (opens the usbmon interfaces; loads the module). Distinct from a normal
capture run. Steps:

1. **Materialize a fixture-owned sysfs tree** → `<outdir>/sysfs/`. The real
   `/sys/bus/usb/devices/<name>` entries are themselves symlinks into
   `/sys/devices/…`; the fixture must **not** carry those (they would dangle on CI
   or resolve against the replay host — see Finding SEC-2). For each device entry
   (skipping `*:*` interface entries), read the 9 attributes through the real
   symlink and **write them into a real, fixture-owned directory** named for the
   port topology (e.g. `sysfs/3-1.4.2/busnum` …). The **only** symlink created is a
   controlled, **relative** `sysfs/usb<bus>` → `<controller-id>/usb<bus>`, where
   `<controller-id>` is the resolved real controller dir name and
   `sysfs/<controller-id>/usb<bus>/` is a fixture-local stand-in directory, so
   `BusReport.controller` resolves. **Invariant:** every path in `sysfs/`, once
   canonicalized, stays inside the bundle — nothing points at the host. A test
   enforces this.
2. **Capture both traces concurrently, sanitizing payload at the source**
   (Finding SEC-1). Read `/dev/usbmonN` (binary) and debugfs `Nu` (text) over the
   window — independent usbmon readers each see every event — but the capturer
   **never writes raw kernel bytes**: it parses each event and re-emits a
   **payload-free** one. Binary: the 48-byte header with `len_cap` forced to `0`
   and no trailing payload bytes (byte accounting uses `length`@32, not `len_cap`
   — see `binary.rs:227`, so this is golden-neutral). Text: the `Nu` line with the
   captured data field elided to `<` (the parser uses the length column, not the
   data). No captured USB payload (disk sectors, keystrokes, packets, secrets) ever
   lands in a fixture. An invariant test asserts it (binary size == N×48; text
   lines carry no data hex).
3. **Reuse the host BASELINE internal snapshot** (the internal-snapshot timing the
   review flagged). The internal
   snapshot must be captured once per host at the **bare-board** stage (stage 1,
   before any external test device is attached) via `Snapshot::capture`; each
   stage's `--capture-fixture` **copies that baseline in**, rather than snapshotting
   the current (externals-attached) topology — otherwise the external ladder
   equipment would be wrongly marked `internal` in the golden. `--capture-fixture`
   takes the baseline path (e.g. `--baseline <internal-devices.toml>`), or captures
   and caches it on the first, bare-board invocation.
4. **Gather host identity** → `meta.toml` (board/SoC, arch, kernel, OS,
   `usbtop-ng --version`, plus the coverage tags computed from the captured data).
5. **Generate the goldens by replaying the just-captured fixture** through the
   exact replay path (`with_sysfs_base(sysfs/)` → the matching reader's
   `with_path(trace, follow=false)` → `enumerate_present_devices` →
   `update_bus_speeds` → `build_report(FIXED_ELAPSED, source, …)`), one per source →
   `golden.<source>.json`, with `timestamp` masked. Generating the golden by replay
   (not from the live session) guarantees the committed golden equals what the
   replay test produces.
6. Write `meta.toml`.

The tester runs this per TESTING.md ladder stage (attach the stage's devices,
start the generator, run `--capture-fixture`), then commits `<outdir>` under
`tests/fixtures/hosts/`. The bare-board baseline snapshot (step 3) is taken first,
before any external device is plugged in.

## The replay harness

A `#[cfg(test)]` module in the **default** suite. Discovers
`tests/fixtures/hosts/*/stage*/` (via `env!("CARGO_MANIFEST_DIR")`). Per bundle,
per available source in `{binary, text}`:

```rust
let mut mgr = DeviceManager::with_sysfs_base(bundle.join("sysfs"));
if let Some(snap) = load_internal_devices(&bundle) { mgr.set_internal_snapshot(Some(snap)); }
// usb.ids overlay left None for determinism (ids come from the sysfs fixture only).
let baseline = Baseline::capture(&mgr);
reader_for(source, bus, &trace_path)              // UsbmonReader/BinaryReader::with_path(follow=false)
    .read_packets(&AtomicBool::new(false), |p| { mgr.apply_packet(&p); Ok(()) });
mgr.enumerate_present_devices();
mgr.update_bus_speeds();                           // resolves BusReport.controller + bus speed_mbps
                                                   // (pub, manager.rs:188; enumeration alone does NOT)
let report = build_report(&mgr, &baseline, FIXED_ELAPSED, source_label, 0,
                          source == Source::Text, &FilterSet::default());
assert_report_eq(&report, &load_golden(&bundle, source));  // JSON Values, timestamp removed
```

`FIXED_ELAPSED = Duration::from_secs(1)`. This drives the **real reader** (so the
parser is covered too), then the aggregation, the bus-speed/controller resolution,
the topology, and the report. `update_bus_speeds()` is used rather than full
`refresh()` — it resolves exactly `controller` + bus speed without `refresh`'s
removed-device bookkeeping, and it does not touch the accumulated byte counts. When
`meta.toml` declares controllers/speed classes, the harness additionally asserts
those golden fields are **non-null**, so a regression to the null-controller bug the
review caught cannot pass silently. One host+stage+source = one regression case.

## Determinism & golden comparison

With a fixed `elapsed` the whole report is reproducible except `timestamp`. Golden
comparison parses both sides to `serde_json::Value`, removes the `"timestamp"` key
from each, and asserts equality — so the report structs need **no** new derives
(`Report` keeps `Serialize` only). The deterministic set covered:

- `version`, `source`, `dropped_packets`, `kernel_dropped_packets`
- per bus: `bus`, `speed_mbps`, `controller`
- per device: `bus`, `address`, `port`, `vendor_id`, `product_id`, `vendor`,
  `product`, `speed_mbps`, `total_rx_bytes`, `total_tx_bytes`, `estimated`,
  `internal`
- per endpoint: `endpoint`, `direction`, `transfer_type`, `total_bytes`
- ordering (buses by id, devices by port-chain then id, endpoints by key)
- `window_seconds` and every `*_bps` (pinned by the fixed `elapsed`)

Only `timestamp` is excluded.

**Configuration parity.** The capturer generates each golden by replaying the
just-captured fixture under the *identical* configuration the test replay uses —
usb.ids overlay `None` (so `vendor`/`product` come only from the captured sysfs
`manufacturer`/`product`, never a live usb.ids lookup), `FIXED_ELAPSED`, the
default `FilterSet`, and the internal snapshot from the bundle. Same inputs, same
config, same code path ⇒ the committed golden equals the test's output by
construction. (Covering the usb.ids name-resolution path is a separate concern; a
pinned usb.ids fixture could be added later, out of scope here.)

## Security & fixture invariants

Two invariants are load-bearing — a committed fixture that violates either is a
data leak or a non-hermetic test — so each is enforced by an assertion in the
capturer *and* an independent test over the committed corpus.

- **SEC-1 — no captured USB payload in any fixture.** Raw usbmon events carry
  `len_cap` bytes of real payload (disk sectors, keystrokes, network frames,
  credentials). usbtop-ng's pipeline never reads them — accounting uses the header
  `length`@32, not `len_cap` (`binary.rs:227`) — so the capturer strips them at
  emit time (binary: header-only, `len_cap=0`; text: data field elided to `<`).
  *Enforcement:* the capturer asserts the sanitized stream is payload-free before
  writing; a corpus test asserts every `trace.bin` size is an exact multiple of the
  48-byte header and every `trace.txt` line carries no data hex. A fixture can never
  regress into carrying payload.
- **SEC-2 — fixture path containment.** The sysfs snapshot is fully materialized
  (real dirs of copied attributes); the single `usbN` symlink is relative and
  points only within the bundle. *Enforcement:* the capturer refuses to write a link
  that escapes; a corpus test canonicalizes every path under each `sysfs/` and
  asserts it stays inside the bundle (no absolute or `..`-escaping link into the
  host `/sys`). This is what keeps replay hermetic and host-independent on CI.

## Coverage

`meta.toml` tags make the corpus's coverage legible when read across fixtures
(controllers, arches, kernels, speed classes, transfer types, sources). A rendered
coverage matrix is **out of scope** for this wave (a later tool can aggregate the
`meta.toml` files).

## Fleet fit

Each capturable host runs the ladder (TESTING.md) and captures a fixture per stage;
partial ladders are first-class (Pi Zero = the stages behind its single OTG hub).
`airbox` contributes **no** fixtures — its 5.4 vendor kernel cannot capture — and
that usbmon-absent state is itself a documented coverage boundary, not a bundle.
`rock-32` (built-in usbmon, `ehci`/`ohci-platform` mix), the Pis (VL805, RP1,
`dwc_otg`), and `asus`/`judge` (Intel/AMD xHCI, 10 Gbps, Thunderbolt) all
contribute.

## Non-goals (YAGNI)

- No rendered compatibility matrix (deferred to a later wave).
- No live ladder orchestration — the tester runs stages per TESTING.md; the
  subcommand captures one stage at a time.
- No verdict computation as assertions — the golden **is** the assertion; the
  `meta.toml` `generator` block is a human-facing annotation only.
- No mmap traces (not file-replayable) — binary + text only.
- No eBPF fixtures — the eBPF backend produces aggregate deltas, not a per-URB
  trace; it keeps its own root/BTF-gated test.
- Capture nothing the device layer does not read (no `bDeviceClass`, `maxchild`,
  descriptor files).
- No WCID / Microsoft OS Descriptor capture. It is not a usbtop-ng pipeline input
  (not in sysfs, not in usbmon traffic), it would require an active ep0
  control-transfer probe (string index 0xEE → vendor code → Extended Compat ID),
  and there is no report field to golden-assert it against. Deferred; revisit as a
  separate device-identification tool/wave if that ever becomes a feature.

## Testing

- **Capture code**: hermetic unit tests over a tempdir sysfs tree — attribute copy
  into materialized dirs, interface-dir drop, and the relative `usbN`→controller
  stand-in link; the trace **sanitizer** (a binary event with payload → header-only
  `len_cap=0`; a text line with data → `<`); and that a golden generated by the
  capturer equals a direct replay of the same fixture.
- **Invariant tests over the committed corpus** (SEC-1, SEC-2): every `trace.bin`
  size is a multiple of 48 and every `trace.txt` line carries no data hex (no
  payload); every path under each `sysfs/`, canonicalized, stays inside its bundle
  (no host-escaping link). These run in the default suite over whatever fixtures are
  committed, so a bad fixture fails CI on the PR that adds it.
- **Replay harness**: seeded with one or two hand-built minimal fixtures so the
  harness lands and is exercised in CI before any real fleet capture exists; real
  captures are added as the ladder runs on each host. Where `meta.toml` declares
  controllers/speeds, the harness asserts those golden fields are non-null (guards
  the controller/`update_bus_speeds` regression the review caught).
- **Gates**: `cargo fmt --check`; `cargo clippy --all-targets -D warnings` across
  default, `integration`, `ebpf`; all test suites; MSRV 1.88; zero `#[allow]`.

## Open items to settle in the implementation plan

- Exact CLI surface and help text for `--capture-fixture` (and whether `--window`
  reuses the existing window flag).
- Whether the concurrent binary+text capture reuses the existing reader/parser
  plumbing to *read* events (then re-emits them sanitized) or reads the interfaces
  directly. Either way the committed fixture is the sanitized re-emission per SEC-1,
  never the raw kernel bytes.
- Fixture discovery: a directory walk under `CARGO_MANIFEST_DIR/tests/fixtures/hosts`
  with a stable, sorted iteration so failures name the bundle+source.
- The minimal seed fixtures' contents (a tiny two-device topology with one iso and
  one bulk endpoint is enough to exercise both goldens).

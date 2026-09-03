# Support bundle and troubleshooting export — design

**Date:** 2026-09-03
**Status:** approved for planning
**Wave:** 2 of the 2026-09 series. Wave 1 (isochronous accounting and
capture fidelity) shipped on 2026-09-02. Later waves: one row per physical
connector; cable and port diagnostics; parked minors.

## Goal

Give a user with a stock usbtop-ng binary one command, `--support`, that
gathers everything a maintainer needs to reproduce and diagnose a problem,
packages it with nothing that identifies the user beyond their hardware
models, and tells them how to file the bug. Give `--once` and `--batch` an
`--output PATH` that writes a self-describing report file usable in the
same report. Add the GitHub issue template the contributing guide already
refers to.

## Decisions taken in brainstorming

- `--support` gathers static system information plus a short usbmon
  capture when it runs as root (chosen over static-only).
- The capture machinery lives in the default build as a shared diagnostic
  core, so a support bundle embeds a real, replayable fixture bundle (chosen
  over keeping capture behind the `capture-fixture` feature).
- Device serial numbers and MAC addresses are never collected, in any
  bundle. This tightens the earlier "stable pseudonym" idea: port chains
  already distinguish identical devices. The fixture capturer's sysfs
  allowlist dropped `serial` on 2026-09-02 and the corpus history was purged
  of the values.
- Archiving shells out to the system `tar`; no new crates.
- `--output` has no append or rotation. It earns its place through the run
  record, not file management.

## Inventory: what `--support` gathers

Each item is a collector in `src/diag/collect.rs`; each returns a typed
struct plus "unavailable: reason" notes and never fails the bundle.

- **A. Build and invocation** (`build.toml`): usbtop-ng version, enabled
  cargo features, target arch, rustc version recorded at build time, the
  command line as run (paths rewritten per the privacy rules), effective
  user id and whether running under `sudo` (as "yes/no", no names),
  `RUST_LOG`.
- **B. Host** (`host.toml`, `usbmon.toml`, `dmesg-usb.txt`): kernel release
  and `/proc/version`; OS pretty name; board and SoC (DMI or device tree,
  NUL-flattened as `capture::meta` already does); CPU model and count;
  memory total; uptime; virtualization if `systemd-detect-virt` answers;
  kernel command line; every file under `/sys/module/usbcore/parameters/`;
  `/sys/kernel/security/lockdown`; the usbmon probe exactly as startup sees
  it (`check_usbmon_status`: module or built-in, debugfs mounted, each
  `/dev/usbmon*` node with owner and mode, text interface readable,
  permission-denied state, available buses); the backend the monitor would
  select for the aggregate bus and why (eBPF, mmap ring with its negotiated
  size, read() binary, or text), determined by the same probe functions
  `start_monitoring` uses, without starting a capture; eBPF readiness (BTF
  file present, feature built in); a `dmesg` tail filtered to lines
  mentioning usb, xhci, ehci, ohci, dwc, hub, or usbmon, masked per the
  privacy rules, or a note when unreadable.
- **C. USB topology** (`topology.toml`, plus the raw allowlisted sysfs in
  `fixture/sysfs/`): per device, the port chain, `vid:pid`, names resolved
  through the usb.ids chain, device class, subclass and protocol, `bcdUSB`,
  speed, each interface with its class and bound driver name, power state
  (`power/control`, `power/autosuspend`, `power/runtime_status`),
  `authorized`, `removable`; per hub, each port's `connect_type` and `peer`
  link target; per controller, PCI vendor, device, revision, and driver; the
  usb.ids source in use and its date.
- **D. Configuration** (`config/`): `preferences.toml` contents,
  `internal-devices.toml` contents, and `config.toml` with the resolved
  config directory as `~/…`, its permissions, and whether the `sudo`
  invoking-user resolution was applied.
- **E. Runtime evidence** (`fixture/`, `report.json`, `usbtop-ng.log`):
  with root and a usable usbmon interface, a capture of the aggregate bus
  for the window through `capture::run_capture_fixture`, producing the
  standard bundle (sanitized `trace.bin` and `trace.txt`, both goldens,
  `meta.toml` with `binary_kernel_dropped`); `report.json`, the same fixture
  replayed with the real window as the elapsed time, written through the
  export sink with a run record; and the run's debug log, captured by
  initialising the logger with a tee target. Without root, `fixture/` still
  holds `sysfs/`, `internal-devices.toml`, and a `meta.toml` with
  `sources = []`, and the summary says how to include a capture.
- **F. Terminal** (`terminal.toml`): `TERM`, `COLORTERM`, `LANG`, `LC_ALL`,
  terminal size, whether stdout is a tty, whether an SSH session is present
  (presence only), and the synchronized-output probe decision
  (`tui::sync::probe_sync_mode`).
- **G. Bundle meta** (`manifest.toml`, `SUMMARY.txt`): format version,
  creation time in UTC, file list with sizes, the redaction summary (each
  rule and its substitution count), every unavailable note, and the printed
  summary block.

## Privacy rules

Implemented in `src/diag/redact.rs` as pure functions with table tests, and
applied by the bundle writer at write time.

- Serial numbers and MAC addresses are never collected: not from sysfs
  (the allowlist has no `serial`), not in `topology.toml`, and in
  `dmesg-usb.txt` the value after `SerialNumber:` and any
  `xx:xx:xx:xx:xx:xx` token are replaced with `[redacted]`.
- Hostname, machine-id, and IP addresses are never collected. `SSH_TTY`,
  `SSH_CONNECTION`, and `SSH_CLIENT` are recorded only as present or absent.
- Every path under the user's home directory is rewritten to `~/…`,
  including inside `preferences.toml`, `config.toml`, and the recorded
  command line. No user name is recorded anywhere; the `sudo` resolution
  appears as "home resolved to ~".
- Environment values are recorded only for `TERM`, `COLORTERM`, `LANG`,
  `LC_ALL`, and `RUST_LOG`.
- Traces are payload-free by construction (SEC-1) and the sysfs snapshot is
  path-contained (SEC-2); the bundle writer re-asserts both with the
  capturer's own functions.
- Nothing is hidden silently: the manifest's redaction summary and the
  printed file list exist so the reporter can review the bundle before
  attaching it.

## Architecture

### Modules

- `src/capture/` is promoted into the default build with no behaviour
  change: `pub mod capture;` becomes unconditional, the three injection
  seams (`DeviceManager::with_sysfs_base`, `BinaryReader::with_path`,
  `UsbmonReader::with_path`) lose their `cfg`, and `fixture_replay` becomes
  always-on because the bundle generates goldens by replay. The
  `capture-fixture` feature keeps gating only the `--capture-fixture`
  subcommand and its CLI fields. `fixture_corpus` stays test-only.
- `src/diag/` (new, default build):
  - `collect.rs`: the collectors above. Each takes its filesystem roots as
    parameters (`/sys`, `/proc`, the config directory) so tests inject a
    fake tree the way `with_sysfs_base` does.
  - `redact.rs`: the privacy rules.
  - `bundle.rs`: the bundle directory layout, `manifest.toml`, the file
    list, and archiving via `tar`.
  - `support.rs`: `run_support(opts)`: orchestrates the collectors, the
    optional capture, the report replay, the manifest, the archive, and
    prints `SUMMARY.txt` and the guidance.
- `src/headless/export.rs` (new): `ReportSink { Stdout, File }`, the run
  record, and the `--output` writer. `emit(report, json)` in
  `src/headless/mod.rs` becomes `sink.write(report)`.
- `.github/ISSUE_TEMPLATE/bug_report.yml` and `config.yml`.

### Data flow of `--support`

1. `main` initialises the logger with a tee target when `--support` is
   present, so the debug log reaches both stderr and `usbtop-ng.log`.
2. Collectors A, B, C, D, and F run, root or not.
3. If the effective user is root, `--no-capture` is absent, and the usbmon
   probe reports a usable interface, `capture::run_capture_fixture` records
   the aggregate bus into `<bundle>/fixture/` for the window. Otherwise the
   bundle records why the capture was skipped.
4. The backend probe records which source the monitor would select.
5. `report.json` is written by replaying the fixture with the real window as
   elapsed time, through the export sink with a run record.
6. `bundle.rs` writes every file with redaction applied, writes the
   manifest, archives with `tar`, and `support.rs` prints the summary and
   guidance.

### Command line

- `--support [PATH]`, optional value like `--update-usbids [MODE]`. No value
  means the current directory; a directory means create the bundle inside
  it; a name ending in `.tar.gz` names the archive. The directory
  `usbtop-ng-support-<UTC timestamp>/` is created first, then archived; both
  are left in place.
- `--window SECONDS` sets the capture length (default 5 s, floor 0.1 s,
  the capture subcommand's rule). `--no-capture` skips the capture.
- `--support` conflicts with `--once`, `--batch`, `--snapshot-internal`,
  and `--capture-fixture`.
- `--support` never changes the system: no `modprobe`, no prompts.
- Exit status 0 whenever the bundle was written, even with unavailable
  notes; non-zero only when the directory or a file could not be written.

### Summary block

```
usbtop-ng support bundle
  bundle:   ./usbtop-ng-support-20260903T091500Z.tar.gz (412 KB, 14 files)
  version:  usbtop-ng 1.5.0 (features: none) x86_64
  host:     Linux 7.0.0-30-generic, Linux Mint 22.3, MG-VCP17A-3080
  usbmon:   module loaded, 4 buses, /dev/usbmon* root:root 0600, running as root
  backend:  mmap ring (64 MiB) would be selected; eBPF: BTF present, not built in
  capture:  5.0 s aggregate, 1,234 events, kernel drops 0, sources binary+text
  devices:  21 across 4 buses (1.5/12/480/5000/10000 Mbps)
  notes:    dmesg unavailable (permission denied)
  redacted: 3 home paths; serials and MACs never collected
```

### Filing guidance (printed after the summary)

```
To report a bug:
  1. Review the bundle before attaching it: `tar tzf <archive>` lists every file.
     Nothing in it identifies you beyond your hardware models, but you decide.
  2. Open https://github.com/wifi-blackout/usbtop-ng/issues/new?template=bug_report.yml
  3. Paste the summary above into "Support summary" and attach the .tar.gz.
  4. Describe what you expected, what happened, and the exact command you ran.
     For a display problem, name the terminal and say whether it was over SSH.
```

### `--output PATH` export

- Every report goes to the file instead of stdout, in the active format
  (text, or NDJSON with `--json`). The file is created or truncated at
  start. One line on stderr at exit reports how many reports were written
  and where. A write error on the file is fatal with a non-zero exit.
- A file export starts with a run record. JSON: the first line is
  `{"record": "run", …}` with usbtop-ng version and features, start time
  (UTC), window seconds, `batch` true or false, the active filters, the
  command line, the backend selected at start, kernel, OS, arch, and the bus
  list; report lines follow unchanged, schema version 1. Text: the same
  fields as a `# key: value` block before the first report. Stdout never
  carries the run record.
- The support bundle's `report.json` is written through the same sink and
  then redacted; a user's own export is not redacted.

### GitHub template

`.github/ISSUE_TEMPLATE/bug_report.yml` is a form with: what happened
(required); what you expected; the exact command; "Support summary"
(required, rendered as code); a required checkbox "I attached the support
bundle, or explained why not"; terminal and SSH details; anything else.
`config.yml` keeps blank issues enabled.

## Error handling

- Collectors never fail the bundle; every missing file, permission error,
  or failed probe becomes a note. A capture failure after a successful probe
  becomes a note and the bundle continues without traces.
- The run fails only when the bundle directory cannot be created or a file
  in it cannot be written; the message names the path and the cause, per the
  user-facing text rules in CONTRIBUTING.
- If `tar` is not installed, the directory stays and the summary says to
  archive it by hand.

## Documentation

- README: a "Reporting a problem" section pointing at `--support`.
- CONTRIBUTING: the bug-report section rewritten around `--support`,
  replacing the `RUST_LOG`, `lsusb`, and `lsmod` asks; the code-organization
  tree gains `src/diag/` and `src/headless/export.rs`.
- SCRIPTING: the `--output` section, the run record, and the one-liner to
  skip it.
- ARCHITECTURE: the diagnostic core and where the capture code now lives.
- CHANGELOG: Added (`--support`, `--output`, the issue template), Changed
  (capture code in the default build; serials never collected).
- The man page and completions follow clap automatically.

## Testing

- Unit: redaction rules as table tests; each collector against an injected
  fake `/sys` and `/proc` tree; run-record serialization; the file sink's
  writes and its fatal error path; summary rendering from a fixed bundle
  struct; the guidance text pinned (URL and the four steps).
- Hermetic end-to-end: `run_support` with `--no-capture` against the fake
  tree writes a bundle whose manifest matches the files on disk, whose
  redaction counts match, and whose `fixture/` passes the capturer's SEC-1
  and SEC-2 assertions. The `tar` step runs when `tar` exists and is skipped
  with a note otherwise.
- Live, behind the `integration` feature: `--support` as root on the
  development host yields a fixture whose goldens replay and, on an idle
  bus, zero kernel drops.
- Gates: MSRV 1.88; zero `#[allow(...)]`; `cargo fmt`; clippy `-D warnings`
  on the default, `capture-fixture`, `integration`, and `ebpf` configs. The
  default CI job now compiles the promoted capture code.

## Non-goals

- Uploading anything anywhere; the user attaches the archive by hand.
- Append or rotation semantics for `--output`.
- A feature-request template beyond CONTRIBUTING's existing guidance.
- Collecting anything the privacy rules exclude, even behind a flag.

## Global constraints

- MSRV 1.88; zero `#[allow(...)]`; `cargo fmt`; clippy `-D warnings` on all
  four configs.
- Kernel and sysfs semantics verified against source when a collector reads
  a kernel interface for the first time, cited by file and line.
- The private reference project is never named in the repo.
- Bundles stay payload-free (SEC-1) and path-contained (SEC-2).
- `#[cfg]` lattice after this wave: `capture` and `fixture_replay` always
  on; `fixture_corpus` test-only; `capture-fixture` gates only the
  subcommand.

## Verification

- All suites green on the four configs.
- A `--support` run as root on the development host, then: `tar tzf` lists
  exactly the manifest's files; `grep` over the extracted bundle finds no
  home path, no hostname, no serial, no MAC; the embedded `fixture/` copied
  into `tests/fixtures/hosts/` passes `cargo test fixture_corpus` unchanged.
- `--batch --json --output run.ndjson` for two windows yields a run record
  followed by two report lines that parse with `serde_json`.

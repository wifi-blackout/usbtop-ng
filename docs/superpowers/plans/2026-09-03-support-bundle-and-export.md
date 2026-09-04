# Support Bundle and Troubleshooting Export Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `--support [PATH]` (a redacted, self-describing diagnostic bundle with an embedded replayable fixture and printed filing guidance), `--output PATH` for `--once`/`--batch` (a file export led by a run record), and the GitHub bug-report form, with the capture core promoted into the default build.

**Architecture:** A new `src/diag/` module holds pure redaction rules, injectable-root collectors, a device inventory reader, a bundle writer, and the `--support` orchestrator. `src/headless/export.rs` adds a `ReportSink` that both `--output` and the bundle's `report.json` write through. `src/capture/` and `src/fixture_replay.rs` become always-on so a support bundle embeds a real fixture; only the `--capture-fixture` subcommand stays behind its feature. Until the orchestrator lands, `diag` is compiled under `#[cfg(test)]` so every intermediate task keeps clippy `-D warnings` clean without `#[allow]`.

**Tech Stack:** Rust 1.88 (MSRV), clap 4 derive, serde + toml + serde_json (already dependencies), env_logger `Target::Pipe` for the log tee, the system `tar` via `std::process::Command`, `tempfile` in tests. No new crates.

**Spec:** `docs/superpowers/specs/2026-09-03-support-bundle-and-export-design.md`

## Global Constraints

- MSRV 1.88; zero `#[allow(...)]`; `cargo fmt`; `cargo clippy --all-targets -- -D warnings` on the default build and on `--features capture-fixture`, `--features integration`, `--features ebpf`.
- Privacy boundary (spec): host identity is never collected (hostname, machine-id, DMI system serial and product UUID, host network MACs, IP addresses, user names, home paths); device identity is collected verbatim (USB serial strings, Thunderbolt `unique_id`, every descriptor field). The embedded `fixture/` never contains a serial (the capturer's allowlist omits it). Environment values recorded only for `TERM`, `COLORTERM`, `LANG`, `LC_ALL`, `RUST_LOG`; `SSH_TTY`/`SSH_CONNECTION`/`SSH_CLIENT` as present or absent only.
- `--support` never changes the system: no `modprobe`, no prompts, no network. Exit 0 whenever the bundle was written; non-zero only when the bundle directory or a file in it could not be written.
- Bundles stay payload-free (SEC-1) and path-contained (SEC-2); the bundle writer re-asserts both with `capture::assert_payload_free` and `capture::assert_sysfs_contained`.
- `#[cfg]` lattice after Task 6: `capture` and `fixture_replay` always on; `fixture_corpus` test-only; `capture-fixture` gates only the `--capture-fixture` CLI fields and dispatch; `diag` always on.
- No new crates. Archiving shells out to `tar`; UTC timestamps come from `SystemTime` plus the civil-date conversion in Task 5.
- The private reference project is never named in the repo, this plan included. `PRIVATE_NAME` below is that name, supplied by the controller in each dispatch prompt and exported in the shell; before every commit `git grep -i -e "$PRIVATE_NAME"` must print nothing.
- Commit messages: conventional prefix, a body that says why, and the trailer block:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t
  ```
- `cargo` is not on PATH: prefix cargo commands with `export PATH="$HOME/.cargo/bin:$PATH";` (or, inside a Claude worktree session where that is refused, call `cargo` by its absolute path under your home directory, `~/.cargo/bin/cargo` spelled out).
- User-facing text follows CONTRIBUTING's "User-facing text" rules: errors lowercase with chained causes and "could not"; prompts and guidance sentence case, imperative, one action per line; log lines lowercase naming the interface and bus.

## File structure

| Path | Responsibility |
|---|---|
| `src/headless/export.rs` (new) | `ReportSink` (stdout or file), `RunRecord`, the `--output` writer and its exit notice |
| `src/headless/mod.rs` | `HeadlessOptions` gains `output` and `run_record`; `run` writes through the sink |
| `src/diag/mod.rs` (new) | module declarations; `Note` (an "unavailable: reason" record) |
| `src/diag/redact.rs` (new) | `Redactor`: home-path rewriting, environment allowlist, substitution counts |
| `src/diag/collect.rs` (new) | collectors A, B, D, F: build, host, usbmon and backend probe, dmesg filter, config, terminal |
| `src/diag/inventory.rs` (new) | collector C: USB device inventory, raw descriptor blobs, Thunderbolt and Type-C attribute dumps |
| `src/diag/bundle.rs` (new) | bundle directory, manifest, UTC stamp, file list, `tar` archive |
| `src/diag/support.rs` (new) | `SupportOpts`, `prepare_dir`, `run_support`, summary and guidance text, the log tee |
| `src/main.rs` | `--output`, `--support`, `--no-capture`; logger tee; dispatch; `mod diag`; `mod capture` and `mod fixture_replay` unconditional |
| `src/capture/mod.rs`, `src/fixture_replay.rs`, `src/capture/meta.rs`, `src/usbmon/{binary,reader}.rs`, `src/device/manager.rs` | feature gates removed from the module and the three seams; static bundles; `replay_fixture_with_elapsed`; `assert_bundle_payload_free`; `CaptureOutcome` |
| `src/usbids/mod.rs` | `active_source`, `parse_header_date`, `resolve_from_chain` become `pub(crate)` |
| `build.rs` | records `rustc --version` as `USBTOP_NG_RUSTC` for `build.toml` |
| `.github/ISSUE_TEMPLATE/bug_report.yml`, `config.yml` (new) | the bug-report form |
| `README.md`, `docs/CONTRIBUTING.md`, `docs/SCRIPTING.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/TESTING.md`, `CHANGELOG.md` | documentation |
| `.github/workflows/ci.yml` | the default job now compiles the capture code; the feature job is unchanged |

Module gating rule for this plan: Tasks 2 through 6 add files under `src/diag/` whose only callers are their own tests, so `src/main.rs` declares `#[cfg(test)] mod diag;` from Task 2 until Task 7 flips it to `mod diag;` when `--support` consumes them. Likewise Task 5 compiles `capture` under `#[cfg(any(test, feature = "capture-fixture"))]` (so the test-gated `diag` can call it) and keeps the live capture entry point behind the feature until Task 7. A `pub` item in a private module of a binary crate that nothing reaches from `main` is dead code under `-D warnings`; the gates are how each task stays green without `#[allow]`, and every item they expose has a test that calls it.

Task order: 1 `--output`; 2 redaction; 3 collectors; 4 device inventory; 5 capture-core promotion; 6 bundle writer; 7 `--support` and the CLI; 8 template and docs; 9 live verification.

---

### Task 1: `--output PATH` export with a run record

**Files:**
- Create: `src/headless/export.rs`
- Modify: `src/headless/mod.rs` (`HeadlessOptions`, `run`, `emit`)
- Modify: `src/main.rs` (CLI field, validation, `HeadlessOptions` construction)

**Interfaces:**
- Consumes: `headless::Report` (serde `Serialize`), `headless::render_text(&Report) -> String`, `usbmon::UsbmonStatus`, `filter::FilterSet` (its `Display`/`terms` — use `cli.filter` strings directly).
- Produces:
  ```rust
  // src/headless/export.rs
  #[derive(Debug, Clone, Serialize, PartialEq)]
  pub struct RunRecord {
      pub record: &'static str,          // always "run"
      pub usbtop_ng: String,             // CARGO_PKG_VERSION
      pub features: Vec<&'static str>,   // enabled cargo features, sorted
      pub started_unix: u64,
      pub window_seconds: f64,
      pub batch: bool,
      pub filters: Vec<String>,          // the raw --filter terms
      pub command: Vec<String>,          // std::env::args()
      pub backend: String,               // "ebpf" | "mmap" | "binary" | "text" | "none" at start
      pub kernel: String,
      pub os: String,
      pub arch: &'static str,
      pub buses: Vec<u8>,
  }
  pub enum ReportSink { Stdout, File { path: PathBuf, file: std::fs::File, written: usize } }
  impl ReportSink {
      pub fn open(output: Option<&Path>, run: &RunRecord, json: bool) -> std::io::Result<ReportSink>;
      pub fn write(&mut self, report: &Report, json: bool) -> std::io::Result<()>;
      /// Consumes the sink; for a file returns (reports written, path) for the exit notice.
      pub fn finish(self) -> Option<(usize, PathBuf)>;
  }
  pub fn render_run_record_text(run: &RunRecord) -> String;  // "# key: value\n" lines
  pub fn enabled_features() -> Vec<&'static str>;
  ```
  `HeadlessOptions` gains `pub output: Option<PathBuf>` and `pub run_record: RunRecord`.

- [ ] **Step 1: Write the failing tests**

Create `src/headless/export.rs` with the test module first (the code in Step 3 goes above it in the same file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{BusReport, Report};

    fn run() -> RunRecord {
        RunRecord {
            record: "run",
            usbtop_ng: "9.9.9".into(),
            features: vec![],
            started_unix: 1_788_000_000,
            window_seconds: 1.5,
            batch: true,
            filters: vec!["bus=1".into()],
            command: vec!["usbtop-ng".into(), "--batch".into()],
            backend: "mmap".into(),
            kernel: "7.0.0-30-generic".into(),
            os: "Linux Mint 22.3".into(),
            arch: "x86_64",
            buses: vec![0, 1],
        }
    }

    fn report() -> Report {
        Report {
            version: 1,
            timestamp: 1.0,
            window_seconds: 1.5,
            source: "mmap",
            dropped_packets: 0,
            kernel_dropped_packets: 0,
            total_rx_bps: 0.0,
            total_tx_bps: 0.0,
            buses: Vec::<BusReport>::new(),
        }
    }

    /// A JSON file export starts with the run record on its own line, then
    /// one report document per line, exactly what `--batch --json` prints
    /// to stdout, so consumers skip line one and keep their parser.
    #[test]
    fn json_file_export_leads_with_the_run_record() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("run.ndjson");
        let mut sink = ReportSink::open(Some(&path), &run(), true).unwrap();
        sink.write(&report(), true).unwrap();
        sink.write(&report(), true).unwrap();
        let (n, p) = sink.finish().unwrap();
        assert_eq!((n, p), (2, path.clone()));

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let head: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(head["record"], "run");
        assert_eq!(head["backend"], "mmap");
        assert_eq!(head["buses"], serde_json::json!([0, 1]));
        let doc: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(doc["version"], 1);
        assert!(doc.get("record").is_none(), "report lines are unchanged");
    }

    /// Text mode: the same fields as a `# key: value` block, then the
    /// rendered report.
    #[test]
    fn text_file_export_leads_with_a_comment_block() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("run.txt");
        let mut sink = ReportSink::open(Some(&path), &run(), false).unwrap();
        sink.write(&report(), false).unwrap();
        sink.finish();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# usbtop_ng: 9.9.9\n"), "{text}");
        assert!(text.contains("# backend: mmap\n"));
        assert!(text.contains("# command: usbtop-ng --batch\n"));
        assert!(text.contains("# filters: bus=1\n"));
    }

    /// Stdout never carries the run record: `open(None, ..)` is the stdout
    /// sink and `finish` reports nothing to announce.
    #[test]
    fn stdout_sink_has_no_header_and_nothing_to_announce() {
        let sink = ReportSink::open(None, &run(), true).unwrap();
        assert!(matches!(sink, ReportSink::Stdout));
        assert!(sink.finish().is_none());
    }

    /// An unwritable path is an error at open time, not a silent stdout
    /// fallback.
    #[test]
    fn unwritable_output_path_fails_at_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing-dir").join("run.ndjson");
        assert!(ReportSink::open(Some(&path), &run(), true).is_err());
    }

    #[test]
    fn enabled_features_is_sorted_and_only_names_real_features() {
        let f = enabled_features();
        let mut sorted = f.clone();
        sorted.sort_unstable();
        assert_eq!(f, sorted);
        for name in &f {
            assert!(["capture-fixture", "ebpf", "integration"].contains(name));
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod export;` at the top of `src/headless/mod.rs` (after its `use` block), then:

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test headless::export 2>&1 | tail -5`
Expected: compile errors (`RunRecord`, `ReportSink`, `enabled_features` not found).

- [ ] **Step 3: Implement `export.rs`**

Above the test module:

```rust
//! The `--output PATH` file sink for `--once` and `--batch`, and the run
//! record that leads every file export so the file describes the run it
//! came from (version, host, backend, window, filters, command). Stdout
//! keeps today's byte-exact behaviour: no record, no notice. The support
//! bundle writes its `report.json` through the same sink.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{render_text, Report};

/// The first line (JSON) or comment block (text) of a file export.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunRecord {
    pub record: &'static str,
    pub usbtop_ng: String,
    pub features: Vec<&'static str>,
    pub started_unix: u64,
    pub window_seconds: f64,
    pub batch: bool,
    pub filters: Vec<String>,
    pub command: Vec<String>,
    pub backend: String,
    pub kernel: String,
    pub os: String,
    pub arch: &'static str,
    pub buses: Vec<u8>,
}

/// Cargo features compiled into this binary, sorted, for the run record and
/// the support bundle.
pub fn enabled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "capture-fixture") {
        features.push("capture-fixture");
    }
    if cfg!(feature = "ebpf") {
        features.push("ebpf");
    }
    if cfg!(feature = "integration") {
        features.push("integration");
    }
    features
}

/// The text-mode header: one `# key: value` line per run-record field, in
/// declaration order, list fields space-joined.
pub fn render_run_record_text(run: &RunRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("# usbtop_ng: {}\n", run.usbtop_ng));
    out.push_str(&format!("# features: {}\n", run.features.join(" ")));
    out.push_str(&format!("# started_unix: {}\n", run.started_unix));
    out.push_str(&format!("# window_seconds: {}\n", run.window_seconds));
    out.push_str(&format!("# batch: {}\n", run.batch));
    out.push_str(&format!("# filters: {}\n", run.filters.join(" ")));
    out.push_str(&format!("# command: {}\n", run.command.join(" ")));
    out.push_str(&format!("# backend: {}\n", run.backend));
    out.push_str(&format!("# kernel: {}\n", run.kernel));
    out.push_str(&format!("# os: {}\n", run.os));
    out.push_str(&format!("# arch: {}\n", run.arch));
    let buses: Vec<String> = run.buses.iter().map(|b| b.to_string()).collect();
    out.push_str(&format!("# buses: {}\n", buses.join(" ")));
    out
}

/// Where reports go: stdout (today's behaviour, unchanged) or a file that
/// was created or truncated at open and led with the run record.
pub enum ReportSink {
    Stdout,
    File {
        path: PathBuf,
        file: File,
        written: usize,
    },
}

impl ReportSink {
    /// `None` is the stdout sink. `Some(path)` creates or truncates the
    /// file and writes the run record (a JSON line, or the text comment
    /// block) before returning; an unwritable path is an error here.
    pub fn open(output: Option<&Path>, run: &RunRecord, json: bool) -> io::Result<ReportSink> {
        let Some(path) = output else {
            return Ok(ReportSink::Stdout);
        };
        let mut file = File::create(path)?;
        if json {
            let line = serde_json::to_string(run).expect("run record serializes");
            writeln!(file, "{line}")?;
        } else {
            file.write_all(render_run_record_text(run).as_bytes())?;
        }
        file.flush()?;
        Ok(ReportSink::File {
            path: path.to_path_buf(),
            file,
            written: 0,
        })
    }

    /// Write one report in the active format. Stdout errors keep the
    /// existing `BrokenPipe` contract in `headless::run`; file errors are
    /// real failures.
    pub fn write(&mut self, report: &Report, json: bool) -> io::Result<()> {
        match self {
            ReportSink::Stdout => {
                let stdout = io::stdout();
                let mut out = stdout.lock();
                write_one(&mut out, report, json)?;
                out.flush()
            }
            ReportSink::File { file, written, .. } => {
                write_one(file, report, json)?;
                file.flush()?;
                *written += 1;
                Ok(())
            }
        }
    }

    /// For a file sink, the count and path for the exit notice; `None` for
    /// stdout, which announces nothing.
    pub fn finish(self) -> Option<(usize, PathBuf)> {
        match self {
            ReportSink::Stdout => None,
            ReportSink::File { path, written, .. } => Some((written, path)),
        }
    }
}

fn write_one(out: &mut impl Write, report: &Report, json: bool) -> io::Result<()> {
    if json {
        let line = serde_json::to_string(report).expect("report serializes");
        writeln!(out, "{line}")
    } else {
        write!(out, "{}", render_text(report))
    }
}
```

`Report` and `BusReport` fields used by the test's constructor must be visible to the module: they are `pub` already (`src/headless/mod.rs:31-60`). If `Report` derives no `Default`, the test constructs it field by field as shown; if a field exists that the test above omits, add it with a zero value.

- [ ] **Step 4: Wire `HeadlessOptions` and `run`**

In `src/headless/mod.rs`:
- Add to `HeadlessOptions`: `pub output: Option<std::path::PathBuf>,` and `pub run_record: export::RunRecord,` with doc comments ("`--output PATH`; `None` prints to stdout" and "leads a file export; never printed to stdout").
- In `run`, before the `loop`: `let mut sink = export::ReportSink::open(opts.output.as_deref(), &opts.run_record, opts.json)?;`
- Replace `if let Err(e) = emit(&report, opts.json) { ... }` with:
  ```rust
          if let Err(e) = sink.write(&report, opts.json) {
              if opts.output.is_none() && is_expected_write_failure(&e) {
                  return Ok(()); // broken pipe on stdout: the reader left
              }
              return Err(anyhow!("could not write the report: {e}"));
          }
          if !opts.batch || stop.load(Ordering::Relaxed) {
              if let Some((n, path)) = sink.finish() {
                  eprintln!("wrote {n} report(s) to {}", path.display());
              }
              return Ok(());
          }
  ```
  (`sink.finish()` consumes the sink, so structure the final branch so the sink is moved only on the return path; simplest is to `break` out of the loop with a flag and finish after it.)
- Delete the old `emit` function; keep `is_expected_write_failure`.
- Update every test in `src/headless/mod.rs` that builds `HeadlessOptions` (grep `HeadlessOptions {`) to add `output: None` and `run_record: export::RunRecord { record: "run", usbtop_ng: String::new(), features: vec![], started_unix: 0, window_seconds: 1.0, batch: false, filters: vec![], command: vec![], backend: "binary".into(), kernel: String::new(), os: String::new(), arch: "x86_64", buses: vec![] }`. Extract that literal into a test helper `fn test_run_record() -> export::RunRecord` in the tests module if more than one site needs it.

In `src/main.rs`:
- CLI field after `json`:
  ```rust
      /// Write the reports to PATH instead of stdout (created or truncated;
      /// the file starts with a run record). Needs --once or --batch.
      #[arg(long, value_name = "PATH")]
      output: Option<String>,
  ```
- Validation beside the existing `--json`/`--window` check: `if (cli.json || cli.window.is_some() || cli.output.is_some()) && !headless { eprintln!("error: --json, --window, and --output need --once or --batch"); process::exit(2); }`
- Build the run record just before `headless::run`:
  ```rust
          let started_unix = std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .map(|d| d.as_secs())
              .unwrap_or(0);
          let run_record = headless::export::RunRecord {
              record: "run",
              usbtop_ng: env!("CARGO_PKG_VERSION").to_string(),
              features: headless::export::enabled_features(),
              started_unix,
              window_seconds: window.as_secs_f64(),
              batch: cli.batch,
              filters: cli.filter.clone(),
              command: env::args().collect(),
              backend: match &capture {
                  usbmon::monitor::CaptureStream::Deltas(_) => "ebpf".to_string(),
                  usbmon::monitor::CaptureStream::Packets(_) if monitor.flags.text_active.load(std::sync::atomic::Ordering::Relaxed) => "text".to_string(),
                  usbmon::monitor::CaptureStream::Packets(_) if monitor.flags.mmap_active.load(std::sync::atomic::Ordering::Relaxed) => "mmap".to_string(),
                  usbmon::monitor::CaptureStream::Packets(_) if usbmon_status.available_buses.is_empty() => "none".to_string(),
                  usbmon::monitor::CaptureStream::Packets(_) => "binary".to_string(),
              },
              kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default(),
              os: capture::meta::os_pretty_name().unwrap_or_default(),
              arch: std::env::consts::ARCH,
              buses: usbmon_status.available_buses.clone(),
          };
  ```
  `capture::meta::os_pretty_name` is private today and `capture` is feature-gated until Task 7; for this task read `/etc/os-release` inline with the same four lines (`PRETTY_NAME=` prefix, trim quotes) as a private `fn os_pretty_name_from(text: &str) -> Option<String>` in `main.rs` with one unit test; Task 7 replaces it with `diag::collect::os_pretty_name_from` (which Task 3 defines) and deletes the private copy. Then pass `output: cli.output.as_deref().map(std::path::PathBuf::from), run_record,` into `HeadlessOptions`.
  Note: `flags.mmap_active` is set by the reader thread shortly after start; reading it at record-build time may race the first fetch. Build the record after `start_capture` and read the flags once; the report lines carry the authoritative per-window `source` anyway, and the record's `backend` documents "selected at start".

- [ ] **Step 5: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E 'test result|FAILED'`
Expected: all ok, including the five new tests.

- [ ] **Step 6: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/headless/export.rs src/headless/mod.rs src/main.rs
git commit -m "feat(headless): --output PATH writes reports to a file led by a run record

A file export now starts with a self-describing run record (version,
features, start time, window, filters, command, backend selected at
start, kernel, OS, arch, buses) as the first NDJSON line or a comment
block in text mode, then the unchanged report documents. Stdout is
untouched: no record, no notice. File write errors are fatal; the
stdout broken-pipe tolerance stays. Reports flow through a ReportSink
the support bundle will reuse.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```


---

### Task 2: `diag` scaffold and the privacy rules (`redact.rs`)

**Files:**
- Create: `src/diag/mod.rs`
- Create: `src/diag/redact.rs`
- Modify: `src/main.rs` (module declaration only)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  ```rust
  // src/diag/mod.rs
  pub mod redact;
  #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
  pub struct Note { pub item: String, pub reason: String }
  pub fn note(item: &str, reason: impl std::fmt::Display) -> Note;

  // src/diag/redact.rs
  pub const ENV_ALLOWLIST: [&str; 5];   // TERM COLORTERM LANG LC_ALL RUST_LOG
  pub const SSH_MARKERS: [&str; 3];     // SSH_TTY SSH_CONNECTION SSH_CLIENT
  pub struct Redactor { .. }
  impl Redactor {
      pub fn new(home: Option<&Path>) -> Redactor;
      pub fn path(&mut self, path: &Path) -> String;
      pub fn text(&mut self, text: &str) -> String;
      pub fn mac_addresses(&mut self, text: &str) -> String;
      pub fn cmdline(&mut self, text: &str) -> String;
      pub fn env_allowlisted(name: &str) -> bool;
      pub fn ssh_present(present: impl Fn(&str) -> bool) -> bool;
      pub fn summary(&self) -> Vec<(String, usize)>;
  }
  ```
  Rule names in the summary: `home_path`, `mac_address`, `fs_uuid`.

Two rules here go one step past the spec's list, both on the "host identity out" side of its boundary, so they are rulings rather than additions: `mac_addresses` masks `aa:bb:cc:dd:ee:ff` tokens (applied by Task 7 to `dmesg-usb.txt` only, where a USB network adapter's kernel line carries the host's own MAC; device serials in the inventory are never passed through it), and `cmdline` masks the value after `UUID=`/`PARTUUID=` in the kernel command line (a root filesystem UUID identifies the installation as surely as a machine-id).

- [ ] **Step 1: Declare the module and write the failing tests**

In `src/main.rs`, after `mod device;` add:

```rust
#[cfg(test)]
mod diag;
```

(The gate stays until Task 7 wires `--support`; see "Module gating rule" above.)

Create `src/diag/mod.rs`:

```rust
//! The diagnostic core behind `--support`: privacy rules (`redact`), the
//! collectors that read the host and its USB tree through injectable roots
//! (`collect`, `inventory`), the bundle writer (`bundle`), and the
//! orchestrator (`support`). Nothing here changes the system; every missing
//! file or failed probe becomes a [`Note`] and the bundle continues.

use serde::Serialize;

pub mod redact;

/// One "unavailable: reason" record. Collectors return these instead of
/// failing; the manifest lists every one so a reporter and a maintainer both
/// know what the bundle lacks.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Note {
    pub item: String,
    pub reason: String,
}

pub fn note(item: &str, reason: impl std::fmt::Display) -> Note {
    Note {
        item: item.to_string(),
        reason: reason.to_string(),
    }
}
```

Create `src/diag/redact.rs` with the tests first (the implementation in Step 3 goes above them):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_under_home_becomes_tilde() {
        let mut r = Redactor::new(Some(Path::new("/home/alice")));
        assert_eq!(r.path(Path::new("/home/alice/.usbtop-ng")), "~/.usbtop-ng");
        assert_eq!(r.path(Path::new("/home/alice")), "~");
        assert_eq!(r.summary(), vec![("home_path".to_string(), 2)]);
    }

    #[test]
    fn a_sibling_directory_sharing_the_prefix_is_left_alone() {
        let mut r = Redactor::new(Some(Path::new("/home/alice")));
        assert_eq!(r.path(Path::new("/home/alice2/x")), "/home/alice2/x");
        assert_eq!(r.path(Path::new("/home/alice-old/x")), "/home/alice-old/x");
        assert!(r.summary().is_empty());
    }

    #[test]
    fn free_text_rewrites_every_occurrence_and_counts_each() {
        let mut r = Redactor::new(Some(Path::new("/home/alice/")));
        let prefs = "usbids_path = \"/home/alice/usb.ids\"\n# was /home/alice/old\n";
        assert_eq!(
            r.text(prefs),
            "usbids_path = \"~/usb.ids\"\n# was ~/old\n"
        );
        assert_eq!(r.summary(), vec![("home_path".to_string(), 2)]);
    }

    #[test]
    fn no_home_or_a_root_home_disables_the_path_rule() {
        let mut none = Redactor::new(None);
        assert_eq!(none.text("/home/alice/x"), "/home/alice/x");
        let mut root = Redactor::new(Some(Path::new("/")));
        assert_eq!(root.text("/etc/passwd"), "/etc/passwd");
        assert!(root.summary().is_empty());
    }

    #[test]
    fn mac_addresses_are_masked_only_when_they_stand_alone() {
        let mut r = Redactor::new(None);
        let line = "usb 1-3: r8152 eth0: MAC 00:1a:2b:3c:4d:5e ready; id 00:1a:2b:3c:4d:5e:ff stays";
        assert_eq!(
            r.mac_addresses(line),
            "usb 1-3: r8152 eth0: MAC xx:xx:xx:xx:xx:xx ready; id 00:1a:2b:3c:4d:5e:ff stays"
        );
        assert_eq!(r.summary(), vec![("mac_address".to_string(), 1)]);
    }

    #[test]
    fn cmdline_masks_filesystem_uuids_and_keeps_everything_else() {
        let mut r = Redactor::new(None);
        let cmd = "BOOT_IMAGE=/boot/vmlinuz root=UUID=307c1732-bacd-4ef4-9050-b4c9e99e5648 ro quiet resume=PARTUUID=abcd-1234";
        assert_eq!(
            r.cmdline(cmd),
            "BOOT_IMAGE=/boot/vmlinuz root=UUID=<redacted> ro quiet resume=PARTUUID=<redacted>"
        );
        assert_eq!(r.summary(), vec![("fs_uuid".to_string(), 2)]);
    }

    #[test]
    fn the_environment_allowlist_is_exactly_five_names() {
        for name in ["TERM", "COLORTERM", "LANG", "LC_ALL", "RUST_LOG"] {
            assert!(Redactor::env_allowlisted(name), "{name}");
        }
        for name in ["HOME", "USER", "LOGNAME", "SSH_CLIENT", "PATH", "term"] {
            assert!(!Redactor::env_allowlisted(name), "{name}");
        }
    }

    #[test]
    fn ssh_presence_is_any_marker_set_and_never_its_value() {
        assert!(Redactor::ssh_present(|n| n == "SSH_CONNECTION"));
        assert!(Redactor::ssh_present(|n| n == "SSH_TTY"));
        assert!(!Redactor::ssh_present(|_| false));
        assert!(!Redactor::ssh_present(|n| n == "SSH_AUTH_SOCK"));
    }

    #[test]
    fn summary_is_sorted_by_rule_name() {
        let mut r = Redactor::new(Some(Path::new("/home/alice")));
        r.mac_addresses("aa:bb:cc:dd:ee:ff");
        r.text("/home/alice/x");
        assert_eq!(
            r.summary(),
            vec![
                ("home_path".to_string(), 1),
                ("mac_address".to_string(), 1)
            ]
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag::redact 2>&1 | tail -5`
Expected: compile errors (`Redactor` not found).

- [ ] **Step 3: Implement `redact.rs`**

Above the test module:

```rust
//! The privacy rules, as pure functions with table tests. The boundary is
//! "host identity out, device identity in": nothing here ever touches a
//! device serial or descriptor; it rewrites the user's home directory to
//! `~`, masks host MAC addresses in kernel log lines, masks filesystem UUIDs
//! in the kernel command line, and decides which environment variables the
//! bundle may record. Every substitution is counted so the manifest can say
//! what was changed.

use std::collections::BTreeMap;
use std::path::Path;

/// The only environment variables whose values a bundle records.
pub const ENV_ALLOWLIST: [&str; 5] = ["TERM", "COLORTERM", "LANG", "LC_ALL", "RUST_LOG"];

/// Variables recorded as present or absent only (see
/// `tui::sync::remote_session` for why any one of them means "over ssh").
pub const SSH_MARKERS: [&str; 3] = ["SSH_TTY", "SSH_CONNECTION", "SSH_CLIENT"];

/// Applies the rules and counts what it changed.
#[derive(Debug, Clone)]
pub struct Redactor {
    /// The home directory to rewrite, without a trailing slash; `None`
    /// disables the path rule (no home known, or a home of `/`, which would
    /// rewrite every absolute path).
    home: Option<String>,
    counts: BTreeMap<&'static str, usize>,
}

impl Redactor {
    pub fn new(home: Option<&Path>) -> Redactor {
        let home = home
            .map(|h| h.to_string_lossy().trim_end_matches('/').to_string())
            .filter(|h| !h.is_empty());
        Redactor {
            home,
            counts: BTreeMap::new(),
        }
    }

    fn bump(&mut self, rule: &'static str) {
        *self.counts.entry(rule).or_insert(0) += 1;
    }

    /// A path under the home directory becomes `~/…`; the home itself
    /// becomes `~`. Anything else is returned as written.
    pub fn path(&mut self, path: &Path) -> String {
        let text = path.to_string_lossy().into_owned();
        self.text(&text)
    }

    /// Every occurrence of the home directory inside free text (a
    /// preferences file, a command line, a report) becomes `~`. An
    /// occurrence counts only at a path boundary: `/home/alice/x` matches,
    /// `/home/alice2/x` and `/home/alice-old` do not.
    pub fn text(&mut self, text: &str) -> String {
        let Some(home) = self.home.clone() else {
            return text.to_string();
        };
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(&home) {
            let after = &rest[at + home.len()..];
            let boundary = after
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')));
            out.push_str(&rest[..at]);
            if boundary {
                out.push('~');
                self.bump("home_path");
            } else {
                out.push_str(&home);
            }
            rest = after;
        }
        out.push_str(rest);
        out
    }

    /// Masks each stand-alone `hh:hh:hh:hh:hh:hh` token as
    /// `xx:xx:xx:xx:xx:xx`. Applied to kernel log lines, where a USB
    /// network adapter's line names the host's own MAC; never to the device
    /// inventory, whose serial strings are device identity and stay.
    pub fn mac_addresses(&mut self, text: &str) -> String {
        const LEN: usize = 17;
        let mut bytes = text.as_bytes().to_vec();
        let mut i = 0;
        while i + LEN <= bytes.len() {
            if is_mac(&bytes[i..i + LEN])
                && !i.checked_sub(1).is_some_and(|p| is_mac_byte(bytes[p]))
                && !bytes.get(i + LEN).is_some_and(|&b| is_mac_byte(b))
            {
                bytes[i..i + LEN].copy_from_slice(b"xx:xx:xx:xx:xx:xx");
                self.bump("mac_address");
                i += LEN;
            } else {
                i += 1;
            }
        }
        // Only ASCII bytes were replaced by ASCII bytes, so the text is
        // still valid UTF-8.
        String::from_utf8(bytes).expect("ASCII-for-ASCII substitution keeps UTF-8 valid")
    }

    /// Masks the value after `UUID=` and `PARTUUID=` in a kernel command
    /// line; every other token is kept whole.
    pub fn cmdline(&mut self, text: &str) -> String {
        let tokens: Vec<String> = text
            .split_whitespace()
            .map(|token| match token.find("UUID=") {
                Some(at) => {
                    self.bump("fs_uuid");
                    format!("{}UUID=<redacted>", &token[..at])
                }
                None => token.to_string(),
            })
            .collect();
        tokens.join(" ")
    }

    pub fn env_allowlisted(name: &str) -> bool {
        ENV_ALLOWLIST.contains(&name)
    }

    /// Whether any ssh marker is set, given a predicate that answers "is this
    /// variable set and non-empty?". The values are never read here.
    pub fn ssh_present(present: impl Fn(&str) -> bool) -> bool {
        SSH_MARKERS.iter().any(|name| present(name))
    }

    /// Every rule that fired and how often, sorted by rule name.
    pub fn summary(&self) -> Vec<(String, usize)> {
        self.counts
            .iter()
            .map(|(rule, n)| (rule.to_string(), *n))
            .collect()
    }
}

fn is_mac_byte(b: u8) -> bool {
    b.is_ascii_hexdigit() || b == b':'
}

fn is_mac(window: &[u8]) -> bool {
    window.iter().enumerate().all(|(i, &b)| {
        if i % 3 == 2 {
            b == b':'
        } else {
            b.is_ascii_hexdigit()
        }
    })
}
```

`Option::is_none_or` is stable since Rust 1.82, inside the 1.88 MSRV.

- [ ] **Step 4: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag:: 2>&1 | grep -E 'test result|FAILED|panicked'`
Expected: 9 tests pass.

- [ ] **Step 5: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/diag/mod.rs src/diag/redact.rs src/main.rs
git commit -m "feat(diag): privacy rules for the support bundle

Adds the diag module scaffold (test-gated until --support consumes it)
and the redaction rules as pure, table-tested functions: home paths to
~, stand-alone MAC addresses in kernel log lines, filesystem UUIDs in
the kernel command line, the five-name environment allowlist, and
ssh-marker presence. Each substitution is counted for the manifest.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 3: Collectors A, B, D, F (`src/diag/collect.rs`)

**Files:**
- Create: `src/diag/collect.rs`
- Modify: `src/diag/mod.rs` (add `pub mod collect;`)
- Modify: `build.rs` (record the compiler version)

**Interfaces:**
- Consumes: `diag::{Note, note}`, `diag::redact::Redactor` (Task 2); `usbmon::UsbmonStatus`, `usbmon::open_nonblocking` (pub(crate)), `usbmon::mmap_ring::MmapReader::probe`, `usbmon::ring::{request_ring_ladder, ring_size}` (pub(crate)); `headless::export::enabled_features` (Task 1).
- Produces:
  ```rust
  pub fn read_trimmed(path: &Path) -> Option<String>;          // NUL-flattening, trimmed
  pub fn os_pretty_name_from(text: &str) -> Option<String>;    // /etc/os-release PRETTY_NAME
  pub struct BuildInfo { .. }   pub fn collect_build(command: &[String], rust_log: Option<String>, effective_uid: u32, under_sudo: bool, redactor: &mut Redactor) -> BuildInfo;
  pub struct HostInfo { .. }    pub fn collect_host(proc_root: &Path, sys_root: &Path, etc_root: &Path, dmi_root: &Path, device_tree_root: &Path, virtualization: Option<String>, redactor: &mut Redactor) -> (HostInfo, Vec<Note>);
  pub fn detect_virtualization() -> Option<String>;            // live: systemd-detect-virt
  pub struct NodeInfo { .. }    pub struct UsbmonInfo { .. }
  pub fn collect_usbmon(status: &Result<UsbmonStatus, String>, dev_root: &Path, debugfs_root: &Path) -> (UsbmonInfo, Vec<Note>);
  pub struct BackendInfo { .. } pub fn probe_backend(buses: &[u8], dev_root: &Path, debugfs_root: &Path, btf_path: &Path) -> BackendInfo;
  pub fn filter_dmesg(text: &str) -> String;                   // pure
  pub fn run_dmesg() -> Result<String, String>;                // live: `dmesg`
  pub struct ConfigInfo { .. }  pub fn collect_config(config_dir: Option<&Path>, preferences_file: Option<&Path>, under_sudo: bool, redactor: &mut Redactor) -> (ConfigInfo, Vec<Note>);
  pub struct TerminalInfo { .. } pub fn collect_terminal(env: &dyn Fn(&str) -> Option<String>, size: Option<(u16, u16)>, stdout_is_tty: bool, stdin_is_tty: bool, sync_mode: &str) -> TerminalInfo;
  ```
  Every struct derives `Debug, Serialize`; every `Option` field is omitted from TOML when `None` (the `toml` serializer skips `None`). Field order in every struct is scalars first, then maps, then vectors of structs, which is the order TOML needs.

- [ ] **Step 1: Record the compiler version at build time**

In `build.rs`, at the top of `main()` before the `CARGO_FEATURE_EBPF` check:

```rust
    // Record the compiler for `--support`'s build.toml (`option_env!`
    // reads it back as `USBTOP_NG_RUSTC`). Best-effort: a missing or odd
    // RUSTC just leaves the value unset.
    if let Some(rustc) = env::var_os("RUSTC") {
        if let Ok(output) = std::process::Command::new(rustc).arg("--version").output() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                println!("cargo:rustc-env=USBTOP_NG_RUSTC={}", text.trim());
            }
        }
    }
```

Update the file's doc comment first line to "Records the compiler version for `--support` and, when the optional `ebpf` feature is enabled, builds the `usbrate` eBPF program into a generated skeleton."

- [ ] **Step 2: Write the failing tests**

Add `pub mod collect;` to `src/diag/mod.rs` after `pub mod redact;`. Create `src/diag/collect.rs` with this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn read_trimmed_flattens_interior_nuls_from_device_tree_string_lists() {
        let temp = tempfile::tempdir().unwrap();
        let compatible = temp.path().join("compatible");
        std::fs::write(&compatible, b"raspberrypi,5-model-b\0brcm,bcm2712\0\n").unwrap();
        assert_eq!(
            read_trimmed(&compatible).as_deref(),
            Some("raspberrypi,5-model-b brcm,bcm2712")
        );
        assert_eq!(read_trimmed(&temp.path().join("missing")), None);
        std::fs::write(temp.path().join("empty"), b"\0\n").unwrap();
        assert_eq!(read_trimmed(&temp.path().join("empty")), None);
    }

    #[test]
    fn os_pretty_name_strips_the_quotes() {
        assert_eq!(
            os_pretty_name_from("NAME=\"Linux Mint\"\nPRETTY_NAME=\"Linux Mint 22.3\"\n").as_deref(),
            Some("Linux Mint 22.3")
        );
        assert_eq!(os_pretty_name_from("NAME=x\n"), None);
    }

    #[test]
    fn build_info_records_the_invocation_without_naming_the_user() {
        let mut r = Redactor::new(Some(Path::new("/home/alice")));
        let info = collect_build(
            &["/home/alice/bin/usbtop-ng".to_string(), "--support".to_string()],
            Some("debug".to_string()),
            0,
            true,
            &mut r,
        );
        assert_eq!(info.command, vec!["~/bin/usbtop-ng", "--support"]);
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(info.running_as_root);
        assert!(info.under_sudo);
        assert_eq!(info.rust_log.as_deref(), Some("debug"));
        assert_eq!(info.arch, std::env::consts::ARCH);
        let text = toml::to_string(&info).unwrap();
        assert!(!text.contains("alice"), "{text}");
    }

    #[test]
    fn host_info_reads_every_source_through_the_injected_roots() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "proc/sys/kernel/osrelease", "7.0.0-30-generic\n");
        write(root, "proc/version", "Linux version 7.0.0-30-generic (buildd@host) #30\n");
        write(root, "proc/cpuinfo", "processor\t: 0\nmodel name\t: AMD Ryzen 9\nprocessor\t: 1\nmodel name\t: AMD Ryzen 9\n");
        write(root, "proc/meminfo", "MemTotal:       32000000 kB\nMemFree: 1 kB\n");
        write(root, "proc/uptime", "12345.67 99999.00\n");
        write(root, "proc/cmdline", "BOOT_IMAGE=/boot/vmlinuz root=UUID=1234-abcd ro\n");
        write(root, "sys/module/usbcore/parameters/autosuspend", "2\n");
        write(root, "sys/module/usbcore/parameters/quirks", "\n");
        write(root, "sys/kernel/security/lockdown", "[none] integrity confidentiality\n");
        write(root, "etc/os-release", "PRETTY_NAME=\"Linux Mint 22.3\"\n");
        write(root, "dmi/product_name", "MG-VCP17A-3080\n");
        write(root, "dmi/sys_vendor", "Example\n");
        // No device tree: an x86 host.
        let mut r = Redactor::new(None);
        let (host, notes) = collect_host(
            &root.join("proc"),
            &root.join("sys"),
            &root.join("etc"),
            &root.join("dmi"),
            &root.join("device-tree"),
            Some("none".to_string()),
            &mut r,
        );
        assert_eq!(host.kernel, "7.0.0-30-generic");
        assert!(host.proc_version.starts_with("Linux version 7.0.0-30-generic"));
        assert_eq!(host.os, "Linux Mint 22.3");
        assert_eq!(host.board, "Example MG-VCP17A-3080");
        assert_eq!(host.soc, "");
        assert_eq!(host.cpu_model, "AMD Ryzen 9");
        assert_eq!(host.cpu_count, 2);
        assert_eq!(host.mem_total_kb, Some(32_000_000));
        assert_eq!(host.uptime_s, Some(12345.67));
        assert_eq!(host.virtualization.as_deref(), Some("none"));
        assert_eq!(host.cmdline, "BOOT_IMAGE=/boot/vmlinuz root=UUID=<redacted> ro");
        assert_eq!(host.usbcore_params.get("autosuspend").map(String::as_str), Some("2"));
        assert_eq!(host.usbcore_params.get("quirks").map(String::as_str), Some(""));
        assert_eq!(host.lockdown, "[none] integrity confidentiality");
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn host_info_notes_what_is_missing_instead_of_failing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "device-tree/model", "Raspberry Pi 5 Model B Rev 1.0\0");
        write(root, "device-tree/compatible", "raspberrypi,5-model-b\0brcm,bcm2712\0");
        let mut r = Redactor::new(None);
        let (host, notes) = collect_host(
            &root.join("proc"),
            &root.join("sys"),
            &root.join("etc"),
            &root.join("dmi"),
            &root.join("device-tree"),
            None,
            &mut r,
        );
        assert_eq!(host.board, "Raspberry Pi 5 Model B Rev 1.0");
        assert_eq!(host.soc, "raspberrypi,5-model-b brcm,bcm2712");
        assert_eq!(host.kernel, "");
        assert_eq!(host.mem_total_kb, None);
        let items: Vec<&str> = notes.iter().map(|n| n.item.as_str()).collect();
        for expected in [
            "proc/sys/kernel/osrelease",
            "proc/version",
            "proc/cpuinfo",
            "proc/meminfo",
            "proc/uptime",
            "proc/cmdline",
            "sys/module/usbcore/parameters",
            "sys/kernel/security/lockdown",
            "etc/os-release",
            "systemd-detect-virt",
        ] {
            assert!(items.contains(&expected), "missing note for {expected}: {items:?}");
        }
    }

    #[test]
    fn detect_virtualization_never_panics() {
        // Live: `systemd-detect-virt` may or may not exist here; either
        // answer is fine, only a panic would not be.
        let _ = detect_virtualization();
    }

    #[test]
    fn usbmon_info_lists_nodes_with_ownership_and_openability() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("usbmon0"), b"").unwrap();
        std::fs::write(dev.join("usbmon1"), b"").unwrap();
        std::fs::write(dev.join("unrelated"), b"").unwrap();
        let debugfs = temp.path().join("usbmon");
        std::fs::create_dir_all(&debugfs).unwrap();
        std::fs::write(debugfs.join("0u"), b"").unwrap();
        std::fs::write(debugfs.join("1u"), b"").unwrap();
        let status = Ok(UsbmonStatus {
            module_loaded: true,
            debugfs_mounted: true,
            usbmon_available: true,
            binary_available: true,
            text_available: true,
            permission_denied: false,
            available_buses: vec![0, 1],
        });
        let (info, notes) = collect_usbmon(&status, &dev, &debugfs);
        assert!(info.module_loaded);
        assert_eq!(info.available_buses, vec![0, 1]);
        assert_eq!(info.nodes.len(), 2);
        assert_eq!(info.nodes[0].path, dev.join("usbmon0").display().to_string());
        assert_eq!(info.nodes[0].mode_octal.len(), 4);
        assert!(info.nodes[0].openable);
        assert_eq!(info.debugfs_entries, vec!["0u", "1u"]);
        assert!(info.status_error.is_none());
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn usbmon_info_carries_a_failed_status_probe_as_a_note() {
        let temp = tempfile::tempdir().unwrap();
        let status = Err("could not read /proc/modules".to_string());
        let (info, notes) = collect_usbmon(&status, temp.path(), &temp.path().join("nope"));
        assert_eq!(info.status_error.as_deref(), Some("could not read /proc/modules"));
        assert!(!info.usbmon_available);
        assert!(info.nodes.is_empty());
        assert_eq!(notes.len(), 2, "status and debugfs: {notes:?}");
    }

    #[test]
    fn backend_probe_walks_the_same_chain_as_start_monitoring() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("dev");
        let debugfs = temp.path().join("usbmon");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::create_dir_all(&debugfs).unwrap();
        let btf = temp.path().join("vmlinux");

        // Nothing at all: no interface.
        let none = probe_backend(&[], &dev, &debugfs, &btf);
        assert_eq!(none.would_select, "none");
        assert!(!none.btf_present);
        assert_eq!(none.ebpf_built_in, cfg!(feature = "ebpf"));

        // Only the debugfs text file for the aggregate bus.
        std::fs::write(debugfs.join("0u"), b"").unwrap();
        let text = probe_backend(&[0, 1], &dev, &debugfs, &btf);
        assert_eq!(text.would_select, "text");
        assert_eq!(text.probed_bus, Some(0));

        // A regular file where the binary node would be: it opens (so the
        // read()-based reader would take it) but has no ring, exactly what
        // `MmapReader::probe` and `ring_size` answer for a fixture file.
        std::fs::write(dev.join("usbmon0"), b"").unwrap();
        std::fs::write(&btf, b"").unwrap();
        let binary = probe_backend(&[0, 1], &dev, &debugfs, &btf);
        assert_eq!(binary.would_select, "binary");
        assert_eq!(binary.ring_bytes, None);
        assert!(binary.btf_present);

        // No aggregate node: the first per-bus node is probed instead.
        std::fs::remove_file(dev.join("usbmon0")).unwrap();
        std::fs::remove_file(debugfs.join("0u")).unwrap();
        std::fs::write(dev.join("usbmon2"), b"").unwrap();
        let per_bus = probe_backend(&[2, 3], &dev, &debugfs, &btf);
        assert_eq!(per_bus.probed_bus, Some(2));
        assert_eq!(per_bus.would_select, "binary");
    }

    #[test]
    fn dmesg_filter_keeps_usb_lines_case_insensitively_and_whole() {
        let text = "[    0.1] Linux version 7.0\n\
                    [    1.2] usb 1-4: new high-speed USB device number 3 using xhci_hcd\n\
                    [    1.3] usb 1-4: SerialNumber: 0123ABCD\n\
                    [    2.0] systemd[1]: Set hostname to <box>.\n\
                    [    3.0] thunderbolt 0-1: new device found\n\
                    [    4.0] hub 3-1:1.0: USB hub found\n\
                    [    5.0] DWC2 controller ready\n\
                    [    6.0] usbmon: debugfs is not available\n";
        let kept = filter_dmesg(text);
        assert!(kept.contains("SerialNumber: 0123ABCD"), "device lines stay whole");
        assert!(kept.contains("thunderbolt 0-1"));
        assert!(kept.contains("USB hub found"));
        assert!(kept.contains("DWC2"));
        assert!(kept.contains("usbmon: debugfs"));
        assert!(!kept.contains("Linux version"));
        assert!(!kept.contains("hostname"));
        assert_eq!(kept.lines().count(), 6);
    }

    #[test]
    fn run_dmesg_returns_text_or_a_reason_and_never_panics() {
        match run_dmesg() {
            Ok(text) => assert!(text.lines().all(|line| {
                let lower = line.to_lowercase();
                DMESG_KEYWORDS.iter().any(|k| lower.contains(k))
            })),
            Err(reason) => assert!(!reason.is_empty()),
        }
    }

    #[test]
    fn config_info_redacts_the_directory_and_copies_both_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home").join("alice");
        let dir = home.join(".usbtop-ng");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("preferences.toml"), format!("usbids_path = \"{}/usb.ids\"\n", home.display())).unwrap();
        std::fs::write(dir.join("internal-devices.toml"), "captured_unix = 1\n").unwrap();
        let prefs = dir.join("preferences.toml");
        let mut r = Redactor::new(Some(home.as_path()));
        let (info, notes) = collect_config(Some(dir.as_path()), Some(prefs.as_path()), true, &mut r);
        assert_eq!(info.dir.as_deref(), Some("~/.usbtop-ng"));
        assert_eq!(info.dir_mode_octal.as_deref().map(str::len), Some(4));
        assert_eq!(info.preferences_path.as_deref(), Some("~/.usbtop-ng/preferences.toml"));
        assert_eq!(info.preferences.as_deref(), Some("usbids_path = \"~/usb.ids\"\n"));
        assert_eq!(info.internal_devices.as_deref(), Some("captured_unix = 1\n"));
        assert_eq!(info.sudo_resolution, "home resolved to ~ (sudo invoker)");
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn config_info_notes_a_missing_directory_and_missing_files() {
        let temp = tempfile::tempdir().unwrap();
        let mut r = Redactor::new(None);
        let absent = temp.path().join("absent");
        let (info, notes) = collect_config(Some(absent.as_path()), None, false, &mut r);
        assert!(info.dir_mode_octal.is_none());
        assert!(info.preferences.is_none());
        assert_eq!(info.sudo_resolution, "not under sudo");
        assert_eq!(notes.len(), 3, "dir, preferences, internal-devices: {notes:?}");
        let (_, none) = collect_config(None, None, false, &mut r);
        assert_eq!(none[0].item, "config directory");
    }

    #[test]
    fn terminal_info_records_allowlisted_values_and_ssh_presence_only() {
        let env = |name: &str| -> Option<String> {
            match name {
                "TERM" => Some("xterm-256color".into()),
                "COLORTERM" => Some("truecolor".into()),
                "LANG" => Some("en_US.UTF-8".into()),
                "SSH_CONNECTION" => Some("10.0.0.2 51234 10.0.0.1 22".into()),
                "HOME" => Some("/home/alice".into()),
                _ => None,
            }
        };
        let info = collect_terminal(&env, Some((120, 40)), true, false, "unsupported");
        assert_eq!(info.term.as_deref(), Some("xterm-256color"));
        assert_eq!(info.colorterm.as_deref(), Some("truecolor"));
        assert_eq!(info.lang.as_deref(), Some("en_US.UTF-8"));
        assert_eq!(info.lc_all, None);
        assert_eq!((info.cols, info.rows), (Some(120), Some(40)));
        assert!(info.stdout_is_tty);
        assert!(!info.stdin_is_tty);
        assert!(info.ssh_present);
        assert_eq!(info.sync_mode, "unsupported");
        let text = toml::to_string(&info).unwrap();
        assert!(!text.contains("10.0.0"), "ssh values never recorded: {text}");
        assert!(!text.contains("alice"), "{text}");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag::collect 2>&1 | tail -5`
Expected: compile errors (nothing in `collect.rs` exists yet).

- [ ] **Step 4: Implement the collectors**

Above the test module in `src/diag/collect.rs`:

```rust
//! Collectors A (build), B (host, usbmon, backend, dmesg), D (configuration),
//! and F (terminal) for the support bundle. Every collector reads through
//! roots its caller passes in, so tests inject a fake tree the way
//! `DeviceManager::with_sysfs_base` does, and every collector returns notes
//! instead of errors: a missing file is a fact about the host, not a failure
//! of the bundle. The device inventory (collector C) lives in `inventory.rs`.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use super::redact::Redactor;
use super::{note, Note};
use crate::headless::export::enabled_features;
use crate::usbmon::mmap_ring::MmapReader;
use crate::usbmon::{open_nonblocking, ring, UsbmonStatus};

/// Read a sysfs/procfs/device-tree file, trimmed. Device-tree files
/// (`model`, `compatible`) are NUL-separated string lists, so beyond
/// edge-trimming, interior NULs are flattened to single spaces:
/// `"raspberrypi,5-model-b\0brcm,bcm2712\0"` reads as
/// `"raspberrypi,5-model-b brcm,bcm2712"` rather than carrying a raw NUL
/// into TOML (where it would serialize as a `\u0000` escape). Unreadable,
/// missing, or empty files are `None`.
pub fn read_trimmed(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!trimmed.is_empty()).then(|| trimmed.replace('\0', " "))
}

/// `PRETTY_NAME` from `/etc/os-release` text, quotes stripped.
pub fn os_pretty_name_from(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim().trim_matches('"').to_string())
}

// --- A. build and invocation ---------------------------------------------

#[derive(Debug, Serialize)]
pub struct BuildInfo {
    pub version: String,
    pub features: Vec<&'static str>,
    pub arch: &'static str,
    /// `rustc --version` at build time (see `build.rs`); absent when the
    /// build script could not run the compiler.
    pub rustc: Option<&'static str>,
    /// The command line as run, home paths rewritten.
    pub command: Vec<String>,
    pub effective_uid: u32,
    pub running_as_root: bool,
    pub under_sudo: bool,
    pub rust_log: Option<String>,
}

pub fn collect_build(
    command: &[String],
    rust_log: Option<String>,
    effective_uid: u32,
    under_sudo: bool,
    redactor: &mut Redactor,
) -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        features: enabled_features(),
        arch: std::env::consts::ARCH,
        rustc: option_env!("USBTOP_NG_RUSTC"),
        command: command.iter().map(|arg| redactor.text(arg)).collect(),
        effective_uid,
        running_as_root: effective_uid == 0,
        under_sudo,
        rust_log,
    }
}

// --- B. host --------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct HostInfo {
    pub kernel: String,
    pub proc_version: String,
    pub os: String,
    /// Device-tree `model`, else DMI `sys_vendor product_name`.
    pub board: String,
    /// Device-tree `compatible`; empty on DMI hosts.
    pub soc: String,
    pub cpu_model: String,
    pub cpu_count: usize,
    pub mem_total_kb: Option<u64>,
    pub uptime_s: Option<f64>,
    /// `systemd-detect-virt`'s answer, when the tool exists.
    pub virtualization: Option<String>,
    /// `/proc/cmdline` with filesystem UUIDs masked.
    pub cmdline: String,
    pub lockdown: String,
    /// Every file under `/sys/module/usbcore/parameters/`.
    pub usbcore_params: BTreeMap<String, String>,
}

/// Read `rel` under `root`, noting its absence under the name `label`.
fn read_or_note(root: &Path, rel: &str, label: &str, notes: &mut Vec<Note>) -> Option<String> {
    let value = read_trimmed(&root.join(rel));
    if value.is_none() {
        notes.push(note(label, "not readable"));
    }
    value
}

pub fn collect_host(
    proc_root: &Path,
    sys_root: &Path,
    etc_root: &Path,
    dmi_root: &Path,
    device_tree_root: &Path,
    virtualization: Option<String>,
    redactor: &mut Redactor,
) -> (HostInfo, Vec<Note>) {
    let mut notes = Vec::new();
    let kernel = read_or_note(proc_root, "sys/kernel/osrelease", "proc/sys/kernel/osrelease", &mut notes);
    let proc_version = read_or_note(proc_root, "version", "proc/version", &mut notes);
    let os = read_or_note(etc_root, "os-release", "etc/os-release", &mut notes)
        .and_then(|text| os_pretty_name_from(&text));

    let board = match read_trimmed(&device_tree_root.join("model")) {
        Some(model) => model,
        None => {
            let vendor = read_trimmed(&dmi_root.join("sys_vendor")).unwrap_or_default();
            let product = read_trimmed(&dmi_root.join("product_name")).unwrap_or_default();
            let joined = format!("{vendor} {product}");
            let joined = joined.trim().to_string();
            if joined.is_empty() {
                notes.push(note("board", "neither device-tree model nor DMI product name is readable"));
            }
            joined
        }
    };
    let soc = read_trimmed(&device_tree_root.join("compatible")).unwrap_or_default();

    let cpuinfo = read_or_note(proc_root, "cpuinfo", "proc/cpuinfo", &mut notes);
    let cpu_model = cpuinfo
        .as_deref()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("model name") || l.starts_with("Model"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_default();
    let cpu_count = cpuinfo
        .as_deref()
        .map(|text| text.lines().filter(|l| l.starts_with("processor")).count())
        .unwrap_or(0);

    let mem_total_kb = read_or_note(proc_root, "meminfo", "proc/meminfo", &mut notes).and_then(|text| {
        text.lines()
            .find(|l| l.starts_with("MemTotal:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
    });
    let uptime_s = read_or_note(proc_root, "uptime", "proc/uptime", &mut notes)
        .and_then(|text| text.split_whitespace().next().and_then(|n| n.parse().ok()));
    if virtualization.is_none() {
        notes.push(note("systemd-detect-virt", "not available"));
    }
    let cmdline = read_or_note(proc_root, "cmdline", "proc/cmdline", &mut notes)
        .map(|text| redactor.cmdline(&text))
        .unwrap_or_default();
    let lockdown = read_or_note(sys_root, "kernel/security/lockdown", "sys/kernel/security/lockdown", &mut notes)
        .unwrap_or_default();

    let mut usbcore_params = BTreeMap::new();
    let params_dir = sys_root.join("module/usbcore/parameters");
    match std::fs::read_dir(&params_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // Parameters may legitimately be empty (`quirks`), so an empty
                // read is a value, not an absence.
                let value = std::fs::read(entry.path())
                    .map(|b| String::from_utf8_lossy(&b).trim().to_string())
                    .unwrap_or_default();
                usbcore_params.insert(name, value);
            }
        }
        Err(e) => notes.push(note("sys/module/usbcore/parameters", e)),
    }

    (
        HostInfo {
            kernel: kernel.unwrap_or_default(),
            proc_version: proc_version.unwrap_or_default(),
            os: os.unwrap_or_default(),
            board,
            soc,
            cpu_model,
            cpu_count,
            mem_total_kb,
            uptime_s,
            virtualization,
            cmdline,
            lockdown,
            usbcore_params,
        },
        notes,
    )
}

/// Live: `systemd-detect-virt`'s first output line (`none` on bare metal).
/// `None` when the tool is missing or fails to run.
pub fn detect_virtualization() -> Option<String> {
    let output = Command::new("systemd-detect-virt").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next()?.trim();
    (!first.is_empty()).then(|| first.to_string())
}

// --- B. usbmon and the backend probe ------------------------------------

#[derive(Debug, Serialize)]
pub struct NodeInfo {
    pub path: String,
    pub owner_uid: u32,
    pub group_gid: u32,
    pub mode_octal: String,
    /// Whether this process could open the node (dropped at once, so the
    /// probe never pins the module).
    pub openable: bool,
}

/// `check_usbmon_status` exactly as startup sees it, plus the `/dev/usbmon*`
/// nodes with ownership and mode, and the debugfs directory listing.
#[derive(Debug, Serialize)]
pub struct UsbmonInfo {
    pub module_loaded: bool,
    pub debugfs_mounted: bool,
    pub usbmon_available: bool,
    pub binary_available: bool,
    pub text_available: bool,
    pub permission_denied: bool,
    pub available_buses: Vec<u8>,
    pub status_error: Option<String>,
    pub debugfs_entries: Vec<String>,
    pub nodes: Vec<NodeInfo>,
}

pub fn collect_usbmon(
    status: &Result<UsbmonStatus, String>,
    dev_root: &Path,
    debugfs_root: &Path,
) -> (UsbmonInfo, Vec<Note>) {
    let mut notes = Vec::new();
    let (status, status_error) = match status {
        Ok(s) => (s.clone(), None),
        Err(e) => {
            notes.push(note("usbmon status probe", e));
            (
                UsbmonStatus {
                    module_loaded: false,
                    debugfs_mounted: false,
                    usbmon_available: false,
                    binary_available: false,
                    text_available: false,
                    permission_denied: false,
                    available_buses: Vec::new(),
                },
                Some(e.clone()),
            )
        }
    };

    let mut nodes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dev_root) {
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.strip_prefix("usbmon").is_some_and(|rest| rest.parse::<u8>().is_ok()))
            })
            .collect();
        paths.sort_by_key(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("usbmon"))
                .and_then(|n| n.parse::<u8>().ok())
                .unwrap_or(u8::MAX)
        });
        for path in paths {
            if let Ok(meta) = std::fs::metadata(&path) {
                nodes.push(NodeInfo {
                    path: path.display().to_string(),
                    owner_uid: meta.uid(),
                    group_gid: meta.gid(),
                    mode_octal: format!("{:04o}", meta.mode() & 0o7777),
                    openable: open_nonblocking(&path).is_ok(),
                });
            }
        }
    }

    let debugfs_entries = match std::fs::read_dir(debugfs_root) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
        Err(e) => {
            notes.push(note("debugfs usbmon directory", e));
            Vec::new()
        }
    };

    (
        UsbmonInfo {
            module_loaded: status.module_loaded,
            debugfs_mounted: status.debugfs_mounted,
            usbmon_available: status.usbmon_available,
            binary_available: status.binary_available,
            text_available: status.text_available,
            permission_denied: status.permission_denied,
            available_buses: status.available_buses,
            status_error,
            debugfs_entries,
            nodes,
        },
        notes,
    )
}

/// Which source `usbmon::monitor::start_monitoring` would pick and why,
/// found with the same probes it uses (`MmapReader::probe`, then a
/// non-blocking open, then the debugfs file) without starting a capture.
#[derive(Debug, Serialize)]
pub struct BackendInfo {
    /// `"mmap"`, `"binary"`, `"text"`, or `"none"`.
    pub would_select: &'static str,
    pub reason: String,
    /// The bus whose node was probed: 0 (the aggregate) when it is listed,
    /// else the first bus, else none.
    pub probed_bus: Option<u8>,
    /// The ring size the kernel granted after the ladder, on a mappable node.
    pub ring_bytes: Option<usize>,
    pub ebpf_built_in: bool,
    pub btf_present: bool,
}

pub fn probe_backend(buses: &[u8], dev_root: &Path, debugfs_root: &Path, btf_path: &Path) -> BackendInfo {
    let ebpf_built_in = cfg!(feature = "ebpf");
    let btf_present = btf_path.exists();
    let probed_bus = if buses.contains(&0) {
        Some(0)
    } else {
        buses.first().copied()
    };
    let Some(bus) = probed_bus else {
        return BackendInfo {
            would_select: "none",
            reason: "no usbmon bus is available".to_string(),
            probed_bus,
            ring_bytes: None,
            ebpf_built_in,
            btf_present,
        };
    };
    let node = dev_root.join(format!("usbmon{bus}"));
    let text_file = debugfs_root.join(format!("{bus}u"));

    let (would_select, reason, ring_bytes) = if MmapReader::probe(&node) {
        // The ladder resizes only this open descriptor's ring; the kernel
        // frees it with the file, so the host is left as found.
        let ring_bytes = open_nonblocking(&node).ok().and_then(|file| {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            ring::request_ring_ladder(fd, &node);
            ring::ring_size(fd).ok()
        });
        ("mmap", format!("{} opened and its ring mapped", node.display()), ring_bytes)
    } else if open_nonblocking(&node).is_ok() {
        ("binary", format!("{} opened but its ring could not be mapped", node.display()), None)
    } else if text_file.exists() {
        ("text", format!("{} could not be opened; {} exists", node.display(), text_file.display()), None)
    } else {
        ("none", format!("neither {} nor {} can be used", node.display(), text_file.display()), None)
    };
    BackendInfo {
        would_select,
        reason,
        probed_bus,
        ring_bytes,
        ebpf_built_in,
        btf_present,
    }
}

// --- B. dmesg -------------------------------------------------------------

const DMESG_KEYWORDS: [&str; 8] = ["usb", "xhci", "ehci", "ohci", "dwc", "thunderbolt", "hub", "usbmon"];

/// Keep the lines that mention USB, a host controller, Thunderbolt, a hub,
/// or usbmon (case-insensitive), whole. Host identity never appears on
/// those lines except a USB network adapter's MAC, which the caller masks
/// with `Redactor::mac_addresses`.
pub fn filter_dmesg(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if DMESG_KEYWORDS.iter().any(|k| lower.contains(k)) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Live: run `dmesg` and filter it. `Err` carries the reason (the tool is
/// missing, or the kernel restricts the log to root) for a note.
pub fn run_dmesg() -> Result<String, String> {
    let output = Command::new("dmesg")
        .output()
        .map_err(|e| format!("could not run dmesg: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("dmesg exited with {}: {}", output.status, stderr.trim()));
    }
    Ok(filter_dmesg(&String::from_utf8_lossy(&output.stdout)))
}

// --- D. configuration -----------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ConfigInfo {
    /// The resolved config directory as `~/…`.
    pub dir: Option<String>,
    pub dir_mode_octal: Option<String>,
    pub preferences_path: Option<String>,
    /// `"home resolved to ~ (sudo invoker)"` or `"not under sudo"`.
    pub sudo_resolution: &'static str,
    /// File bodies, redacted, written as their own files by the bundle
    /// writer rather than inlined into `config.toml`.
    #[serde(skip)]
    pub preferences: Option<String>,
    #[serde(skip)]
    pub internal_devices: Option<String>,
}

pub fn collect_config(
    config_dir: Option<&Path>,
    preferences_file: Option<&Path>,
    under_sudo: bool,
    redactor: &mut Redactor,
) -> (ConfigInfo, Vec<Note>) {
    let mut notes = Vec::new();
    let sudo_resolution = if under_sudo {
        "home resolved to ~ (sudo invoker)"
    } else {
        "not under sudo"
    };
    let Some(dir) = config_dir else {
        notes.push(note("config directory", "could not be resolved (HOME is not set)"));
        return (
            ConfigInfo {
                dir: None,
                dir_mode_octal: None,
                preferences_path: None,
                sudo_resolution,
                preferences: None,
                internal_devices: None,
            },
            notes,
        );
    };
    let dir_mode_octal = match std::fs::metadata(dir) {
        Ok(meta) => Some(format!("{:04o}", meta.mode() & 0o7777)),
        Err(e) => {
            notes.push(note("config directory", e));
            None
        }
    };
    let preferences_file = preferences_file
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.join("preferences.toml"));
    let preferences = match std::fs::read_to_string(&preferences_file) {
        Ok(text) => Some(redactor.text(&text)),
        Err(e) => {
            notes.push(note("preferences.toml", e));
            None
        }
    };
    let internal_devices = match std::fs::read_to_string(dir.join("internal-devices.toml")) {
        Ok(text) => Some(redactor.text(&text)),
        Err(e) => {
            notes.push(note("internal-devices.toml", e));
            None
        }
    };
    (
        ConfigInfo {
            dir: Some(redactor.path(dir)),
            dir_mode_octal,
            preferences_path: Some(redactor.path(&preferences_file)),
            sudo_resolution,
            preferences,
            internal_devices,
        },
        notes,
    )
}

// --- F. terminal ----------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct TerminalInfo {
    pub term: Option<String>,
    pub colorterm: Option<String>,
    pub lang: Option<String>,
    pub lc_all: Option<String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub stdout_is_tty: bool,
    pub stdin_is_tty: bool,
    /// Whether an ssh marker variable is set; its value is never recorded.
    pub ssh_present: bool,
    /// The synchronized-output decision: `supported`, `unsupported`, or a
    /// `not probed: …` reason.
    pub sync_mode: String,
}

/// Pure: `env` answers each variable lookup, so the live gatherer in
/// `diag::support` and the tests share one function.
pub fn collect_terminal(
    env: &dyn Fn(&str) -> Option<String>,
    size: Option<(u16, u16)>,
    stdout_is_tty: bool,
    stdin_is_tty: bool,
    sync_mode: &str,
) -> TerminalInfo {
    // The allowlist is enforced here, not just documented: a name outside
    // it can never reach the bundle by value.
    let allowed = |name: &str| {
        if Redactor::env_allowlisted(name) {
            env(name)
        } else {
            None
        }
    };
    TerminalInfo {
        term: allowed("TERM"),
        colorterm: allowed("COLORTERM"),
        lang: allowed("LANG"),
        lc_all: allowed("LC_ALL"),
        cols: size.map(|s| s.0),
        rows: size.map(|s| s.1),
        stdout_is_tty,
        stdin_is_tty,
        ssh_present: Redactor::ssh_present(|name| env(name).is_some_and(|v| !v.is_empty())),
        sync_mode: sync_mode.to_string(),
    }
}
```

`UsbmonStatus` already derives `Clone` (`src/usbmon/mod.rs:38`). `open_nonblocking`, `ring::request_ring_ladder`, and `ring::ring_size` are `pub(crate)`; `MmapReader::probe` is `pub`. The unused-import lint is satisfied because every import above is used.

- [ ] **Step 5: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag:: 2>&1 | grep -E 'test result|FAILED|panicked'`
Expected: all pass (9 from Task 2 plus 15 here).

- [ ] **Step 6: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add build.rs src/diag/mod.rs src/diag/collect.rs
git commit -m "feat(diag): build, host, usbmon, backend, dmesg, config, and terminal collectors

Each collector reads through injected roots and returns typed data plus
unavailable notes, never an error. The backend probe uses the same
MmapReader::probe / open / debugfs chain start_monitoring uses and
reports the ring size the kernel grants, without starting a capture.
build.rs records rustc --version for build.toml.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 4: Device inventory (`src/diag/inventory.rs`)

**Files:**
- Create: `src/diag/inventory.rs`
- Modify: `src/diag/mod.rs` (add `pub mod inventory;`)
- Modify: `src/usbids/mod.rs` (`active_source` and `parse_header_date` become `pub(crate)`)

**Interfaces:**
- Consumes: `diag::{Note, note}`, `diag::redact::Redactor`, `diag::collect::read_trimmed`; `usbids::{UsbIds, active_source, parse_header_date}`.
- Produces:
  ```rust
  pub struct UsbidsInfo { pub source: Option<String>, pub date: Option<String> }
  pub fn usbids_info(chain: &[&Path], redactor: &mut Redactor) -> UsbidsInfo;
  pub struct UsbInventory { pub usbids: UsbidsInfo, pub controllers: Vec<ControllerInfo>, pub devices: Vec<UsbDeviceInfo> }
  pub fn collect_usb_inventory(sysfs_devices: &Path, usbids: Option<&UsbIds>, usbids_info: UsbidsInfo) -> (UsbInventory, Vec<Note>);
  pub struct DescriptorBlob { pub port_chain: String, pub descriptors: Vec<u8>, pub bos: Option<Vec<u8>> }
  pub fn read_descriptor_blobs(sysfs_devices: &Path) -> (Vec<DescriptorBlob>, Vec<Note>);
  pub struct AttrDump { pub name: String, pub attrs: BTreeMap<String, String> }
  pub fn dump_attrs(root: &Path, max_depth: usize) -> (Vec<AttrDump>, Vec<Note>);
  pub const ATTR_VALUE_CAP: usize = 4096;
  ```

Kernel semantics this task relies on, verified against `v7.0` (cite these in the module doc comment):
- `drivers/usb/core/sysfs.c:855-892` `descriptors_read`: the `descriptors` file is the device descriptor followed by each configuration's raw descriptors up to that configuration's `wTotalLength`, and a read past that returns 0 bytes; line 893 declares the attribute size as `18 + 65535`, which is why `stat` reports 65553 while `std::fs::read` (which reads to EOF) returns the real length (989 bytes for the development host's webcam).
- `sysfs.c:895-916` `bos_descriptors_read` returns the BOS descriptor to its `wTotalLength`; `sysfs.c:927-944` `dev_bin_attrs_are_visible` hides the file entirely when the device has no BOS, so its absence is normal, not a note.
- `sysfs.c:1105-1121` and `1262-1284`: the `iad_*` interface attributes exist only on an interface that belongs to an interface association.
- `drivers/usb/core/port.c:166-227`: `location` prints `0x%08x`, `connect_type` is one of `hotplug`, `hardwired`, `not used`, `unknown`, `state` a device-state string, `over_current_count` a count, `quirks` `%08x`; `port.c:484-516` `link_peers` creates the `peer` symlink in each of two paired ports (USB 2 and USB 3 ports of one connector).
- `drivers/usb/core/endpoint.c:47-116`: `bEndpointAddress`, `bmAttributes`, `bInterval` are `%02x`, `wMaxPacketSize` is `%04x` of `usb_endpoint_maxp`, `type` is `Control`/`Isoc`/`Bulk`/`Interrupt`, `direction` is `both`/`in`/`out`; line 168 names each directory `ep_%02x`.

- [ ] **Step 1: Expose the two usbids helpers**

In `src/usbids/mod.rs` change `fn active_source<'a>(paths: &[&'a Path]) -> Option<&'a Path>` to `pub(crate) fn active_source<'a>(…)` and `fn parse_header_date(text: &str) -> Option<(u16, u8, u8)>` to `pub(crate) fn parse_header_date(…)`. No other change.

- [ ] **Step 2: Write the failing tests**

Add `pub mod inventory;` to `src/diag/mod.rs`. Create `src/diag/inventory.rs` with this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A sysfs tree shaped like the real one: one xHCI controller with a
    /// symlinked root hub `usb3`, a hub `3-1` with two ports (one carrying a
    /// `peer` link), a leaf `3-1.2` with two interfaces (one in an IAD) and
    /// endpoints, and an 18-byte device descriptor blob.
    fn build_tree(root: &Path) -> std::path::PathBuf {
        let devices = root.join("devices");
        let ctrl = root.join("pci/0000:06:00.3");
        write(&ctrl, "vendor", "0x1022\n");
        write(&ctrl, "device", "0x1639\n");
        write(&ctrl, "revision", "0x00\n");
        std::fs::create_dir_all(root.join("drivers/xhci_hcd")).unwrap();
        std::os::unix::fs::symlink(root.join("drivers/xhci_hcd"), ctrl.join("driver")).unwrap();
        let usb3 = ctrl.join("usb3");
        write(&usb3, "busnum", "3\n");
        write(&usb3, "devnum", "1\n");
        write(&usb3, "idVendor", "1d6b\n");
        write(&usb3, "idProduct", "0002\n");
        write(&usb3, "speed", "480\n");
        write(&usb3, "maxchild", "4\n");
        write(&usb3, "descriptors", "");
        std::fs::create_dir_all(&devices).unwrap();
        std::os::unix::fs::symlink(&usb3, devices.join("usb3")).unwrap();

        let hub = devices.join("3-1");
        write(&hub, "busnum", "3\n");
        write(&hub, "devnum", "2\n");
        write(&hub, "idVendor", "05e3\n");
        write(&hub, "idProduct", "0610\n");
        write(&hub, "bcdDevice", "0663\n");
        write(&hub, "speed", "480\n");
        write(&hub, "maxchild", "4\n");
        write(&hub, "bDeviceClass", "09\n");
        write(&hub, "version", " 2.10\n");
        write(&hub, "descriptors", "\x12\x01\x10\x02\x09\x00\x01\x40\xe3\x05\x10\x06\x63\x06\x00\x01\x00\x01");
        write(&hub, "power/control", "auto\n");
        write(&hub, "power/runtime_status", "active\n");
        let hub_if = hub.join("3-1:1.0");
        write(&hub_if, "bInterfaceNumber", "00\n");
        write(&hub_if, "bInterfaceClass", "09\n");
        write(&hub_if, "bNumEndpoints", "01\n");
        std::fs::create_dir_all(root.join("drivers/hub")).unwrap();
        std::os::unix::fs::symlink(root.join("drivers/hub"), hub_if.join("driver")).unwrap();
        let port1 = hub_if.join("3-1-port1");
        write(&port1, "connect_type", "hotplug\n");
        write(&port1, "location", "0x00000001\n");
        write(&port1, "over_current_count", "0\n");
        write(&port1, "quirks", "00000000\n");
        write(&port1, "state", "configured\n");
        let peer_target = root.join("elsewhere/4-1:1.0/4-1-port1");
        std::fs::create_dir_all(&peer_target).unwrap();
        std::os::unix::fs::symlink(&peer_target, port1.join("peer")).unwrap();
        let port2 = hub_if.join("3-1-port2");
        write(&port2, "connect_type", "not used\n");
        write(&port2, "over_current_count", "0\n");

        let leaf = devices.join("3-1.2");
        write(&leaf, "busnum", "3\n");
        write(&leaf, "devnum", "5\n");
        write(&leaf, "idVendor", "04f2\n");
        write(&leaf, "idProduct", "b71a\n");
        write(&leaf, "serial", "SN0001\n");
        write(&leaf, "manufacturer", "SunplusIT Inc\n");
        write(&leaf, "product", "HD Webcam\n");
        write(&leaf, "speed", "480\n");
        write(&leaf, "bMaxPower", "500mA\n");
        write(&leaf, "bNumInterfaces", " 2\n");
        write(&leaf, "descriptors", "\x12\x01\x00\x02\xef\x02\x01\x40\xf2\x04\x1a\xb7\x03\x00\x01\x02\x03\x01");
        write(&leaf, "bos_descriptors", "\x05\x0f\x05\x00\x00");
        write(&leaf, "physical_location/panel", "front\n");
        write(&leaf, "physical_location/lid", "no\n");
        let if0 = leaf.join("3-1.2:1.0");
        write(&if0, "bInterfaceNumber", "00\n");
        write(&if0, "bAlternateSetting", " 0\n");
        write(&if0, "bInterfaceClass", "0e\n");
        write(&if0, "bInterfaceSubClass", "01\n");
        write(&if0, "bInterfaceProtocol", "01\n");
        write(&if0, "bNumEndpoints", "01\n");
        write(&if0, "interface", "HD Webcam\n");
        write(&if0, "iad_bFirstInterface", "00\n");
        write(&if0, "iad_bInterfaceCount", "02\n");
        write(&if0, "iad_bFunctionClass", "0e\n");
        write(&if0, "iad_bFunctionSubClass", "03\n");
        write(&if0, "iad_bFunctionProtocol", "00\n");
        std::fs::create_dir_all(root.join("drivers/uvcvideo")).unwrap();
        std::os::unix::fs::symlink(root.join("drivers/uvcvideo"), if0.join("driver")).unwrap();
        let ep = if0.join("ep_87");
        write(&ep, "bEndpointAddress", "87\n");
        write(&ep, "bmAttributes", "03\n");
        write(&ep, "wMaxPacketSize", "0010\n");
        write(&ep, "bInterval", "08\n");
        write(&ep, "direction", "in\n");
        write(&ep, "type", "Interrupt\n");
        let if1 = leaf.join("3-1.2:1.1");
        write(&if1, "bInterfaceNumber", "01\n");
        write(&if1, "bInterfaceClass", "0e\n");
        write(&if1, "bInterfaceSubClass", "02\n");
        write(&if1, "bNumEndpoints", "01\n");
        let ep81 = if1.join("ep_81");
        write(&ep81, "bEndpointAddress", "81\n");
        write(&ep81, "bmAttributes", "05\n");
        write(&ep81, "wMaxPacketSize", "0c00\n");
        write(&ep81, "direction", "in\n");
        write(&ep81, "type", "Isoc\n");
        devices
    }

    #[test]
    fn inventory_reads_devices_interfaces_endpoints_ports_and_controllers() {
        let temp = tempfile::tempdir().unwrap();
        let devices = build_tree(temp.path());
        let db = UsbIds::parse("04f2  Chicony Electronics Co., Ltd\n\tb71a  HD WebCam\n05e3  Genesys Logic, Inc.\n\t0610  Hub\n");
        let (inv, notes) = collect_usb_inventory(&devices, Some(&db), UsbidsInfo::default());
        assert!(notes.is_empty(), "{notes:?}");

        assert_eq!(inv.controllers.len(), 1);
        let ctrl = &inv.controllers[0];
        assert_eq!(ctrl.name, "0000:06:00.3");
        assert_eq!(ctrl.buses, vec![3]);
        assert_eq!(ctrl.pci_vendor.as_deref(), Some("0x1022"));
        assert_eq!(ctrl.pci_device.as_deref(), Some("0x1639"));
        assert_eq!(ctrl.pci_revision.as_deref(), Some("0x00"));
        assert_eq!(ctrl.driver.as_deref(), Some("xhci_hcd"));

        let chains: Vec<&str> = inv.devices.iter().map(|d| d.port_chain.as_str()).collect();
        assert_eq!(chains, vec!["3-1", "3-1.2", "usb3"], "sorted by name; no interface dirs");

        let hub = &inv.devices[0];
        assert_eq!(hub.bus, Some(3));
        assert_eq!(hub.devnum, Some(2));
        assert_eq!(hub.bcd_device.as_deref(), Some("0663"));
        assert_eq!(hub.bcd_usb.as_deref(), Some("2.10"));
        assert_eq!(hub.vendor_name.as_deref(), Some("Genesys Logic, Inc."));
        assert_eq!(hub.product_name.as_deref(), Some("Hub"));
        assert_eq!(hub.power.get("control").map(String::as_str), Some("auto"));
        assert_eq!(hub.interfaces.len(), 1);
        assert_eq!(hub.interfaces[0].driver.as_deref(), Some("hub"));
        assert_eq!(hub.ports.len(), 2);
        assert_eq!(hub.ports[0].name, "3-1-port1");
        assert_eq!(hub.ports[0].connect_type.as_deref(), Some("hotplug"));
        assert_eq!(hub.ports[0].peer.as_deref(), Some("4-1-port1"));
        assert_eq!(hub.ports[0].location.as_deref(), Some("0x00000001"));
        assert_eq!(hub.ports[0].state.as_deref(), Some("configured"));
        assert_eq!(hub.ports[1].connect_type.as_deref(), Some("not used"));
        assert_eq!(hub.ports[1].peer, None);

        let leaf = &inv.devices[1];
        assert_eq!(leaf.serial.as_deref(), Some("SN0001"), "device identity is kept verbatim");
        assert_eq!(leaf.manufacturer.as_deref(), Some("SunplusIT Inc"));
        assert_eq!(leaf.vendor_name.as_deref(), Some("Chicony Electronics Co., Ltd"));
        assert_eq!(leaf.num_interfaces.as_deref(), Some("2"));
        assert_eq!(leaf.max_power.as_deref(), Some("500mA"));
        assert_eq!(leaf.physical_location.get("panel").map(String::as_str), Some("front"));
        assert_eq!(leaf.interfaces.len(), 2);
        let if0 = &leaf.interfaces[0];
        assert_eq!(if0.name, "3-1.2:1.0");
        assert_eq!(if0.class.as_deref(), Some("0e"));
        assert_eq!(if0.description.as_deref(), Some("HD Webcam"));
        assert_eq!(if0.driver.as_deref(), Some("uvcvideo"));
        let iad = if0.iad.as_ref().expect("interface 0 belongs to an IAD");
        assert_eq!(iad.interface_count.as_deref(), Some("02"));
        assert_eq!(iad.function_class.as_deref(), Some("0e"));
        assert_eq!(if0.endpoints.len(), 1);
        assert_eq!(if0.endpoints[0].name, "ep_87");
        assert_eq!(if0.endpoints[0].max_packet_size.as_deref(), Some("0010"));
        assert_eq!(if0.endpoints[0].kind.as_deref(), Some("Interrupt"));
        assert_eq!(if0.endpoints[0].direction.as_deref(), Some("in"));
        let if1 = &leaf.interfaces[1];
        assert!(if1.iad.is_none(), "no iad_* files, no IAD");
        assert_eq!(if1.endpoints[0].kind.as_deref(), Some("Isoc"));

        let root_hub = &inv.devices[2];
        assert_eq!(root_hub.port_chain, "usb3");
        assert_eq!(root_hub.id_vendor.as_deref(), Some("1d6b"));
        assert!(root_hub.serial.is_none());

        let text = toml::to_string(&inv).unwrap();
        assert!(text.contains("[[devices]]"), "{text}");
        assert!(text.contains("[[devices.interfaces]]"), "{text}");
        assert!(text.contains("[[devices.ports]]"), "{text}");
    }

    #[test]
    fn inventory_without_a_usbids_database_leaves_resolved_names_absent() {
        let temp = tempfile::tempdir().unwrap();
        let devices = build_tree(temp.path());
        let (inv, _) = collect_usb_inventory(&devices, None, UsbidsInfo::default());
        assert!(inv.devices.iter().all(|d| d.vendor_name.is_none() && d.product_name.is_none()));
        assert_eq!(inv.usbids.source, None);
    }

    #[test]
    fn inventory_notes_an_unreadable_root() {
        let temp = tempfile::tempdir().unwrap();
        let (inv, notes) = collect_usb_inventory(&temp.path().join("absent"), None, UsbidsInfo::default());
        assert!(inv.devices.is_empty());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].item, "sysfs usb devices");
    }

    #[test]
    fn descriptor_blobs_are_read_to_their_real_length() {
        let temp = tempfile::tempdir().unwrap();
        let devices = build_tree(temp.path());
        let (blobs, notes) = read_descriptor_blobs(&devices);
        assert!(notes.is_empty(), "{notes:?}");
        let chains: Vec<&str> = blobs.iter().map(|b| b.port_chain.as_str()).collect();
        assert_eq!(chains, vec!["3-1", "3-1.2", "usb3"]);
        assert_eq!(blobs[0].descriptors.len(), 18);
        assert_eq!(blobs[0].descriptors[0], 0x12);
        assert!(blobs[0].bos.is_none(), "no bos_descriptors file: no BOS, not a note");
        assert_eq!(blobs[1].bos.as_deref().map(<[u8]>::len), Some(5));
        assert!(blobs[2].descriptors.is_empty(), "an empty file is an empty blob");
    }

    #[test]
    fn descriptor_blobs_note_a_device_without_a_descriptors_file() {
        let temp = tempfile::tempdir().unwrap();
        let devices = temp.path().join("devices");
        write(&devices.join("1-1"), "busnum", "1\n");
        let (blobs, notes) = read_descriptor_blobs(&devices);
        assert!(blobs.is_empty());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].item, "1-1/descriptors");
    }

    #[test]
    fn attr_dump_follows_top_level_links_caps_depth_and_records_inner_links_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real/port0");
        write(&real, "data_role", "[host] device\n");
        write(&real, "power_role", "[source] sink\n");
        write(&real, "port0-partner/accessory_mode", "none\n");
        write(&real, "port0-partner/port0-partner.0/mode1/active", "yes\n");
        write(&real, "port0-partner/port0-partner.0/mode1/deep/too-deep", "x\n");
        write(&real, "nvm_active0/nvmem", "binary\n");
        write(&real, "power/control", "auto\n");
        write(&real, "big", &"x".repeat(ATTR_VALUE_CAP + 10));
        std::fs::write(real.join("bytes"), [0xff, 0xfe, 0x00]).unwrap();
        std::fs::create_dir_all(temp.path().join("drivers/typec")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("drivers/typec"), real.join("driver")).unwrap();
        let class = temp.path().join("class/typec");
        std::fs::create_dir_all(&class).unwrap();
        std::os::unix::fs::symlink(&real, class.join("port0")).unwrap();

        let (dumps, notes) = dump_attrs(&class, 3);
        assert_eq!(dumps.len(), 1);
        let d = &dumps[0];
        assert_eq!(d.name, "port0");
        assert_eq!(d.attrs.get("data_role").map(String::as_str), Some("[host] device"));
        assert_eq!(d.attrs.get("port0-partner/accessory_mode").map(String::as_str), Some("none"));
        assert_eq!(d.attrs.get("port0-partner/port0-partner.0/mode1/active").map(String::as_str), Some("yes"));
        assert!(!d.attrs.contains_key("port0-partner/port0-partner.0/mode1/deep/too-deep"), "depth 4 is past the cap");
        assert!(!d.attrs.contains_key("nvm_active0/nvmem"), "nvmem is never read");
        assert!(!d.attrs.contains_key("power/control"), "power/ is skipped");
        assert_eq!(d.attrs.get("driver").map(String::as_str), Some("-> typec"), "inner links are recorded, not followed");
        assert_eq!(d.attrs.get("big").map(String::len), Some(ATTR_VALUE_CAP));
        assert!(!d.attrs.contains_key("bytes"), "non-UTF-8 values are skipped");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].item.ends_with("port0/bytes"));
    }

    #[test]
    fn attr_dump_of_a_missing_root_is_empty_with_one_note() {
        let temp = tempfile::tempdir().unwrap();
        let (dumps, notes) = dump_attrs(&temp.path().join("thunderbolt"), 2);
        assert!(dumps.is_empty());
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn usbids_info_names_the_active_source_and_its_date() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home/alice");
        let ids = home.join(".usbtop-ng/usb.ids");
        write(&home, ".usbtop-ng/usb.ids", "# Date:\t2026-08-30 20:34:02\n1d6b  Linux Foundation\n");
        let missing = temp.path().join("missing.ids");
        let mut r = Redactor::new(Some(home.as_path()));
        let info = usbids_info(&[missing.as_path(), ids.as_path()], &mut r);
        assert_eq!(info.source.as_deref(), Some("~/.usbtop-ng/usb.ids"));
        assert_eq!(info.date.as_deref(), Some("2026-08-30"));
        let none = usbids_info(&[missing.as_path()], &mut r);
        assert_eq!(none.source, None);
        assert_eq!(none.date, None);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag::inventory 2>&1 | tail -5`
Expected: compile errors.

- [ ] **Step 4: Implement the inventory**

Above the test module:

```rust
//! Collector C: the full self-description of every USB controller, hub,
//! dock, and device, read from sysfs and stored verbatim (serial strings
//! included: device identity is what a maintainer needs to tell a clone from
//! the real thing, and the reporter reviews the file list before attaching).
//! Also the raw descriptor blobs and generic attribute dumps of the
//! Thunderbolt and Type-C trees. This is the foundation the device
//! disclosure audit on the roadmap will consume.
//!
//! Kernel semantics, verified against v7.0:
//! - `drivers/usb/core/sysfs.c:855-893` `descriptors_read`: the file is the
//!   device descriptor followed by each configuration's raw descriptors up
//!   to its `wTotalLength`; a read past that returns 0 bytes, so
//!   `std::fs::read` (to EOF) yields the real length even though the
//!   attribute declares `18 + 65535`.
//! - `sysfs.c:895-916` `bos_descriptors_read` returns the BOS block to its
//!   `wTotalLength`; `sysfs.c:927-944` hides the file when the device has
//!   no BOS, so its absence is normal.
//! - `sysfs.c:1105-1121`, `1262-1284`: `iad_*` interface attributes exist
//!   only on an interface inside an interface association.
//! - `drivers/usb/core/port.c:166-227`: `location` (`0x%08x`),
//!   `connect_type` (`hotplug`/`hardwired`/`not used`/`unknown`), `state`,
//!   `over_current_count`, `quirks` (`%08x`); `port.c:484-516` `link_peers`
//!   creates the `peer` symlink between the USB 2 and USB 3 ports of one
//!   connector.
//! - `drivers/usb/core/endpoint.c:47-116`: `bEndpointAddress`,
//!   `bmAttributes`, `bInterval` (`%02x`), `wMaxPacketSize` (`%04x`),
//!   `type` (`Control`/`Isoc`/`Bulk`/`Interrupt`), `direction`
//!   (`both`/`in`/`out`); line 168 names each directory `ep_%02x`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::collect::read_trimmed;
use super::redact::Redactor;
use super::{note, Note};
use crate::usbids::{self, UsbIds};

/// Largest attribute value `dump_attrs` records; longer values are cut.
pub const ATTR_VALUE_CAP: usize = 4096;

#[derive(Debug, Default, Serialize)]
pub struct UsbidsInfo {
    /// The active usb.ids source, home paths rewritten.
    pub source: Option<String>,
    /// Its `# Date:` header as `YYYY-MM-DD`.
    pub date: Option<String>,
}

/// Which usb.ids file name resolution would use, and how old it is.
pub fn usbids_info(chain: &[&Path], redactor: &mut Redactor) -> UsbidsInfo {
    let Some(active) = usbids::active_source(chain) else {
        return UsbidsInfo::default();
    };
    let date = std::fs::read_to_string(active)
        .ok()
        .and_then(|text| usbids::parse_header_date(&text))
        .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"));
    UsbidsInfo {
        source: Some(redactor.path(active)),
        date,
    }
}

#[derive(Debug, Serialize)]
pub struct ControllerInfo {
    /// The controller's sysfs name (`0000:06:00.3`, or a platform id).
    pub name: String,
    pub buses: Vec<u8>,
    pub pci_vendor: Option<String>,
    pub pci_device: Option<String>,
    pub pci_revision: Option<String>,
    pub driver: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EndpointInfo {
    pub name: String,
    pub address: Option<String>,
    pub attributes: Option<String>,
    pub max_packet_size: Option<String>,
    pub interval: Option<String>,
    pub direction: Option<String>,
    /// The kernel's `type` attribute (`kind` because `type` is reserved).
    pub kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IadInfo {
    pub first_interface: Option<String>,
    pub interface_count: Option<String>,
    pub function_class: Option<String>,
    pub function_subclass: Option<String>,
    pub function_protocol: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InterfaceInfo {
    pub name: String,
    pub number: Option<String>,
    pub alt_setting: Option<String>,
    pub class: Option<String>,
    pub subclass: Option<String>,
    pub protocol: Option<String>,
    pub num_endpoints: Option<String>,
    /// The `interface` string attribute.
    pub description: Option<String>,
    pub driver: Option<String>,
    pub iad: Option<IadInfo>,
    pub endpoints: Vec<EndpointInfo>,
}

#[derive(Debug, Serialize)]
pub struct HubPortInfo {
    pub name: String,
    pub connect_type: Option<String>,
    /// The paired port's name (the `peer` link target's last component).
    pub peer: Option<String>,
    /// The Type-C connector's name when the port has a `connector` link.
    pub connector: Option<String>,
    pub location: Option<String>,
    pub state: Option<String>,
    pub over_current_count: Option<String>,
    pub quirks: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UsbDeviceInfo {
    pub port_chain: String,
    pub bus: Option<u8>,
    pub devnum: Option<u8>,
    pub id_vendor: Option<String>,
    pub id_product: Option<String>,
    pub bcd_device: Option<String>,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub vendor_name: Option<String>,
    pub product_name: Option<String>,
    pub device_class: Option<String>,
    pub device_subclass: Option<String>,
    pub device_protocol: Option<String>,
    /// sysfs `version` (bcdUSB).
    pub bcd_usb: Option<String>,
    pub speed: Option<String>,
    pub max_packet_size0: Option<String>,
    pub num_configurations: Option<String>,
    pub configuration_value: Option<String>,
    pub num_interfaces: Option<String>,
    pub bm_attributes: Option<String>,
    pub max_power: Option<String>,
    pub quirks: Option<String>,
    pub avoid_reset_quirk: Option<String>,
    pub ltm_capable: Option<String>,
    pub rx_lanes: Option<String>,
    pub tx_lanes: Option<String>,
    pub maxchild: Option<String>,
    pub urbnum: Option<String>,
    pub authorized: Option<String>,
    pub removable: Option<String>,
    pub physical_location: BTreeMap<String, String>,
    /// `power/control`, `power/autosuspend`, `power/runtime_status`.
    pub power: BTreeMap<String, String>,
    pub interfaces: Vec<InterfaceInfo>,
    pub ports: Vec<HubPortInfo>,
}

#[derive(Debug, Serialize)]
pub struct UsbInventory {
    pub usbids: UsbidsInfo,
    pub controllers: Vec<ControllerInfo>,
    pub devices: Vec<UsbDeviceInfo>,
}

fn attr(dir: &Path, name: &str) -> Option<String> {
    read_trimmed(&dir.join(name))
}

fn link_name(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

fn is_root_hub(name: &str) -> bool {
    name.strip_prefix("usb").is_some_and(|rest| rest.parse::<u8>().is_ok())
}

/// Sorted directory entry names under `dir`; empty when unreadable.
fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn read_endpoints(interface_dir: &Path) -> Vec<EndpointInfo> {
    entry_names(interface_dir)
        .into_iter()
        .filter(|n| n.starts_with("ep_"))
        .map(|name| {
            let dir = interface_dir.join(&name);
            EndpointInfo {
                address: attr(&dir, "bEndpointAddress"),
                attributes: attr(&dir, "bmAttributes"),
                max_packet_size: attr(&dir, "wMaxPacketSize"),
                interval: attr(&dir, "bInterval"),
                direction: attr(&dir, "direction"),
                kind: attr(&dir, "type"),
                name,
            }
        })
        .collect()
}

/// Interface directories are the only entries of a device directory whose
/// name carries a `:` (`3-1:1.0`; a root hub's is `3-0:1.0` under `usb3`).
fn interface_names(device_dir: &Path) -> Vec<String> {
    entry_names(device_dir)
        .into_iter()
        .filter(|n| n.contains(':'))
        .collect()
}

fn read_interfaces(device_dir: &Path) -> Vec<InterfaceInfo> {
    interface_names(device_dir)
        .into_iter()
        .map(|name| {
            let dir = device_dir.join(&name);
            let iad = attr(&dir, "iad_bFirstInterface").map(|first| IadInfo {
                first_interface: Some(first),
                interface_count: attr(&dir, "iad_bInterfaceCount"),
                function_class: attr(&dir, "iad_bFunctionClass"),
                function_subclass: attr(&dir, "iad_bFunctionSubClass"),
                function_protocol: attr(&dir, "iad_bFunctionProtocol"),
            });
            InterfaceInfo {
                number: attr(&dir, "bInterfaceNumber"),
                alt_setting: attr(&dir, "bAlternateSetting"),
                class: attr(&dir, "bInterfaceClass"),
                subclass: attr(&dir, "bInterfaceSubClass"),
                protocol: attr(&dir, "bInterfaceProtocol"),
                num_endpoints: attr(&dir, "bNumEndpoints"),
                description: attr(&dir, "interface"),
                driver: link_name(&dir.join("driver")),
                iad,
                endpoints: read_endpoints(&dir),
                name,
            }
        })
        .collect()
}

/// A hub's ports live under its interface 0 directory as
/// `<device>-port<N>` (`3-1:1.0/3-1-port1`, `usb3/3-0:1.0/usb3-port1`).
fn read_ports(device_dir: &Path, device_name: &str) -> Vec<HubPortInfo> {
    let mut ports = Vec::new();
    let port_prefix = format!("{device_name}-port");
    for interface in interface_names(device_dir) {
        let interface_dir = device_dir.join(&interface);
        for name in entry_names(&interface_dir)
            .into_iter()
            .filter(|n| n.starts_with(&port_prefix))
        {
            let dir = interface_dir.join(&name);
            ports.push(HubPortInfo {
                connect_type: attr(&dir, "connect_type"),
                peer: link_name(&dir.join("peer")),
                connector: link_name(&dir.join("connector")),
                location: attr(&dir, "location"),
                state: attr(&dir, "state"),
                over_current_count: attr(&dir, "over_current_count"),
                quirks: attr(&dir, "quirks"),
                name,
            });
        }
    }
    ports.sort_by(|a, b| port_number(&a.name).cmp(&port_number(&b.name)));
    ports
}

fn port_number(name: &str) -> u32 {
    name.rsplit("port")
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX)
}

fn read_map(dir: &Path) -> BTreeMap<String, String> {
    entry_names(dir)
        .into_iter()
        .filter_map(|name| attr(dir, &name).map(|v| (name, v)))
        .collect()
}

fn read_device(device_dir: &Path, name: &str, usbids: Option<&UsbIds>) -> UsbDeviceInfo {
    let id_vendor = attr(device_dir, "idVendor");
    let id_product = attr(device_dir, "idProduct");
    let vid = id_vendor.as_deref().and_then(|v| u16::from_str_radix(v, 16).ok());
    let pid = id_product.as_deref().and_then(|p| u16::from_str_radix(p, 16).ok());
    let vendor_name = usbids.zip(vid).and_then(|(db, vid)| db.vendor_name(vid).map(str::to_string));
    let product_name = usbids
        .zip(vid)
        .zip(pid)
        .and_then(|((db, vid), pid)| db.product_name(vid, pid).map(str::to_string));
    let power_dir = device_dir.join("power");
    let power = ["control", "autosuspend", "runtime_status"]
        .into_iter()
        .filter_map(|k| attr(&power_dir, k).map(|v| (k.to_string(), v)))
        .collect();
    UsbDeviceInfo {
        port_chain: name.to_string(),
        bus: attr(device_dir, "busnum").and_then(|s| s.parse().ok()),
        devnum: attr(device_dir, "devnum").and_then(|s| s.parse().ok()),
        id_vendor,
        id_product,
        bcd_device: attr(device_dir, "bcdDevice"),
        serial: attr(device_dir, "serial"),
        manufacturer: attr(device_dir, "manufacturer"),
        product: attr(device_dir, "product"),
        vendor_name,
        product_name,
        device_class: attr(device_dir, "bDeviceClass"),
        device_subclass: attr(device_dir, "bDeviceSubClass"),
        device_protocol: attr(device_dir, "bDeviceProtocol"),
        bcd_usb: attr(device_dir, "version"),
        speed: attr(device_dir, "speed"),
        max_packet_size0: attr(device_dir, "bMaxPacketSize0"),
        num_configurations: attr(device_dir, "bNumConfigurations"),
        configuration_value: attr(device_dir, "bConfigurationValue"),
        num_interfaces: attr(device_dir, "bNumInterfaces"),
        bm_attributes: attr(device_dir, "bmAttributes"),
        max_power: attr(device_dir, "bMaxPower"),
        quirks: attr(device_dir, "quirks"),
        avoid_reset_quirk: attr(device_dir, "avoid_reset_quirk"),
        ltm_capable: attr(device_dir, "ltm_capable"),
        rx_lanes: attr(device_dir, "rx_lanes"),
        tx_lanes: attr(device_dir, "tx_lanes"),
        maxchild: attr(device_dir, "maxchild"),
        urbnum: attr(device_dir, "urbnum"),
        authorized: attr(device_dir, "authorized"),
        removable: attr(device_dir, "removable"),
        physical_location: read_map(&device_dir.join("physical_location")),
        power,
        interfaces: read_interfaces(device_dir),
        ports: read_ports(device_dir, name),
    }
}

/// The controller behind a root hub entry: canonicalize `usbN` (through the
/// host's symlink) and take its parent directory.
fn read_controllers(sysfs_devices: &Path, names: &[String]) -> Vec<ControllerInfo> {
    let mut by_name: BTreeMap<String, ControllerInfo> = BTreeMap::new();
    for name in names.iter().filter(|n| is_root_hub(n)) {
        let Ok(real) = std::fs::canonicalize(sysfs_devices.join(name)) else {
            continue;
        };
        let Some(parent) = real.parent() else {
            continue;
        };
        let ctrl_name = parent.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let bus = name.strip_prefix("usb").and_then(|n| n.parse::<u8>().ok());
        let entry = by_name.entry(ctrl_name.clone()).or_insert_with(|| ControllerInfo {
            name: ctrl_name,
            buses: Vec::new(),
            pci_vendor: attr(parent, "vendor"),
            pci_device: attr(parent, "device"),
            pci_revision: attr(parent, "revision"),
            driver: link_name(&parent.join("driver")),
        });
        entry.buses.extend(bus);
        entry.buses.sort_unstable();
    }
    by_name.into_values().collect()
}

/// Every device directory under `sysfs_devices` (interface directories,
/// which carry a `:`, are skipped at the top level and read per device).
pub fn collect_usb_inventory(
    sysfs_devices: &Path,
    usbids: Option<&UsbIds>,
    usbids_info: UsbidsInfo,
) -> (UsbInventory, Vec<Note>) {
    let mut notes = Vec::new();
    if let Err(e) = std::fs::read_dir(sysfs_devices) {
        notes.push(note("sysfs usb devices", format!("{}: {e}", sysfs_devices.display())));
        return (
            UsbInventory {
                usbids: usbids_info,
                controllers: Vec::new(),
                devices: Vec::new(),
            },
            notes,
        );
    }
    let names: Vec<String> = entry_names(sysfs_devices)
        .into_iter()
        .filter(|n| !n.contains(':'))
        .collect();
    let controllers = read_controllers(sysfs_devices, &names);
    let devices = names
        .iter()
        .map(|name| read_device(&sysfs_devices.join(name), name, usbids))
        .collect();
    (
        UsbInventory {
            usbids: usbids_info,
            controllers,
            devices,
        },
        notes,
    )
}

/// The raw `descriptors` and `bos_descriptors` blobs of one device.
#[derive(Debug)]
pub struct DescriptorBlob {
    pub port_chain: String,
    pub descriptors: Vec<u8>,
    pub bos: Option<Vec<u8>>,
}

/// Read every device's descriptor blobs to their real length (see the
/// module doc for why `std::fs::read`, not the declared size). A device
/// without a readable `descriptors` file is noted; a missing
/// `bos_descriptors` is normal (no BOS) and is not.
pub fn read_descriptor_blobs(sysfs_devices: &Path) -> (Vec<DescriptorBlob>, Vec<Note>) {
    let mut blobs = Vec::new();
    let mut notes = Vec::new();
    for name in entry_names(sysfs_devices).into_iter().filter(|n| !n.contains(':')) {
        let dir = sysfs_devices.join(&name);
        match std::fs::read(dir.join("descriptors")) {
            Ok(descriptors) => {
                let bos_path = dir.join("bos_descriptors");
                let bos = if bos_path.exists() {
                    match std::fs::read(&bos_path) {
                        Ok(bytes) => Some(bytes),
                        Err(e) => {
                            notes.push(note(&format!("{name}/bos_descriptors"), e));
                            None
                        }
                    }
                } else {
                    None
                };
                blobs.push(DescriptorBlob {
                    port_chain: name,
                    descriptors,
                    bos,
                });
            }
            Err(e) => notes.push(note(&format!("{name}/descriptors"), e)),
        }
    }
    (blobs, notes)
}

/// One top-level entry of a class or bus directory and every readable
/// attribute under it, keyed by relative path.
#[derive(Debug, Serialize)]
pub struct AttrDump {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
}

/// Names never read: `nvmem` is a device's firmware image, `power/` is
/// runtime-PM noise recorded elsewhere for USB devices, and the two links
/// below point out of the tree.
const SKIPPED_DIRS: [&str; 3] = ["power", "subsystem", "firmware_node"];
const SKIPPED_FILES: [&str; 1] = ["nvmem"];

fn walk_attrs(
    base: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    attrs: &mut BTreeMap<String, String>,
    notes: &mut Vec<Note>,
) {
    if depth > max_depth {
        return;
    }
    for name in entry_names(dir) {
        let path = dir.join(&name);
        let rel = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| name.clone());
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            // Inner links (`driver`, `connector`, `device`) lead out of the
            // entry; record where they point, never follow them.
            if let Some(target) = link_name(&path) {
                attrs.insert(rel, format!("-> {target}"));
            }
        } else if meta.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                walk_attrs(base, &path, depth + 1, max_depth, attrs, notes);
            }
        } else if !SKIPPED_FILES.contains(&name.as_str()) {
            // Attribute reads are bounded to the cap; a write-only or
            // otherwise unreadable file is simply not an attribute here.
            let Ok(bytes) = read_capped(&path, ATTR_VALUE_CAP) else {
                continue;
            };
            match String::from_utf8(bytes) {
                Ok(text) => {
                    attrs.insert(rel, text.trim().to_string());
                }
                Err(_) => notes.push(note(&path.display().to_string(), "not UTF-8, skipped")),
            }
        }
    }
}

fn read_capped(path: &Path, cap: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(cap.min(4096));
    std::fs::File::open(path)?.take(cap as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Every entry under `root` (a class or bus `devices` directory, whose
/// entries are symlinks into the device tree: those are followed) with its
/// attributes to `max_depth` levels below the entry. Values are trimmed and
/// capped at [`ATTR_VALUE_CAP`] bytes.
pub fn dump_attrs(root: &Path, max_depth: usize) -> (Vec<AttrDump>, Vec<Note>) {
    let mut notes = Vec::new();
    if let Err(e) = std::fs::read_dir(root) {
        notes.push(note(&root.display().to_string(), e));
        return (Vec::new(), notes);
    }
    let mut dumps = Vec::new();
    for name in entry_names(root) {
        let entry = root.join(&name);
        // The entry itself is followed (canonicalized) so `strip_prefix`
        // works on the real tree below it.
        let Ok(real) = std::fs::canonicalize(&entry) else {
            notes.push(note(&entry.display().to_string(), "could not be resolved"));
            continue;
        };
        let mut attrs = BTreeMap::new();
        // Depth counts directories below the entry: the entry itself is 0,
        // so with `max_depth = 3` a file in `a/b/c/` is recorded and one in
        // `a/b/c/d/` is not.
        walk_attrs(&real, &real, 0, max_depth, &mut attrs, &mut notes);
        dumps.push(AttrDump { name, attrs });
    }
    (dumps, notes)
}
```

- [ ] **Step 5: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag:: 2>&1 | grep -E 'test result|FAILED|panicked'`
Expected: all pass (33 in `diag`).

- [ ] **Step 6: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/diag/mod.rs src/diag/inventory.rs src/usbids/mod.rs
git commit -m "feat(diag): USB device inventory, raw descriptor blobs, Thunderbolt and Type-C dumps

Reads every device's self-description from sysfs verbatim (serials
included, per the spec's device-identity rule), each interface with its
IAD and endpoints, each hub port with its peer link, and each controller
with its PCI ids; reads descriptors and bos_descriptors to their real
length (sysfs.c v7.0:855-944); and dumps the thunderbolt, typec, and
usb_power_delivery trees generically with a depth and size cap.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 5: Promote the capture core toward the default build

The support bundle embeds a fixture bundle, so the capturer, the replay
core, and their SEC-1/SEC-2 guards must be callable from the default build.
This task makes them *compile* in the default build's test target and adds
the three capabilities the orchestrator needs (a static bundle with no
traces, replay with a real elapsed time, and a capture outcome to summarize)
while leaving the `--capture-fixture` CLI and its live entry point behind
the feature until Task 7 gives them a default-build caller. Every item that
becomes visible here has a test, which is what keeps `-D warnings` green
without `#[allow]`.

**Files:**
- Modify: `src/main.rs:20-21` (the `capture` module gate)
- Modify: `src/capture/mod.rs`
- Modify: `src/fixture_replay.rs`

**Interfaces:**
- Consumes: `capture::{assemble_bundle, assert_payload_free, assert_sysfs_contained, CapturedTrace, BaselineSource}`, `fixture_replay::{replay_fixture, FixtureSource, FIXED_ELAPSED}` as they exist today.
- Produces:
  ```rust
  // src/fixture_replay.rs
  pub fn replay_fixture_with_elapsed(bundle_dir: &Path, source: Option<FixtureSource>, elapsed: Duration) -> anyhow::Result<Report>;
  // `replay_fixture(bundle_dir, source)` == `replay_fixture_with_elapsed(bundle_dir, Some(source), FIXED_ELAPSED)`.
  // A `None` source replays no trace: the report enumerates the sysfs devices with zero traffic and `source == "none"`.

  // src/capture/mod.rs
  pub fn assemble_bundle(src_sysfs, outdir, traces: &[CapturedTrace], baseline, stage_id) -> Result<()>;
  //   now accepts an empty `traces`: writes sysfs/, internal-devices.toml, meta.toml with `sources = []`, no trace or golden files.
  pub fn assert_bundle_payload_free(bundle_dir: &Path) -> anyhow::Result<()>;   // SEC-1 over whichever trace files exist
  pub fn assert_sysfs_contained(sysfs: &Path) -> anyhow::Result<()>;            // now pub
  pub fn count_events(traces: &[CapturedTrace]) -> u64;                          // binary records, else text lines
  #[cfg(feature = "capture-fixture")] pub struct CaptureOutcome { pub sources: Vec<FixtureSource>, pub events: u64, pub binary_kernel_dropped: Option<u64> }
  #[cfg(feature = "capture-fixture")] pub fn run_capture_fixture(opts: CaptureFixtureOpts) -> anyhow::Result<CaptureOutcome>;
  ```

- [ ] **Step 1: Write the failing tests**

In `src/fixture_replay.rs`'s test module add:

```rust
    #[test]
    fn replay_with_a_real_elapsed_time_scales_the_rates() {
        let temp = tempfile::tempdir().unwrap();
        build_min_bundle(temp.path());
        let report = replay_fixture_with_elapsed(
            temp.path(),
            Some(FixtureSource::Binary),
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(report.window_seconds, 2.0);
        let dev = report.buses[0].devices.iter().find(|d| d.address == 3).unwrap();
        assert_eq!(dev.total_rx_bytes, 1000);
        assert_eq!(dev.rx_bps, 500.0, "1000 bytes over 2 s");
    }

    #[test]
    fn replay_without_a_trace_enumerates_devices_with_zero_traffic() {
        let temp = tempfile::tempdir().unwrap();
        build_min_bundle(temp.path());
        std::fs::remove_file(temp.path().join("trace.bin")).unwrap();
        std::fs::remove_file(temp.path().join("trace.txt")).unwrap();
        let report = replay_fixture_with_elapsed(temp.path(), None, FIXED_ELAPSED).unwrap();
        assert_eq!(report.source, "none");
        let bus = &report.buses[0];
        assert_eq!(bus.controller.as_deref(), Some("0000:00:14.0"));
        let dev = bus.devices.iter().find(|d| d.address == 3).unwrap();
        assert_eq!(dev.vendor_id.as_deref(), Some("0430"));
        assert_eq!(dev.total_rx_bytes, 0);
        assert!(dev.endpoints.is_empty());
    }
```

In `src/capture/mod.rs`'s test module add:

```rust
    /// The orchestrator's non-root path: a bundle with no traces still
    /// carries the sysfs snapshot, the baseline, and a meta.toml that says
    /// `sources = []`, and replays to a device list.
    #[test]
    fn assemble_bundle_with_no_traces_writes_a_static_bundle() {
        let temp = tempfile::tempdir().unwrap();
        build_src_sysfs(temp.path());
        let outdir = temp.path().join("bundle");
        assemble_bundle(
            &temp.path().join("devices"),
            &outdir,
            &[],
            &BaselineSource::CaptureFrom(temp.path().join("devices")),
            None,
        )
        .unwrap();
        for f in ["sysfs", "internal-devices.toml", "meta.toml"] {
            assert!(outdir.join(f).exists(), "missing {f}");
        }
        for f in ["trace.bin", "trace.txt", "golden.binary.json", "golden.text.json"] {
            assert!(!outdir.join(f).exists(), "{f} must not exist without a capture");
        }
        let meta: crate::fixture_replay::Meta =
            toml::from_str(&std::fs::read_to_string(outdir.join("meta.toml")).unwrap()).unwrap();
        assert!(meta.sources.is_empty());
        assert_eq!(meta.controllers, vec!["0000:00:14.0".to_string()]);
        let report =
            crate::fixture_replay::replay_fixture_with_elapsed(&outdir, None, FIXED_ELAPSED).unwrap();
        assert_eq!(report.source, "none");
    }

    #[test]
    fn assemble_bundle_copies_a_supplied_baseline_file() {
        let temp = tempfile::tempdir().unwrap();
        build_src_sysfs(temp.path());
        let baseline = temp.path().join("stage1-internal-devices.toml");
        std::fs::write(&baseline, "captured_unix = 7\n\n[[devices]]\nport_path = \"usb1\"\n").unwrap();
        let outdir = temp.path().join("bundle");
        assemble_bundle(
            &temp.path().join("devices"),
            &outdir,
            &[],
            &BaselineSource::CopyFile(baseline.clone()),
            Some(2),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(outdir.join("internal-devices.toml")).unwrap(),
            std::fs::read_to_string(&baseline).unwrap()
        );
    }

    #[test]
    fn assert_bundle_payload_free_checks_whichever_traces_exist() {
        let temp = tempfile::tempdir().unwrap();
        assert!(assert_bundle_payload_free(temp.path()).is_ok(), "no traces, nothing to check");
        std::fs::write(temp.path().join("trace.bin"), one_binary_event()).unwrap();
        std::fs::write(temp.path().join("trace.txt"), "ffff0000aaaa0001 200 C Bi:1:003:1 0 1000 <\n").unwrap();
        assert!(assert_bundle_payload_free(temp.path()).is_ok());
        let mut bad = one_binary_event();
        bad.extend_from_slice(&[0xAB; 4]);
        std::fs::write(temp.path().join("trace.bin"), bad).unwrap();
        let err = assert_bundle_payload_free(temp.path()).unwrap_err();
        assert!(err.to_string().contains("SEC-1"), "{err}");
        std::fs::write(temp.path().join("trace.bin"), one_binary_event()).unwrap();
        std::fs::write(temp.path().join("trace.txt"), "ffff0000aaaa0001 200 C Bo:1:003:1 0 4 = 01020304\n").unwrap();
        let err = assert_bundle_payload_free(temp.path()).unwrap_err();
        assert!(err.to_string().contains("SEC-1"), "{err}");
    }

    #[test]
    fn count_events_prefers_the_binary_trace_and_falls_back_to_text_lines() {
        let mut two = one_binary_event();
        two.extend_from_slice(&one_binary_event());
        let binary = CapturedTrace {
            source: FixtureSource::Binary,
            bytes: two,
            kernel_dropped: None,
        };
        let text = CapturedTrace {
            source: FixtureSource::Text,
            bytes: b"a 1 C Bi:1:003:1 0 1 <\nb 2 C Bi:1:003:1 0 1 <\nc 3 C Bi:1:003:1 0 1 <\n".to_vec(),
            kernel_dropped: None,
        };
        assert_eq!(count_events(&[binary, text]), 2);
        let text_only = CapturedTrace {
            source: FixtureSource::Text,
            bytes: b"a 1 C Bi:1:003:1 0 1 <\nb 2 C Bi:1:003:1 0 1 <\n".to_vec(),
            kernel_dropped: None,
        };
        assert_eq!(count_events(&[text_only]), 2);
        assert_eq!(count_events(&[]), 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test --features capture-fixture capture:: 2>&1 | tail -5`
Expected: compile errors (`replay_fixture_with_elapsed`, `assert_bundle_payload_free`, `count_events` missing).

- [ ] **Step 3: Implement**

`src/main.rs:20-21`: replace `#[cfg(feature = "capture-fixture")]\nmod capture;` with `#[cfg(any(test, feature = "capture-fixture"))]\nmod capture;`.

`src/fixture_replay.rs`:
- Add `use std::time::Duration;` to the imports.
- Replace `replay_fixture` with the pair:

```rust
/// Replay one bundle's trace for one source into a deterministic report. The
/// bus id passed to the reader is cosmetic — every packet carries its own bus
/// id, so a single trace over the aggregate interface routes to the right bus.
/// This is the exact sequence the capturer uses to generate goldens, so a
/// committed golden equals this output by construction.
pub fn replay_fixture(bundle_dir: &Path, source: FixtureSource) -> anyhow::Result<Report> {
    replay_fixture_with_elapsed(bundle_dir, Some(source), FIXED_ELAPSED)
}

/// [`replay_fixture`] with the window length chosen by the caller: goldens
/// use [`FIXED_ELAPSED`], the support bundle's `report.json` uses the real
/// capture window so its rates are the rates that were seen. A `None`
/// source replays no trace at all (a static bundle captured without root):
/// the report still enumerates every device the sysfs snapshot holds, with
/// zero traffic and `source == "none"`.
pub fn replay_fixture_with_elapsed(
    bundle_dir: &Path,
    source: Option<FixtureSource>,
    elapsed: Duration,
) -> anyhow::Result<Report> {
    let mut manager = DeviceManager::with_sysfs_base(bundle_dir.join("sysfs"));
    if let Some(snapshot) = load_internal_devices(bundle_dir) {
        manager.set_internal_snapshot(Some(snapshot));
    }
    // usb.ids overlay left None on purpose: names come only from the captured
    // sysfs strings, so replay is host-independent (see the spec's config parity).
    let baseline = Baseline::capture(&manager);

    let shutdown = AtomicBool::new(false);
    match source {
        Some(FixtureSource::Binary) => {
            let trace = bundle_dir.join(FixtureSource::Binary.trace_filename());
            BinaryReader::with_path(0, trace, false).read_packets(
                &shutdown,
                &AtomicU64::new(0),
                |packet| {
                    manager.apply_packet(&packet);
                    Ok(())
                },
            )?;
        }
        Some(FixtureSource::Text) => {
            let trace = bundle_dir.join(FixtureSource::Text.trace_filename());
            UsbmonReader::with_path(0, trace, false).read_packets(&shutdown, |packet| {
                manager.apply_packet(&packet);
                Ok(())
            })?;
        }
        None => {}
    }

    manager.enumerate_present_devices();
    // Resolves BusReport.controller + bus speed_mbps. Enumeration alone does
    // NOT (see manager.rs:188); without this the controller/speed fields are null.
    manager.update_bus_speeds();

    Ok(build_report(
        &manager,
        &baseline,
        elapsed,
        source.map_or("none", FixtureSource::label),
        0,
        source == Some(FixtureSource::Text),
        &FilterSet::default(),
    ))
}
```

`src/capture/mod.rs`:
- Module doc comment becomes: `//! Fixture capture and assembly: the \`--capture-fixture\` subcommand (behind the \`capture-fixture\` feature) and the shared core \`--support\` uses to embed a replayable bundle. Every bundle is payload-free (SEC-1) and path-contained (SEC-2) by construction, and both guards are re-runnable over a bundle on disk.`
- Add `use crate::fixture_replay::replay_fixture_with_elapsed;` and `use crate::fixture_replay::FIXED_ELAPSED;` to the imports (keep the existing `replay_fixture` import only if still used; after this change `assemble_bundle` calls `replay_fixture` for goldens, so it stays).
- In `assemble_bundle`, replace the golden loop's report plumbing:

```rust
    // Generate each golden by replaying the just-written bundle. With no
    // traces (a static bundle) the report comes from the sysfs snapshot
    // alone, so meta.toml still carries the coverage tags.
    let mut report_for_meta = None;
    for &source in &sources {
        let report = replay_fixture(outdir, source)?;
        std::fs::write(
            outdir.join(source.golden_filename()),
            report_to_golden_json(&report)?,
        )
        .with_context(|| format!("write {}", source.golden_filename()))?;
        report_for_meta.get_or_insert(report);
    }
    let report = match report_for_meta {
        Some(report) => report,
        None => replay_fixture_with_elapsed(outdir, None, FIXED_ELAPSED)?,
    };
```

- Make `assert_sysfs_contained` `pub fn`, and add after `assert_payload_free`:

```rust
/// SEC-1 over a bundle on disk: every trace file present is checked with
/// [`assert_payload_free`]. Used by `--support` to re-assert the invariant
/// on the fixture it embeds, the same way the corpus test does.
pub fn assert_bundle_payload_free(bundle_dir: &Path) -> anyhow::Result<()> {
    for source in [FixtureSource::Binary, FixtureSource::Text] {
        let path = bundle_dir.join(source.trace_filename());
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        assert_payload_free(&CapturedTrace {
            source,
            bytes,
            kernel_dropped: None,
        })?;
    }
    Ok(())
}

/// How many usbmon events a capture recorded: the binary trace's 48-byte
/// record count when it exists (exact), else the text trace's line count.
pub fn count_events(traces: &[CapturedTrace]) -> u64 {
    if let Some(binary) = traces.iter().find(|t| t.source == FixtureSource::Binary) {
        return (binary.bytes.len() / 48) as u64;
    }
    traces
        .iter()
        .find(|t| t.source == FixtureSource::Text)
        .map(|t| t.bytes.iter().filter(|&&b| b == b'\n').count() as u64)
        .unwrap_or(0)
}
```

- Gate the live entry point and its options behind the feature until Task 7 (they have no default-build caller yet), and make it return an outcome:

```rust
/// What a live capture recorded, for `--support`'s summary line.
#[cfg(feature = "capture-fixture")]
#[derive(Debug)]
pub struct CaptureOutcome {
    pub sources: Vec<FixtureSource>,
    pub events: u64,
    pub binary_kernel_dropped: Option<u64>,
}
```

  Put `#[cfg(feature = "capture-fixture")]` on `pub struct CaptureFixtureOpts`, on `pub fn run_capture_fixture`, and on `fn stage_id_from_outdir`. Change `run_capture_fixture`'s signature to `-> anyhow::Result<CaptureOutcome>` and its tail to:

```rust
    let stage_id = stage_id_from_outdir(&opts.outdir);
    assemble_bundle(src_sysfs, &opts.outdir, &traces, &baseline, stage_id)?;
    eprintln!("captured fixture bundle at {}", opts.outdir.display());
    Ok(CaptureOutcome {
        sources: traces.iter().map(|t| t.source).collect(),
        events: count_events(&traces),
        binary_kernel_dropped: traces
            .iter()
            .find(|t| t.source == FixtureSource::Binary)
            .and_then(|t| t.kernel_dropped),
    })
```

  `src/main.rs`'s `--capture-fixture` dispatch already ends with `capture::run_capture_fixture(...)?; return Ok(());` — the returned outcome is dropped there, which is fine (`CaptureOutcome` is not `#[must_use]`).

- [ ] **Step 4: Run the tests on both configurations**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test 2>&1 | grep -E 'test result|FAILED|panicked'; cargo test --features capture-fixture 2>&1 | grep -E 'test result|FAILED|panicked'`
Expected: all pass on both. The default run now includes the capture module's own tests (the ones the feature job ran before) plus the six new ones.

- [ ] **Step 5: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/main.rs src/capture/mod.rs src/fixture_replay.rs
git commit -m "refactor(capture): compile the capture core in the default test build; static bundles; replay with a real window

The support bundle needs the capturer's assembly and guards outside the
capture-fixture feature. assemble_bundle now accepts no traces (a static
bundle with sources = []), replay_fixture_with_elapsed replays with the
caller's window or no trace at all, assert_bundle_payload_free re-checks
SEC-1 over a bundle on disk, and run_capture_fixture reports what it
captured. The live entry point stays behind the feature until --support
calls it.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 6: Bundle writer (`src/diag/bundle.rs`)

**Files:**
- Create: `src/diag/bundle.rs`
- Modify: `src/diag/mod.rs` (add `pub mod bundle;`)

**Interfaces:**
- Consumes: `diag::{Note, note}`, `diag::redact::Redactor`; `capture::{assert_bundle_payload_free, assert_sysfs_contained}` (Task 5); `config::chown_created_to_invoker`.
- Produces:
  ```rust
  pub const FORMAT_VERSION: u32 = 1;
  pub fn utc_stamp(unix_secs: u64) -> String;      // "20260903T091500Z"
  pub fn utc_iso(unix_secs: u64) -> String;        // "2026-09-03T09:15:00Z"
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
  pub struct FileEntry { pub path: String, pub bytes: u64, pub symlink: bool }
  #[derive(Debug, Serialize, Deserialize)]
  pub struct Manifest { pub format_version: u32, pub created_unix: u64, pub created_utc: String, pub usbtop_ng: String, pub redaction: BTreeMap<String, usize>, pub unavailable: Vec<Note>, pub files: Vec<FileEntry> }
  pub struct BundleWriter { .. }
  impl BundleWriter {
      pub fn create(root: &Path, redactor: Redactor) -> io::Result<BundleWriter>;
      pub fn redactor(&mut self) -> &mut Redactor;
      pub fn files(&self) -> &[FileEntry];                     // recorded so far; the manifest is never listed
      pub fn write_text(&mut self, rel: &str, text: &str) -> io::Result<()>;      // redacted
      pub fn write_bytes(&mut self, rel: &str, bytes: &[u8]) -> io::Result<()>;   // verbatim
      pub fn write_toml<T: Serialize>(&mut self, rel: &str, value: &T) -> anyhow::Result<()>;
      pub fn redact_file(&mut self, rel: &str) -> io::Result<()>;   // rewrite an existing text file through the redactor, then record it
      pub fn adopt_file(&mut self, rel: &str) -> io::Result<()>;    // record an existing file as-is
      pub fn record_dir(&mut self, rel: &str) -> io::Result<()>;    // record every file and symlink under a subtree
      pub fn write_manifest(&mut self, created_unix: u64, unavailable: &[Note]) -> anyhow::Result<()>;
      pub fn archive(&self, archive: &Path) -> Result<u64, Note>;               // `tar -czf`; Ok(bytes)
      pub fn archive_with(&self, archive: &Path, program: &str) -> Result<u64, Note>;
  }
  pub fn assert_fixture_invariants(fixture_dir: &Path) -> anyhow::Result<()>;
  pub fn own_tree(root: &Path);   // under sudo, hand the bundle to the invoking user (best-effort)
  ```
  `Note` gains `Deserialize` (add it to the derive in `src/diag/mod.rs`) so the manifest parses back in tests.

- [ ] **Step 1: Write the failing tests**

Add `pub mod bundle;` to `src/diag/mod.rs` and `Deserialize` to `Note`'s derive (`use serde::{Deserialize, Serialize};`). Create `src/diag/bundle.rs` with this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tar_available() -> bool {
        Command::new("tar").arg("--version").output().is_ok()
    }

    #[test]
    fn utc_stamp_pins_the_epoch_a_recent_date_two_leap_days_and_a_non_leap_century() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_788_000_000), "20260829T104000Z");
        assert_eq!(utc_stamp(1_709_164_800), "20240229T000000Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
        assert_eq!(utc_stamp(4_102_444_800), "21000101T000000Z");
        assert_eq!(utc_iso(1_788_000_000), "2026-08-29T10:40:00Z");
    }

    #[test]
    fn write_text_redacts_records_and_creates_parents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w = BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        w.write_text("config/preferences.toml", "usbids_path = \"/home/alice/usb.ids\"\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("config/preferences.toml")).unwrap(),
            "usbids_path = \"~/usb.ids\"\n"
        );
        assert_eq!(
            w.files(),
            &[FileEntry {
                path: "config/preferences.toml".into(),
                bytes: 26,
                symlink: false
            }]
        );
        assert_eq!(w.redactor().summary(), vec![("home_path".to_string(), 1)]);
    }

    #[test]
    fn write_bytes_is_verbatim_and_write_toml_serializes_then_redacts() {
        #[derive(Serialize)]
        struct Doc {
            dir: String,
            n: u32,
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w = BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        w.write_bytes("inventory/descriptors/1-4.bin", &[0x12, 0x01, 0x00, 0x02]).unwrap();
        assert_eq!(std::fs::read(root.join("inventory/descriptors/1-4.bin")).unwrap(), vec![0x12, 0x01, 0x00, 0x02]);
        w.write_toml("config/config.toml", &Doc { dir: "/home/alice/.usbtop-ng".into(), n: 3 }).unwrap();
        let text = std::fs::read_to_string(root.join("config/config.toml")).unwrap();
        assert!(text.contains("dir = \"~/.usbtop-ng\""), "{text}");
        assert!(text.contains("n = 3"), "{text}");
        assert_eq!(w.files().len(), 2);
    }

    #[test]
    fn redact_file_rewrites_in_place_and_adopt_file_records_as_is() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w = BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        std::fs::write(root.join("report.json"), "{\"command\":[\"/home/alice/bin/usbtop-ng\"]}\n").unwrap();
        w.redact_file("report.json").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("report.json")).unwrap(),
            "{\"command\":[\"~/bin/usbtop-ng\"]}\n"
        );
        std::fs::write(root.join("usbtop-ng.log"), "[INFO] starting\n").unwrap();
        w.adopt_file("usbtop-ng.log").unwrap();
        assert_eq!(w.files()[1], FileEntry { path: "usbtop-ng.log".into(), bytes: 16, symlink: false });
        assert!(w.adopt_file("missing").is_err());
    }

    #[test]
    fn record_dir_lists_every_file_and_symlink_under_a_subtree() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w = BundleWriter::create(&root, Redactor::new(None)).unwrap();
        let fixture = root.join("fixture");
        std::fs::create_dir_all(fixture.join("sysfs/0000:00:14.0/usb1")).unwrap();
        std::fs::write(fixture.join("sysfs/0000:00:14.0/usb1/busnum"), "1\n").unwrap();
        std::os::unix::fs::symlink("0000:00:14.0/usb1", fixture.join("sysfs/usb1")).unwrap();
        std::fs::write(fixture.join("meta.toml"), "sources = []\n").unwrap();
        w.record_dir("fixture").unwrap();
        let mut paths: Vec<(String, u64, bool)> = w.files().iter().map(|f| (f.path.clone(), f.bytes, f.symlink)).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                ("fixture/meta.toml".to_string(), 13, false),
                ("fixture/sysfs/0000:00:14.0/usb1/busnum".to_string(), 2, false),
                ("fixture/sysfs/usb1".to_string(), 0, true),
            ]
        );
    }

    #[test]
    fn manifest_lists_files_redaction_and_notes_and_parses_back() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w = BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        w.write_text("build.toml", "command = [\"/home/alice/x\"]\n").unwrap();
        w.write_manifest(1_788_000_000, &[note("dmesg", "permission denied")]).unwrap();
        let text = std::fs::read_to_string(root.join("manifest.toml")).unwrap();
        let manifest: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest.format_version, FORMAT_VERSION);
        assert_eq!(manifest.created_unix, 1_788_000_000);
        assert_eq!(manifest.created_utc, "2026-08-29T10:40:00Z");
        assert_eq!(manifest.usbtop_ng, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.redaction.get("home_path"), Some(&1));
        assert_eq!(manifest.unavailable, vec![note("dmesg", "permission denied")]);
        assert_eq!(manifest.files.len(), 1, "the manifest never lists itself");
        assert_eq!(manifest.files[0].path, "build.toml");
        assert_eq!(manifest.files[0].bytes, std::fs::metadata(root.join("build.toml")).unwrap().len());
    }

    #[test]
    fn archive_with_a_missing_program_is_a_note_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("usbtop-ng-support-x");
        let w = BundleWriter::create(&root, Redactor::new(None)).unwrap();
        let err = w.archive_with(&temp.path().join("x.tar.gz"), "no-such-tar-program").unwrap_err();
        assert_eq!(err.item, "archive");
        assert!(err.reason.contains("no-such-tar-program"), "{}", err.reason);
        assert!(!temp.path().join("x.tar.gz").exists());
    }

    #[test]
    fn archive_with_tar_lists_exactly_the_manifest_files_plus_the_manifest() {
        if !tar_available() {
            eprintln!("skipping: tar is not installed");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("usbtop-ng-support-20260829T104000Z");
        let mut w = BundleWriter::create(&root, Redactor::new(None)).unwrap();
        w.write_text("SUMMARY.txt", "usbtop-ng support bundle\n").unwrap();
        w.write_bytes("inventory/descriptors/1-4.bin", &[1, 2, 3]).unwrap();
        w.write_manifest(1_788_000_000, &[]).unwrap();
        let archive = temp.path().join("usbtop-ng-support-20260829T104000Z.tar.gz");
        let bytes = w.archive(&archive).unwrap();
        assert_eq!(bytes, std::fs::metadata(&archive).unwrap().len());
        let listing = Command::new("tar").args(["tzf"]).arg(&archive).output().unwrap();
        let mut listed: Vec<String> = String::from_utf8(listing.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.ends_with('/'))
            .map(|l| l.trim_start_matches("usbtop-ng-support-20260829T104000Z/").to_string())
            .collect();
        listed.sort();
        let mut expected: Vec<String> = w.files().iter().map(|f| f.path.clone()).collect();
        expected.push("manifest.toml".to_string());
        expected.sort();
        assert_eq!(listed, expected);
    }

    #[test]
    fn fixture_invariants_reject_payload_and_an_escaping_link() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = temp.path().join("fixture");
        std::fs::create_dir_all(fixture.join("sysfs/1-1")).unwrap();
        std::fs::write(fixture.join("sysfs/1-1/busnum"), "1\n").unwrap();
        assert!(assert_fixture_invariants(&fixture).is_ok());

        let mut bad = vec![0u8; 48];
        bad[36..40].copy_from_slice(&48u32.to_ne_bytes());
        bad.extend_from_slice(&[0xAB; 48]);
        std::fs::write(fixture.join("trace.bin"), &bad).unwrap();
        let err = assert_fixture_invariants(&fixture).unwrap_err();
        assert!(err.to_string().contains("SEC-1"), "{err}");
        std::fs::remove_file(fixture.join("trace.bin")).unwrap();

        std::os::unix::fs::symlink(temp.path(), fixture.join("sysfs/escape")).unwrap();
        let err = assert_fixture_invariants(&fixture).unwrap_err();
        assert!(err.to_string().contains("SEC-2"), "{err}");
    }

    #[test]
    fn own_tree_is_a_silent_no_op_without_sudo() {
        // This test process is not running under sudo, so `sudo_invoker()`
        // is None and nothing is chowned; the walk itself must survive a
        // symlink and a nested directory without error.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        std::fs::create_dir_all(root.join("fixture/sysfs")).unwrap();
        std::fs::write(root.join("fixture/sysfs/busnum"), "1\n").unwrap();
        std::os::unix::fs::symlink("sysfs", root.join("fixture/link")).unwrap();
        own_tree(&root);
        assert!(root.join("fixture/sysfs/busnum").exists());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag::bundle 2>&1 | tail -5`
Expected: compile errors.

- [ ] **Step 3: Implement `bundle.rs`**

Above the test module:

```rust
//! The bundle on disk: `usbtop-ng-support-<UTC stamp>/`, every file written
//! through the redactor and recorded with its size, the manifest that lists
//! them all with the redaction counts and unavailable notes, and the `tar`
//! archive beside the directory. UTC comes from `SystemTime` plus the
//! civil-from-days conversion below, so no date crate is needed.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::redact::Redactor;
use super::{note, Note};
use crate::capture::{assert_bundle_payload_free, assert_sysfs_contained};
use crate::config::chown_created_to_invoker;

/// The manifest's `format_version`; bump when a file's layout changes.
pub const FORMAT_VERSION: u32 = 1;

/// `(year, month, day, hour, minute, second)` in UTC for a Unix time.
fn utc_parts(unix_secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (unix_secs / 86_400) as i64;
    let secs = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a proleptic
/// Gregorian date, exact for every day the `u64` above can name.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `YYYYMMDDTHHMMSSZ`, the bundle directory's suffix.
pub fn utc_stamp(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(unix_secs);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the manifest's `created_utc`.
pub fn utc_iso(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(unix_secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub symlink: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub created_unix: u64,
    pub created_utc: String,
    pub usbtop_ng: String,
    /// Each redaction rule that fired and how often.
    pub redaction: BTreeMap<String, usize>,
    pub unavailable: Vec<Note>,
    /// Every file in the bundle except this manifest, with its size.
    pub files: Vec<FileEntry>,
}

/// Writes files into the bundle directory, redacting text on the way and
/// recording every file for the manifest.
pub struct BundleWriter {
    root: PathBuf,
    redactor: Redactor,
    files: Vec<FileEntry>,
}

/// Create or truncate `path` (parents included), refusing a symlinked
/// final component, write `bytes`, and hand the file to the sudo invoker
/// when there is one. Returns the byte count.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<u64> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    chown_created_to_invoker(path, file.as_raw_fd());
    Ok(bytes.len() as u64)
}

impl BundleWriter {
    pub fn create(root: &Path, redactor: Redactor) -> io::Result<BundleWriter> {
        std::fs::create_dir_all(root)?;
        Ok(BundleWriter {
            root: root.to_path_buf(),
            redactor,
            files: Vec::new(),
        })
    }

    pub fn redactor(&mut self) -> &mut Redactor {
        &mut self.redactor
    }

    /// Every file recorded so far. The manifest never lists itself.
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    fn record(&mut self, rel: &str, bytes: u64, symlink: bool) {
        self.files.push(FileEntry {
            path: rel.to_string(),
            bytes,
            symlink,
        });
    }

    /// Write text through the redactor.
    pub fn write_text(&mut self, rel: &str, text: &str) -> io::Result<()> {
        let redacted = self.redactor.text(text);
        let bytes = write_new(&self.root.join(rel), redacted.as_bytes())?;
        self.record(rel, bytes, false);
        Ok(())
    }

    /// Write bytes verbatim (descriptor blobs are device identity, never
    /// redacted).
    pub fn write_bytes(&mut self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let n = write_new(&self.root.join(rel), bytes)?;
        self.record(rel, n, false);
        Ok(())
    }

    /// Serialize as TOML, then write through the redactor.
    pub fn write_toml<T: Serialize>(&mut self, rel: &str, value: &T) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(value)?;
        self.write_text(rel, &text)?;
        Ok(())
    }

    /// Rewrite an existing text file (one written by another component,
    /// such as the report sink) through the redactor, then record it.
    pub fn redact_file(&mut self, rel: &str) -> io::Result<()> {
        let path = self.root.join(rel);
        let text = std::fs::read_to_string(&path)?;
        let redacted = self.redactor.text(&text);
        let bytes = write_new(&path, redacted.as_bytes())?;
        self.record(rel, bytes, false);
        Ok(())
    }

    /// Record an existing file as it is (the log the tee already redacted).
    pub fn adopt_file(&mut self, rel: &str) -> io::Result<()> {
        let bytes = std::fs::metadata(self.root.join(rel))?.len();
        self.record(rel, bytes, false);
        Ok(())
    }

    /// Record every regular file and symlink under `rel` (the fixture
    /// subtree the capturer wrote). Symlinks are recorded with zero bytes
    /// and never followed; directories are not entries.
    pub fn record_dir(&mut self, rel: &str) -> io::Result<()> {
        let base = self.root.join(rel);
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path)?;
                let rel_path = path
                    .strip_prefix(&self.root)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| path.display().to_string());
                if meta.file_type().is_symlink() {
                    self.record(&rel_path, 0, true);
                } else if meta.is_dir() {
                    stack.push(path);
                } else {
                    self.record(&rel_path, meta.len(), false);
                }
            }
        }
        Ok(())
    }

    /// Write `manifest.toml`: format version, creation time, the redaction
    /// summary, every unavailable note, and the file list (sorted by path;
    /// the manifest never lists itself).
    pub fn write_manifest(&mut self, created_unix: u64, unavailable: &[Note]) -> anyhow::Result<()> {
        let mut files = self.files.clone();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            created_unix,
            created_utc: utc_iso(created_unix),
            usbtop_ng: env!("CARGO_PKG_VERSION").to_string(),
            redaction: self.redactor.summary().into_iter().collect(),
            unavailable: unavailable.to_vec(),
            files,
        };
        let text = toml::to_string_pretty(&manifest)?;
        write_new(&self.root.join("manifest.toml"), text.as_bytes())?;
        Ok(())
    }

    /// `tar -czf <archive> -C <parent> <dirname>`; `Ok` carries the
    /// archive's size. A missing `tar` (or a failing one) is a note, and the
    /// directory stays for the user to archive by hand.
    pub fn archive(&self, archive: &Path) -> Result<u64, Note> {
        self.archive_with(archive, "tar")
    }

    pub fn archive_with(&self, archive: &Path, program: &str) -> Result<u64, Note> {
        let parent = self.root.parent().unwrap_or(Path::new("."));
        let dirname = self
            .root
            .file_name()
            .ok_or_else(|| note("archive", "bundle directory has no name"))?;
        let output = Command::new(program)
            .arg("-czf")
            .arg(archive)
            .arg("-C")
            .arg(parent)
            .arg(dirname)
            .output()
            .map_err(|e| note("archive", format!("could not run {program}: {e}")))?;
        if !output.status.success() {
            return Err(note(
                "archive",
                format!(
                    "{program} exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        let bytes = std::fs::metadata(archive)
            .map_err(|e| note("archive", format!("{} was not written: {e}", archive.display())))?
            .len();
        if let Ok(file) = File::open(archive) {
            chown_created_to_invoker(archive, file.as_raw_fd());
        }
        Ok(bytes)
    }
}

/// Re-assert the capturer's own invariants over the embedded fixture:
/// SEC-1 (no payload in either trace) and SEC-2 (nothing under `sysfs/`
/// escapes it). A violation is a bug in the capturer, so it fails the run
/// rather than shipping the bundle.
pub fn assert_fixture_invariants(fixture_dir: &Path) -> anyhow::Result<()> {
    assert_bundle_payload_free(fixture_dir)?;
    let sysfs = fixture_dir.join("sysfs");
    if sysfs.is_dir() {
        assert_sysfs_contained(&sysfs)?;
    }
    Ok(())
}

/// Under `sudo`, hand every directory and file under `root` to the invoking
/// user, so a bundle written into their directory is theirs to delete.
/// Best-effort and fd-based (see `config::chown_created_to_invoker`): a
/// no-op when not under sudo or when `root` is outside the invoker's home;
/// symlinks are left alone (removing one needs only the directory).
pub fn own_tree(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(handle) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&dir)
        {
            chown_created_to_invoker(&dir, handle.as_raw_fd());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if let Ok(file) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
            {
                chown_created_to_invoker(&path, file.as_raw_fd());
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag:: 2>&1 | grep -E 'test result|FAILED|panicked|skipping'`
Expected: all pass (44 in `diag`; the tar test prints `skipping` only on a host without `tar`).

- [ ] **Step 5: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/diag/mod.rs src/diag/bundle.rs
git commit -m "feat(diag): bundle writer with manifest, UTC stamp, and tar archive

Every text file goes through the redactor and every file is recorded
with its size; the manifest carries the format version, UTC creation
time, redaction counts, unavailable notes, and the file list. Archiving
shells out to tar and degrades to a note. The embedded fixture is
re-checked with the capturer's SEC-1 and SEC-2 guards, and under sudo
the finished bundle is handed to the invoking user.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 7: `--support`, the orchestrator, the log tee, and the promotion to the default build

**Files:**
- Create: `src/diag/support.rs`
- Modify: `src/diag/mod.rs` (add `pub mod support;`)
- Modify: `src/main.rs` (module gates, CLI fields, logger, dispatch, window helper, `os_pretty_name_from` removal)
- Modify: `src/capture/mod.rs` (drop the three feature gates from Task 5; doc)
- Modify: `src/capture/meta.rs` (use `diag::collect::{read_trimmed, os_pretty_name_from}`)
- Modify: `src/fixture_replay.rs` (module doc)
- Modify: `src/usbmon/binary.rs:64`, `src/usbmon/reader.rs:40`, `src/device/manager.rs:130` (seam gates)
- Modify: `src/usbids/mod.rs` (`resolve_from_chain` becomes `pub(crate)`)
- Modify: `.github/workflows/ci.yml` (comment)

**Interfaces:**
- Consumes: everything Tasks 2 through 6 produced; `capture::{run_capture_fixture, CaptureFixtureOpts, CaptureOutcome, assemble_bundle, BaselineSource}`; `fixture_replay::{replay_fixture_with_elapsed, FixtureSource}`; `headless::export::{ReportSink, RunRecord, enabled_features}`; `tui::sync::{probe_decision, probe_sync_mode, ProbeDecision, SyncMode}` (both `pub` in a `pub(crate) mod sync`); `usbmon::{check_usbmon_status, UsbmonStatus}`; `usbids::{source_chain, resolve_from_chain}`; `config::{config_home, preferences_path, sudo_invoker, Preferences, CONFIG_DIR_NAME}`.
- Produces:
  ```rust
  pub struct SupportOpts { pub window: Duration, pub no_capture: bool, pub command: Vec<String> }
  pub struct Prepared { pub dir: PathBuf, pub archive: PathBuf }
  pub fn prepare_dir(target: &Path, now_unix: u64) -> anyhow::Result<Prepared>;
  pub struct Roots { pub sysfs_devices, proc, sys, etc, dev, debugfs_usbmon, dmi, device_tree, btf, thunderbolt, typec, power_delivery: PathBuf, pub home: Option<PathBuf>, pub config_dir: Option<PathBuf>, pub preferences_file: Option<PathBuf>, pub usbids_chain: Vec<PathBuf> }
  impl Roots { pub fn live(cli_config: Option<&Path>, cli_usbids: Option<&Path>) -> Roots; }
  pub struct Environment { pub usbmon: Result<UsbmonStatus, String>, pub terminal: TerminalInfo, pub effective_uid: u32, pub under_sudo: bool, pub rust_log: Option<String>, pub virtualization: Option<String>, pub dmesg: Result<String, String>, pub usbids: Option<UsbIds> }
  impl Environment { pub fn live(roots: &Roots) -> Environment; pub fn capture_decision(&self, no_capture: bool) -> Result<(), String>; }
  pub enum CaptureState { Captured { window: Duration, sources: Vec<FixtureSource>, events: u64, kernel_dropped: Option<u64> }, Skipped(String), Failed(String) }
  pub enum ArchiveState { Pending, Written(PathBuf, u64), Missing(String) }
  pub struct Summary { pub dir_name: String, pub archive: ArchiveState, pub file_count: usize, pub version: String, pub host: String, pub usbmon: String, pub backend: String, pub capture: String, pub devices: String, pub notes: Vec<Note>, pub redacted: String }
  pub fn run_support(opts: &SupportOpts, roots: &Roots, env: &Environment, prepared: &Prepared, now_unix: u64) -> anyhow::Result<Summary>;
  pub fn render_summary(summary: &Summary) -> String;
  pub const GUIDANCE: &str;
  pub struct TeeWriter { .. }  impl TeeWriter { pub fn create(path: &Path, home: Option<&Path>) -> io::Result<TeeWriter>; }  impl Write for TeeWriter
  pub fn init_logger(verbose: bool, tee: Option<TeeWriter>);
  pub fn live_terminal() -> TerminalInfo;
  ```

- [ ] **Step 1: Write the failing tests**

Add `pub mod support;` to `src/diag/mod.rs`. Create `src/diag/support.rs` with this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::collect::collect_terminal;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn prepare_dir_places_the_bundle_inside_a_directory_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("out");
        let p = prepare_dir(&target, 1_788_000_000).unwrap();
        assert_eq!(p.dir, target.canonicalize().unwrap().join("usbtop-ng-support-20260829T104000Z"));
        assert_eq!(p.archive, target.canonicalize().unwrap().join("usbtop-ng-support-20260829T104000Z.tar.gz"));
        assert!(p.dir.is_dir(), "the directory is created up front");
        assert!(!p.archive.exists());
    }

    #[test]
    fn prepare_dir_treats_a_tar_gz_target_as_the_archive_name() {
        let temp = tempfile::tempdir().unwrap();
        let p = prepare_dir(&temp.path().join("bug-42.tar.gz"), 0).unwrap();
        let parent = temp.path().canonicalize().unwrap();
        assert_eq!(p.dir, parent.join("usbtop-ng-support-19700101T000000Z"));
        assert_eq!(p.archive, parent.join("bug-42.tar.gz"));
    }

    #[test]
    fn prepare_dir_refuses_an_existing_bundle_directory() {
        let temp = tempfile::tempdir().unwrap();
        prepare_dir(temp.path(), 0).unwrap();
        let err = prepare_dir(temp.path(), 0).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    fn status(available: bool) -> UsbmonStatus {
        UsbmonStatus {
            module_loaded: available,
            debugfs_mounted: available,
            usbmon_available: available,
            binary_available: available,
            text_available: available,
            permission_denied: false,
            available_buses: if available { vec![0, 1] } else { Vec::new() },
        }
    }

    fn environment(uid: u32, usbmon: Result<UsbmonStatus, String>) -> Environment {
        Environment {
            usbmon,
            terminal: collect_terminal(&|_| None, None, false, false, "not probed: test"),
            effective_uid: uid,
            under_sudo: false,
            rust_log: None,
            virtualization: Some("none".to_string()),
            dmesg: Err("permission denied".to_string()),
            usbids: None,
        }
    }

    #[test]
    fn capture_decision_explains_each_skip() {
        let root_ok = environment(0, Ok(status(true)));
        assert_eq!(root_ok.capture_decision(false), Ok(()));
        assert_eq!(root_ok.capture_decision(true).unwrap_err(), "skipped: --no-capture");
        let user = environment(1000, Ok(status(true)));
        assert!(user.capture_decision(false).unwrap_err().contains("not running as root"));
        let no_usbmon = environment(0, Ok(status(false)));
        assert!(no_usbmon.capture_decision(false).unwrap_err().contains("no usbmon interface"));
        let broken = environment(0, Err("boom".to_string()));
        assert!(broken.capture_decision(false).unwrap_err().contains("no usbmon interface"));
    }

    #[test]
    fn summary_lines_match_the_spec_shapes() {
        let mut r = Redactor::new(None);
        let build = collect::collect_build(&["usbtop-ng".to_string()], None, 0, false, &mut r);
        let version = version_line(&build);
        assert!(version.starts_with(&format!("usbtop-ng {} (features: ", env!("CARGO_PKG_VERSION"))), "{version}");
        assert!(version.ends_with(std::env::consts::ARCH), "{version}");

        let host = collect::HostInfo {
            kernel: "7.0.0-30-generic".into(),
            proc_version: String::new(),
            os: "Linux Mint 22.3".into(),
            board: "MG-VCP17A-3080".into(),
            soc: String::new(),
            cpu_model: String::new(),
            cpu_count: 0,
            mem_total_kb: None,
            uptime_s: None,
            virtualization: None,
            cmdline: String::new(),
            lockdown: String::new(),
            usbcore_params: Default::default(),
        };
        assert_eq!(host_line(&host), "Linux 7.0.0-30-generic, Linux Mint 22.3, MG-VCP17A-3080");

        let (usbmon_info, _) = collect::collect_usbmon(&Ok(status(true)), Path::new("/nonexistent"), Path::new("/nonexistent"));
        assert_eq!(usbmon_line(&usbmon_info, true), "module loaded, 2 buses, no /dev/usbmon* nodes, running as root");

        let backend = collect::BackendInfo {
            would_select: "mmap",
            reason: String::new(),
            probed_bus: Some(0),
            ring_bytes: Some(64 * 1024 * 1024),
            ebpf_built_in: false,
            btf_present: true,
        };
        assert_eq!(backend_line(&backend), "mmap ring (64 MiB) would be selected; eBPF: BTF present, not built in");

        let captured = CaptureState::Captured {
            window: Duration::from_secs(5),
            sources: vec![FixtureSource::Binary, FixtureSource::Text],
            events: 1234,
            kernel_dropped: Some(0),
        };
        assert_eq!(capture_line(&captured), "5.0 s aggregate, 1,234 events, kernel drops 0, sources binary+text");
        assert_eq!(capture_line(&CaptureState::Skipped("skipped: --no-capture".into())), "skipped: --no-capture");

        assert_eq!(
            redacted_line(&[("home_path".to_string(), 3), ("mac_address".to_string(), 1)]),
            "3 home paths, 1 MAC address; host identity never collected; device serials included"
        );
        assert_eq!(redacted_line(&[]), "nothing rewritten; host identity never collected; device serials included");
        assert_eq!(with_commas(1_234_567), "1,234,567");
        assert_eq!(format_size(412_300), "412 KB");
        assert_eq!(format_size(3_400_000), "3.4 MB");
    }

    #[test]
    fn render_summary_has_the_ten_line_layout() {
        let summary = Summary {
            dir_name: "usbtop-ng-support-20260903T091500Z".into(),
            archive: ArchiveState::Written(PathBuf::from("./usbtop-ng-support-20260903T091500Z.tar.gz"), 412_300),
            file_count: 14,
            version: "usbtop-ng 1.5.0 (features: none) x86_64".into(),
            host: "Linux 7.0.0-30-generic, Linux Mint 22.3, MG-VCP17A-3080".into(),
            usbmon: "module loaded, 4 buses, /dev/usbmon* root:root 0600, running as root".into(),
            backend: "mmap ring (64 MiB) would be selected; eBPF: BTF present, not built in".into(),
            capture: "5.0 s aggregate, 1,234 events, kernel drops 0, sources binary+text".into(),
            devices: "21 across 4 buses (1.5/12/480/5000/10000 Mbps)".into(),
            notes: vec![note("dmesg", "permission denied")],
            redacted: "3 home paths; host identity never collected; device serials included".into(),
        };
        let text = render_summary(&summary);
        let expected = "usbtop-ng support bundle\n\
                        \x20 bundle:   ./usbtop-ng-support-20260903T091500Z.tar.gz (412 KB, 14 files)\n\
                        \x20 version:  usbtop-ng 1.5.0 (features: none) x86_64\n\
                        \x20 host:     Linux 7.0.0-30-generic, Linux Mint 22.3, MG-VCP17A-3080\n\
                        \x20 usbmon:   module loaded, 4 buses, /dev/usbmon* root:root 0600, running as root\n\
                        \x20 backend:  mmap ring (64 MiB) would be selected; eBPF: BTF present, not built in\n\
                        \x20 capture:  5.0 s aggregate, 1,234 events, kernel drops 0, sources binary+text\n\
                        \x20 devices:  21 across 4 buses (1.5/12/480/5000/10000 Mbps)\n\
                        \x20 notes:    dmesg: permission denied\n\
                        \x20 redacted: 3 home paths; host identity never collected; device serials included\n";
        assert_eq!(text, expected);

        let pending = Summary { archive: ArchiveState::Pending, notes: Vec::new(), ..summary };
        let text = render_summary(&pending);
        assert!(text.contains("  bundle:   usbtop-ng-support-20260903T091500Z/ (14 files)\n"), "{text}");
        assert!(text.contains("  notes:    none\n"), "{text}");
        let missing = Summary { archive: ArchiveState::Missing("could not run tar: not found".into()), ..pending };
        let text = render_summary(&missing);
        assert!(text.contains("not archived: could not run tar: not found"), "{text}");
        assert!(text.contains("tar -czf usbtop-ng-support-20260903T091500Z.tar.gz usbtop-ng-support-20260903T091500Z"), "{text}");
    }

    #[test]
    fn guidance_pins_the_url_and_the_four_steps() {
        assert!(GUIDANCE.contains("https://github.com/wifi-blackout/usbtop-ng/issues/new?template=bug_report.yml"));
        for step in ["  1. Review the bundle", "  2. Open https://", "  3. Paste the summary", "  4. Describe what you expected"] {
            assert!(GUIDANCE.contains(step), "{step}");
        }
        assert!(GUIDANCE.contains("tar tzf <archive>"));
        assert!(GUIDANCE.contains("serial numbers"));
    }

    #[test]
    fn tee_writer_redacts_the_file_copy() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbtop-ng.log");
        let mut tee = TeeWriter::create(&path, Some(Path::new("/home/alice"))).unwrap();
        tee.write_all(b"[INFO] usb.ids loaded from /home/alice/.usbtop-ng/usb.ids\n").unwrap();
        tee.flush().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[INFO] usb.ids loaded from ~/.usbtop-ng/usb.ids\n"
        );
    }

    /// A sysfs tree the capturer can materialize: a controller with a
    /// symlinked root hub and one device carrying a descriptor blob.
    fn fake_roots(base: &Path) -> Roots {
        let devices = base.join("sys/bus/usb/devices");
        let ctrl = base.join("sys/devices/pci0000:00/0000:00:14.0");
        let usb1 = ctrl.join("usb1");
        write(&usb1, "busnum", "1\n");
        write(&usb1, "devnum", "1\n");
        write(&usb1, "speed", "480\n");
        write(&usb1, "idVendor", "1d6b\n");
        write(&usb1, "idProduct", "0002\n");
        write(&usb1, "descriptors", "");
        std::fs::create_dir_all(&devices).unwrap();
        std::os::unix::fs::symlink(&usb1, devices.join("usb1")).unwrap();
        let dev = devices.join("1-1");
        write(&dev, "busnum", "1\n");
        write(&dev, "devnum", "3\n");
        write(&dev, "speed", "480\n");
        write(&dev, "idVendor", "0430\n");
        write(&dev, "idProduct", "0100\n");
        write(&dev, "serial", "SN-KEEP-ME\n");
        write(&dev, "descriptors", "\x12\x01\x00\x02\x00\x00\x00\x40\x30\x04\x00\x01\x00\x01\x00\x00\x00\x01");
        write(base, "proc/sys/kernel/osrelease", "7.0.0-30-generic\n");
        write(base, "proc/version", "Linux version 7.0.0-30-generic\n");
        write(base, "proc/cpuinfo", "processor\t: 0\nmodel name\t: Test CPU\n");
        write(base, "proc/meminfo", "MemTotal: 1024 kB\n");
        write(base, "proc/uptime", "1.5 2.0\n");
        write(base, "proc/cmdline", "root=UUID=aaaa-bbbb ro\n");
        write(base, "sys/module/usbcore/parameters/autosuspend", "2\n");
        write(base, "sys/kernel/security/lockdown", "[none]\n");
        write(base, "etc/os-release", "PRETTY_NAME=\"Test OS\"\n");
        write(base, "dmi/product_name", "Test Board\n");
        write(base, "dmi/sys_vendor", "Test\n");
        std::fs::create_dir_all(base.join("dev")).unwrap();
        let typec_real = base.join("sys/devices/platform/typec/port0");
        write(&typec_real, "data_role", "[host] device\n");
        std::fs::create_dir_all(base.join("sys/class/typec")).unwrap();
        std::os::unix::fs::symlink(&typec_real, base.join("sys/class/typec/port0")).unwrap();
        let home = base.join("home/alice");
        write(&home, ".usbtop-ng/preferences.toml", &format!("usbids_path = \"{}/usb.ids\"\n", home.display()));
        Roots {
            sysfs_devices: devices,
            proc: base.join("proc"),
            sys: base.join("sys"),
            etc: base.join("etc"),
            dev: base.join("dev"),
            debugfs_usbmon: base.join("sys/kernel/debug/usb/usbmon"),
            dmi: base.join("dmi"),
            device_tree: base.join("proc/device-tree"),
            btf: base.join("sys/kernel/btf/vmlinux"),
            thunderbolt: base.join("sys/bus/thunderbolt/devices"),
            typec: base.join("sys/class/typec"),
            power_delivery: base.join("sys/class/usb_power_delivery"),
            home: Some(home.clone()),
            config_dir: Some(home.join(".usbtop-ng")),
            preferences_file: Some(home.join(".usbtop-ng/preferences.toml")),
            usbids_chain: vec![home.join(".usbtop-ng/usb.ids")],
        }
    }

    /// The hermetic end-to-end: a non-root `--support --no-capture` against
    /// a fake tree writes a bundle whose manifest matches the files on disk,
    /// whose redaction counts match, whose fixture is static and passes the
    /// capturer's invariants, and which carries no home path anywhere.
    #[test]
    fn run_support_without_capture_writes_a_consistent_static_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let roots = fake_roots(temp.path());
        let home = roots.home.clone().unwrap();
        let prepared = prepare_dir(&temp.path().join("out"), 1_788_000_000).unwrap();
        // What main's tee would have written before run_support starts.
        std::fs::write(prepared.dir.join("usbtop-ng.log"), "[INFO] starting usbtop-ng\n").unwrap();
        let env = environment(1000, Ok(status(false)));
        let opts = SupportOpts {
            window: Duration::from_secs(1),
            no_capture: true,
            command: vec!["usbtop-ng".into(), "--support".into(), "--no-capture".into()],
        };

        let summary = run_support(&opts, &roots, &env, &prepared, 1_788_000_000).unwrap();
        let dir = &prepared.dir;

        // The manifest lists every file on disk (except itself) with its size.
        let manifest: bundle::Manifest =
            toml::from_str(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
        let mut listed: Vec<String> = manifest.files.iter().map(|f| f.path.clone()).collect();
        listed.sort();
        for entry in &manifest.files {
            let meta = std::fs::symlink_metadata(dir.join(&entry.path)).unwrap();
            if entry.symlink {
                assert!(meta.file_type().is_symlink(), "{}", entry.path);
            } else {
                assert_eq!(meta.len(), entry.bytes, "{}", entry.path);
            }
        }
        let mut on_disk = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let m = std::fs::symlink_metadata(&p).unwrap();
                if m.is_dir() && !m.file_type().is_symlink() {
                    stack.push(p);
                } else {
                    on_disk.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        on_disk.retain(|p| p != "manifest.toml");
        on_disk.sort();
        assert_eq!(listed, on_disk);
        for expected in [
            "build.toml", "host.toml", "usbmon.toml", "inventory/usb.toml", "inventory/descriptors/1-1.bin",
            "inventory/thunderbolt.toml", "inventory/typec.toml", "config/config.toml", "config/preferences.toml",
            "terminal.toml", "fixture/meta.toml", "fixture/internal-devices.toml", "fixture/sysfs/usb1",
            "report.json", "SUMMARY.txt", "usbtop-ng.log",
        ] {
            assert!(listed.iter().any(|p| p == expected), "missing {expected}: {listed:?}");
        }
        assert_eq!(summary.file_count, listed.len() + 1, "files plus the manifest");

        // The fixture is static, valid, and replayed into report.json.
        let meta = std::fs::read_to_string(dir.join("fixture/meta.toml")).unwrap();
        assert!(meta.contains("sources = []"), "{meta}");
        assert!(!dir.join("fixture/sysfs/1-1/serial").exists(), "the fixture never carries a serial");
        bundle::assert_fixture_invariants(&dir.join("fixture")).unwrap();
        let report = std::fs::read_to_string(dir.join("report.json")).unwrap();
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines.len(), 2);
        let head: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(head["record"], "run");
        assert_eq!(head["backend"], "none");
        let doc: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(doc["source"], "none");
        assert_eq!(doc["window_seconds"], 1.0);

        // Device identity in, host identity out.
        let usb = std::fs::read_to_string(dir.join("inventory/usb.toml")).unwrap();
        assert!(usb.contains("serial = \"SN-KEEP-ME\""), "{usb}");
        let home_text = home.display().to_string();
        for path in &on_disk {
            if path.ends_with(".toml") || path.ends_with(".txt") || path.ends_with(".json") || path.ends_with(".log") {
                let text = std::fs::read_to_string(dir.join(path)).unwrap();
                assert!(!text.contains(&home_text), "{path} leaks the home path: {text}");
            }
        }
        // config dir, preferences path, and the preferences body: three rewrites.
        assert_eq!(manifest.redaction.get("home_path"), Some(&3));
        assert_eq!(manifest.redaction.get("fs_uuid"), Some(&1));
        // Rules sort by name in the summary: fs_uuid before home_path.
        assert!(summary.redacted.starts_with("1 filesystem UUID, 3 home paths"), "{}", summary.redacted);

        // Notes and summary.
        let items: Vec<&str> = manifest.unavailable.iter().map(|n| n.item.as_str()).collect();
        assert!(items.contains(&"dmesg"), "{items:?}");
        assert!(items.contains(&"capture"), "{items:?}");
        assert_eq!(summary.capture, "skipped: --no-capture");
        assert_eq!(summary.devices, "1 across 1 buses (480 Mbps)");
        let summary_text = std::fs::read_to_string(dir.join("SUMMARY.txt")).unwrap();
        assert!(summary_text.starts_with("usbtop-ng support bundle\n"));
        assert!(summary_text.contains("  bundle:   usbtop-ng-support-20260829T104000Z/ ("), "{summary_text}");
        match &summary.archive {
            ArchiveState::Written(path, bytes) => {
                assert_eq!(path, &prepared.archive);
                assert_eq!(*bytes, std::fs::metadata(path).unwrap().len());
            }
            ArchiveState::Missing(reason) => assert!(reason.contains("tar"), "{reason}"),
            ArchiveState::Pending => panic!("run_support must settle the archive state"),
        }
    }

    /// Live, behind the `integration` feature, following the convention of
    /// the other root-only tests (`config`, `usbmon::mmap_ring`): as root
    /// with a usable usbmon interface, a real `--support` run captures a
    /// fixture whose goldens replay and, on an idle bus, reports zero kernel
    /// drops. Skips with a message otherwise.
    #[cfg(all(test, feature = "integration"))]
    mod live {
        use super::*;
        use crate::fixture_replay::{replay_fixture, to_masked_value};

        #[test]
        fn live_support_as_root_captures_a_replayable_fixture() {
            let roots = Roots::live(None, None);
            let env = Environment::live(&roots);
            if let Err(reason) = env.capture_decision(false) {
                eprintln!("skipping: {reason}");
                return;
            }
            let temp = tempfile::tempdir().unwrap();
            let prepared = prepare_dir(temp.path(), 1_788_000_000).unwrap();
            let opts = SupportOpts {
                window: Duration::from_secs(1),
                no_capture: false,
                command: vec!["usbtop-ng".into(), "--support".into()],
            };
            let summary = run_support(&opts, &roots, &env, &prepared, 1_788_000_000).unwrap();
            assert!(summary.capture.starts_with("1.0 s aggregate"), "{}", summary.capture);
            assert!(
                summary.capture.contains("kernel drops 0"),
                "an idle bus loses nothing (run this with no device streaming): {}",
                summary.capture
            );
            let fixture = prepared.dir.join("fixture");
            for source in [FixtureSource::Binary, FixtureSource::Text] {
                if !fixture.join(source.trace_filename()).exists() {
                    continue;
                }
                let report = replay_fixture(&fixture, source).unwrap();
                let got = to_masked_value(&serde_json::to_string(&report).unwrap()).unwrap();
                let golden = to_masked_value(
                    &std::fs::read_to_string(fixture.join(source.golden_filename())).unwrap(),
                )
                .unwrap();
                assert_eq!(got, golden, "golden must equal replay for {source:?}");
            }
            let mut stack = vec![fixture.join("sysfs")];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                    let path = entry.path();
                    assert_ne!(
                        path.file_name().and_then(|n| n.to_str()),
                        Some("serial"),
                        "the fixture never carries a serial: {}",
                        path.display()
                    );
                    if path.is_dir() {
                        stack.push(path);
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test diag::support 2>&1 | tail -5`
Expected: compile errors.

- [ ] **Step 3: Promote the modules and seams**

`src/main.rs`:
- Replace `#[cfg(any(test, feature = "capture-fixture"))]\nmod capture;` with `mod capture;`; replace `#[cfg(any(test, feature = "capture-fixture"))]\nmod fixture_replay;` with `mod fixture_replay;`; replace `#[cfg(test)]\nmod diag;` with `mod diag;`.
- Delete the private `os_pretty_name_from` and its test that Task 1 added; the run-record construction in the headless branch calls `diag::collect::os_pretty_name_from(&std::fs::read_to_string("/etc/os-release").unwrap_or_default()).unwrap_or_default()` instead.
- Rename `resolve_capture_fixture_window` to `resolve_capture_window`, drop its `#[cfg(feature = "capture-fixture")]` and the cfg on its three tests, and change its doc comment's first sentence to "Resolve `--window` for `--capture-fixture` and `--support` into a [`Duration`]: …". The `--capture-fixture` dispatch calls the new name.

`src/usbmon/binary.rs:64`, `src/usbmon/reader.rs:40`, `src/device/manager.rs:130`: delete the `#[cfg(any(test, feature = "capture-fixture"))]` line above each `with_path`/`with_sysfs_base`; leave the doc comments.

`src/capture/mod.rs`: delete the three `#[cfg(feature = "capture-fixture")]` attributes Task 5 placed on `CaptureOutcome`, `CaptureFixtureOpts`, `run_capture_fixture`, and `stage_id_from_outdir`.

`src/capture/meta.rs`: delete the private `read_trimmed` and `os_pretty_name` functions and the `read_trimmed_flattens_interior_nuls_from_device_tree_string_lists` test (Task 3 owns both now); add `use crate::diag::collect::{os_pretty_name_from, read_trimmed};` and `use std::path::Path;`, and in `gather_host_identity` call `read_trimmed(Path::new("/proc/device-tree/model"))` (and the other three paths the same way) and `os: std::fs::read_to_string("/etc/os-release").ok().and_then(|t| os_pretty_name_from(&t)).unwrap_or_default()`.

`src/fixture_replay.rs` module doc: replace the first paragraph with "Shared replay core: the default test suite's corpus harness, the `--capture-fixture` capturer, and `--support`'s embedded fixture all generate reports by this one path, so a committed golden equals what replay produces, by construction. The corpus-discovery items stay `cfg(test)`."

`src/usbids/mod.rs`: `fn resolve_from_chain(paths: &[&Path]) -> Option<UsbIds>` becomes `pub(crate) fn resolve_from_chain(…)`.

`.github/workflows/ci.yml`: under the `test` job's `Run clippy` step, add the comment line `# The default build now compiles the capture core (src/capture, src/fixture_replay) and src/diag; only --capture-fixture stays behind its feature.` above `run: cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 4: Implement `support.rs`**

Above the test module:

```rust
//! The `--support` orchestrator: runs every collector, embeds a fixture (a
//! live usbmon capture as root, a static one otherwise), replays it into
//! `report.json`, writes the manifest, archives the directory, and returns
//! the summary the CLI prints with the filing guidance. Nothing here changes
//! the system: no modprobe, no prompts, no network. Every filesystem root is
//! injectable through [`Roots`] and every live probe result arrives through
//! [`Environment`], so the whole run is testable against a fake tree.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context};
use log::info;
use serde::Serialize;

use super::bundle::{self, utc_stamp, BundleWriter};
use super::collect::{self, BackendInfo, BuildInfo, HostInfo, TerminalInfo, UsbmonInfo};
use super::inventory::{self, AttrDump, UsbInventory};
use super::redact::Redactor;
use super::{note, Note};
use crate::capture::{self, BaselineSource, CaptureFixtureOpts};
use crate::config;
use crate::fixture_replay::{replay_fixture_with_elapsed, FixtureSource};
use crate::headless::export::{enabled_features, ReportSink, RunRecord};
use crate::tui::sync::{probe_decision, probe_sync_mode, ProbeDecision, SyncMode};
use crate::usbids::{self, UsbIds};
use crate::usbmon::{self, UsbmonStatus};

pub struct SupportOpts {
    pub window: Duration,
    pub no_capture: bool,
    /// The command line as run (redacted when written).
    pub command: Vec<String>,
}

/// The bundle directory (created) and the archive path (not yet written).
pub struct Prepared {
    pub dir: PathBuf,
    pub archive: PathBuf,
}

/// Resolve `--support`'s target: a directory (existing or not) holds the
/// bundle directory and the archive; a name ending in `.tar.gz` names the
/// archive and the bundle directory goes beside it. The bundle directory is
/// created here so the logger can tee into it before anything else runs.
pub fn prepare_dir(target: &Path, now_unix: u64) -> anyhow::Result<Prepared> {
    let stamp = utc_stamp(now_unix);
    let name = format!("usbtop-ng-support-{stamp}");
    let is_archive = target.to_string_lossy().ends_with(".tar.gz");
    let (parent, archive_name) = if is_archive {
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file = target
            .file_name()
            .ok_or_else(|| anyhow!("{} has no file name", target.display()))?;
        (parent.to_path_buf(), file.to_os_string())
    } else {
        (target.to_path_buf(), format!("{name}.tar.gz").into())
    };
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    let parent = std::fs::canonicalize(&parent)
        .with_context(|| format!("could not resolve {}", parent.display()))?;
    let dir = parent.join(&name);
    if dir.exists() {
        return Err(anyhow!("{} already exists", dir.display()));
    }
    std::fs::create_dir(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(Prepared {
        dir,
        archive: parent.join(archive_name),
    })
}

/// Every filesystem root the collectors read.
pub struct Roots {
    pub sysfs_devices: PathBuf,
    pub proc: PathBuf,
    pub sys: PathBuf,
    pub etc: PathBuf,
    pub dev: PathBuf,
    pub debugfs_usbmon: PathBuf,
    pub dmi: PathBuf,
    pub device_tree: PathBuf,
    pub btf: PathBuf,
    pub thunderbolt: PathBuf,
    pub typec: PathBuf,
    pub power_delivery: PathBuf,
    pub home: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub preferences_file: Option<PathBuf>,
    pub usbids_chain: Vec<PathBuf>,
}

impl Roots {
    /// The real roots, with the config directory and usb.ids chain resolved
    /// the way the monitoring path resolves them (sudo invoker's home,
    /// `--config`, the preferences' `usbids_path`, the home copy, the distro
    /// files). The preferences file is read if present and never created.
    pub fn live(cli_config: Option<&Path>, cli_usbids: Option<&Path>) -> Roots {
        let home = config::config_home().ok();
        let config_dir = home.as_ref().map(|h| h.join(config::CONFIG_DIR_NAME));
        let preferences_file = match cli_config {
            Some(path) => Some(path.to_path_buf()),
            None => config::preferences_path().ok(),
        };
        let preferences: Option<config::Preferences> = preferences_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str(&text).ok());
        let pref_usbids = preferences
            .as_ref()
            .and_then(|p| p.usbids_path.clone())
            .map(PathBuf::from);
        let home_copy = config_dir.as_ref().map(|d| d.join("usb.ids"));
        let usbids_chain = usbids::source_chain(cli_usbids, pref_usbids.as_deref(), home_copy.as_deref());
        Roots {
            sysfs_devices: PathBuf::from("/sys/bus/usb/devices"),
            proc: PathBuf::from("/proc"),
            sys: PathBuf::from("/sys"),
            etc: PathBuf::from("/etc"),
            dev: PathBuf::from("/dev"),
            debugfs_usbmon: PathBuf::from("/sys/kernel/debug/usb/usbmon"),
            dmi: PathBuf::from("/sys/devices/virtual/dmi/id"),
            device_tree: PathBuf::from("/proc/device-tree"),
            btf: PathBuf::from("/sys/kernel/btf/vmlinux"),
            thunderbolt: PathBuf::from("/sys/bus/thunderbolt/devices"),
            typec: PathBuf::from("/sys/class/typec"),
            power_delivery: PathBuf::from("/sys/class/usb_power_delivery"),
            home,
            config_dir,
            preferences_file,
            usbids_chain,
        }
    }
}

/// Everything that comes from a live probe rather than a file under a root,
/// gathered once by `main` so `run_support` itself is pure over its inputs.
pub struct Environment {
    pub usbmon: Result<UsbmonStatus, String>,
    pub terminal: TerminalInfo,
    pub effective_uid: u32,
    pub under_sudo: bool,
    pub rust_log: Option<String>,
    pub virtualization: Option<String>,
    pub dmesg: Result<String, String>,
    pub usbids: Option<UsbIds>,
}

impl Environment {
    pub fn live(roots: &Roots) -> Environment {
        let chain: Vec<&Path> = roots.usbids_chain.iter().map(PathBuf::as_path).collect();
        Environment {
            usbmon: usbmon::check_usbmon_status().map_err(|e| e.to_string()),
            terminal: live_terminal(),
            // SAFETY: geteuid() takes no arguments, touches no memory, and
            // cannot fail.
            effective_uid: unsafe { libc::geteuid() },
            under_sudo: config::sudo_invoker().is_some(),
            rust_log: std::env::var("RUST_LOG").ok(),
            virtualization: collect::detect_virtualization(),
            dmesg: collect::run_dmesg(),
            usbids: usbids::resolve_from_chain(&chain),
        }
    }

    /// Whether a live capture can run, or the reason it is skipped (the
    /// note's text and the summary's `capture:` line).
    pub fn capture_decision(&self, no_capture: bool) -> Result<(), String> {
        if no_capture {
            return Err("skipped: --no-capture".to_string());
        }
        if self.effective_uid != 0 {
            return Err(
                "skipped: not running as root; run with sudo to include a usbmon capture".to_string(),
            );
        }
        if !self.usbmon.as_ref().is_ok_and(|s| s.usbmon_available) {
            return Err(
                "skipped: no usbmon interface is available; run 'sudo modprobe usbmon' first"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// The live terminal facts for `terminal.toml`. The mode-2026 handshake
/// runs only when both stdin and stdout are terminals and the session is
/// local (the same policy the TUI applies), inside a raw-mode bracket that
/// is undone even if the probe panics.
pub fn live_terminal() -> TerminalInfo {
    // SAFETY: isatty() reads a descriptor's type; no memory, cannot fail.
    let stdin_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    let stdout_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1;
    let env = |name: &str| std::env::var(name).ok();
    let sync_mode = if !(stdin_tty && stdout_tty) {
        "not probed: stdin or stdout is not a terminal".to_string()
    } else {
        match probe_decision(
            env("SSH_TTY").as_deref(),
            env("SSH_CONNECTION").as_deref(),
            env("SSH_CLIENT").as_deref(),
            env("TERM").as_deref(),
        ) {
            ProbeDecision::AssumeUnsupported => {
                "not probed: remote session, assumed unsupported".to_string()
            }
            ProbeDecision::Probe => match probe_in_raw_mode() {
                SyncMode::Supported => "supported".to_string(),
                SyncMode::Unsupported => "unsupported".to_string(),
            },
        }
    };
    collect::collect_terminal(
        &env,
        crossterm::terminal::size().ok(),
        stdout_tty,
        stdin_tty,
        &sync_mode,
    )
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn probe_in_raw_mode() -> SyncMode {
    if crossterm::terminal::enable_raw_mode().is_err() {
        return SyncMode::Unsupported;
    }
    let _guard = RawModeGuard;
    probe_sync_mode()
}

/// What happened to the capture.
pub enum CaptureState {
    Captured {
        window: Duration,
        sources: Vec<FixtureSource>,
        events: u64,
        kernel_dropped: Option<u64>,
    },
    Skipped(String),
    Failed(String),
}

/// Where the archive stands: not yet attempted (the copy of the summary
/// inside the bundle), written, or not producible.
pub enum ArchiveState {
    Pending,
    Written(PathBuf, u64),
    Missing(String),
}

pub struct Summary {
    pub dir_name: String,
    pub archive: ArchiveState,
    pub file_count: usize,
    pub version: String,
    pub host: String,
    pub usbmon: String,
    pub backend: String,
    pub capture: String,
    pub devices: String,
    pub notes: Vec<Note>,
    pub redacted: String,
}

#[derive(Serialize)]
struct UsbmonFile<'a> {
    usbmon: &'a UsbmonInfo,
    backend: &'a BackendInfo,
}

#[derive(Serialize)]
struct ThunderboltFile {
    devices: Vec<AttrDump>,
}

#[derive(Serialize)]
struct TypecFile {
    typec: Vec<AttrDump>,
    power_delivery: Vec<AttrDump>,
}

/// Embed the fixture: a live capture when [`Environment::capture_decision`]
/// allows it, else (or after a capture failure) a static bundle from the
/// sysfs tree alone. Returns what happened; failures become notes.
fn write_fixture(
    opts: &SupportOpts,
    roots: &Roots,
    env: &Environment,
    fixture_dir: &Path,
    notes: &mut Vec<Note>,
) -> CaptureState {
    let state = match env.capture_decision(opts.no_capture) {
        Ok(()) => {
            info!(
                "capturing the usbmon aggregate bus for {:.1} s",
                opts.window.as_secs_f64()
            );
            match capture::run_capture_fixture(CaptureFixtureOpts {
                outdir: fixture_dir.to_path_buf(),
                window: opts.window,
                bus: None,
                baseline: None,
            }) {
                Ok(outcome) => {
                    return CaptureState::Captured {
                        window: opts.window,
                        sources: outcome.sources,
                        events: outcome.events,
                        kernel_dropped: outcome.binary_kernel_dropped,
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(fixture_dir);
                    CaptureState::Failed(format!("failed: {e:#}; static fixture written instead"))
                }
            }
        }
        Err(reason) => CaptureState::Skipped(reason),
    };
    if let Err(e) = capture::assemble_bundle(
        &roots.sysfs_devices,
        fixture_dir,
        &[],
        &BaselineSource::CaptureFrom(roots.sysfs_devices.clone()),
        None,
    ) {
        let _ = std::fs::remove_dir_all(fixture_dir);
        notes.push(note("fixture", format!("could not write the static fixture: {e:#}")));
    }
    match &state {
        CaptureState::Skipped(reason) | CaptureState::Failed(reason) => {
            notes.push(note("capture", reason));
        }
        CaptureState::Captured { .. } => {}
    }
    state
}

/// Run the whole collection into `prepared.dir`. Fails only when a file in
/// the bundle cannot be written or the embedded fixture violates SEC-1 or
/// SEC-2; everything else becomes a note.
pub fn run_support(
    opts: &SupportOpts,
    roots: &Roots,
    env: &Environment,
    prepared: &Prepared,
    now_unix: u64,
) -> anyhow::Result<Summary> {
    let dir = &prepared.dir;
    let mut notes: Vec<Note> = Vec::new();
    let mut writer = BundleWriter::create(dir, Redactor::new(roots.home.as_deref()))
        .with_context(|| format!("could not create {}", dir.display()))?;

    info!("collecting build and host information");
    let build = collect::collect_build(
        &opts.command,
        env.rust_log.clone(),
        env.effective_uid,
        env.under_sudo,
        writer.redactor(),
    );
    writer.write_toml("build.toml", &build)?;

    let (host, host_notes) = collect::collect_host(
        &roots.proc,
        &roots.sys,
        &roots.etc,
        &roots.dmi,
        &roots.device_tree,
        env.virtualization.clone(),
        writer.redactor(),
    );
    notes.extend(host_notes);
    writer.write_toml("host.toml", &host)?;

    let (usbmon_info, usbmon_notes) =
        collect::collect_usbmon(&env.usbmon, &roots.dev, &roots.debugfs_usbmon);
    notes.extend(usbmon_notes);
    let backend = collect::probe_backend(
        &usbmon_info.available_buses,
        &roots.dev,
        &roots.debugfs_usbmon,
        &roots.btf,
    );
    writer.write_toml(
        "usbmon.toml",
        &UsbmonFile {
            usbmon: &usbmon_info,
            backend: &backend,
        },
    )?;
    match &env.dmesg {
        Ok(text) => {
            let masked = writer.redactor().mac_addresses(text);
            writer.write_text("dmesg-usb.txt", &masked)?;
        }
        Err(reason) => notes.push(note("dmesg", reason)),
    }

    info!("reading the USB device inventory");
    let chain: Vec<&Path> = roots.usbids_chain.iter().map(PathBuf::as_path).collect();
    let usbids_info = inventory::usbids_info(&chain, writer.redactor());
    let (inv, inv_notes) =
        inventory::collect_usb_inventory(&roots.sysfs_devices, env.usbids.as_ref(), usbids_info);
    notes.extend(inv_notes);
    writer.write_toml("inventory/usb.toml", &inv)?;
    let (blobs, blob_notes) = inventory::read_descriptor_blobs(&roots.sysfs_devices);
    notes.extend(blob_notes);
    for blob in &blobs {
        writer.write_bytes(
            &format!("inventory/descriptors/{}.bin", blob.port_chain),
            &blob.descriptors,
        )?;
        if let Some(bos) = &blob.bos {
            writer.write_bytes(
                &format!("inventory/descriptors/{}.bos.bin", blob.port_chain),
                bos,
            )?;
        }
    }
    let (thunderbolt, tb_notes) = inventory::dump_attrs(&roots.thunderbolt, 3);
    notes.extend(tb_notes);
    writer.write_toml(
        "inventory/thunderbolt.toml",
        &ThunderboltFile {
            devices: thunderbolt,
        },
    )?;
    let (typec, typec_notes) = inventory::dump_attrs(&roots.typec, 3);
    notes.extend(typec_notes);
    let (power_delivery, pd_notes) = inventory::dump_attrs(&roots.power_delivery, 3);
    notes.extend(pd_notes);
    writer.write_toml(
        "inventory/typec.toml",
        &TypecFile {
            typec,
            power_delivery,
        },
    )?;

    let (config_info, config_notes) = collect::collect_config(
        roots.config_dir.as_deref(),
        roots.preferences_file.as_deref(),
        env.under_sudo,
        writer.redactor(),
    );
    notes.extend(config_notes);
    writer.write_toml("config/config.toml", &config_info)?;
    if let Some(text) = &config_info.preferences {
        writer.write_text("config/preferences.toml", text)?;
    }
    if let Some(text) = &config_info.internal_devices {
        writer.write_text("config/internal-devices.toml", text)?;
    }

    writer.write_toml("terminal.toml", &env.terminal)?;

    let fixture_dir = dir.join("fixture");
    let capture_state = write_fixture(opts, roots, env, &fixture_dir, &mut notes);
    if fixture_dir.join("meta.toml").exists() {
        bundle::assert_fixture_invariants(&fixture_dir)?;
        writer.record_dir("fixture")?;
        let source = match &capture_state {
            CaptureState::Captured { sources, .. } => sources
                .iter()
                .copied()
                .find(|s| *s == FixtureSource::Binary)
                .or_else(|| sources.first().copied()),
            _ => None,
        };
        match replay_fixture_with_elapsed(&fixture_dir, source, opts.window) {
            Ok(report) => {
                let run = RunRecord {
                    record: "run",
                    usbtop_ng: build.version.clone(),
                    features: enabled_features(),
                    started_unix: now_unix,
                    window_seconds: opts.window.as_secs_f64(),
                    batch: false,
                    filters: Vec::new(),
                    command: build.command.clone(),
                    backend: backend.would_select.to_string(),
                    kernel: host.kernel.clone(),
                    os: host.os.clone(),
                    arch: std::env::consts::ARCH,
                    buses: usbmon_info.available_buses.clone(),
                };
                let mut sink = ReportSink::open(Some(&dir.join("report.json")), &run, true)?;
                sink.write(&report, true)?;
                sink.finish();
                writer.redact_file("report.json")?;
            }
            Err(e) => notes.push(note("report.json", format!("replay failed: {e:#}"))),
        }
    }

    let log_present = dir.join("usbtop-ng.log").exists();
    let mut summary = Summary {
        dir_name: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        archive: ArchiveState::Pending,
        // Recorded so far, plus SUMMARY.txt, the manifest, and the log.
        file_count: writer.files().len() + 2 + usize::from(log_present),
        version: version_line(&build),
        host: host_line(&host),
        usbmon: usbmon_line(&usbmon_info, build.running_as_root),
        backend: backend_line(&backend),
        capture: capture_line(&capture_state),
        devices: devices_line(&inv),
        notes: notes.clone(),
        redacted: redacted_line(&writer.redactor().summary()),
    };
    writer.write_text("SUMMARY.txt", &render_summary(&summary))?;

    info!("bundle assembled; writing the manifest");
    // Nothing logs past this line: the log is adopted with its final size,
    // and the archive must match the manifest.
    if log_present {
        writer.adopt_file("usbtop-ng.log")?;
    }
    writer.write_manifest(now_unix, &notes)?;
    summary.archive = match writer.archive(&prepared.archive) {
        Ok(bytes) => ArchiveState::Written(prepared.archive.clone(), bytes),
        Err(n) => {
            let reason = n.reason.clone();
            summary.notes.push(n);
            ArchiveState::Missing(reason)
        }
    };
    bundle::own_tree(dir);
    Ok(summary)
}

// --- summary lines --------------------------------------------------------

fn with_commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_size(bytes: u64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", (bytes + 500) / 1000)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

fn version_line(build: &BuildInfo) -> String {
    let features = if build.features.is_empty() {
        "none".to_string()
    } else {
        build.features.join(" ")
    };
    format!("usbtop-ng {} (features: {features}) {}", build.version, build.arch)
}

fn host_line(host: &HostInfo) -> String {
    let mut parts = Vec::new();
    if !host.kernel.is_empty() {
        parts.push(format!("Linux {}", host.kernel));
    }
    for s in [&host.os, &host.board] {
        if !s.is_empty() {
            parts.push(s.clone());
        }
    }
    parts.join(", ")
}

fn usbmon_line(u: &UsbmonInfo, running_as_root: bool) -> String {
    let mut parts = vec![
        if u.module_loaded {
            "module loaded".to_string()
        } else {
            "module not loaded".to_string()
        },
        format!("{} buses", u.available_buses.len()),
    ];
    match u.nodes.first() {
        Some(node) => {
            let owner = |id: u32| if id == 0 { "root".to_string() } else { id.to_string() };
            parts.push(format!(
                "/dev/usbmon* {}:{} {}",
                owner(node.owner_uid),
                owner(node.group_gid),
                node.mode_octal
            ));
        }
        None => parts.push("no /dev/usbmon* nodes".to_string()),
    }
    if u.permission_denied {
        parts.push("permission denied".to_string());
    }
    parts.push(if running_as_root {
        "running as root".to_string()
    } else {
        "not running as root".to_string()
    });
    parts.join(", ")
}

fn backend_line(b: &BackendInfo) -> String {
    let chosen = match b.would_select {
        "mmap" => format!(
            "mmap ring ({}) would be selected",
            b.ring_bytes
                .map(|n| format!("{} MiB", n / (1024 * 1024)))
                .unwrap_or_else(|| "size unknown".to_string())
        ),
        "binary" => "read()-based binary interface would be selected".to_string(),
        "text" => "debugfs text interface would be selected".to_string(),
        _ => "no usbmon interface would be selected".to_string(),
    };
    format!(
        "{chosen}; eBPF: BTF {}, {}",
        if b.btf_present { "present" } else { "absent" },
        if b.ebpf_built_in { "built in" } else { "not built in" }
    )
}

fn capture_line(state: &CaptureState) -> String {
    match state {
        CaptureState::Captured {
            window,
            sources,
            events,
            kernel_dropped,
        } => format!(
            "{:.1} s aggregate, {} events, kernel drops {}, sources {}",
            window.as_secs_f64(),
            with_commas(*events),
            kernel_dropped.map_or("unknown".to_string(), |n| n.to_string()),
            sources
                .iter()
                .map(|s| s.tag())
                .collect::<Vec<_>>()
                .join("+")
        ),
        CaptureState::Skipped(reason) | CaptureState::Failed(reason) => reason.clone(),
    }
}

fn devices_line(inv: &UsbInventory) -> String {
    let devices = inv
        .devices
        .iter()
        .filter(|d| !d.port_chain.starts_with("usb"))
        .count();
    let buses: usize = inv.controllers.iter().map(|c| c.buses.len()).sum();
    // Speeds carry a fraction (1.5 Mbps), so they are kept as the strings
    // sysfs printed and sorted numerically.
    let mut speeds: Vec<String> = inv
        .devices
        .iter()
        .filter_map(|d| d.speed.clone())
        .collect();
    speeds.sort_by(|a, b| {
        a.parse::<f64>()
            .unwrap_or(0.0)
            .partial_cmp(&b.parse::<f64>().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    speeds.dedup();
    if speeds.is_empty() {
        format!("{devices} across {buses} buses")
    } else {
        format!("{devices} across {buses} buses ({} Mbps)", speeds.join("/"))
    }
}

fn redacted_line(redaction: &[(String, usize)]) -> String {
    let label = |rule: &str, n: usize| -> String {
        let (one, many) = match rule {
            "home_path" => ("home path", "home paths"),
            "mac_address" => ("MAC address", "MAC addresses"),
            "fs_uuid" => ("filesystem UUID", "filesystem UUIDs"),
            other => (other, other),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    };
    let rewritten = if redaction.is_empty() {
        "nothing rewritten".to_string()
    } else {
        redaction
            .iter()
            .map(|(rule, n)| label(rule, *n))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{rewritten}; host identity never collected; device serials included")
}

fn bundle_line(s: &Summary) -> String {
    match &s.archive {
        ArchiveState::Pending => format!("{}/ ({} files)", s.dir_name, s.file_count),
        ArchiveState::Written(path, bytes) => format!(
            "{} ({}, {} files)",
            path.display(),
            format_size(*bytes),
            s.file_count
        ),
        ArchiveState::Missing(reason) => format!(
            "{}/ ({} files; not archived: {reason}. Archive it by hand from its parent directory: tar -czf {}.tar.gz {})",
            s.dir_name, s.file_count, s.dir_name, s.dir_name
        ),
    }
}

/// The ten-line block from the spec.
pub fn render_summary(s: &Summary) -> String {
    let mut out = String::from("usbtop-ng support bundle\n");
    out.push_str(&format!("  bundle:   {}\n", bundle_line(s)));
    out.push_str(&format!("  version:  {}\n", s.version));
    out.push_str(&format!("  host:     {}\n", s.host));
    out.push_str(&format!("  usbmon:   {}\n", s.usbmon));
    out.push_str(&format!("  backend:  {}\n", s.backend));
    out.push_str(&format!("  capture:  {}\n", s.capture));
    out.push_str(&format!("  devices:  {}\n", s.devices));
    if s.notes.is_empty() {
        out.push_str("  notes:    none\n");
    } else {
        for (i, n) in s.notes.iter().enumerate() {
            let label = if i == 0 { "  notes:    " } else { "            " };
            out.push_str(&format!("{label}{}: {}\n", n.item, n.reason));
        }
    }
    out.push_str(&format!("  redacted: {}\n", s.redacted));
    out
}

/// Printed after the summary. Sentence case, one action per line.
pub const GUIDANCE: &str = "\nTo report a bug:\n  \
1. Review the bundle before attaching it: `tar tzf <archive>` lists every file.\n     \
It carries your devices' full details, including their serial numbers, and\n     \
nothing about the host itself; you decide what to attach.\n  \
2. Open https://github.com/wifi-blackout/usbtop-ng/issues/new?template=bug_report.yml\n  \
3. Paste the summary above into \"Support summary\" and attach the .tar.gz.\n  \
4. Describe what you expected, what happened, and the exact command you ran.\n     \
For a display problem, name the terminal and say whether it was over SSH.\n";

// --- the log tee ----------------------------------------------------------

/// Writes every log record to stderr as before and, with home paths
/// rewritten, to `usbtop-ng.log` inside the bundle.
pub struct TeeWriter {
    file: File,
    redactor: Redactor,
}

impl TeeWriter {
    pub fn create(path: &Path, home: Option<&Path>) -> io::Result<TeeWriter> {
        Ok(TeeWriter {
            file: File::create(path)?,
            redactor: Redactor::new(home),
        })
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // stderr is best-effort, as it is for every other log line.
        let _ = io::stderr().write_all(buf);
        let text = String::from_utf8_lossy(buf);
        self.file.write_all(self.redactor.text(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The process logger: the same default-env builder and levels as before;
/// with a tee, records go through it (styles off, so the file has no escape
/// codes).
pub fn init_logger(verbose: bool, tee: Option<TeeWriter>) {
    let mut builder = env_logger::Builder::from_default_env();
    builder.filter_level(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
    if let Some(tee) = tee {
        builder.target(env_logger::Target::Pipe(Box::new(tee)));
        builder.write_style(env_logger::WriteStyle::Never);
    }
    builder.init();
}
```

- [ ] **Step 5: Wire `main.rs`**

CLI fields, after `snapshot_internal`:

```rust
    /// Gather a diagnostic bundle for a bug report into PATH (default: the
    /// current directory; a name ending in .tar.gz names the archive), then exit
    #[arg(
        long,
        value_name = "PATH",
        num_args = 0..=1,
        default_missing_value = ".",
        conflicts_with_all = [
            "once", "batch", "snapshot_internal", "update_usbids",
            "setup", "create_alias", "print_man", "print_completions"
        ]
    )]
    support: Option<String>,

    /// Skip the usbmon capture in --support (static information only)
    #[arg(long, requires = "support")]
    no_capture: bool,
```

On the `capture_fixture` field add `conflicts_with = "support"` to its `#[arg(...)]`.

Replace the logger block (`// Initialize logging` through `.init();` and the `info!("starting …")` line) with:

```rust
    // `--support` owns the logger: its bundle directory must exist before
    // the logger is built so every record can be teed into it.
    let started_unix = now_unix();
    let prepared = match cli.support.as_deref() {
        Some(target) => match diag::support::prepare_dir(Path::new(target), started_unix) {
            Ok(prepared) => Some(prepared),
            Err(e) => {
                eprintln!("error: {e:#}");
                process::exit(1);
            }
        },
        None => None,
    };
    let tee = match &prepared {
        Some(prepared) => {
            let log_path = prepared.dir.join("usbtop-ng.log");
            match diag::support::TeeWriter::create(&log_path, config_home().ok().as_deref()) {
                Ok(tee) => Some(tee),
                Err(e) => {
                    eprintln!("error: could not create {}: {e}", log_path.display());
                    process::exit(1);
                }
            }
        }
        None => None,
    };
    diag::support::init_logger(cli.verbose, tee);

    info!("starting usbtop-ng v{}", env!("CARGO_PKG_VERSION"));

    if let Some(prepared) = prepared {
        let window = resolve_capture_window(cli.window).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            process::exit(2);
        });
        let roots = diag::support::Roots::live(
            cli.config.as_deref().map(Path::new),
            cli.usbids.as_deref().map(Path::new),
        );
        let environment = diag::support::Environment::live(&roots);
        let opts = diag::support::SupportOpts {
            window,
            no_capture: cli.no_capture,
            command: env::args().collect(),
        };
        match diag::support::run_support(&opts, &roots, &environment, &prepared, started_unix) {
            Ok(summary) => {
                print!("{}", diag::support::render_summary(&summary));
                print!("{}", diag::support::GUIDANCE);
                return Ok(());
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                process::exit(1);
            }
        }
    }
```

and add near `resolve_window`:

```rust
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
```

(The headless run-record construction from Task 1 computes the same value inline; replace that with `started_unix`.)

Add to `main.rs`'s tests:

```rust
    #[test]
    fn cli_parses_support_with_and_without_a_value() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["usbtop-ng", "--support"]).unwrap();
        assert_eq!(bare.support.as_deref(), Some("."));
        assert!(!bare.no_capture);
        let named = Cli::try_parse_from(["usbtop-ng", "--support", "bug.tar.gz", "--no-capture", "--window", "2"]).unwrap();
        assert_eq!(named.support.as_deref(), Some("bug.tar.gz"));
        assert!(named.no_capture);
        assert_eq!(named.window, Some(2.0));
        assert!(Cli::try_parse_from(["usbtop-ng"]).unwrap().support.is_none());
    }

    #[test]
    fn support_conflicts_with_the_report_modes_and_no_capture_needs_it() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["usbtop-ng", "--support", "--once"]).is_err());
        assert!(Cli::try_parse_from(["usbtop-ng", "--support", "--batch"]).is_err());
        assert!(Cli::try_parse_from(["usbtop-ng", "--support", "--snapshot-internal"]).is_err());
        assert!(Cli::try_parse_from(["usbtop-ng", "--no-capture"]).is_err());
    }
```

- [ ] **Step 6: Run the whole suite on every configuration**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; for f in "" "--features capture-fixture" "--features ebpf" "--features integration"; do cargo test --all-targets $f 2>&1 | grep -E 'test result|FAILED|panicked'; done`
Expected: every run passes (the `ebpf` run needs clang and libbpf-dev on the host; if that toolchain is absent, report it and run the other three).

- [ ] **Step 7: Smoke the binary without root**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build -q && ./target/debug/usbtop-ng --support /tmp/claude-support-smoke --no-capture; echo "exit=$?"; ls /tmp/claude-support-smoke; rm -rf /tmp/claude-support-smoke`
Expected: exit 0, the ten-line summary and the guidance on stdout, a directory and a `.tar.gz` in the target. (Use the session's scratchpad directory instead of `/tmp` when one is available.)

- [ ] **Step 8: Gates and commit**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features capture-fixture -- -D warnings && cargo clippy --all-targets --features ebpf -- -D warnings && cargo clippy --all-targets --features integration -- -D warnings && git grep -i -e "$PRIVATE_NAME"`
Expected: clean; the grep prints nothing.

```bash
git add src/main.rs src/diag/mod.rs src/diag/support.rs src/capture/mod.rs src/capture/meta.rs src/fixture_replay.rs src/usbmon/binary.rs src/usbmon/reader.rs src/device/manager.rs src/usbids/mod.rs .github/workflows/ci.yml
git commit -m "feat: --support gathers a redacted diagnostic bundle with an embedded fixture

usbtop-ng --support [PATH] runs every collector, embeds a replayable
fixture (a live aggregate-bus capture as root, a static sysfs bundle
otherwise), replays it into report.json through the export sink, writes
the manifest and the tar archive, tees the debug log into the bundle,
and prints the summary and the filing guidance. It never loads a
module, prompts, or touches the network, and exits 0 whenever the
bundle was written. The capture core and fixture_replay now live in the
default build; the capture-fixture feature gates only its subcommand.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 8: GitHub template and documentation

**Files:**
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`, `.github/ISSUE_TEMPLATE/config.yml`
- Modify: `README.md`, `docs/CONTRIBUTING.md`, `docs/SCRIPTING.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/TESTING.md`, `CHANGELOG.md`

**Interfaces:**
- Consumes: the CLI and behaviour of Tasks 1 and 7 (`--output`, `--support`, `--no-capture`, the run record, the summary, `GUIDANCE`'s URL).
- Produces: documentation only.

- [ ] **Step 1: The issue form**

Create `.github/ISSUE_TEMPLATE/bug_report.yml`:

```yaml
name: Bug report
description: Something is wrong, or does not work as the documentation says
labels: ["bug"]
body:
  - type: markdown
    attributes:
      value: |
        Run `usbtop-ng --support` first (with `sudo` if you can, so it includes a short usbmon capture).
        It writes a diagnostic bundle, prints a summary, and lists every file it gathered.
        The bundle carries your devices' full details, including serial numbers, and nothing that
        identifies the machine or you; review it before attaching it.
  - type: textarea
    id: what-happened
    attributes:
      label: What happened
      description: What you saw, including any error text, exactly as printed.
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: What you expected
  - type: input
    id: command
    attributes:
      label: The exact command
      placeholder: sudo usbtop-ng --once --json
  - type: textarea
    id: summary
    attributes:
      label: Support summary
      description: Paste the block `usbtop-ng --support` printed, starting at "usbtop-ng support bundle".
      render: text
    validations:
      required: true
  - type: checkboxes
    id: bundle
    attributes:
      label: Support bundle
      options:
        - label: I attached the support bundle (the .tar.gz), or explained above why not
          required: true
  - type: textarea
    id: terminal
    attributes:
      label: Terminal and SSH details
      description: For a display problem, name the terminal program and say whether the session was over SSH.
  - type: textarea
    id: more
    attributes:
      label: Anything else
```

Create `.github/ISSUE_TEMPLATE/config.yml`:

```yaml
blank_issues_enabled: true
```

- [ ] **Step 2: README**

Insert after the "Scriptable output" bullets (before `### The chart pane`):

```markdown
### Reporting a problem

- `usbtop-ng --support` gathers a diagnostic bundle for a bug report: the
  build and host details, the usbmon probe and the backend it would pick,
  the USB lines of the kernel log, every USB device's full self-description
  (serial numbers included, as device identity), your configuration with
  home paths rewritten to `~`, the terminal setup, and, when run with
  `sudo`, a short capture of the aggregate bus packaged as a replayable
  fixture. It writes `usbtop-ng-support-<UTC time>/` plus a `.tar.gz` beside
  it, prints a summary, and says how to file the issue.
- Nothing that identifies the machine or its owner is collected: no
  hostname, machine-id, DMI serial, host MAC address, IP address, or user
  name. `tar tzf` lists every file; you decide what to attach.
- `--window SECONDS` sets the capture length (default 5), `--no-capture`
  skips it, and a `PATH` ending in `.tar.gz` names the archive. See
  [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md#bug-reports).
```

Add to the "Scriptable output" bullets, after the `--json` bullet: ``- `--output PATH` writes the reports to a file instead of stdout, led by a run record that names the version, backend, window, filters, and command.``

Replace the fenced block under "## Command line options" with the exact output of `./target/debug/usbtop-ng --help` after `cargo build` (the three new flags appear in field order: `--output` after `--json`, `--support` and `--no-capture` after `--snapshot-internal`).

In the "### Tests" bullets replace the two test counts with the numbers `cargo test --all-targets 2>&1 | grep 'test result'` and `cargo test --all-targets --features ebpf 2>&1 | grep 'test result'` print now (sum the `passed` figures of the unit binary and the two harnesses for the first; quote the unit-binary figure for the second, as the current text does).

- [ ] **Step 3: CONTRIBUTING**

Replace the "### Bug reports" section with:

```markdown
### Bug reports

Open a bug with the [bug report form](https://github.com/wifi-blackout/usbtop-ng/issues/new?template=bug_report.yml)
after running:

```bash
sudo usbtop-ng --support
```

It writes `usbtop-ng-support-<UTC time>/` and a `.tar.gz` beside it in the
current directory (or in the `PATH` you pass), prints a summary, and lists
every file it gathered. Paste the summary into the form and attach the
archive. Without `sudo` the bundle still holds everything but the capture;
`--no-capture` skips the capture on purpose, `--window SECONDS` sets its
length.

What the bundle holds: build and host details (`build.toml`, `host.toml`),
the usbmon probe and the backend the monitor would select (`usbmon.toml`),
the USB lines of the kernel log (`dmesg-usb.txt`), every device's full
self-description with its raw descriptors (`inventory/`), your preferences
and internal-device snapshot with home paths rewritten (`config/`), the
terminal setup (`terminal.toml`), the embedded fixture (`fixture/`, the same
layout as `tests/fixtures/hosts/`), a replayed report (`report.json`), the
run's debug log, and a `manifest.toml` listing each file with its size, the
redaction counts, and everything that was unavailable.

What it never holds: the hostname, machine-id, DMI serial or UUID, any host
MAC address or IP address, or a user name. Device serial numbers and
Thunderbolt `unique_id` values are kept, because a cloned or re-badged
device is often only distinguishable by them. The `inventory/` files are for
the maintainer reading the issue and are never committed; the `fixture/`
directory carries no serial and is what becomes a regression fixture.
```

In the code-organization tree, add after the `usbmon/` block:

```
├── capture/          # Fixture capture and assembly (shared by --capture-fixture and --support)
├── diag/             # --support: redaction rules, collectors, device inventory, bundle writer
│   ├── redact.rs     # Home paths to ~, MAC and UUID masking, the environment allowlist
│   ├── collect.rs    # Build, host, usbmon, backend probe, dmesg, config, terminal
│   ├── inventory.rs  # USB devices, interfaces, endpoints, hub ports, descriptors, Type-C, Thunderbolt
│   ├── bundle.rs     # Bundle directory, manifest, UTC stamp, tar archive
│   └── support.rs    # The orchestrator, the summary, the log tee
├── headless/         # --once and --batch reports
│   ├── mod.rs        # Report model, text renderer, the sampling loop
│   └── export.rs     # --output file sink and the run record
```

In "### Hermetic feature tests (the `capture-fixture` feature)", replace the first paragraph with: "`capture-fixture` is a third opt-in feature build in the gate matrix, alongside `integration` and `ebpf` above. The capture core itself (`src/capture/`, `src/fixture_replay.rs`) is part of the default build, since `--support` embeds a fixture bundle; the feature gates only the `--capture-fixture` subcommand that records a hardware fixture into `tests/fixtures/hosts/` (see [TESTING.md](TESTING.md#capturing-hardware-fixtures) for the capture procedure). Needs no extra toolchain: the feature builds with just the MSRV Rust toolchain." Update the two test counts in that section from `cargo test --features capture-fixture 2>&1 | grep 'test result'` and the default run.

In "### Live system tests (the `integration` feature)", change "adds 4 tests" to "adds 5 tests" and append to the list sentence: ", and one that runs `--support`'s orchestrator live as root and checks that the embedded fixture's goldens replay with zero kernel drops on an idle bus"; change "each of the four extra tests" to "each of the five extra tests" and update the passed count from `cargo test --features integration 2>&1 | grep 'test result'`.

In "### Where new work goes" add: `5. **Diagnostics**: \`src/diag/\` for anything \`--support\` gathers. A new collector takes its filesystem roots as parameters, returns typed data plus notes, and never fails the bundle; add the file to the tree in CONTRIBUTING and to the manifest test in \`src/diag/support.rs\`.`

- [ ] **Step 4: SCRIPTING**

In "## Window length", change the error text to `error: --json, --window, and --output need --once or --batch` and the sentence before it to "`--window`, `--json`, and `--output` all require `--once` or `--batch` (`--window` is also accepted by `--support`, where it sets the capture length).".

Add before "## Exit behavior":

````markdown
## `--output PATH`: write to a file

`--output PATH` sends every report to `PATH` instead of stdout, in the active
format (text, or NDJSON with `--json`). The file is created or truncated when
the run starts; there is no append and no rotation (redirect stdout if you
want either). One line on stderr at exit says how many reports were written
and where. A write error on the file is fatal with a non-zero exit.

```bash
sudo usbtop-ng --batch --json --window 1 --output run.ndjson
```

A file export starts with a run record so the file describes the run it
came from. In JSON it is the first line:

```json
{"record":"run","usbtop_ng":"1.5.0","features":[],"started_unix":1788354946,"window_seconds":1.0,"batch":true,"filters":[],"command":["usbtop-ng","--batch","--json","--window","1","--output","run.ndjson"],"backend":"mmap","kernel":"7.0.0-30-generic","os":"Linux Mint 22.3","arch":"x86_64","buses":[0,1,2,3,4]}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `record` | string | always `"run"`; report lines never carry this key |
| `usbtop_ng` | string | the version that wrote the file |
| `features` | array | cargo features compiled in, sorted (`capture-fixture`, `ebpf`, `integration`) |
| `started_unix` | u64 | Unix time the run started, seconds |
| `window_seconds` | f64 | the requested window |
| `batch` | bool | `true` for `--batch`, `false` for `--once` |
| `filters` | array | the `--filter` terms as given |
| `command` | array | the command line as run |
| `backend` | string | the source selected at start: `ebpf`, `mmap`, `binary`, `text`, or `none`; each report's own `source` stays authoritative |
| `kernel` | string | kernel release |
| `os` | string | the OS pretty name |
| `arch` | string | target architecture |
| `buses` | array | the usbmon buses available at start |

The report lines that follow are unchanged, schema version 1. A consumer
that only wants reports skips the record by key:

```bash
jq -c 'select(.record != "run") | .total_rx_bps' run.ndjson
```

In text mode the same fields lead the file as a `# key: value` block, one
per line, before the first report. Stdout never carries the run record, so
`--batch --json | jq` scripts need no change.
````

- [ ] **Step 5: ARCHITECTURE, ROADMAP, TESTING, CHANGELOG**

ARCHITECTURE, after "#### 6. Configuration (`config/`)", add:

```markdown
#### 7. Diagnostics (`diag/`, `capture/`, `headless/export.rs`)

- `diag/redact.rs`: the privacy rules as pure functions. Home paths become
  `~`, stand-alone MAC addresses in kernel log lines and filesystem UUIDs in
  the kernel command line are masked, only five environment variables are
  ever recorded by value, and every substitution is counted for the
  manifest. Device identity (serial strings, descriptors, Thunderbolt
  `unique_id`) is deliberately not redacted.
- `diag/collect.rs` and `diag/inventory.rs`: the collectors. Each reads
  through filesystem roots its caller passes in, so the whole run is
  testable against a fake tree, and each returns typed data plus
  "unavailable" notes rather than errors. The inventory reads every USB
  device's sysfs self-description, its interfaces, endpoints, and hub ports,
  the raw `descriptors`/`bos_descriptors` blobs to their real length, and
  the Thunderbolt and Type-C attribute trees; the backend probe answers
  which usbmon source `start_monitoring` would select with the same probes
  it uses.
- `diag/bundle.rs`: the bundle directory, the manifest (format version, UTC
  time, file list with sizes, redaction counts, notes), and the `tar`
  archive.
- `diag/support.rs`: the `--support` orchestrator. It embeds a fixture from
  `capture/` (a live capture as root, a static sysfs bundle otherwise),
  re-asserts SEC-1 and SEC-2 over it, replays it into `report.json` through
  the export sink, and prints the summary and filing guidance. The logger is
  built with a tee so the run's own log lands in the bundle.
- `capture/` and `fixture_replay.rs` are part of the default build: the
  capturer's assembly and guards are what `--support` embeds, and the replay
  path is shared with the corpus tests so a golden equals a replay by
  construction. Only the `--capture-fixture` subcommand stays behind the
  `capture-fixture` feature.
- `headless/export.rs`: the `ReportSink` behind `--output` and the support
  bundle's `report.json`, and the run record that leads every file export.
```

ROADMAP: delete the "Document file export." bullet (the item shipped as `--output`; the decision against append and rotation is recorded in SCRIPTING).

TESTING, at the end of "### Capturing hardware fixtures", add: "`--support` embeds the same bundle layout under `fixture/` (with a live capture when run as root, without traces otherwise), so a bug reporter's bundle can be promoted to a corpus fixture by copying its `fixture/` directory into `tests/fixtures/hosts/<board>-<date>/stage<N>/` and re-running `cargo test fixture_corpus`."

CHANGELOG, under `## [Unreleased]`:

- `### Added`: 
  - "`--support [PATH]` gathers a diagnostic bundle for a bug report: build, host, and usbmon details, the backend the monitor would select, the USB lines of the kernel log, every USB device's full self-description with its raw descriptors and the Thunderbolt and Type-C attribute trees, the configuration with home paths rewritten, the terminal setup, a replayable fixture (with a short usbmon capture as root), a replayed `report.json`, the run's log, and a manifest. Host identity (hostname, machine-id, DMI serial, host MACs, IPs, user names) is never collected; device identity is kept. It prints a summary and the filing steps. See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md#bug-reports)."
  - "`--output PATH` for `--once` and `--batch` writes the reports to a file, led by a run record (version, features, start time, window, filters, command, backend, kernel, OS, arch, buses). See [docs/SCRIPTING.md](docs/SCRIPTING.md#--output-path-write-to-a-file)."
  - "A GitHub bug-report form that asks for the `--support` summary and bundle."
- `### Changed`:
  - "The fixture capture and replay code is compiled into the default build (it is what `--support` embeds); the `capture-fixture` feature now gates only the `--capture-fixture` subcommand. No change to the shipped binary's behaviour."
  - "Fixture bundles no longer copy a device's `serial` attribute; the committed corpus was rewritten without them."

- [ ] **Step 6: Verify the docs build nothing and the links resolve**

Run: `git grep -n "template=bug_report.yml" README.md docs/CONTRIBUTING.md src/diag/support.rs | wc -l` (expect 3 or more) and `grep -n "^## \`--output PATH\`" docs/SCRIPTING.md` (expect one hit, matching the CHANGELOG anchor `--output-path-write-to-a-file`).

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test --all-targets 2>&1 | grep -E 'test result|FAILED' && git grep -i -e "$PRIVATE_NAME"`
Expected: green; the grep prints nothing.

- [ ] **Step 7: Commit**

```bash
git add .github/ISSUE_TEMPLATE/bug_report.yml .github/ISSUE_TEMPLATE/config.yml README.md docs/CONTRIBUTING.md docs/SCRIPTING.md docs/ARCHITECTURE.md docs/ROADMAP.md docs/TESTING.md CHANGELOG.md
git commit -m "docs: --support, --output, and the bug-report form

Adds the GitHub issue form the contributing guide referred to, a
README section on reporting a problem, the rewritten bug-report
guidance in CONTRIBUTING (replacing the RUST_LOG/lsusb/lsmod asks),
the --output section with the run record in SCRIPTING, the diagnostic
core in ARCHITECTURE, and the CHANGELOG entries; drops the roadmap's
file-export item now that it shipped.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011Q8hG1q7GtEWzYuSRDyb1t"
```

---

### Task 9: Live verification (operator task)

This task runs the shipped binary on real hosts and records the evidence.
No code is expected to change; if a check fails, fix it in a follow-up
commit on the branch and re-run the failing check. Host account names come
from the controller's dispatch, never from a tracked file (the docs write
them as `<user>@host`).

**Files:**
- Create: `<sdd workspace>/task-9-report.md` (untracked; the workspace is git-ignored)

- [ ] **Step 1: Build**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo build --release 2>&1 | tail -2`
Expected: a clean release build.

- [ ] **Step 2: Root run on the development host**

Run (the scratch directory stands in for `/tmp`; `sudo -n` works on this host):

```bash
S=$(mktemp -d)
sudo -n ./target/release/usbtop-ng --support "$S" --window 5; echo "exit=$?"
```

Expected: exit 0; the summary's `capture:` line names a 5.0 s aggregate capture with `sources binary+text` and `kernel drops 0` on an idle bus; `backend:` names the mmap ring with its size; `devices:` counts the host's devices; the guidance follows.

Then, with `D=$(ls -d "$S"/usbtop-ng-support-*/)` and `A=$(ls "$S"/*.tar.gz)`:

1. `tar tzf "$A" | grep -v '/$' | sed 's#^[^/]*/##' | sort > "$S/listed.txt"; python3 - "$D" "$S/listed.txt" <<'EOF'` … a short script that parses `manifest.toml` (`tomllib`), adds `manifest.toml`, sorts the paths, and asserts equality with `listed.txt`, printing `manifest == archive` on success. Expected: `manifest == archive`.
2. Privacy grep over everything but the device inventory: `grep -rInE "$(hostname)|/home/|$(cat /etc/machine-id)|([0-9a-f]{2}:){5}[0-9a-f]{2}|\b([0-9]{1,3}\.){3}[0-9]{1,3}\b" "$D" --exclude-dir=inventory --exclude-dir=descriptors; echo "grep exit=$?"`. Expected: `grep exit=1` (no match). Any hit is reviewed: a device string that happens to match (a firmware version shaped like an IP) is recorded in the report, anything else is a defect to fix.
3. `grep -c 'serial = ' "$D/inventory/usb.toml"` prints at least 1 and `find "$D/fixture" -name serial | wc -l` prints 0.
4. `ls "$D/inventory/descriptors" | head` shows one `.bin` per device and `.bos.bin` files for the USB 3 devices; `stat -c %s "$D/inventory/descriptors/1-4.bin"` (or any device) is far below 65553.
5. Promote the fixture and replay it: `mkdir -p tests/fixtures/hosts/verify-$(date +%Y%m%d)/stage9 && cp -r "$D/fixture/." tests/fixtures/hosts/verify-$(date +%Y%m%d)/stage9/ && export PATH="$HOME/.cargo/bin:$PATH"; cargo test fixture_corpus 2>&1 | grep -E 'test result|FAILED'; rm -rf tests/fixtures/hosts/verify-*`. Expected: green, and the temporary corpus directory is removed (confirm with `git status --short`, which must be clean).
6. `head -c 300 "$D/report.json"` shows the run record with `"backend":"mmap"`; the second line's `window_seconds` is about 5.
7. `cat "$D/usbtop-ng.log" | head -5` shows the run's info lines, none containing `/home/`.
8. `ls -l "$S"` shows the bundle directory and archive owned by the invoking user (the sudo hand-over), not root.
9. The live integration test, run as root the way the other root-only tests are (the user's toolchain, root's privileges): `export PATH="$HOME/.cargo/bin:$PATH"; sudo -n env "PATH=$PATH" "HOME=$HOME" cargo test --features integration live_support 2>&1 | grep -E 'test result|skipping'`. Expected: `1 passed` and no `skipping` line. Run it with no device streaming (the assertion is zero kernel drops on an idle bus).

- [ ] **Step 3: Non-root run over ssh on the Kali laptop**

The host has no passwordless sudo and usbmon unloaded, which is the fleet's static-bundle case. Copy the release binary and run it (the account name comes from the dispatch):

```bash
scp target/release/usbtop-ng <user>@alamo-kali:/tmp/usbtop-ng
ssh <user>@alamo-kali '/tmp/usbtop-ng --support /tmp/usbtop-ng-verify; echo "exit=$?"; D=$(ls -d /tmp/usbtop-ng-verify/usbtop-ng-support-*/); grep -n "sources" "$D/fixture/meta.toml"; grep -c "^\[\[typec\]\]" "$D/inventory/typec.toml"; grep -c "^\[\[power_delivery\]\]" "$D/inventory/typec.toml"; grep -n "capture" "$D/manifest.toml"; grep -n "ssh_present\|sync_mode" "$D/terminal.toml"; rm -rf /tmp/usbtop-ng-verify /tmp/usbtop-ng'
```

Expected: `exit=0`; `sources = []`; at least one `[[typec]]` and one `[[power_delivery]]` entry (the laptop's Type-C port with PD); a `capture` note saying the run was not root; `ssh_present = true` and a `not probed` sync mode. The summary's `capture:` line says how to include a capture.

- [ ] **Step 4: `--output` on the development host**

```bash
S=$(mktemp -d)
timeout -s INT 2.6 sudo -n ./target/release/usbtop-ng --batch --json --window 1 --output "$S/run.ndjson"; echo "exit=$?"
python3 -c "import json,sys; lines=open('$S/run.ndjson').read().splitlines(); docs=[json.loads(l) for l in lines]; assert docs[0]['record']=='run' and docs[0]['batch'] is True; assert len(docs)>=3 and all('record' not in d and d['version']==1 for d in docs[1:]); print('ok', len(docs)-1, 'reports, backend', docs[0]['backend'])"
sudo -n ./target/release/usbtop-ng --once --window 1 --output "$S/run.txt"; head -3 "$S/run.txt"
```

Expected: the batch run exits 0 after SIGINT with a stderr line `wrote N report(s) to …` (N is 2 or 3), the checker prints `ok`, and the text file starts with `# usbtop_ng: ` followed by the run-record block and then a `ts=` report line.

- [ ] **Step 5: Write the report**

Write `<sdd workspace>/task-9-report.md` with each check above, its command, the observed output (trimmed), and PASS/FAIL, plus any privacy-grep hits with their disposition. No commit unless a check failed and a fix was made; then commit the fix with a `fix(diag): …` message carrying the standard trailers.

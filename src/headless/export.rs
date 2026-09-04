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
        let mut file = File::create(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("could not create {}: {e}", path.display()),
            )
        })?;
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
        let err = match ReportSink::open(Some(&path), &run(), true) {
            Ok(_) => panic!("a missing parent directory must fail ReportSink::open"),
            Err(e) => e,
        };
        let message = err.to_string();
        assert!(message.starts_with("could not create "), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
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

//! The `--output PATH` file sink for `--once` and `--batch`, and the run
//! record that leads every file export so the file describes the run it
//! came from (version, host, backend, window, filters, command). Stdout
//! keeps today's byte-exact behaviour: no record, no notice. The support
//! bundle writes its `report.json` through the same sink.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::OpenOptionsExt;
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

/// `/dev/stdout`, `/dev/stderr`, and `/dev/fd/N` are symlinks into
/// `/proc/self/fd`, which the `O_NOFOLLOW` open below would refuse. They name
/// descriptors this process already holds, so the sink duplicates the
/// descriptor instead of opening a path at all; nothing on disk is created
/// or truncated either way. Only these exact spellings qualify.
fn inherited_descriptor(path: &Path) -> Option<i32> {
    let text = path.to_str()?;
    match text {
        "/dev/stdout" => Some(1),
        "/dev/stderr" => Some(2),
        _ => text
            .strip_prefix("/dev/fd/")?
            .parse::<i32>()
            .ok()
            .filter(|fd| *fd >= 0),
    }
}

impl ReportSink {
    /// `None` is the stdout sink. `Some(path)` creates or truncates the
    /// file and writes the run record (a JSON line, or the text comment
    /// block) before returning; an unwritable path is an error here.
    pub fn open(output: Option<&Path>, run: &RunRecord, json: bool) -> io::Result<ReportSink> {
        let Some(path) = output else {
            return Ok(ReportSink::Stdout);
        };
        // `O_NOFOLLOW`: the monitor usually runs as root, and `--output`
        // names a path the invoker may share with other local users. A
        // symlink planted there must not turn "create or truncate the
        // report" into "truncate whatever the link points at". Only the
        // final component is affected; a symlinked parent directory still
        // resolves. (`fs.protected_symlinks` already blocks the sticky-dir
        // case on most kernels; this closes the rest.)
        let file = if let Some(fd) = inherited_descriptor(path) {
            // SAFETY: matches fcntl.h, `int fcntl(int fd, int cmd, ...)`;
            // `F_DUPFD_CLOEXEC` with a lowest-fd argument of 0 returns a
            // fresh duplicate with close-on-exec set (plain `dup(2)` would
            // clear it, and no child of this process should inherit the
            // report file), or -1 with errno set. The new descriptor is
            // owned by nobody else, so `from_raw_fd` may take it over.
            let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            if duplicate < 0 {
                let e = io::Error::last_os_error();
                return Err(io::Error::new(
                    e.kind(),
                    format!("could not create {}: {e}", path.display()),
                ));
            }
            unsafe { File::from_raw_fd(duplicate) }
        } else {
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .map_err(|e| {
                    // ELOOP also means "too many links in the ancestors";
                    // blame the leaf only when the leaf really is a link.
                    let leaf_is_link = e.raw_os_error() == Some(libc::ELOOP)
                        && fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
                    let why = if leaf_is_link {
                        "it is a symbolic link, and --output never follows one".to_string()
                    } else {
                        e.to_string()
                    };
                    io::Error::new(
                        e.kind(),
                        format!("could not create {}: {why}", path.display()),
                    )
                })?
        };
        Self::from_open_file(file, path.to_path_buf(), run, json)
    }

    /// Build a file sink from an already-open file (created by the caller,
    /// e.g. beneath the support bundle's pinned root fd so the create never
    /// re-resolves a path). Writes the leading run record exactly as
    /// [`ReportSink::open`] does, then returns the file sink. `path` is kept
    /// only for the exit notice.
    pub fn from_open_file(
        mut file: File,
        path: PathBuf,
        run: &RunRecord,
        json: bool,
    ) -> io::Result<ReportSink> {
        if json {
            let line = serde_json::to_string(run).expect("run record serializes");
            writeln!(file, "{line}")?;
        } else {
            file.write_all(render_run_record_text(run).as_bytes())?;
        }
        file.flush()?;
        Ok(ReportSink::File {
            path,
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
    fn a_symlink_at_the_output_path_is_refused_and_its_target_untouched() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("precious");
        std::fs::write(&target, "keep me").unwrap();
        let link = temp.path().join("run.ndjson");
        symlink(&target, &link).unwrap();

        let err = match ReportSink::open(Some(&link), &run(), true) {
            Ok(_) => panic!("a symlink at --output must never be followed"),
            Err(e) => e,
        };
        let message = err.to_string();
        assert!(message.starts_with("could not create "), "{message}");
        assert!(message.contains("symbolic link"), "{message}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "keep me",
            "the link target must not be truncated"
        );
    }

    #[test]
    fn a_symlinked_parent_directory_still_resolves() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        symlink(&real, &link).unwrap();

        // Only the final component is protected: a home on a symlinked
        // mount (`/home -> /var/home`) must keep working.
        ReportSink::open(Some(&link.join("run.ndjson")), &run(), true).unwrap();
        assert!(real.join("run.ndjson").is_file());
    }

    #[test]
    fn an_eloop_from_an_ancestor_is_not_blamed_on_the_leaf() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        symlink(&b, &a).unwrap();
        symlink(&a, &b).unwrap();

        let err = match ReportSink::open(Some(&a.join("run.ndjson")), &run(), true) {
            Ok(_) => panic!("a symlink loop in the ancestors cannot open"),
            Err(e) => e,
        };
        let message = err.to_string();
        assert!(message.starts_with("could not create "), "{message}");
        assert!(
            !message.contains("symbolic link, and --output"),
            "a loop in the ancestors is not 'PATH is a symlink': {message}"
        );
    }

    #[test]
    fn dev_fd_paths_duplicate_the_descriptor_instead_of_opening_a_path() {
        use std::os::fd::AsRawFd;

        let temp = tempfile::tempdir().unwrap();
        let backing = temp.path().join("backing.ndjson");
        let file = std::fs::File::create(&backing).unwrap();
        let via_fd = format!("/dev/fd/{}", file.as_raw_fd());

        // /dev/fd/N (and /dev/stdout, /dev/stderr) are symlinks into
        // /proc/self/fd that O_NOFOLLOW would refuse; they name descriptors
        // this process already holds, so the sink duplicates the descriptor.
        let sink = ReportSink::open(Some(Path::new(&via_fd)), &run(), true).unwrap();
        drop(sink);
        let written = std::fs::read_to_string(&backing).unwrap();
        assert!(written.contains("\"record\":\"run\""), "{written}");
    }

    #[test]
    fn inherited_descriptor_recognizes_only_the_standard_forms() {
        assert_eq!(inherited_descriptor(Path::new("/dev/stdout")), Some(1));
        assert_eq!(inherited_descriptor(Path::new("/dev/stderr")), Some(2));
        assert_eq!(inherited_descriptor(Path::new("/dev/fd/7")), Some(7));
        assert_eq!(inherited_descriptor(Path::new("/dev/fd/-1")), None);
        assert_eq!(inherited_descriptor(Path::new("/dev/fd/x")), None);
        assert_eq!(inherited_descriptor(Path::new("/dev/stdin")), None);
        assert_eq!(inherited_descriptor(Path::new("/tmp/dev/stdout")), None);
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

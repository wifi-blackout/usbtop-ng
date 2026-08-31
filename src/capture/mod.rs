//! The `--capture-fixture` subcommand: capture one ladder stage into a
//! committed, hermetic fixture bundle. Feature-gated developer/CI tooling.

pub mod meta;
pub mod sanitize;
pub mod sysfs;
pub mod trace;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};

use crate::fixture_replay::{replay_fixture, report_to_golden_json, FixtureSource};
use crate::snapshot::Snapshot;

/// One sanitized trace ready to be written into a bundle.
pub struct CapturedTrace {
    pub source: FixtureSource,
    pub bytes: Vec<u8>,
}

/// Where a bundle's `internal-devices.toml` comes from: a fresh bare-board
/// snapshot of a sysfs tree, or a copy of a baseline the tester captured at the
/// bare-board stage and passes to every later stage.
pub enum BaselineSource {
    CaptureFrom(PathBuf),
    CopyFile(PathBuf),
}

/// Assemble a committed fixture bundle from a source sysfs tree and
/// already-sanitized traces: materialize sysfs, resolve the baseline, write the
/// traces (asserting SEC-1), generate each golden by replaying the bundle, and
/// write meta.toml. Pure of any live device, so it is fully unit-tested.
pub fn assemble_bundle(
    src_sysfs: &Path,
    outdir: &Path,
    traces: &[CapturedTrace],
    baseline: &BaselineSource,
    stage_id: Option<u32>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(outdir).with_context(|| format!("create {}", outdir.display()))?;

    sysfs::materialize_sysfs(src_sysfs, &outdir.join("sysfs"))?;
    assert_sysfs_contained(&outdir.join("sysfs"))?; // SEC-2, capturer side

    // Baseline internal-devices snapshot (bare-board; reused across stages).
    let internal = outdir.join("internal-devices.toml");
    match baseline {
        BaselineSource::CaptureFrom(base) => {
            Snapshot::capture(Some(base))
                .with_context(|| format!("snapshot {}", base.display()))?
                .write_to(&internal)?;
        }
        BaselineSource::CopyFile(path) => {
            std::fs::copy(path, &internal)
                .with_context(|| format!("copy baseline {}", path.display()))?;
        }
    }

    // Write each sanitized trace, asserting SEC-1 first.
    let mut sources = Vec::new();
    for trace in traces {
        assert_payload_free(trace)?;
        std::fs::write(outdir.join(trace.source.trace_filename()), &trace.bytes)
            .with_context(|| format!("write {}", trace.source.trace_filename()))?;
        sources.push(trace.source);
    }

    // Generate each golden by replaying the just-written bundle.
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

    let report = report_for_meta.ok_or_else(|| anyhow!("no sources captured"))?;
    std::fs::write(
        outdir.join("meta.toml"),
        meta::build_meta(&report, &sources, stage_id)?,
    )
    .context("write meta.toml")?;
    Ok(())
}

/// SEC-1 capturer-side guard: a binary trace must be a whole number of 48-byte
/// headers (no payload); a text trace must carry no `=` data tag.
fn assert_payload_free(trace: &CapturedTrace) -> anyhow::Result<()> {
    match trace.source {
        FixtureSource::Binary => {
            if !trace.bytes.len().is_multiple_of(48) {
                return Err(anyhow!(
                    "SEC-1: binary trace is {} bytes, not a multiple of 48 (payload leaked?)",
                    trace.bytes.len()
                ));
            }
        }
        FixtureSource::Text => {
            let text = std::str::from_utf8(&trace.bytes)
                .map_err(|_| anyhow!("SEC-1: text trace is not UTF-8"))?;
            if text.split_whitespace().any(|t| t == "=") {
                return Err(anyhow!(
                    "SEC-1: text trace carries a `=` data tag (payload leaked?)"
                ));
            }
        }
    }
    Ok(())
}

/// SEC-2 capturer-side guard: every path under `sysfs/`, canonicalized, stays
/// inside it.
fn assert_sysfs_contained(sysfs: &Path) -> anyhow::Result<()> {
    let root = std::fs::canonicalize(sysfs)
        .with_context(|| format!("canonicalize {}", sysfs.display()))?;
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let real = std::fs::canonicalize(entry.path())
                .with_context(|| format!("canonicalize {}", entry.path().display()))?;
            if !real.starts_with(&root) {
                return Err(anyhow!(
                    "SEC-2: {} escapes the bundle",
                    entry.path().display()
                ));
            }
            if real.is_dir() {
                stack.push(real);
            }
        }
    }
    Ok(())
}

/// `--capture-fixture` options (from the CLI).
pub struct CaptureFixtureOpts {
    pub outdir: PathBuf,
    pub window: Duration,
    /// usbmon interface to read: `None` = the aggregate (bus 0), which carries
    /// every bus's events in one stream.
    pub bus: Option<u8>,
    /// Bare-board baseline to copy in; `None` captures a fresh one (bare-board
    /// stage only).
    pub baseline: Option<PathBuf>,
}

/// Live entry point: open the binary and text usbmon interfaces, capture a
/// window of raw events, sanitize them, and assemble the bundle. Needs root.
pub fn run_capture_fixture(opts: CaptureFixtureOpts) -> anyhow::Result<()> {
    let bus = opts.bus.unwrap_or(0);
    let stop = AtomicBool::new(false);

    let src_sysfs = Path::new("/sys/bus/usb/devices");
    let mut traces = Vec::new();

    // Binary interface, sanitized.
    let bin_dev = PathBuf::from(format!("/dev/usbmon{bus}"));
    if let Ok(bytes) = capture_window(&bin_dev, opts.window, &stop) {
        let sanitized = trace::sanitize_binary_stream(&mut std::io::Cursor::new(bytes))?;
        traces.push(CapturedTrace {
            source: FixtureSource::Binary,
            bytes: sanitized,
        });
    }
    // Text interface, sanitized.
    let text_dev = PathBuf::from(format!("/sys/kernel/debug/usb/usbmon/{bus}u"));
    if let Ok(bytes) = capture_window(&text_dev, opts.window, &stop) {
        let sanitized =
            trace::sanitize_text_stream(&mut std::io::BufReader::new(std::io::Cursor::new(bytes)))?;
        traces.push(CapturedTrace {
            source: FixtureSource::Text,
            bytes: sanitized.into_bytes(),
        });
    }
    if traces.is_empty() {
        return Err(anyhow!(
            "no usbmon interface for bus {bus} could be opened (need root and a loaded usbmon module)"
        ));
    }

    let baseline = match &opts.baseline {
        Some(path) => BaselineSource::CopyFile(path.clone()),
        None => BaselineSource::CaptureFrom(src_sysfs.to_path_buf()),
    };
    let stage_id = stage_id_from_outdir(&opts.outdir);
    assemble_bundle(src_sysfs, &opts.outdir, &traces, &baseline, stage_id)?;
    eprintln!("captured fixture bundle at {}", opts.outdir.display());
    Ok(())
}

/// Read raw bytes from a usbmon interface for `window`, polling a non-blocking
/// open (idle buses return `WouldBlock`). The raw buffer is framed and
/// sanitized afterward, so no framing happens here. Thin live glue.
fn capture_window(path: &Path, window: Duration, stop: &AtomicBool) -> std::io::Result<Vec<u8>> {
    let mut file = crate::usbmon::open_nonblocking(path)?;
    let deadline = Instant::now() + window;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        match file.read(&mut chunk) {
            Ok(0) => std::thread::sleep(Duration::from_millis(50)),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

/// Parse a trailing `stageN` component of the output dir into a stage id, for
/// meta.toml. Best-effort documentation only.
fn stage_id_from_outdir(outdir: &Path) -> Option<u32> {
    outdir
        .file_name()?
        .to_str()?
        .strip_prefix("stage")?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_replay::{replay_fixture, to_masked_value};

    fn write(dir: &Path, name: &str, value: &str) {
        std::fs::write(dir.join(name), value).unwrap();
    }

    fn build_src_sysfs(root: &Path) {
        // usb1 symlinked into a controller dir (like real sysfs), one device.
        let ctrl = root.join("real/0000:00:14.0/usb1");
        std::fs::create_dir_all(&ctrl).unwrap();
        write(&ctrl, "busnum", "1\n");
        write(&ctrl, "devnum", "1\n");
        write(&ctrl, "speed", "480\n");
        std::fs::create_dir_all(root.join("devices")).unwrap();
        std::os::unix::fs::symlink(
            root.join("real/0000:00:14.0/usb1"),
            root.join("devices/usb1"),
        )
        .unwrap();
        let dev = root.join("devices/1-1");
        std::fs::create_dir_all(&dev).unwrap();
        write(&dev, "busnum", "1\n");
        write(&dev, "devnum", "3\n");
        write(&dev, "speed", "480\n");
    }

    fn one_binary_event() -> Vec<u8> {
        let mut b = vec![0u8; 48];
        b[8] = b'C';
        b[9] = 3;
        b[10] = 0x81;
        b[11] = 3;
        b[12..14].copy_from_slice(&1u16.to_ne_bytes());
        b[32..36].copy_from_slice(&1000u32.to_ne_bytes());
        b // len_cap already 0 (sanitized)
    }

    #[test]
    fn assemble_bundle_writes_a_replayable_bundle_whose_golden_equals_replay() {
        let temp = tempfile::tempdir().unwrap();
        build_src_sysfs(temp.path());
        let outdir = temp.path().join("bundle");
        std::fs::create_dir_all(&outdir).unwrap();

        let traces = vec![
            CapturedTrace {
                source: FixtureSource::Binary,
                bytes: one_binary_event(),
            },
            CapturedTrace {
                source: FixtureSource::Text,
                bytes: b"ffff0000aaaa0001 200 C Bi:1:003:1 0 1000 <\n".to_vec(),
            },
        ];
        assemble_bundle(
            &temp.path().join("devices"),
            &outdir,
            &traces,
            &BaselineSource::CaptureFrom(temp.path().join("devices")),
            Some(3),
        )
        .unwrap();

        // Every expected file exists.
        for f in [
            "sysfs",
            "trace.bin",
            "trace.txt",
            "golden.binary.json",
            "golden.text.json",
            "internal-devices.toml",
            "meta.toml",
        ] {
            assert!(outdir.join(f).exists(), "missing {f}");
        }

        // SEC-1: binary is a whole number of 48-byte headers; text has no `=`.
        assert_eq!(
            std::fs::metadata(outdir.join("trace.bin")).unwrap().len() % 48,
            0
        );
        assert!(!std::fs::read_to_string(outdir.join("trace.txt"))
            .unwrap()
            .contains('='));

        // The committed golden equals a direct replay (by construction).
        for source in [FixtureSource::Binary, FixtureSource::Text] {
            let report = replay_fixture(&outdir, source).unwrap();
            let got = to_masked_value(&serde_json::to_string(&report).unwrap()).unwrap();
            let golden = to_masked_value(
                &std::fs::read_to_string(outdir.join(source.golden_filename())).unwrap(),
            )
            .unwrap();
            assert_eq!(got, golden, "golden must equal replay for {source:?}");
        }
    }

    #[test]
    fn assemble_bundle_rejects_a_binary_trace_carrying_payload() {
        let temp = tempfile::tempdir().unwrap();
        build_src_sysfs(temp.path());
        let outdir = temp.path().join("bundle");
        std::fs::create_dir_all(&outdir).unwrap();
        let mut bad = one_binary_event();
        bad.extend_from_slice(&[0xAB; 4]); // 52 bytes: not a whole header count
        let traces = vec![CapturedTrace {
            source: FixtureSource::Binary,
            bytes: bad,
        }];
        let err = assemble_bundle(
            &temp.path().join("devices"),
            &outdir,
            &traces,
            &BaselineSource::CaptureFrom(temp.path().join("devices")),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("SEC-1"), "{err}");
    }
}

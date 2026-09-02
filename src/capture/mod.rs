//! The `--capture-fixture` subcommand: capture one ladder stage into a
//! committed, hermetic fixture bundle. Feature-gated developer/CI tooling.

pub mod meta;
pub mod sanitize;
pub mod sysfs;
pub mod trace;

use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};

use crate::fixture_replay::{replay_fixture, report_to_golden_json, FixtureSource};
use crate::snapshot::Snapshot;
use crate::usbmon::ring;

/// One sanitized trace ready to be written into a bundle.
pub struct CapturedTrace {
    pub source: FixtureSource,
    pub bytes: Vec<u8>,
    /// Kernel-side events lost during the capture, when the interface could
    /// report them (the binary device; `None` for the text file).
    pub kernel_dropped: Option<u64>,
}

/// Raw bytes read from one usbmon interface, plus the kernel's drop count
/// when the interface has one.
pub struct RawCapture {
    pub bytes: Vec<u8>,
    /// `Some(n)`: `MON_IOCG_STATS` worked and the kernel lost `n` events
    /// during this capture. `None`: no such counter on this interface (the
    /// debugfs text file, or a kernel without the ioctl), or any other
    /// ioctl failure.
    pub kernel_dropped: Option<u64>,
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

    // Refuse a pre-existing, non-empty sysfs/: materializing into it would
    // merge in stale device dirs left over from a prior partial run, silently
    // mixing two captures into one bundle.
    let sysfs_out = outdir.join("sysfs");
    if let Ok(mut entries) = std::fs::read_dir(&sysfs_out) {
        if entries.next().is_some() {
            return Err(anyhow!(
                "{} already exists and is not empty (stale from a prior run?); use a fresh outdir",
                sysfs_out.display()
            ));
        }
    }

    sysfs::materialize_sysfs(src_sysfs, &sysfs_out)?;
    assert_sysfs_contained(&sysfs_out)?; // SEC-2, capturer side

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
    let binary_kernel_dropped = traces
        .iter()
        .find(|t| t.source == FixtureSource::Binary)
        .and_then(|t| t.kernel_dropped);
    std::fs::write(
        outdir.join("meta.toml"),
        meta::build_meta(&report, &sources, stage_id, binary_kernel_dropped)?,
    )
    .context("write meta.toml")?;
    Ok(())
}

/// SEC-1 capturer-side guard: a binary trace must be a whole number of 48-byte
/// headers, each with `len_cap` (bytes 36..40) zero (no payload); a text
/// trace must carry no `=` data tag. A whole-header-count check alone isn't
/// enough: a header (len_cap=48) followed by exactly 48 payload bytes is
/// itself a whole multiple of 48 and would slip past it, so every record's
/// `len_cap` is checked too.
fn assert_payload_free(trace: &CapturedTrace) -> anyhow::Result<()> {
    match trace.source {
        FixtureSource::Binary => {
            if !trace.bytes.len().is_multiple_of(48) {
                return Err(anyhow!(
                    "SEC-1: binary trace is {} bytes, not a multiple of 48 (payload leaked?)",
                    trace.bytes.len()
                ));
            }
            for (i, record) in trace.bytes.chunks_exact(48).enumerate() {
                let len_cap = u32::from_ne_bytes(record[36..40].try_into().unwrap());
                if len_cap != 0 {
                    return Err(anyhow!(
                        "SEC-1: binary trace record {i} carries len_cap={len_cap} (payload leaked?)"
                    ));
                }
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

/// Live entry point: open the binary and text usbmon interfaces, capture one
/// shared window of raw events concurrently, sanitize them, and assemble the
/// bundle. Needs root.
pub fn run_capture_fixture(opts: CaptureFixtureOpts) -> anyhow::Result<()> {
    let bus = opts.bus.unwrap_or(0);
    let stop = AtomicBool::new(false);

    let src_sysfs = Path::new("/sys/bus/usb/devices");
    let mut traces = Vec::new();

    let bin_dev = PathBuf::from(format!("/dev/usbmon{bus}"));
    let text_dev = PathBuf::from(format!("/sys/kernel/debug/usb/usbmon/{bus}u"));

    // One shared deadline: the two usbmon interfaces are captured
    // concurrently over the same window, not one after another (which would
    // double the wall time and let the two sources describe different
    // traffic).
    let deadline = Instant::now() + opts.window;
    let (bin_result, text_result) =
        capture_pair(&bin_dev, &text_dev, deadline, &stop, capture_until);

    // Binary interface, sanitized.
    match bin_result {
        Ok(raw) => {
            if let Some(n) = raw.kernel_dropped.filter(|&n| n > 0) {
                eprintln!(
                    "warning: the kernel dropped {n} events from {} during the capture; \
                     the binary golden still pins the pipeline but understates the traffic, \
                     so lower the rate or widen the window before citing it for accuracy",
                    bin_dev.display()
                );
            }
            let sanitized = trace::sanitize_binary_stream(&mut std::io::Cursor::new(raw.bytes))?;
            traces.push(CapturedTrace {
                source: FixtureSource::Binary,
                bytes: sanitized,
                kernel_dropped: raw.kernel_dropped,
            });
        }
        Err(e) => eprintln!(
            "warning: could not capture {} (binary usbmon interface): {e}",
            bin_dev.display()
        ),
    }
    // Text interface, sanitized.
    match text_result {
        Ok(raw) => {
            let sanitized = trace::sanitize_text_stream(&mut std::io::BufReader::new(
                std::io::Cursor::new(raw.bytes),
            ))?;
            traces.push(CapturedTrace {
                source: FixtureSource::Text,
                bytes: sanitized.into_bytes(),
                kernel_dropped: raw.kernel_dropped,
            });
        }
        Err(e) => eprintln!(
            "warning: could not capture {} (text usbmon interface): {e}",
            text_dev.display()
        ),
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

/// Capture the binary and text usbmon interfaces concurrently, both against
/// the same `deadline`, so they describe one shared traffic window rather
/// than two disjoint ones. `read` is the per-interface reader (`capture_until`
/// in production); generic and injectable so the pairing logic can be tested
/// without touching a real usbmon device or the wall clock.
fn capture_pair<T, F>(
    bin_dev: &Path,
    text_dev: &Path,
    deadline: Instant,
    stop: &AtomicBool,
    read: F,
) -> (std::io::Result<T>, std::io::Result<T>)
where
    T: Send,
    F: Fn(&Path, Instant, &AtomicBool) -> std::io::Result<T> + Sync,
{
    std::thread::scope(|scope| {
        let text_handle = scope.spawn(|| read(text_dev, deadline, stop));
        let bin_result = read(bin_dev, deadline, stop);
        let text_result = text_handle.join().expect("text capture thread panicked");
        (bin_result, text_result)
    })
}

/// Read raw bytes from a usbmon interface until `deadline`, polling a
/// non-blocking open (idle buses return `WouldBlock`). The raw buffer is
/// framed and sanitized afterward, so no framing happens here. Thin live glue.
///
/// The kernel ring is enlarged first (see [`ring::request_ring_ladder`]): on
/// the default ~300 KiB ring the 2026-09-01 spike measured this reader
/// keeping 32% of an isochronous stream's events. The debugfs text file
/// answers `ENOTTY` to both ioctls, which the helper and the final stats
/// read ignore, so one function serves both interfaces. The drop count is
/// read once at the end: the kernel zeroes it on every read, and nothing
/// else reads it during a capture, so that single read is the whole
/// capture's loss.
fn capture_until(path: &Path, deadline: Instant, stop: &AtomicBool) -> std::io::Result<RawCapture> {
    let mut file = crate::usbmon::open_nonblocking(path)?;
    let fd = file.as_raw_fd();
    ring::request_ring_ladder(fd, path);
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
    let kernel_dropped = ring::stats(fd).ok().map(|s| u64::from(s.dropped));
    Ok(RawCapture {
        bytes: buf,
        kernel_dropped,
    })
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
    use std::sync::mpsc;
    use std::sync::Mutex;

    /// `capture_pair` must give both sides the identical `Instant` deadline
    /// and actually run them concurrently, not one after another. A recorder
    /// closure logs each invocation's deadline, and a channel rendezvous
    /// forces each side to wait for a signal from the other before it can
    /// return: if `capture_pair` regressed to running the two reads
    /// sequentially, the second call would never see the first side's signal
    /// (it hasn't started yet) and this test would fail cleanly on the
    /// bounded `recv_timeout` instead of hanging or passing by luck.
    #[test]
    fn capture_pair_shares_one_deadline_and_runs_both_sides_concurrently() {
        let deadline = Instant::now() + Duration::from_secs(3600);
        let stop = AtomicBool::new(false);
        let seen_deadlines: Mutex<Vec<Instant>> = Mutex::new(Vec::new());

        let (bin_started_tx, bin_started_rx) = mpsc::channel::<()>();
        let (text_started_tx, text_started_rx) = mpsc::channel::<()>();
        let bin_started_tx = Mutex::new(bin_started_tx);
        let text_started_tx = Mutex::new(text_started_tx);
        let bin_started_rx = Mutex::new(bin_started_rx);
        let text_started_rx = Mutex::new(text_started_rx);

        let bin_path = Path::new("/dev/fake-bin");
        let text_path = Path::new("/dev/fake-text");

        let read =
            |path: &Path, deadline: Instant, _stop: &AtomicBool| -> std::io::Result<Vec<u8>> {
                seen_deadlines.lock().unwrap().push(deadline);
                if path == bin_path {
                    bin_started_tx.lock().unwrap().send(()).unwrap();
                    text_started_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .expect("text side never started: capture_pair regressed to sequential?");
                } else {
                    text_started_tx.lock().unwrap().send(()).unwrap();
                    bin_started_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .expect("bin side never started: capture_pair regressed to sequential?");
                }
                Ok(Vec::new())
            };

        let (bin_result, text_result) = capture_pair(bin_path, text_path, deadline, &stop, read);

        assert!(bin_result.is_ok());
        assert!(text_result.is_ok());
        let seen = seen_deadlines.lock().unwrap();
        assert_eq!(seen.len(), 2, "both sides must have been invoked");
        assert_eq!(
            seen[0], seen[1],
            "both invocations must receive the identical deadline Instant"
        );
    }

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
                kernel_dropped: None,
            },
            CapturedTrace {
                source: FixtureSource::Text,
                bytes: b"ffff0000aaaa0001 200 C Bi:1:003:1 0 1000 <\n".to_vec(),
                kernel_dropped: None,
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
            kernel_dropped: None,
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

    #[test]
    fn assert_payload_free_rejects_a_binary_trace_whose_len_cap_claims_payload() {
        // 96 bytes: a whole multiple of 48, so the length-only check would
        // pass it. But it's a header claiming len_cap=48 followed by 48
        // payload bytes — SEC-1 must catch this by len_cap, not just length.
        let mut bytes = vec![0u8; 48];
        bytes[36..40].copy_from_slice(&48u32.to_ne_bytes());
        bytes.extend_from_slice(&[0xAB; 48]);
        assert_eq!(bytes.len() % 48, 0, "precondition: a whole header count");

        let trace = CapturedTrace {
            source: FixtureSource::Binary,
            bytes,
            kernel_dropped: None,
        };
        let err = assert_payload_free(&trace).unwrap_err();
        assert!(err.to_string().contains("SEC-1"), "{err}");
    }

    #[test]
    fn assemble_bundle_rejects_a_stale_nonempty_sysfs_dir() {
        let temp = tempfile::tempdir().unwrap();
        build_src_sysfs(temp.path());
        let outdir = temp.path().join("bundle");
        let stale = outdir.join("sysfs").join("leftover-device");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("busnum"), "9\n").unwrap();

        let traces = vec![CapturedTrace {
            source: FixtureSource::Binary,
            bytes: one_binary_event(),
            kernel_dropped: None,
        }];
        let err = assemble_bundle(
            &temp.path().join("devices"),
            &outdir,
            &traces,
            &BaselineSource::CaptureFrom(temp.path().join("devices")),
            None,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sysfs"), "{msg}");
        assert!(msg.contains("fresh outdir"), "{msg}");
        // Nothing else got written: the stale dir was the only thing there.
        assert!(!outdir.join("meta.toml").exists());
    }

    /// The binary trace's kernel drop count lands in meta.toml so a bundle
    /// declares its own completeness.
    #[test]
    fn assemble_bundle_records_binary_kernel_drops_in_meta() {
        let temp = tempfile::tempdir().unwrap();
        build_src_sysfs(temp.path());
        let outdir = temp.path().join("bundle");
        let traces = vec![CapturedTrace {
            source: FixtureSource::Binary,
            bytes: one_binary_event(),
            kernel_dropped: Some(7),
        }];
        assemble_bundle(
            &temp.path().join("devices"),
            &outdir,
            &traces,
            &BaselineSource::CaptureFrom(temp.path().join("devices")),
            Some(2),
        )
        .unwrap();
        let meta = std::fs::read_to_string(outdir.join("meta.toml")).unwrap();
        assert!(meta.contains("binary_kernel_dropped = 7"), "{meta}");
    }

    /// `capture_until` on a regular file: the ring ladder and the stats
    /// ioctl both answer ENOTTY, so the bytes are read and no drop count is
    /// reported (`None`), which is also what the debugfs text file yields.
    #[test]
    fn capture_until_reads_a_regular_file_and_reports_no_drop_count() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usbmon1");
        std::fs::write(&path, one_binary_event()).unwrap();
        let raw = capture_until(
            &path,
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(raw.bytes, one_binary_event());
        assert_eq!(raw.kernel_dropped, None);
    }
}

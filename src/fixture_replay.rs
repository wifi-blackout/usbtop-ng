//! Shared replay core for the fixture-capture harness. Compiled under
//! `cfg(test)` (so the default test suite's replay harness can use it) and
//! under the `capture-fixture` feature (so `--capture-fixture` generates
//! goldens by the same path). Being one module keeps a committed golden equal
//! to what the replay test produces, by construction.

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[cfg(test)]
use serde::Deserialize;

use crate::device::manager::DeviceManager;
use crate::filter::FilterSet;
use crate::headless::{build_report, Baseline, Report};
use crate::snapshot::Snapshot;
use crate::usbmon::binary::BinaryReader;
use crate::usbmon::reader::UsbmonReader;

/// Which usbmon interface a trace came from: selects the reader on replay and
/// the `Report.source` label.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FixtureSource {
    Binary,
    Text,
}

impl FixtureSource {
    /// The `Report.source` label this source produces (`build_report`'s
    /// `source` argument).
    pub fn label(self) -> &'static str {
        match self {
            FixtureSource::Binary => "binary",
            FixtureSource::Text => "text",
        }
    }

    pub fn trace_filename(self) -> &'static str {
        match self {
            FixtureSource::Binary => "trace.bin",
            FixtureSource::Text => "trace.txt",
        }
    }

    pub fn golden_filename(self) -> &'static str {
        match self {
            FixtureSource::Binary => "golden.binary.json",
            FixtureSource::Text => "golden.text.json",
        }
    }

    /// The token used in `meta.toml`'s `sources` list.
    pub fn tag(self) -> &'static str {
        self.label()
    }

    /// Parses a `meta.toml` `sources` tag back into a source. Corpus-discovery
    /// only (`fixture_corpus.rs`'s harness) — the capturer always constructs
    /// `FixtureSource` directly, so this seam is test-only.
    #[cfg(test)]
    pub fn from_tag(tag: &str) -> Option<FixtureSource> {
        match tag {
            "binary" => Some(FixtureSource::Binary),
            "text" => Some(FixtureSource::Text),
            _ => None,
        }
    }
}

/// Fixed replay window: every golden is computed as if exactly one second
/// elapsed, so all `*_bps` fields are deterministic.
pub const FIXED_ELAPSED: std::time::Duration = std::time::Duration::from_secs(1);

/// The subset of `meta.toml` the harness and discovery read. Other keys the
/// capturer writes (board, soc, kernel, `[generator]`, …) are documentation
/// and ignored here. Corpus-discovery only (`fixture_corpus.rs`'s harness
/// reads back committed bundles); the capturer writes `meta.toml`, it never
/// reads one back, so this type is test-only.
#[cfg(test)]
#[derive(Debug, Deserialize)]
pub struct Meta {
    pub sources: Vec<String>,
    #[serde(default)]
    pub controllers: Vec<String>,
    #[serde(default)]
    pub speed_classes: Vec<String>,
}

/// A discovered fixture bundle: its directory and its parsed `meta.toml`.
/// Corpus-discovery only; see `Meta`'s doc comment.
#[cfg(test)]
pub struct Bundle {
    pub dir: PathBuf,
    pub meta: Meta,
}

/// Serialize a report to golden JSON with the non-deterministic `timestamp`
/// key removed, pretty-printed for readable diffs, trailing newline.
pub fn report_to_golden_json(report: &Report) -> anyhow::Result<String> {
    let mut value = serde_json::to_value(report)?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("timestamp");
    }
    let mut out = serde_json::to_string_pretty(&value)?;
    out.push('\n');
    Ok(out)
}

/// Parse report/golden JSON and drop the top-level `timestamp` key, so two
/// reports compare equal regardless of wall-clock. Idempotent: text that
/// already lacks `timestamp` is returned unchanged. Corpus-comparison only
/// (`fixture_corpus.rs`'s harness diffs a fresh replay against the committed
/// golden with this); the capturer only writes goldens
/// (`report_to_golden_json`), it never re-masks one to compare, so this is
/// test-only.
#[cfg(test)]
pub fn to_masked_value(json: &str) -> anyhow::Result<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_str(json)?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("timestamp");
    }
    Ok(value)
}

/// Discover committed bundles under `root` (`tests/fixtures/hosts`): every
/// `*/stage*/` directory holding a readable `meta.toml`. Sorted by path so a
/// failure names a stable bundle. A malformed `meta.toml` is skipped (the
/// SEC/golden tests over well-formed bundles still run); discovery never
/// panics on a stray directory. Corpus-discovery only; see `Meta`'s doc
/// comment.
#[cfg(test)]
pub fn discover_bundles_in(root: &Path) -> Vec<Bundle> {
    let mut bundles = Vec::new();
    let Ok(hosts) = std::fs::read_dir(root) else {
        return bundles;
    };
    let mut host_dirs: Vec<PathBuf> = hosts.flatten().map(|e| e.path()).collect();
    host_dirs.sort();
    for host in host_dirs {
        let Ok(stages) = std::fs::read_dir(&host) else {
            continue;
        };
        let mut stage_dirs: Vec<PathBuf> = stages.flatten().map(|e| e.path()).collect();
        stage_dirs.sort();
        for stage in stage_dirs {
            let meta_path = stage.join("meta.toml");
            let Ok(text) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let Ok(meta) = toml::from_str::<Meta>(&text) else {
                continue;
            };
            bundles.push(Bundle { dir: stage, meta });
        }
    }
    bundles
}

/// Discover the committed corpus under `CARGO_MANIFEST_DIR/tests/fixtures/hosts`.
/// Corpus-discovery only; see `Meta`'s doc comment.
#[cfg(test)]
pub fn discover_bundles() -> Vec<Bundle> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("hosts");
    discover_bundles_in(&root)
}

/// Load a bundle's `internal-devices.toml` for internal-device marking, or
/// `None` when the file is absent or unparsable (`Snapshot::load`'s contract).
pub fn load_internal_devices(bundle_dir: &Path) -> Option<Arc<Snapshot>> {
    Snapshot::load(&bundle_dir.join("internal-devices.toml")).map(Arc::new)
}

/// Replay one bundle's trace for one source into a deterministic report. The
/// bus id passed to the reader is cosmetic — every packet carries its own bus
/// id, so a single trace over the aggregate interface routes to the right bus.
/// This is the exact sequence the capturer uses to generate goldens, so a
/// committed golden equals this output by construction.
pub fn replay_fixture(bundle_dir: &Path, source: FixtureSource) -> anyhow::Result<Report> {
    let mut manager = DeviceManager::with_sysfs_base(bundle_dir.join("sysfs"));
    if let Some(snapshot) = load_internal_devices(bundle_dir) {
        manager.set_internal_snapshot(Some(snapshot));
    }
    // usb.ids overlay left None on purpose: names come only from the captured
    // sysfs strings, so replay is host-independent (see the spec's config parity).
    let baseline = Baseline::capture(&manager);

    let trace = bundle_dir.join(source.trace_filename());
    let shutdown = AtomicBool::new(false);
    match source {
        FixtureSource::Binary => {
            BinaryReader::with_path(0, trace, false).read_packets(&shutdown, |packet| {
                manager.apply_packet(&packet);
                Ok(())
            })?;
        }
        FixtureSource::Text => {
            UsbmonReader::with_path(0, trace, false).read_packets(&shutdown, |packet| {
                manager.apply_packet(&packet);
                Ok(())
            })?;
        }
    }

    manager.enumerate_present_devices();
    // Resolves BusReport.controller + bus speed_mbps. Enumeration alone does
    // NOT (see manager.rs:188); without this the controller/speed fields are null.
    manager.update_bus_speeds();

    Ok(build_report(
        &manager,
        &baseline,
        FIXED_ELAPSED,
        source.label(),
        0,
        source == FixtureSource::Text,
        &FilterSet::default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_round_trips_through_its_tag() {
        assert_eq!(
            FixtureSource::from_tag("binary"),
            Some(FixtureSource::Binary)
        );
        assert_eq!(FixtureSource::from_tag("text"), Some(FixtureSource::Text));
        assert_eq!(FixtureSource::from_tag("mmap"), None);
        assert_eq!(FixtureSource::Binary.trace_filename(), "trace.bin");
        assert_eq!(FixtureSource::Text.golden_filename(), "golden.text.json");
    }

    #[test]
    fn to_masked_value_drops_only_the_timestamp_key() {
        let v = to_masked_value(r#"{"version":1,"timestamp":123.5,"source":"binary"}"#).unwrap();
        assert!(v.get("timestamp").is_none(), "timestamp removed");
        assert_eq!(v["version"], 1);
        assert_eq!(v["source"], "binary");
    }

    #[test]
    fn discovery_finds_stage_dirs_with_meta_sorted_by_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let stage = root.join("board-a").join("stage1");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("meta.toml"), "sources = [\"binary\"]\n").unwrap();
        // A stage dir without meta.toml is ignored.
        std::fs::create_dir_all(root.join("board-b").join("stage1")).unwrap();

        let bundles = discover_bundles_in(root);
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].meta.sources, vec!["binary".to_string()]);
        assert!(bundles[0].meta.controllers.is_empty());
        assert!(bundles[0].meta.speed_classes.is_empty());
        assert!(bundles[0].dir.ends_with("board-a/stage1"));
    }

    #[test]
    fn tag_matches_label_for_both_sources() {
        assert_eq!(FixtureSource::Binary.tag(), FixtureSource::Binary.label());
        assert_eq!(FixtureSource::Text.tag(), FixtureSource::Text.label());
        assert_eq!(FixtureSource::Binary.tag(), "binary");
        assert_eq!(FixtureSource::Text.tag(), "text");
    }

    #[test]
    fn fixed_elapsed_is_exactly_one_second() {
        assert_eq!(FIXED_ELAPSED, std::time::Duration::from_secs(1));
    }

    #[test]
    fn report_to_golden_json_strips_timestamp_and_pretty_prints_with_trailing_newline() {
        let report = Report {
            version: 1,
            timestamp: 123.5,
            window_seconds: 1.0,
            source: "binary",
            dropped_packets: 0,
            kernel_dropped_packets: 0,
            total_rx_bps: 0.0,
            total_tx_bps: 0.0,
            buses: Vec::new(),
        };
        let json = report_to_golden_json(&report).unwrap();
        assert!(json.ends_with('\n'), "trailing newline");
        assert!(json.contains('\n'), "pretty-printed (multi-line)");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("timestamp").is_none(), "timestamp stripped");
        assert_eq!(value["version"], 1);
        assert_eq!(value["source"], "binary");
    }

    #[test]
    fn discover_bundles_resolves_the_manifest_relative_corpus_root_without_panicking() {
        // No corpus is committed yet, so this only proves the
        // `CARGO_MANIFEST_DIR`-relative path resolves and a missing directory
        // is handled gracefully (empty, no panic) rather than asserting on
        // its contents.
        let _ = discover_bundles();
    }

    fn write_attr(dir: &std::path::Path, name: &str, value: &str) {
        std::fs::write(dir.join(name), value).unwrap();
    }

    fn build_min_bundle(dir: &std::path::Path) {
        // sysfs: controller stand-in + relative usb1 symlink + one device 1-1 (dev 3).
        let sysfs = dir.join("sysfs");
        let ctrl = sysfs.join("0000:00:14.0").join("usb1");
        std::fs::create_dir_all(&ctrl).unwrap();
        write_attr(&ctrl, "busnum", "1\n");
        write_attr(&ctrl, "devnum", "1\n");
        write_attr(&ctrl, "speed", "480\n");
        std::os::unix::fs::symlink("0000:00:14.0/usb1", sysfs.join("usb1")).unwrap();

        let dev = sysfs.join("1-1");
        std::fs::create_dir_all(&dev).unwrap();
        write_attr(&dev, "busnum", "1\n");
        write_attr(&dev, "devnum", "3\n");
        write_attr(&dev, "speed", "480\n");
        write_attr(&dev, "idVendor", "0430\n");
        write_attr(&dev, "idProduct", "0100\n");

        // trace.bin: one sanitized callback, bus 1 dev 3 ep 0x81 IN bulk length 1000.
        let mut hdr = vec![0u8; 48];
        hdr[8] = b'C';
        hdr[9] = 3; // bulk
        hdr[10] = 0x81; // ep1 IN
        hdr[11] = 3; // devnum
        hdr[12..14].copy_from_slice(&1u16.to_ne_bytes()); // busnum
        hdr[32..36].copy_from_slice(&1000u32.to_ne_bytes()); // length
                                                             // len_cap@36 stays 0 (sanitized).
        std::fs::write(dir.join("trace.bin"), &hdr).unwrap();

        // trace.txt: same traffic, data field elided.
        std::fs::write(
            dir.join("trace.txt"),
            "ffff0000aaaa0001 200 C Bi:1:003:1 0 1000 <\n",
        )
        .unwrap();
    }

    #[test]
    fn replay_binary_produces_a_deterministic_report_with_controller_and_speed() {
        let temp = tempfile::tempdir().unwrap();
        build_min_bundle(temp.path());

        let report = replay_fixture(temp.path(), FixtureSource::Binary).unwrap();
        assert_eq!(report.source, "binary");
        assert_eq!(report.window_seconds, 1.0);
        let bus = &report.buses[0];
        assert_eq!(bus.bus, 1);
        assert_eq!(bus.controller.as_deref(), Some("0000:00:14.0"));
        assert_eq!(bus.speed_mbps, 480.0);
        // The root hub (usb1, empty port-chain) sorts ahead of 1-1 in
        // `build_report`'s device ordering, so the traffic device is found
        // by address rather than assumed to be devices[0].
        let dev = bus.devices.iter().find(|d| d.address == 3).unwrap();
        assert_eq!(dev.address, 3);
        assert_eq!(dev.total_rx_bytes, 1000);
        assert_eq!(dev.rx_bps, 1000.0, "1000 bytes over the fixed 1s window");
        assert_eq!(dev.vendor_id.as_deref(), Some("0430"));

        // Replaying twice is byte-identical after masking the timestamp.
        let a = to_masked_value(&serde_json::to_string(&report).unwrap()).unwrap();
        let report2 = replay_fixture(temp.path(), FixtureSource::Binary).unwrap();
        let b = to_masked_value(&serde_json::to_string(&report2).unwrap()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn replay_text_labels_source_text_end_to_end() {
        let temp = tempfile::tempdir().unwrap();
        build_min_bundle(temp.path());
        let report = replay_fixture(temp.path(), FixtureSource::Text).unwrap();
        assert_eq!(report.source, "text");
        // The bulk device here has no iso traffic, so estimated stays false; the
        // point pinned is that the text path runs end to end and labels the source.
        let dev = report
            .buses
            .iter()
            .flat_map(|b| b.devices.iter())
            .find(|d| d.address == 3)
            .unwrap();
        assert_eq!(dev.total_rx_bytes, 1000);
    }
}

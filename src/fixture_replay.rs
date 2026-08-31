//! Shared replay core for the fixture-capture harness. Compiled under
//! `cfg(test)` (so the default test suite's replay harness can use it) and
//! under the `capture-fixture` feature (so `--capture-fixture` generates
//! goldens by the same path). Being one module keeps a committed golden equal
//! to what the replay test produces, by construction.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::headless::Report;

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
/// and ignored here.
#[derive(Debug, Deserialize)]
pub struct Meta {
    pub sources: Vec<String>,
    #[serde(default)]
    pub controllers: Vec<String>,
    #[serde(default)]
    pub speed_classes: Vec<String>,
}

/// A discovered fixture bundle: its directory and its parsed `meta.toml`.
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
/// already lacks `timestamp` is returned unchanged.
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
/// panics on a stray directory.
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
pub fn discover_bundles() -> Vec<Bundle> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("hosts");
    discover_bundles_in(&root)
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
}

//! Default-suite regression harness over the committed fixture corpus. Each
//! bundle under tests/fixtures/hosts/*/stage*/ is replayed per declared source
//! and compared to its committed golden; the SEC-1/SEC-2 invariants are
//! enforced over every committed fixture. Runs with no feature and no hardware.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::fixture_replay::{
    discover_bundles, replay_fixture, report_to_golden_json, to_masked_value, Bundle,
    FixtureSource, Meta,
};

fn sources_of(bundle: &Bundle) -> Vec<FixtureSource> {
    bundle
        .meta
        .sources
        .iter()
        .filter_map(|s| FixtureSource::from_tag(s))
        .collect()
}

/// Root of the committed fixture corpus: `CARGO_MANIFEST_DIR/tests/fixtures/hosts`.
/// Used directly by the filesystem-walk SEC tests and the strict-corpus test
/// below, independent of `discover_bundles`'s (lenient) bundle discovery.
fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("hosts")
}

/// Recursively collect every path beneath `root` whose file name is exactly
/// `name`, regardless of what (if anything) its enclosing directory's
/// `meta.toml` says. Shared by the SEC-1 trace-file walks (`trace.bin` /
/// `trace.txt`) and the SEC-2 `sysfs`-directory walk below, so the SEC
/// invariants cover every matching file/dir in the corpus even when a stage's
/// `meta.toml` is missing or malformed. A canonicalized-directory visited set
/// guards against a symlink cycle turning a tampered bundle into a hang
/// instead of a clean test failure.
fn find_named(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut visited = HashSet::new();
    if let Ok(real_root) = std::fs::canonicalize(root) {
        visited.insert(real_root);
    }
    walk_find_named(root, name, &mut visited, &mut found);
    found
}

fn walk_find_named(
    dir: &Path,
    name: &str,
    visited: &mut HashSet<PathBuf>,
    found: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            found.push(path.clone());
        }
        if path.is_dir() {
            let Ok(real) = std::fs::canonicalize(&path) else {
                continue;
            };
            if visited.insert(real) {
                walk_find_named(&path, name, visited, found);
            }
        }
    }
}

#[test]
fn every_bundle_replays_to_its_golden() {
    let bundles = discover_bundles();
    assert!(
        !bundles.is_empty(),
        "no fixture bundles found — seeds missing?"
    );
    for bundle in &bundles {
        for source in sources_of(bundle) {
            let report = replay_fixture(&bundle.dir, source)
                .unwrap_or_else(|e| panic!("replay {} {source:?}: {e}", bundle.dir.display()));
            let got = to_masked_value(&serde_json::to_string(&report).unwrap()).unwrap();
            let golden_path = bundle.dir.join(source.golden_filename());
            let golden_text = std::fs::read_to_string(&golden_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", golden_path.display()));
            let want = to_masked_value(&golden_text).unwrap();
            assert_eq!(
                got,
                want,
                "golden mismatch: {} {source:?}",
                bundle.dir.display()
            );
        }
    }
}

#[test]
fn declared_controllers_and_speeds_are_non_null_in_the_golden() {
    for bundle in discover_bundles() {
        for source in sources_of(&bundle) {
            let report = replay_fixture(&bundle.dir, source).unwrap();
            let value = to_masked_value(&serde_json::to_string(&report).unwrap()).unwrap();
            for bus in value["buses"].as_array().into_iter().flatten() {
                if !bundle.meta.controllers.is_empty() {
                    assert!(
                        !bus["controller"].is_null(),
                        "null controller in {}",
                        bundle.dir.display()
                    );
                }
                if !bundle.meta.speed_classes.is_empty() {
                    assert!(
                        bus["speed_mbps"].as_f64().unwrap_or(0.0) > 0.0,
                        "zero speed in {}",
                        bundle.dir.display()
                    );
                }
            }
        }
    }
}

/// Read a fixture source file, distinguishing "legitimately absent" from
/// every other failure: a missing file (that source wasn't captured for this
/// bundle) skips the check by returning `None`; any other read error (a
/// permission error, say) panics rather than silently skipping it.
fn read_source_bytes(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("read {}: {e}", path.display()),
    }
}

// The three tests below walk the fixture corpus directly on disk (via
// `find_named`), rather than iterating `discover_bundles()`'s bundle list.
// `discover_bundles`/`discover_bundles_in` silently skip a stage directory
// whose `meta.toml` is missing, unreadable, or malformed — so a tampered
// bundle with a broken `meta.toml` would otherwise escape every SEC check.
// Walking the filesystem for `trace.bin`/`trace.txt`/`sysfs` directly makes
// these invariants hold independent of `meta.toml` entirely.

#[test]
fn sec1_no_binary_payload() {
    for bin in find_named(&fixtures_root(), "trace.bin") {
        let Some(bytes) = read_source_bytes(&bin) else {
            continue;
        };
        assert_eq!(
            bytes.len() % 48,
            0,
            "SEC-1: {} not a whole header count",
            bin.display()
        );
        // A whole header count alone doesn't prove payload-freedom: a trace
        // could carry a header (len_cap=48) followed by exactly 48 payload
        // bytes and still pass that check. Walk each 48-byte record and
        // assert its len_cap (bytes 36..40, native-endian u32) is 0, which
        // proves both payload-freedom and correct framing.
        for (i, record) in bytes.chunks_exact(48).enumerate() {
            let len_cap = u32::from_ne_bytes(record[36..40].try_into().unwrap());
            assert_eq!(
                len_cap,
                0,
                "SEC-1: {} record {i} carries len_cap={len_cap} (payload leaked?)",
                bin.display()
            );
        }
    }
}

#[test]
fn sec1_no_text_data_tag() {
    for txt in find_named(&fixtures_root(), "trace.txt") {
        let Some(bytes) = read_source_bytes(&txt) else {
            continue;
        };
        let text = String::from_utf8(bytes)
            .unwrap_or_else(|e| panic!("SEC-1: {} is not valid UTF-8: {e}", txt.display()));
        for (i, line) in text.lines().enumerate() {
            assert!(
                !line.split_whitespace().any(|t| t == "="),
                "SEC-1: {}:{} carries a data tag",
                txt.display(),
                i + 1
            );
        }
    }
}

#[test]
fn sec2_sysfs_paths_stay_inside_the_bundle() {
    for sysfs in find_named(&fixtures_root(), "sysfs") {
        let root = std::fs::canonicalize(&sysfs).unwrap();
        assert_contained(&root, &root);
    }
}

fn assert_contained(root: &Path, dir: &Path) {
    let mut visited = HashSet::new();
    visited.insert(dir.to_path_buf());
    walk_contained(root, dir, &mut visited);
}

/// Recursive walk behind [`assert_contained`], tracking canonicalized dirs
/// already visited. A hand-authored in-bundle symlink can resolve back to an
/// already-visited ancestor (e.g. `sysfs/x -> ..` resolving inside the
/// bundle); without this guard that would recurse forever and hang instead of
/// failing. `real.starts_with(root)` is still checked on every entry before
/// the cycle check, so escape detection is unaffected — only re-descending
/// into a dir already walked is skipped.
fn walk_contained(root: &Path, dir: &Path, visited: &mut HashSet<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let real = std::fs::canonicalize(entry.path())
            .unwrap_or_else(|e| panic!("canonicalize {}: {e}", entry.path().display()));
        assert!(
            real.starts_with(root),
            "SEC-2: {} escapes {}",
            real.display(),
            root.display()
        );
        if real.is_dir() && visited.insert(real.clone()) {
            walk_contained(root, &real, visited);
        }
    }
}

/// Strict-corpus invariant: every directory exactly two levels below `root`
/// (`hosts/<host>/<stage>/` — the same candidate set `discover_bundles_in`
/// walks) must be a well-formed bundle: `meta.toml` exists and parses as
/// [`Meta`], every tag in its `sources` list is recognized by
/// `FixtureSource::from_tag`, and each declared source's trace file exists.
/// Unlike `discover_bundles_in` (which stays lenient on purpose, so the bless
/// helper and failure locality keep working over a still-being-authored
/// corpus), this never skips a candidate directory: a broken `meta.toml`, an
/// unrecognized source tag, or a missing declared trace file fails the corpus
/// instead of silently dropping its coverage.
fn check_corpus_strict(root: &Path) -> Result<(), String> {
    let hosts = std::fs::read_dir(root).map_err(|e| format!("read {}: {e}", root.display()))?;
    let mut host_dirs: Vec<PathBuf> = hosts
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    host_dirs.sort();

    for host in host_dirs {
        let stages =
            std::fs::read_dir(&host).map_err(|e| format!("read {}: {e}", host.display()))?;
        let mut stage_dirs: Vec<PathBuf> = stages
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        stage_dirs.sort();

        for stage in stage_dirs {
            let meta_path = stage.join("meta.toml");
            let text = std::fs::read_to_string(&meta_path)
                .map_err(|e| format!("{}: meta.toml unreadable: {e}", meta_path.display()))?;
            let meta: Meta = toml::from_str(&text)
                .map_err(|e| format!("{}: meta.toml does not parse: {e}", meta_path.display()))?;

            for tag in &meta.sources {
                let source = FixtureSource::from_tag(tag).ok_or_else(|| {
                    format!("{}: unrecognized source tag {tag:?}", meta_path.display())
                })?;
                let trace_path = stage.join(source.trace_filename());
                if !trace_path.exists() {
                    return Err(format!(
                        "{}: declares source {tag:?} but {} is missing",
                        meta_path.display(),
                        trace_path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn every_stage_dir_is_a_wellformed_bundle() {
    check_corpus_strict(&fixtures_root()).unwrap();
}

/// `binary_kernel_dropped` is optional documentation of a bundle's own
/// completeness; when a bundle declares it, it must be a non-negative
/// integer. Read through `toml::Value` so the key stays out of `Meta`
/// (which nothing else would read; see the plan's ruling).
#[test]
fn declared_binary_kernel_drops_are_non_negative_integers() {
    for bundle in discover_bundles() {
        let text = std::fs::read_to_string(bundle.dir.join("meta.toml")).unwrap();
        let value: toml::Value = toml::from_str(&text).unwrap();
        if let Some(v) = value.get("binary_kernel_dropped") {
            let n = v.as_integer().unwrap_or_else(|| {
                panic!(
                    "{}: binary_kernel_dropped is not an integer",
                    bundle.dir.display()
                )
            });
            assert!(n >= 0, "{}: negative drop count {n}", bundle.dir.display());
        }
    }
}

/// Every mainrag ground-truth iso bundle (see its `[generator]` note) is a
/// corpus accuracy anchor: captured with the enlarged ring, its binary
/// golden matched a concurrent eBPF capture and the v4l2 frame bytes. Each
/// one must keep declaring zero kernel drops.
#[test]
fn the_ground_truth_bundle_declares_zero_binary_drops() {
    let bundles: Vec<_> = discover_bundles()
        .into_iter()
        .filter(|b| {
            b.dir.ends_with("stage2")
                && b.dir
                    .parent()
                    .and_then(|host| host.file_name())
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("mainrag-"))
        })
        .collect::<Vec<_>>();
    assert!(
        !bundles.is_empty(),
        "the mainrag ground-truth bundle is committed"
    );
    for bundle in bundles {
        let text = std::fs::read_to_string(bundle.dir.join("meta.toml")).unwrap();
        let value: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            value
                .get("binary_kernel_dropped")
                .and_then(toml::Value::as_integer),
            Some(0),
            "{}",
            bundle.dir.display()
        );
    }
}

/// Bless helper: regenerate every *seed's* `trace.bin` and both goldens by
/// replay, so committed goldens equal harness output by construction. Only
/// touches bundles whose host directory (the bundle dir's parent) is named
/// `seed-*` — a real fleet capture's host directory won't match that prefix,
/// so a bless run can never silently replace its real binary trace with a
/// text-derived reconstruction. Not run in CI. Run once after
/// authoring/altering a seed:
///   cargo test bless_seed_goldens -- --ignored --nocapture
#[test]
#[ignore]
fn bless_seed_goldens() {
    for bundle in discover_bundles() {
        if !is_seed_bundle(&bundle) {
            eprintln!("skipping non-seed bundle {}", bundle.dir.display());
            continue;
        }
        // (Re)write trace.bin from the bundle's text trace so binary and text
        // describe the same traffic. Seeds only: real captures already have both.
        write_seed_binary_from_text(&bundle.dir);
        bless_bundle_goldens(&bundle);
    }
}

/// Bless helper for one named *real* bundle after an intentional pipeline
/// change (a parser fix that alters what a committed trace replays to).
/// Regenerates only the goldens -- never a trace -- of the bundle named by
/// `USBTOP_NG_BLESS_BUNDLE=<host-dir>/<stage-dir>`, relative to
/// `tests/fixtures/hosts`. Not run in CI:
///   USBTOP_NG_BLESS_BUNDLE=asus-2026-08-31/stage2 cargo test bless_named_bundle -- --ignored --nocapture
#[test]
#[ignore]
fn bless_named_bundle() {
    let name = std::env::var("USBTOP_NG_BLESS_BUNDLE")
        .expect("set USBTOP_NG_BLESS_BUNDLE=<host-dir>/<stage-dir>");
    let bundle = discover_bundles()
        .into_iter()
        .find(|b| b.dir.ends_with(&name))
        .unwrap_or_else(|| panic!("no bundle named {name} under {}", fixtures_root().display()));
    bless_bundle_goldens(&bundle);
}

/// Regenerates a bundle's goldens by replay -- never its trace. Shared by
/// `bless_seed_goldens` (after it rewrites the seed's `trace.bin`) and
/// `bless_named_bundle`.
fn bless_bundle_goldens(bundle: &Bundle) {
    for source in sources_of(bundle) {
        let report = replay_fixture(&bundle.dir, source).unwrap();
        std::fs::write(
            bundle.dir.join(source.golden_filename()),
            report_to_golden_json(&report).unwrap(),
        )
        .unwrap();
    }
    eprintln!("blessed {}", bundle.dir.display());
}

/// A bundle counts as a seed only when its host directory (`dir`'s parent)
/// exists and its name starts with `seed-`.
fn is_seed_bundle(bundle: &Bundle) -> bool {
    bundle
        .dir
        .parent()
        .and_then(|host| host.file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("seed-"))
}

/// Build a `trace.bin` whose events mirror the bundle's `trace.txt` callbacks,
/// so a seed's two sources agree. Handles the fields the pipeline reads:
/// type, xfer, endpoint+dir, devnum, busnum, length.
fn write_seed_binary_from_text(dir: &Path) {
    use crate::usbmon::parser::{parse_usbmon_text_line, TransferType, UrbType};
    let Ok(text) = std::fs::read_to_string(dir.join("trace.txt")) else {
        return;
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(p) = parse_usbmon_text_line(line) else {
            continue;
        };
        if p.urb_type != UrbType::Callback {
            continue;
        }
        let mut b = vec![0u8; 48];
        b[8] = b'C';
        b[9] = match p.transfer_type {
            Some(TransferType::Isochronous) => 0,
            Some(TransferType::Interrupt) => 1,
            Some(TransferType::Control) => 2,
            Some(TransferType::Bulk) => 3,
            None => 3,
        };
        b[10] = p.endpoint | if p.direction { 0x80 } else { 0 };
        b[11] = p.device_id;
        b[12..14].copy_from_slice(&u16::from(p.bus_id).to_ne_bytes());
        b[32..36].copy_from_slice(&p.data_length.to_ne_bytes());
        // len_cap@36 stays 0.
        out.extend_from_slice(&b);
    }
    std::fs::write(dir.join("trace.bin"), &out).unwrap();
}

#[cfg(test)]
mod check_corpus_strict_tests {
    use super::*;

    #[test]
    fn fails_closed_on_a_broken_meta_toml() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("host-a").join("stage1");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("meta.toml"), "not valid toml {{{").unwrap();

        let err = check_corpus_strict(temp.path()).unwrap_err();
        assert!(
            err.contains(&stage.join("meta.toml").display().to_string()),
            "error must name the offending path: {err}"
        );
    }

    #[test]
    fn fails_closed_on_an_unrecognized_source_tag() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("host-a").join("stage1");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(
            stage.join("meta.toml"),
            "sources = [\"binary\", \"bogus\"]\n",
        )
        .unwrap();
        std::fs::write(stage.join("trace.bin"), []).unwrap();

        let err = check_corpus_strict(temp.path()).unwrap_err();
        assert!(err.contains("bogus"), "error must name the tag: {err}");
        assert!(
            err.contains(&stage.join("meta.toml").display().to_string()),
            "error must name the offending path: {err}"
        );
    }

    #[test]
    fn fails_closed_on_a_missing_declared_trace_file() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("host-a").join("stage1");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("meta.toml"), "sources = [\"binary\"]\n").unwrap();
        // trace.bin deliberately not written.

        let err = check_corpus_strict(temp.path()).unwrap_err();
        assert!(err.contains("trace.bin"), "{err}");
    }

    #[test]
    fn accepts_a_wellformed_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("host-a").join("stage1");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("meta.toml"), "sources = [\"binary\"]\n").unwrap();
        std::fs::write(stage.join("trace.bin"), []).unwrap();

        check_corpus_strict(temp.path()).unwrap();
    }
}

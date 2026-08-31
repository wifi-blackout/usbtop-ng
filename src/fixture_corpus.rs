//! Default-suite regression harness over the committed fixture corpus. Each
//! bundle under tests/fixtures/hosts/*/stage*/ is replayed per declared source
//! and compared to its committed golden; the SEC-1/SEC-2 invariants are
//! enforced over every committed fixture. Runs with no feature and no hardware.

use std::path::Path;

use crate::fixture_replay::{
    discover_bundles, replay_fixture, report_to_golden_json, to_masked_value, Bundle, FixtureSource,
};

fn sources_of(bundle: &Bundle) -> Vec<FixtureSource> {
    bundle
        .meta
        .sources
        .iter()
        .filter_map(|s| FixtureSource::from_tag(s))
        .collect()
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

#[test]
fn sec1_no_binary_payload() {
    for bundle in discover_bundles() {
        let bin = bundle.dir.join("trace.bin");
        if let Ok(meta) = std::fs::metadata(&bin) {
            assert_eq!(
                meta.len() % 48,
                0,
                "SEC-1: {} not a whole header count",
                bin.display()
            );
        }
    }
}

#[test]
fn sec1_no_text_data_tag() {
    for bundle in discover_bundles() {
        let txt = bundle.dir.join("trace.txt");
        if let Ok(text) = std::fs::read_to_string(&txt) {
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
}

#[test]
fn sec2_sysfs_paths_stay_inside_the_bundle() {
    for bundle in discover_bundles() {
        let sysfs = bundle.dir.join("sysfs");
        let root = std::fs::canonicalize(&sysfs).unwrap();
        assert_contained(&root, &root);
    }
}

fn assert_contained(root: &Path, dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let real = std::fs::canonicalize(entry.path())
            .unwrap_or_else(|e| panic!("canonicalize {}: {e}", entry.path().display()));
        assert!(
            real.starts_with(root),
            "SEC-2: {} escapes {}",
            real.display(),
            root.display()
        );
        if real.is_dir() {
            assert_contained(root, &real);
        }
    }
}

/// Bless helper: regenerate every seed's `trace.bin` and both goldens by
/// replay, so committed goldens equal harness output by construction. Not run
/// in CI. Run once after authoring/altering a seed:
///   cargo test bless_seed_goldens -- --ignored --nocapture
#[test]
#[ignore]
fn bless_seed_goldens() {
    for bundle in discover_bundles() {
        // (Re)write trace.bin from the bundle's text trace so binary and text
        // describe the same traffic. Seeds only: real captures already have both.
        write_seed_binary_from_text(&bundle.dir);
        for source in sources_of(&bundle) {
            let report = replay_fixture(&bundle.dir, source).unwrap();
            std::fs::write(
                bundle.dir.join(source.golden_filename()),
                report_to_golden_json(&report).unwrap(),
            )
            .unwrap();
        }
        eprintln!("blessed {}", bundle.dir.display());
    }
}

/// Build a `trace.bin` whose events mirror the bundle's `trace.txt` callbacks,
/// so a seed's two sources agree. Handles the fields the pipeline reads:
/// type, xfer, endpoint+dir, devnum, busnum, length.
#[cfg(test)]
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

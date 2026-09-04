//! Coverage tags (computed from the replayed report) and the `meta.toml`
//! writer. Only `sources`, `controllers`, and `speed_classes` are read back by
//! the harness; the rest is human-facing documentation.

use std::path::Path;

use serde::Serialize;

use crate::diag::collect::{os_pretty_name_from, read_trimmed};
use crate::fixture_replay::FixtureSource;
use crate::headless::Report;

pub struct CoverageTags {
    pub controllers: Vec<String>,
    pub speed_classes: Vec<String>,
    pub transfer_types: Vec<String>,
}

/// The coverage a report exercises: distinct non-null controllers, distinct
/// device speeds (Mbps, skipping unknown 0; integral values print bare,
/// fractional ones — e.g. 1.5 Mbps low speed — keep their fraction rather
/// than truncating or rounding to a fictitious whole number), and distinct
/// endpoint transfer types. Each list is sorted so meta.toml is stable across
/// runs.
pub fn compute_coverage_tags(report: &Report) -> CoverageTags {
    let mut controllers = Vec::new();
    let mut speeds = Vec::new();
    let mut transfer_types = Vec::new();
    for bus in &report.buses {
        if let Some(controller) = &bus.controller {
            controllers.push(controller.clone());
        }
        for device in &bus.devices {
            let speed = device.speed_mbps;
            if speed > 0.0 {
                speeds.push(if speed.fract() == 0.0 {
                    format!("{}", speed as u64)
                } else {
                    format!("{speed}")
                });
            }
            for ep in &device.endpoints {
                transfer_types.push(ep.transfer_type.to_string());
            }
        }
    }
    CoverageTags {
        controllers: sorted_unique(controllers),
        speed_classes: sorted_unique(speeds),
        transfer_types: sorted_unique(transfer_types),
    }
}

fn sorted_unique(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

#[derive(Serialize)]
struct MetaOut {
    board: String,
    soc: String,
    arch: String,
    kernel: String,
    os: String,
    usbtop_ng_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_id: Option<u32>,
    captured_unix: u64,
    /// Kernel-side events lost from the binary source during the capture.
    /// Absent when the source could not report one. Documentation of the
    /// bundle's own completeness; never asserted by the strict corpus
    /// check; the mainrag anchor test asserts it is zero for the
    /// ground-truth bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    binary_kernel_dropped: Option<u64>,
    controllers: Vec<String>,
    speed_classes: Vec<String>,
    transfer_types: Vec<String>,
    sources: Vec<String>,
}

/// The `meta.toml` text for a freshly captured bundle: host identity plus the
/// coverage tags and the sources captured. The tester may hand-append a
/// `[generator]` block afterward (documentation only, never asserted).
/// `binary_kernel_dropped` is the kernel's drop count for the binary source,
/// when it reported one.
pub fn build_meta(
    report: &Report,
    sources: &[FixtureSource],
    stage_id: Option<u32>,
    binary_kernel_dropped: Option<u64>,
) -> anyhow::Result<String> {
    let tags = compute_coverage_tags(report);
    let host = gather_host_identity();
    let out = MetaOut {
        board: host.board,
        soc: host.soc,
        arch: host.arch,
        kernel: host.kernel,
        os: host.os,
        usbtop_ng_version: env!("CARGO_PKG_VERSION").to_string(),
        stage_id,
        captured_unix: now_unix(),
        binary_kernel_dropped,
        controllers: tags.controllers,
        speed_classes: tags.speed_classes,
        transfer_types: tags.transfer_types,
        sources: sources.iter().map(|s| s.tag().to_string()).collect(),
    };
    Ok(toml::to_string(&out)?)
}

struct HostIdentity {
    board: String,
    soc: String,
    arch: String,
    kernel: String,
    os: String,
}

/// Best-effort host identity from the environment. Unreadable fields become an
/// empty string; this is documentation, not asserted, so it never fails capture.
fn gather_host_identity() -> HostIdentity {
    HostIdentity {
        board: read_trimmed(Path::new("/proc/device-tree/model"))
            .or_else(|| read_trimmed(Path::new("/sys/devices/virtual/dmi/id/product_name")))
            .unwrap_or_default(),
        soc: read_trimmed(Path::new("/proc/device-tree/compatible")).unwrap_or_default(),
        arch: std::env::consts::ARCH.to_string(),
        kernel: read_trimmed(Path::new("/proc/sys/kernel/osrelease")).unwrap_or_default(),
        os: std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| os_pretty_name_from(&t))
            .unwrap_or_default(),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::manager::DeviceManager;
    use crate::filter::FilterSet;
    use crate::fixture_replay::FIXED_ELAPSED;
    use crate::headless::{build_report, Baseline};
    use crate::usbmon::parser::parse_usbmon_text_line;

    #[test]
    fn coverage_tags_are_distinct_sorted_and_skip_zero_speed() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("1-4");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("busnum"), "1\n").unwrap();
        std::fs::write(dev.join("devnum"), "4\n").unwrap();
        std::fs::write(dev.join("speed"), "480\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let iso = parse_usbmon_text_line("ffff0000aaaa0001 200 C Zi:1:004:1 0:1:6672:0 32 27000 =")
            .unwrap();
        let bulk = parse_usbmon_text_line("ffff0000aaaa0002 300 C Bo:1:004:2 0 512 >").unwrap();
        mgr.apply_packet(&iso);
        mgr.apply_packet(&bulk);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        let report = build_report(
            &mgr,
            &baseline,
            FIXED_ELAPSED,
            "binary",
            0,
            false,
            &FilterSet::default(),
        );

        let tags = compute_coverage_tags(&report);
        assert_eq!(tags.speed_classes, vec!["480".to_string()]);
        assert_eq!(
            tags.transfer_types,
            vec!["bulk".to_string(), "iso".to_string()]
        );
    }

    #[test]
    fn speed_classes_keep_the_fraction_for_a_low_speed_device_instead_of_truncating() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("1-2");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("busnum"), "1\n").unwrap();
        std::fs::write(dev.join("devnum"), "2\n").unwrap();
        // 1.5 Mbps is a real Low Speed link; `as u64` truncates it to "1"
        // (not a real USB speed) and `.round()` gives "2" (equally
        // fictitious) -- the fraction must survive.
        std::fs::write(dev.join("speed"), "1.5\n").unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        let report = build_report(
            &mgr,
            &baseline,
            FIXED_ELAPSED,
            "binary",
            0,
            false,
            &FilterSet::default(),
        );

        let tags = compute_coverage_tags(&report);
        assert_eq!(tags.speed_classes, vec!["1.5".to_string()]);
    }

    #[test]
    fn controllers_tag_resolves_through_a_root_hub_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("sysfs");
        let controller_dir = base.join("0000:00:14.0").join("usb1");
        std::fs::create_dir_all(&controller_dir).unwrap();
        std::fs::write(controller_dir.join("busnum"), "1\n").unwrap();
        std::fs::write(controller_dir.join("devnum"), "1\n").unwrap();
        std::fs::write(controller_dir.join("speed"), "480\n").unwrap();
        std::os::unix::fs::symlink("0000:00:14.0/usb1", base.join("usb1")).unwrap();

        let mut mgr = DeviceManager::with_sysfs_base(base);
        let baseline = Baseline::capture(&mgr);
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        let report = build_report(
            &mgr,
            &baseline,
            FIXED_ELAPSED,
            "binary",
            0,
            false,
            &FilterSet::default(),
        );

        let tags = compute_coverage_tags(&report);
        assert_eq!(tags.controllers, vec!["0000:00:14.0".to_string()]);
    }

    #[test]
    fn build_meta_emits_the_three_keys_the_harness_reads() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let report = build_report(
            &mgr,
            &baseline,
            FIXED_ELAPSED,
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let toml_text = build_meta(
            &report,
            &[FixtureSource::Binary, FixtureSource::Text],
            Some(7),
            None,
        )
        .unwrap();
        let parsed: crate::fixture_replay::Meta = toml::from_str(&toml_text).unwrap();
        assert_eq!(
            parsed.sources,
            vec!["binary".to_string(), "text".to_string()]
        );
        assert!(toml_text.contains("stage_id = 7"));
        assert!(
            !toml_text.contains("binary_kernel_dropped"),
            "no count reported: the key must be absent, not zero: {toml_text}"
        );
    }

    /// The capturer writes the kernel's drop count for the binary source so
    /// a bundle declares its own completeness; a bundle captured without the
    /// stats ioctl (old kernel) simply lacks the key.
    #[test]
    fn build_meta_records_the_binary_kernel_drop_count_when_reported() {
        let temp = tempfile::tempdir().unwrap();
        let mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let baseline = Baseline::capture(&mgr);
        let report = build_report(
            &mgr,
            &baseline,
            FIXED_ELAPSED,
            "binary",
            0,
            false,
            &FilterSet::default(),
        );
        let toml_text = build_meta(&report, &[FixtureSource::Binary], None, Some(1_621)).unwrap();
        assert!(
            toml_text.contains("binary_kernel_dropped = 1621"),
            "{toml_text}"
        );
        let value: toml::Value = toml::from_str(&toml_text).unwrap();
        assert_eq!(
            value
                .get("binary_kernel_dropped")
                .and_then(toml::Value::as_integer),
            Some(1_621)
        );
    }
}

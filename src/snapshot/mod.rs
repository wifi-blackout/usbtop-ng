//! The internal-device snapshot: which USB devices are built into this
//! machine. Captured with external gear unplugged, stored as TOML, and
//! queried to mark internal devices apart from external ones.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::usbids::UsbIds;

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotDevice {
    /// The sysfs directory name: physical port chain (`1-4`, `3-3.1`) or
    /// `usbN` for a root hub.
    pub port_path: String,
    /// 4-digit lowercase hex, absent when sysfs had no readable ID file.
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub captured_unix: u64,
    pub devices: Vec<SnapshotDevice>,
}

impl Snapshot {
    /// Record every USB device under `base` (default: the real sysfs).
    /// The same walk idle enumeration does: directory per device,
    /// interface entries carry `:` and are skipped.
    pub fn capture(base: Option<&Path>) -> std::io::Result<Snapshot> {
        let default = Path::new("/sys/bus/usb/devices");
        let base = base.unwrap_or(default);
        let mut devices = Vec::new();
        for entry in std::fs::read_dir(base)? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains(':') {
                continue;
            }
            let dir = entry.path();
            devices.push(SnapshotDevice {
                port_path: name.into_owned(),
                vendor_id: read_id(&dir.join("idVendor")),
                product_id: read_id(&dir.join("idProduct")),
            });
        }
        devices.sort_by(|a, b| a.port_path.cmp(&b.port_path));
        let captured_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(Snapshot {
            captured_unix,
            devices,
        })
    }

    /// Does not create `path`'s parent directory -- it errors like a plain
    /// `fs::write` would if that directory does not exist yet. The caller
    /// owns directory creation (e.g. the CLI handler calls
    /// `ensure_private_config_dir` first), the same division `--update-usbids
    /// pull` uses for its own destination.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(self).context("could not serialize the snapshot")?;
        std::fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
    }

    /// `None` when the file is absent. A file that exists but does not
    /// parse warns once and reads as no snapshot, so a corrupt file
    /// degrades to today's unmarked display instead of failing startup.
    pub fn load(path: &Path) -> Option<Snapshot> {
        let text = std::fs::read_to_string(path).ok()?;
        match toml::from_str(&text) {
            Ok(snapshot) => Some(snapshot),
            Err(e) => {
                log::warn!("could not parse {}: {e}", path.display());
                None
            }
        }
    }

    /// Internal means: same physical port AND the same device on it.
    /// An ID the snapshot lacks matches only a device that also lacks it.
    pub fn is_internal(
        &self,
        port_path: &str,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    ) -> bool {
        self.devices.iter().any(|d| {
            d.port_path == port_path
                && parse_id(&d.vendor_id) == vendor_id
                && parse_id(&d.product_id) == product_id
                && (d.vendor_id.is_some() == vendor_id.is_some())
                && (d.product_id.is_some() == product_id.is_some())
        })
    }
}

fn read_id(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    Some(raw.trim().to_lowercase())
}

/// A stored hex ID as a comparable number; an unparsable entry is `None`,
/// which the `is_some` guards in `is_internal` keep from matching a real
/// absent ID.
fn parse_id(id: &Option<String>) -> Option<u16> {
    u16::from_str_radix(id.as_deref()?, 16).ok()
}

/// The snapshot's fixed home, sibling of the preferences file.
/// `--config` moves preferences only, never this.
pub fn snapshot_path() -> Result<PathBuf> {
    Ok(crate::config::preferences_path()?.with_file_name("internal-devices.toml"))
}

/// One captured device's vendor+product name, resolved against `db` the same
/// way `headless::render_text` composes a live device's name: both names
/// join with a space, a single resolved field stands alone. Unlike that
/// display, an unresolved field here contributes nothing rather than
/// "Unknown" -- a snapshot line with no match is unresolved, not unnamed, so
/// an empty string means the caller appends nothing. Empty whenever `db` is
/// `None` or the stored hex IDs don't parse or aren't in the database.
pub fn describe(device: &SnapshotDevice, db: Option<&UsbIds>) -> String {
    let Some(db) = db else {
        return String::new();
    };
    let vid = parse_id(&device.vendor_id);
    let pid = parse_id(&device.product_id);
    let vendor = vid.and_then(|v| db.vendor_name(v));
    let product = match (vid, pid) {
        (Some(v), Some(p)) => db.product_name(v, p),
        _ => None,
    };
    match (vendor, product) {
        (Some(v), Some(p)) => format!("{v} {p}"),
        (Some(v), None) => v.to_string(),
        (None, Some(p)) => p.to_string(),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sysfs_device(base: &std::path::Path, name: &str, vid: Option<&str>, pid: Option<&str>) {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(v) = vid {
            std::fs::write(dir.join("idVendor"), format!("{v}\n")).unwrap();
        }
        if let Some(p) = pid {
            std::fs::write(dir.join("idProduct"), format!("{p}\n")).unwrap();
        }
    }

    #[test]
    fn capture_records_port_paths_and_ids_and_skips_interfaces() {
        let temp = tempfile::tempdir().unwrap();
        sysfs_device(temp.path(), "usb1", Some("1d6b"), Some("0002"));
        sysfs_device(temp.path(), "1-4", Some("04f2"), Some("b71a"));
        sysfs_device(temp.path(), "3-3.1", None, None);
        std::fs::create_dir_all(temp.path().join("1-4:1.0")).unwrap();

        let snap = Snapshot::capture(Some(temp.path())).unwrap();
        assert_eq!(
            snap.devices.len(),
            3,
            "3 devices, the interface dir skipped"
        );
        let paths: Vec<&str> = snap.devices.iter().map(|d| d.port_path.as_str()).collect();
        assert_eq!(paths, ["1-4", "3-3.1", "usb1"], "sorted by port path");
        let cam = snap.devices.iter().find(|d| d.port_path == "1-4").unwrap();
        assert_eq!(cam.vendor_id.as_deref(), Some("04f2"));
        assert_eq!(cam.product_id.as_deref(), Some("b71a"));
        let bare = snap
            .devices
            .iter()
            .find(|d| d.port_path == "3-3.1")
            .unwrap();
        assert_eq!(bare.vendor_id, None);
    }

    #[test]
    fn is_internal_requires_port_and_both_ids() {
        let temp = tempfile::tempdir().unwrap();
        sysfs_device(temp.path(), "1-4", Some("04f2"), Some("b71a"));
        sysfs_device(temp.path(), "3-3.1", None, None);
        let snap = Snapshot::capture(Some(temp.path())).unwrap();

        assert!(snap.is_internal("1-4", Some(0x04f2), Some(0xb71a)));
        assert!(
            !snap.is_internal("1-4", Some(0x0fd9), Some(0xb71a)),
            "other device on the port"
        );
        assert!(
            !snap.is_internal("1-5", Some(0x04f2), Some(0xb71a)),
            "other port"
        );
        assert!(
            snap.is_internal("3-3.1", None, None),
            "missing IDs match missing IDs"
        );
        assert!(!snap.is_internal("3-3.1", Some(0x04f2), None));
    }

    #[test]
    fn write_and_load_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("sysfs");
        sysfs_device(&base, "1-4", Some("04f2"), Some("b71a"));
        let snap = Snapshot::capture(Some(&base)).unwrap();
        let file = temp.path().join("internal-devices.toml");
        snap.write_to(&file).unwrap();

        let loaded = Snapshot::load(&file).expect("file exists and parses");
        assert_eq!(loaded.devices.len(), 1);
        assert!(loaded.is_internal("1-4", Some(0x04f2), Some(0xb71a)));
        assert_eq!(loaded.captured_unix, snap.captured_unix);
    }

    #[test]
    fn write_and_load_round_trip_with_missing_ids() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("sysfs");
        sysfs_device(&base, "3-3.1", None, None);
        let snap = Snapshot::capture(Some(&base)).unwrap();
        let file = temp.path().join("internal-devices.toml");
        snap.write_to(&file).unwrap();

        let loaded = Snapshot::load(&file).expect("file exists and parses");
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices[0].vendor_id, None);
        assert_eq!(loaded.devices[0].product_id, None);
        assert!(
            loaded.is_internal("3-3.1", None, None),
            "a device with no IDs at capture time must still match after a real write_to/load round trip"
        );
    }

    #[test]
    fn write_to_errors_when_the_parent_directory_does_not_exist() {
        // `write_to` does not create directories -- callers own that (see
        // its doc comment). This pins today's plain `fs::write` failure
        // mode so a caller-side fix (like the CLI handler's
        // `ensure_private_config_dir` call) never quietly stops mattering.
        let temp = tempfile::tempdir().unwrap();
        let snap = Snapshot {
            captured_unix: 0,
            devices: vec![],
        };
        let dest = temp
            .path()
            .join("does-not-exist")
            .join("internal-devices.toml");
        assert!(snap.write_to(&dest).is_err());
    }

    #[test]
    fn load_is_none_for_missing_and_garbage_files() {
        let temp = tempfile::tempdir().unwrap();
        assert!(Snapshot::load(&temp.path().join("absent.toml")).is_none());
        let garbage = temp.path().join("garbage.toml");
        std::fs::write(&garbage, "not [ valid toml").unwrap();
        assert!(Snapshot::load(&garbage).is_none());
    }

    #[test]
    fn garbage_stored_ids_match_nothing() {
        let snap = Snapshot {
            captured_unix: 0,
            devices: vec![SnapshotDevice {
                port_path: "1-4".into(),
                vendor_id: Some("zzzz".into()),
                product_id: Some("b71a".into()),
            }],
        };
        assert!(!snap.is_internal("1-4", None, Some(0xb71a)));
        assert!(!snap.is_internal("1-4", Some(0x04f2), Some(0xb71a)));
    }

    fn device(vid: Option<&str>, pid: Option<&str>) -> SnapshotDevice {
        SnapshotDevice {
            port_path: "1-4".into(),
            vendor_id: vid.map(String::from),
            product_id: pid.map(String::from),
        }
    }

    #[test]
    fn describe_joins_vendor_and_product_when_both_resolve() {
        let db = UsbIds::parse("04f2  Chicony Electronics Co., Ltd\n\tb71a  Integrated Camera\n");
        let d = device(Some("04f2"), Some("b71a"));
        assert_eq!(
            describe(&d, Some(&db)),
            "Chicony Electronics Co., Ltd Integrated Camera"
        );
    }

    #[test]
    fn describe_falls_back_to_the_vendor_alone_when_the_product_is_unknown() {
        let db = UsbIds::parse("04f2  Chicony Electronics Co., Ltd\n");
        let d = device(Some("04f2"), Some("b71a"));
        assert_eq!(describe(&d, Some(&db)), "Chicony Electronics Co., Ltd");
    }

    #[test]
    fn describe_is_empty_when_the_vendor_is_unknown() {
        let db = UsbIds::parse("04f2  Chicony Electronics Co., Ltd\n\tb71a  Integrated Camera\n");
        let d = device(Some("0fd9"), Some("b71a"));
        assert_eq!(
            describe(&d, Some(&db)),
            "",
            "unknown vendor resolves nothing, product ignored without it"
        );
    }

    #[test]
    fn describe_is_empty_with_no_database() {
        let d = device(Some("04f2"), Some("b71a"));
        assert_eq!(describe(&d, None), "");
    }

    #[test]
    fn describe_is_empty_when_ids_are_missing_or_unparsable() {
        let db = UsbIds::parse("04f2  Chicony Electronics Co., Ltd\n\tb71a  Integrated Camera\n");
        assert_eq!(describe(&device(None, None), Some(&db)), "");
        assert_eq!(describe(&device(Some("zzzz"), Some("b71a")), Some(&db)), "");
    }
}

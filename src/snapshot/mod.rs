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
    ///
    /// Writes via [`crate::config::replace_file_owned`]: the file is
    /// replaced atomically through a same-directory temporary file, so
    /// `--forget-internal`, `--snapshot-internal`, and the TUI's `S` key can
    /// never leave a truncated snapshot behind -- an interrupted or failing
    /// write leaves the previous file exactly as it was. Still chowned to
    /// the invoking user under sudo (fd-based -- see that function's doc
    /// comment), so callers need no separate chown call; still refuses a
    /// symlink at `path`.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(self).context("could not serialize the snapshot")?;
        crate::config::replace_file_owned(path, text.as_bytes())
            .with_context(|| format!("could not write {}", path.display()))
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

    /// Remove every entry whose `port_path` is in `port_paths`, returning
    /// the removed entries in file order. All or nothing: when any path
    /// matches no entry, nothing is removed and every unmatched path comes
    /// back as the error, so a typo cannot silently forget nothing.
    pub fn forget(&mut self, port_paths: &[String]) -> Result<Vec<SnapshotDevice>, Vec<String>> {
        let unmatched: Vec<String> = port_paths
            .iter()
            .filter(|path| !self.devices.iter().any(|d| &d.port_path == *path))
            .cloned()
            .collect();
        if !unmatched.is_empty() {
            return Err(unmatched);
        }
        let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.devices)
            .into_iter()
            .partition(|d| port_paths.contains(&d.port_path));
        self.devices = kept;
        Ok(removed)
    }
}

/// Printed after a snapshot is recorded, by `--snapshot-internal` and by the
/// TUI's `S` result overlay, from this one constant so the two never drift.
/// On a desktop the keyboard, the mouse, or their receiver stay plugged in
/// to take the snapshot at all, and are recorded as internal with the rest.
pub const REMOVABLE_HINT: &str = "Anything that stayed plugged in to type with (a keyboard, mouse, or RF receiver) is recorded too. Drop it with: usbtop-ng --forget-internal <port_path>";

/// The two lines `--snapshot-internal` prints after writing `dest`.
pub fn recorded_message(count: usize, dest: &Path) -> String {
    format!(
        "{count} devices recorded as internal in {}\n{REMOVABLE_HINT}",
        dest.display()
    )
}

/// One `--forget-internal` output line: the entry in the same
/// `port_path  vid:pid  name` shape the snapshot listing prints, with the
/// name resolved through `db` by [`describe`] (nothing appended when it
/// resolves to nothing).
pub fn forgot_line(device: &SnapshotDevice, db: Option<&UsbIds>) -> String {
    let name = describe(device, db);
    let suffix = if name.is_empty() {
        String::new()
    } else {
        format!("  {name}")
    };
    format!(
        "forgot {}  {}:{}{suffix}",
        device.port_path,
        device.vendor_id.as_deref().unwrap_or("----"),
        device.product_id.as_deref().unwrap_or("----"),
    )
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

/// An exclusive advisory lock on the snapshot, held for the life of the
/// value: `--forget-internal` takes it across its load, edit, and write, and
/// `--snapshot-internal` and the TUI's `S` key across their write, so an
/// edit can never be written over a snapshot another run replaced in the
/// meantime. The last complete operation wins; an interleaving cannot. The
/// lock is `flock(2)` on `internal-devices.toml.lock` beside the snapshot
/// (per open file description, released on drop or process exit); the
/// file is created 0600 and handed to the sudo invoker like every other
/// file in the directory, and a symlink planted at its path is refused.
#[derive(Debug)]
pub struct SnapshotLock {
    _file: std::fs::File,
}

impl SnapshotLock {
    /// Block until the lock is held.
    pub fn acquire(snapshot_path: &Path) -> std::io::Result<SnapshotLock> {
        Self::open(snapshot_path, 0)
    }

    /// The lock, or `None` when another holder has it right now.
    #[cfg(test)]
    fn try_acquire(snapshot_path: &Path) -> std::io::Result<Option<SnapshotLock>> {
        match Self::open(snapshot_path, libc::LOCK_NB) {
            Ok(lock) => Ok(Some(lock)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn open(snapshot_path: &Path, extra_flock_flags: libc::c_int) -> std::io::Result<SnapshotLock> {
        use std::os::fd::AsRawFd;
        let path = lock_path_for(snapshot_path);
        let name = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "snapshot path has no name",
            )
        })?;
        // Created relative to the pinned directory, so the same containment
        // rule as the snapshot itself applies (see `config::PinnedDir`).
        let dir = crate::config::PinnedDir::for_file(&path)?;
        let file = dir.open_or_create(name, 0o600)?;
        dir.chown_created(file.as_raw_fd(), &path);
        // SAFETY: `file` owns a valid open descriptor for the whole call;
        // `flock` takes the descriptor and an operation and touches no memory.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | extra_flock_flags) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(SnapshotLock { _file: file })
    }
}

/// `<snapshot>.lock`, beside the snapshot.
fn lock_path_for(snapshot_path: &Path) -> PathBuf {
    let mut name = snapshot_path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".lock");
    snapshot_path.with_file_name(name)
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

    fn three_entry_snapshot() -> Snapshot {
        Snapshot {
            captured_unix: 42,
            devices: vec![
                device_at("1-4", Some("04f2"), Some("b71a")),
                device_at("3-3.1", Some("046d"), Some("c52b")),
                device_at("usb1", Some("1d6b"), Some("0002")),
            ],
        }
    }

    fn device_at(port_path: &str, vid: Option<&str>, pid: Option<&str>) -> SnapshotDevice {
        SnapshotDevice {
            port_path: port_path.into(),
            vendor_id: vid.map(String::from),
            product_id: pid.map(String::from),
        }
    }

    #[test]
    fn the_snapshot_lock_is_exclusive_until_dropped_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let snapshot = temp.path().join("internal-devices.toml");
        let held = SnapshotLock::acquire(&snapshot).unwrap();
        let lock_file = temp.path().join("internal-devices.toml.lock");
        assert_eq!(
            std::fs::metadata(&lock_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            SnapshotLock::try_acquire(&snapshot).unwrap().is_none(),
            "a second holder must wait while the first holds the lock"
        );
        drop(held);
        assert!(
            SnapshotLock::try_acquire(&snapshot).unwrap().is_some(),
            "released on drop"
        );
    }

    #[test]
    fn the_snapshot_lock_refuses_a_symlink_at_its_path() {
        let temp = tempfile::tempdir().unwrap();
        let decoy = temp.path().join("decoy");
        std::fs::write(&decoy, b"x").unwrap();
        std::os::unix::fs::symlink(&decoy, temp.path().join("internal-devices.toml.lock")).unwrap();
        let err = SnapshotLock::acquire(&temp.path().join("internal-devices.toml")).unwrap_err();
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
        assert_eq!(
            std::fs::read(&decoy).unwrap(),
            b"x",
            "the decoy is untouched"
        );
    }

    #[test]
    fn forget_removes_exactly_the_named_entries_in_file_order() {
        let mut snap = three_entry_snapshot();
        let removed = snap
            .forget(&["usb1".to_string(), "3-3.1".to_string()])
            .unwrap();
        assert_eq!(
            removed
                .iter()
                .map(|d| d.port_path.as_str())
                .collect::<Vec<_>>(),
            vec!["3-3.1", "usb1"],
            "file order, not argument order"
        );
        assert_eq!(removed[0].vendor_id.as_deref(), Some("046d"));
        assert_eq!(
            snap.devices
                .iter()
                .map(|d| d.port_path.as_str())
                .collect::<Vec<_>>(),
            vec!["1-4"]
        );
        assert_eq!(
            snap.captured_unix, 42,
            "the capture time is not an edit time"
        );
    }

    #[test]
    fn forget_is_all_or_nothing_and_names_every_unmatched_path() {
        let mut snap = three_entry_snapshot();
        let err = snap
            .forget(&["1-4".to_string(), "9-9".to_string(), "8-8".to_string()])
            .unwrap_err();
        assert_eq!(err, vec!["9-9".to_string(), "8-8".to_string()]);
        assert_eq!(
            snap.devices.len(),
            3,
            "nothing removed when any path is unmatched"
        );
    }

    #[test]
    fn forget_round_trips_through_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("internal-devices.toml");
        three_entry_snapshot().write_to(&file).unwrap();
        let mut snap = Snapshot::load(&file).unwrap();
        snap.forget(&["3-3.1".to_string()]).unwrap();
        snap.write_to(&file).unwrap();
        let again = Snapshot::load(&file).unwrap();
        assert!(!again.is_internal("3-3.1", Some(0x046d), Some(0xc52b)));
        assert!(again.is_internal("1-4", Some(0x04f2), Some(0xb71a)));
    }

    #[test]
    fn a_failed_forget_rewrite_leaves_the_snapshot_byte_for_byte_intact() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("internal-devices.toml");
        three_entry_snapshot().write_to(&file).unwrap();
        let before = std::fs::read(&file).unwrap();
        // Make the atomic rewrite fail: a directory squats on the temp name
        // this process will use (`<file_name>.<pid>.tmp`).
        std::fs::create_dir(
            temp.path()
                .join(format!("internal-devices.toml.{}.tmp", std::process::id())),
        )
        .unwrap();

        let mut snap = Snapshot::load(&file).unwrap();
        snap.forget(&["3-3.1".to_string()]).unwrap();
        assert!(snap.write_to(&file).is_err(), "the rewrite must fail here");

        assert_eq!(
            std::fs::read(&file).unwrap(),
            before,
            "nothing was truncated or partially written"
        );
        let reloaded = Snapshot::load(&file).unwrap();
        assert_eq!(
            reloaded.devices.len(),
            3,
            "every entry, including 3-3.1, is still there"
        );
    }

    #[test]
    fn recorded_message_names_the_file_and_carries_the_hint() {
        let text = recorded_message(7, Path::new("/home/u/.usbtop-ng/internal-devices.toml"));
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("7 devices recorded as internal in /home/u/.usbtop-ng/internal-devices.toml")
        );
        assert_eq!(lines.next(), Some(REMOVABLE_HINT));
        assert_eq!(lines.next(), None);
        assert!(REMOVABLE_HINT.contains("--forget-internal <port_path>"));
        assert!(REMOVABLE_HINT.contains("keyboard, mouse, or RF receiver"));
    }

    #[test]
    fn forgot_line_shows_port_ids_and_the_resolved_name_when_known() {
        let db = UsbIds::parse("046d  Logitech, Inc.\n\tc52b  Unifying Receiver\n");
        let d = device_at("3-3.1", Some("046d"), Some("c52b"));
        assert_eq!(
            forgot_line(&d, Some(&db)),
            "forgot 3-3.1  046d:c52b  Logitech, Inc. Unifying Receiver"
        );
        assert_eq!(forgot_line(&d, None), "forgot 3-3.1  046d:c52b");
        let bare = device_at("2-1", None, None);
        assert_eq!(forgot_line(&bare, Some(&db)), "forgot 2-1  ----:----");
    }
}

//! The bundle on disk: `usbtop-ng-support-<UTC stamp>/`, every file written
//! through the redactor and recorded with its size, the manifest that lists
//! them all with the redaction counts and unavailable notes, and the `tar`
//! archive beside the directory. UTC comes from `SystemTime` plus the
//! civil-from-days conversion below, so no date crate is needed.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::redact::Redactor;
use super::{note, Note};
use crate::capture::{assert_bundle_payload_free, assert_sysfs_contained};
use crate::config::chown_created_to_invoker;

/// The manifest's `format_version`; bump when a file's layout changes.
pub const FORMAT_VERSION: u32 = 1;

/// `(year, month, day, hour, minute, second)` in UTC for a Unix time.
fn utc_parts(unix_secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (unix_secs / 86_400) as i64;
    let secs = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a proleptic
/// Gregorian date, exact for every day the `u64` above can name.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `YYYYMMDDTHHMMSSZ`, the bundle directory's suffix.
pub fn utc_stamp(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(unix_secs);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the manifest's `created_utc`.
pub fn utc_iso(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(unix_secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub symlink: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub created_unix: u64,
    pub created_utc: String,
    pub usbtop_ng: String,
    /// Each redaction rule that fired and how often.
    pub redaction: BTreeMap<String, usize>,
    pub unavailable: Vec<Note>,
    /// Every file in the bundle except this manifest, with its size.
    pub files: Vec<FileEntry>,
}

/// Writes files into the bundle directory, redacting text on the way and
/// recording every file for the manifest.
pub struct BundleWriter {
    root: PathBuf,
    redactor: Redactor,
    files: Vec<FileEntry>,
}

/// Create or truncate `path` (parents included), refusing a symlinked
/// final component, write `bytes`, and hand the file to the sudo invoker
/// when there is one. Returns the byte count.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<u64> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    chown_created_to_invoker(path, file.as_raw_fd());
    Ok(bytes.len() as u64)
}

impl BundleWriter {
    pub fn create(root: &Path, redactor: Redactor) -> io::Result<BundleWriter> {
        std::fs::create_dir_all(root)?;
        Ok(BundleWriter {
            root: root.to_path_buf(),
            redactor,
            files: Vec::new(),
        })
    }

    pub fn redactor(&mut self) -> &mut Redactor {
        &mut self.redactor
    }

    /// Every file recorded so far. The manifest never lists itself.
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    fn record(&mut self, rel: &str, bytes: u64, symlink: bool) {
        self.files.push(FileEntry {
            path: rel.to_string(),
            bytes,
            symlink,
        });
    }

    /// Write text through the redactor.
    pub fn write_text(&mut self, rel: &str, text: &str) -> io::Result<()> {
        let redacted = self.redactor.text(text);
        let bytes = write_new(&self.root.join(rel), redacted.as_bytes())?;
        self.record(rel, bytes, false);
        Ok(())
    }

    /// Write bytes verbatim (descriptor blobs are device identity, never
    /// redacted).
    pub fn write_bytes(&mut self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let n = write_new(&self.root.join(rel), bytes)?;
        self.record(rel, n, false);
        Ok(())
    }

    /// Serialize as TOML, then write through the redactor.
    pub fn write_toml<T: Serialize>(&mut self, rel: &str, value: &T) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(value)?;
        self.write_text(rel, &text)?;
        Ok(())
    }

    /// Rewrite an existing text file (one written by another component,
    /// such as the report sink) through the redactor, then record it.
    pub fn redact_file(&mut self, rel: &str) -> io::Result<()> {
        let path = self.root.join(rel);
        let text = std::fs::read_to_string(&path)?;
        let redacted = self.redactor.text(&text);
        let bytes = write_new(&path, redacted.as_bytes())?;
        self.record(rel, bytes, false);
        Ok(())
    }

    /// Record an existing file as it is (the log the tee already redacted).
    pub fn adopt_file(&mut self, rel: &str) -> io::Result<()> {
        let bytes = std::fs::metadata(self.root.join(rel))?.len();
        self.record(rel, bytes, false);
        Ok(())
    }

    /// Record every regular file and symlink under `rel` (the fixture
    /// subtree the capturer wrote). Symlinks are recorded with zero bytes
    /// and never followed; directories are not entries.
    pub fn record_dir(&mut self, rel: &str) -> io::Result<()> {
        let base = self.root.join(rel);
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path)?;
                let rel_path = path
                    .strip_prefix(&self.root)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| path.display().to_string());
                if meta.file_type().is_symlink() {
                    self.record(&rel_path, 0, true);
                } else if meta.is_dir() {
                    stack.push(path);
                } else {
                    self.record(&rel_path, meta.len(), false);
                }
            }
        }
        Ok(())
    }

    /// Write `manifest.toml`: format version, creation time, the redaction
    /// summary, every unavailable note, and the file list (sorted by path;
    /// the manifest never lists itself).
    pub fn write_manifest(
        &mut self,
        created_unix: u64,
        unavailable: &[Note],
    ) -> anyhow::Result<()> {
        let mut files = self.files.clone();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            created_unix,
            created_utc: utc_iso(created_unix),
            usbtop_ng: env!("CARGO_PKG_VERSION").to_string(),
            redaction: self.redactor.summary().into_iter().collect(),
            unavailable: unavailable.to_vec(),
            files,
        };
        let text = toml::to_string_pretty(&manifest)?;
        write_new(&self.root.join("manifest.toml"), text.as_bytes())?;
        Ok(())
    }

    /// `tar -czf <archive> -C <parent> <dirname>`; `Ok` carries the
    /// archive's size. A missing `tar` (or a failing one) is a note, and the
    /// directory stays for the user to archive by hand.
    pub fn archive(&self, archive: &Path) -> Result<u64, Note> {
        self.archive_with(archive, "tar")
    }

    pub fn archive_with(&self, archive: &Path, program: &str) -> Result<u64, Note> {
        let parent = self.root.parent().unwrap_or(Path::new("."));
        let dirname = self
            .root
            .file_name()
            .ok_or_else(|| note("archive", "could not determine the bundle directory name"))?;
        let output = Command::new(program)
            .arg("-czf")
            .arg(archive)
            .arg("-C")
            .arg(parent)
            .arg(dirname)
            .output()
            .map_err(|e| note("archive", format!("could not run {program}: {e}")))?;
        if !output.status.success() {
            return Err(note(
                "archive",
                format!(
                    "could not run {program}: exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        let bytes = std::fs::metadata(archive)
            .map_err(|e| {
                note(
                    "archive",
                    format!("could not read {} after tar: {e}", archive.display()),
                )
            })?
            .len();
        if let Ok(file) = File::open(archive) {
            chown_created_to_invoker(archive, file.as_raw_fd());
        }
        Ok(bytes)
    }
}

/// Re-assert the capturer's own invariants over the embedded fixture:
/// SEC-1 (no payload in either trace) and SEC-2 (nothing under `sysfs/`
/// escapes it). A violation is a bug in the capturer, so it fails the run
/// rather than shipping the bundle.
pub fn assert_fixture_invariants(fixture_dir: &Path) -> anyhow::Result<()> {
    assert_bundle_payload_free(fixture_dir)?;
    let sysfs = fixture_dir.join("sysfs");
    if sysfs.is_dir() {
        assert_sysfs_contained(&sysfs)?;
    }
    Ok(())
}

/// Under `sudo`, hand every directory and file under `root` to the invoking
/// user, so a bundle written into their directory is theirs to delete.
/// Best-effort and fd-based (see `config::chown_created_to_invoker`): a
/// no-op when not under sudo or when `root` is outside the invoker's home;
/// symlinks are left alone (removing one needs only the directory).
pub fn own_tree(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(handle) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&dir)
        {
            chown_created_to_invoker(&dir, handle.as_raw_fd());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if let Ok(file) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
            {
                chown_created_to_invoker(&path, file.as_raw_fd());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_available() -> bool {
        Command::new("tar").arg("--version").output().is_ok()
    }

    #[test]
    fn utc_stamp_pins_the_epoch_a_recent_date_two_leap_days_and_a_non_leap_century() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_788_000_000), "20260829T104000Z");
        assert_eq!(utc_stamp(1_709_164_800), "20240229T000000Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
        assert_eq!(utc_stamp(4_102_444_800), "21000101T000000Z");
        assert_eq!(utc_iso(1_788_000_000), "2026-08-29T10:40:00Z");
    }

    #[test]
    fn write_text_redacts_records_and_creates_parents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w =
            BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        w.write_text(
            "config/preferences.toml",
            "usbids_path = \"/home/alice/usb.ids\"\n",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("config/preferences.toml")).unwrap(),
            "usbids_path = \"~/usb.ids\"\n"
        );
        assert_eq!(
            w.files(),
            &[FileEntry {
                path: "config/preferences.toml".into(),
                bytes: 26,
                symlink: false
            }]
        );
        assert_eq!(w.redactor().summary(), vec![("home_path".to_string(), 1)]);
    }

    #[test]
    fn write_bytes_is_verbatim_and_write_toml_serializes_then_redacts() {
        #[derive(Serialize)]
        struct Doc {
            dir: String,
            n: u32,
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w =
            BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        w.write_bytes("inventory/descriptors/1-4.bin", &[0x12, 0x01, 0x00, 0x02])
            .unwrap();
        assert_eq!(
            std::fs::read(root.join("inventory/descriptors/1-4.bin")).unwrap(),
            vec![0x12, 0x01, 0x00, 0x02]
        );
        w.write_toml(
            "config/config.toml",
            &Doc {
                dir: "/home/alice/.usbtop-ng".into(),
                n: 3,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(root.join("config/config.toml")).unwrap();
        assert!(text.contains("dir = \"~/.usbtop-ng\""), "{text}");
        assert!(text.contains("n = 3"), "{text}");
        assert_eq!(w.files().len(), 2);
    }

    #[test]
    fn redact_file_rewrites_in_place_and_adopt_file_records_as_is() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w =
            BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        std::fs::write(
            root.join("report.json"),
            "{\"command\":[\"/home/alice/bin/usbtop-ng\"]}\n",
        )
        .unwrap();
        w.redact_file("report.json").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("report.json")).unwrap(),
            "{\"command\":[\"~/bin/usbtop-ng\"]}\n"
        );
        std::fs::write(root.join("usbtop-ng.log"), "[INFO] starting\n").unwrap();
        w.adopt_file("usbtop-ng.log").unwrap();
        assert_eq!(
            w.files()[1],
            FileEntry {
                path: "usbtop-ng.log".into(),
                bytes: 16,
                symlink: false
            }
        );
        assert!(w.adopt_file("missing").is_err());
    }

    #[test]
    fn record_dir_lists_every_file_and_symlink_under_a_subtree() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w = BundleWriter::create(&root, Redactor::new(None)).unwrap();
        let fixture = root.join("fixture");
        std::fs::create_dir_all(fixture.join("sysfs/0000:00:14.0/usb1")).unwrap();
        std::fs::write(fixture.join("sysfs/0000:00:14.0/usb1/busnum"), "1\n").unwrap();
        std::os::unix::fs::symlink("0000:00:14.0/usb1", fixture.join("sysfs/usb1")).unwrap();
        std::fs::write(fixture.join("meta.toml"), "sources = []\n").unwrap();
        w.record_dir("fixture").unwrap();
        let mut paths: Vec<(String, u64, bool)> = w
            .files()
            .iter()
            .map(|f| (f.path.clone(), f.bytes, f.symlink))
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                ("fixture/meta.toml".to_string(), 13, false),
                (
                    "fixture/sysfs/0000:00:14.0/usb1/busnum".to_string(),
                    2,
                    false
                ),
                ("fixture/sysfs/usb1".to_string(), 0, true),
            ]
        );
    }

    #[test]
    fn manifest_lists_files_redaction_and_notes_and_parses_back() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        let mut w =
            BundleWriter::create(&root, Redactor::new(Some(Path::new("/home/alice")))).unwrap();
        w.write_text("build.toml", "command = [\"/home/alice/x\"]\n")
            .unwrap();
        w.write_manifest(1_788_000_000, &[note("dmesg", "permission denied")])
            .unwrap();
        let text = std::fs::read_to_string(root.join("manifest.toml")).unwrap();
        let manifest: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest.format_version, FORMAT_VERSION);
        assert_eq!(manifest.created_unix, 1_788_000_000);
        assert_eq!(manifest.created_utc, "2026-08-29T10:40:00Z");
        assert_eq!(manifest.usbtop_ng, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.redaction.get("home_path"), Some(&1));
        assert_eq!(
            manifest.unavailable,
            vec![note("dmesg", "permission denied")]
        );
        assert_eq!(manifest.files.len(), 1, "the manifest never lists itself");
        assert_eq!(manifest.files[0].path, "build.toml");
        assert_eq!(
            manifest.files[0].bytes,
            std::fs::metadata(root.join("build.toml")).unwrap().len()
        );
    }

    #[test]
    fn archive_with_a_missing_program_is_a_note_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("usbtop-ng-support-x");
        let w = BundleWriter::create(&root, Redactor::new(None)).unwrap();
        let err = w
            .archive_with(&temp.path().join("x.tar.gz"), "no-such-tar-program")
            .unwrap_err();
        assert_eq!(err.item, "archive");
        assert!(err.reason.contains("no-such-tar-program"), "{}", err.reason);
        assert!(err.reason.starts_with("could not run "), "{}", err.reason);
        assert!(!temp.path().join("x.tar.gz").exists());
    }

    #[test]
    fn archive_with_a_failing_program_is_a_note() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("usbtop-ng-support-x");
        let w = BundleWriter::create(&root, Redactor::new(None)).unwrap();
        let archive = temp.path().join("x.tar.gz");
        let err = w.archive_with(&archive, "false").unwrap_err();
        assert_eq!(err.item, "archive");
        assert!(
            err.reason.starts_with("could not run false: exited with "),
            "{}",
            err.reason
        );
    }

    #[test]
    fn archive_with_tar_lists_exactly_the_manifest_files_plus_the_manifest() {
        if !tar_available() {
            eprintln!("skipping: tar is not installed");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("usbtop-ng-support-20260829T104000Z");
        let mut w = BundleWriter::create(&root, Redactor::new(None)).unwrap();
        w.write_text("SUMMARY.txt", "usbtop-ng support bundle\n")
            .unwrap();
        w.write_bytes("inventory/descriptors/1-4.bin", &[1, 2, 3])
            .unwrap();
        w.write_manifest(1_788_000_000, &[]).unwrap();
        let archive = temp
            .path()
            .join("usbtop-ng-support-20260829T104000Z.tar.gz");
        let bytes = w.archive(&archive).unwrap();
        assert_eq!(bytes, std::fs::metadata(&archive).unwrap().len());
        let listing = Command::new("tar")
            .args(["tzf"])
            .arg(&archive)
            .output()
            .unwrap();
        let mut listed: Vec<String> = String::from_utf8(listing.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.ends_with('/'))
            .map(|l| {
                l.trim_start_matches("usbtop-ng-support-20260829T104000Z/")
                    .to_string()
            })
            .collect();
        listed.sort();
        let mut expected: Vec<String> = w.files().iter().map(|f| f.path.clone()).collect();
        expected.push("manifest.toml".to_string());
        expected.sort();
        assert_eq!(listed, expected);
    }

    #[test]
    fn fixture_invariants_reject_payload_and_an_escaping_link() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = temp.path().join("fixture");
        std::fs::create_dir_all(fixture.join("sysfs/1-1")).unwrap();
        std::fs::write(fixture.join("sysfs/1-1/busnum"), "1\n").unwrap();
        assert!(assert_fixture_invariants(&fixture).is_ok());

        let mut bad = vec![0u8; 48];
        bad[36..40].copy_from_slice(&48u32.to_ne_bytes());
        bad.extend_from_slice(&[0xAB; 48]);
        std::fs::write(fixture.join("trace.bin"), &bad).unwrap();
        let err = assert_fixture_invariants(&fixture).unwrap_err();
        assert!(err.to_string().contains("SEC-1"), "{err}");
        std::fs::remove_file(fixture.join("trace.bin")).unwrap();

        std::os::unix::fs::symlink(temp.path(), fixture.join("sysfs/escape")).unwrap();
        let err = assert_fixture_invariants(&fixture).unwrap_err();
        assert!(err.to_string().contains("SEC-2"), "{err}");
    }

    #[test]
    fn own_tree_is_a_silent_no_op_without_sudo() {
        // This test process is not running under sudo, so `sudo_invoker()`
        // is None and nothing is chowned; the walk itself must survive a
        // symlink and a nested directory without error.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        std::fs::create_dir_all(root.join("fixture/sysfs")).unwrap();
        std::fs::write(root.join("fixture/sysfs/busnum"), "1\n").unwrap();
        std::os::unix::fs::symlink("sysfs", root.join("fixture/link")).unwrap();
        own_tree(&root);
        assert!(root.join("fixture/sysfs/busnum").exists());
    }
}

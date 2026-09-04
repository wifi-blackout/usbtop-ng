//! The bundle on disk: `usbtop-ng-support-<UTC stamp>/`, every file written
//! through the redactor and recorded with its size, the manifest that lists
//! them all with the redaction counts and unavailable notes, and the `tar`
//! archive beside the directory. UTC comes from `SystemTime` plus the
//! civil-from-days conversion below, so no date crate is needed.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
/// recording every file for the manifest. Every create/truncate is done
/// relative to `root_fd` (see [`create_file_at`]), never by re-resolving a
/// path, so a symlink swapped into the bundle tree during the capture window
/// cannot redirect a root-owned write.
pub struct BundleWriter {
    /// The bundle root as a *logical* path -- consulted only for the
    /// ownership containment check (which is itself fd-based, see
    /// [`chown_created_to_invoker`]) and for naming the archive. Writes never
    /// resolve through it.
    root: PathBuf,
    /// A descriptor pinning the real bundle-root inode. Opened once (in
    /// `prepare_dir`) with `O_DIRECTORY|O_NOFOLLOW` and dup'd here; every
    /// write resolves component-by-component relative to it.
    root_fd: OwnedFd,
    redactor: Redactor,
    files: Vec<FileEntry>,
}

/// Turn one bundle path component into a `CString`, rejecting an interior NUL.
/// Components never legitimately carry one; this is defence in depth.
fn component_cstring(comp: &str) -> io::Result<CString> {
    CString::new(comp).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bundle path component {comp:?} contains an interior NUL"),
        )
    })
}

/// Split a relative bundle path (always `/`-separated in this crate) into its
/// components, refusing any empty, `.` or `..` component. A single `openat`
/// on a multi-component path would still follow a symlink at every
/// intermediate component (`O_NOFOLLOW` guards only the *final* one), so the
/// caller walks these one at a time instead.
fn split_bundle_rel(rel: &str) -> io::Result<Vec<&str>> {
    let comps: Vec<&str> = rel.split('/').collect();
    if comps
        .iter()
        .any(|c| c.is_empty() || *c == "." || *c == "..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsafe bundle path {rel:?}"),
        ));
    }
    Ok(comps)
}

/// `mkdirat(dirfd, name, 0o700)`, tolerating an already-existing entry.
fn mkdirat_tolerant(dirfd: BorrowedFd, name: &CString) -> io::Result<()> {
    // SAFETY: `dirfd` is a valid open directory descriptor for the whole
    // call; `name` is a valid NUL-terminated C string. `mkdirat` reads no
    // other memory.
    let rc = unsafe { libc::mkdirat(dirfd.as_raw_fd(), name.as_ptr(), 0o700) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EEXIST) {
        Ok(())
    } else {
        Err(err)
    }
}

/// `openat(dirfd, name, O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC)`. A symlink (or any
/// non-directory) at `name` is refused by the kernel (`ELOOP`/`ENOTDIR`),
/// which is exactly the intermediate-symlink attack we must reject.
fn open_dir_at(dirfd: BorrowedFd, name: &CString) -> io::Result<OwnedFd> {
    // SAFETY: `dirfd` is a valid open directory descriptor; `name` is a valid
    // C string. On success `openat` returns a fresh owned descriptor.
    let fd = unsafe {
        libc::openat(
            dirfd.as_raw_fd(),
            name.as_ptr(),
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh, valid descriptor this process now owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Walk `comps` beneath `root_fd`, creating and opening each as a directory
/// (`O_DIRECTORY|O_NOFOLLOW`), and return a descriptor to the deepest one. An
/// empty `comps` yields a dup of `root_fd`. Every step refuses a symlink, so
/// no intermediate component can be followed off the pinned tree.
fn walk_dirs(root_fd: BorrowedFd, comps: &[&str]) -> io::Result<OwnedFd> {
    let mut cur = root_fd.try_clone_to_owned()?;
    for comp in comps {
        let name = component_cstring(comp)?;
        mkdirat_tolerant(cur.as_fd(), &name)?;
        cur = open_dir_at(cur.as_fd(), &name)?;
    }
    Ok(cur)
}

/// Create or truncate the file named by the relative bundle path `rel`,
/// resolving every component relative to `root_fd` with `O_NOFOLLOW` so a
/// symlink swapped anywhere along the path is refused rather than followed.
/// Intermediate directories are created (mode `0o700`) as needed; the final
/// file is opened `O_WRONLY|O_CREAT|O_TRUNC|O_NOFOLLOW|O_CLOEXEC` at `0o600`.
/// Ownership fixup is the caller's job (it needs the logical path).
pub fn create_file_at(root_fd: BorrowedFd, rel: &str) -> io::Result<File> {
    let comps = split_bundle_rel(rel)?;
    let (last, dirs) = comps
        .split_last()
        .expect("split_bundle_rel rejects an empty path");
    let dir_fd = walk_dirs(root_fd, dirs)?;
    let name = component_cstring(last)?;
    // SAFETY: `dir_fd` is a valid open directory descriptor; `name` is a
    // valid C string; the mode argument matches `O_CREAT`. `openat` returns a
    // fresh owned descriptor on success.
    let fd = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh, valid descriptor this process now owns.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Create or truncate `rel` beneath `root_fd`, write `bytes`, and hand the
/// file to the sudo invoker when there is one. `logical` is the bundle-root
/// join of `rel`, used only for [`chown_created_to_invoker`]'s (fd-based)
/// containment check. Returns the byte count.
fn write_new_at(root_fd: BorrowedFd, rel: &str, logical: &Path, bytes: &[u8]) -> io::Result<u64> {
    let mut file = create_file_at(root_fd, rel)?;
    file.write_all(bytes)?;
    file.flush()?;
    chown_created_to_invoker(logical, file.as_raw_fd());
    Ok(bytes.len() as u64)
}

/// Open the freshly created bundle root as a pinned directory descriptor:
/// `O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`. `prepare_dir` calls this immediately
/// after `create_dir(dir)` so the descriptor covers the whole capture window;
/// the `O_NOFOLLOW` refuses the case where the directory was already swapped
/// for a symlink in the create->open gap.
pub fn open_bundle_root(dir: &Path) -> io::Result<OwnedFd> {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)?;
    Ok(OwnedFd::from(handle))
}

/// `fstat` a descriptor.
fn fstat_fd(fd: BorrowedFd) -> io::Result<libc::stat> {
    // SAFETY: `st` is a valid, writable `stat`; `fd` is a valid descriptor.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), &mut st) };
    if rc == 0 {
        Ok(st)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `fstatat(AT_FDCWD, path, AT_SYMLINK_NOFOLLOW)` -- stats the path without
/// following a final-component symlink, so a swapped-in link is seen as the
/// link itself rather than its target.
fn lstat_path(path: &Path) -> io::Result<libc::stat> {
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "bundle path contains an interior NUL",
        )
    })?;
    // SAFETY: `st` is a valid, writable `stat`; `c` is a valid C string.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            c.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(st)
    } else {
        Err(io::Error::last_os_error())
    }
}

impl BundleWriter {
    /// `root` is the logical bundle-root path (already created); `root_fd` is
    /// the descriptor pinning it (from [`open_bundle_root`], held in
    /// [`crate::diag::support::Prepared`]). The descriptor is dup'd, so the
    /// writer owns its own handle to the same inode.
    pub fn create(
        root: &Path,
        root_fd: BorrowedFd,
        redactor: Redactor,
    ) -> io::Result<BundleWriter> {
        Ok(BundleWriter {
            root: root.to_path_buf(),
            root_fd: root_fd.try_clone_to_owned()?,
            redactor,
            files: Vec::new(),
        })
    }

    /// Anchored create/truncate + write + ownership fixup for `rel`.
    fn write_at(&self, rel: &str, bytes: &[u8]) -> io::Result<u64> {
        write_new_at(self.root_fd.as_fd(), rel, &self.root.join(rel), bytes)
    }

    /// Create (or truncate) `rel` beneath the anchor and return the open file
    /// without writing or recording it -- for a component (the report sink,
    /// the log tee) that writes its own bytes and is recorded later.
    pub fn open_new_file(&self, rel: &str) -> io::Result<File> {
        create_file_at(self.root_fd.as_fd(), rel)
    }

    /// Create the directory `rel` (nested components included) beneath the
    /// anchor, refusing a symlink at any step. Used to pin `fixture/` before
    /// the capturer writes into it.
    pub fn mkdir_at(&self, rel: &str) -> io::Result<()> {
        let comps = split_bundle_rel(rel)?;
        walk_dirs(self.root_fd.as_fd(), &comps)?;
        Ok(())
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
        let bytes = self.write_at(rel, redacted.as_bytes())?;
        self.record(rel, bytes, false);
        Ok(())
    }

    /// Write bytes verbatim (descriptor blobs are device identity, never
    /// redacted).
    pub fn write_bytes(&mut self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let n = self.write_at(rel, bytes)?;
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
        let text = std::fs::read_to_string(self.root.join(rel))?;
        let redacted = self.redactor.text(&text);
        let bytes = self.write_at(rel, redacted.as_bytes())?;
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
        self.write_at("manifest.toml", text.as_bytes())?;
        Ok(())
    }

    /// `tar -czf <archive> -C <parent> <dirname>`; `Ok` carries the
    /// archive's size. A missing `tar` (or a failing one) is a note, and the
    /// directory stays for the user to archive by hand.
    pub fn archive(&self, archive: &Path) -> Result<u64, Note> {
        self.archive_with(archive, "tar")
    }

    pub fn archive_with(&self, archive: &Path, program: &str) -> Result<u64, Note> {
        let dirname = self
            .root
            .file_name()
            .ok_or_else(|| note("archive", "could not determine the bundle directory name"))?;

        // The bundle dir must still be the very inode we pinned at creation.
        // If the path now names a different inode (or a symlink), it was
        // swapped during the run: skip tar and keep the directory, rather
        // than archive through a redirected path.
        let root_st = fstat_fd(self.root_fd.as_fd()).map_err(|e| {
            note(
                "archive",
                format!("could not stat the bundle directory: {e}"),
            )
        })?;
        match lstat_path(&self.root) {
            Ok(path_st) => {
                let is_symlink = (path_st.st_mode & libc::S_IFMT) == libc::S_IFLNK;
                if is_symlink
                    || path_st.st_dev != root_st.st_dev
                    || path_st.st_ino != root_st.st_ino
                {
                    return Err(note(
                        "archive",
                        format!(
                            "{} was replaced during the run; not archiving",
                            self.root.display()
                        ),
                    ));
                }
            }
            Err(e) => {
                return Err(note(
                    "archive",
                    format!(
                        "could not verify {} before archiving: {e}",
                        self.root.display()
                    ),
                ))
            }
        }

        // Feed tar the source through the pinned inode rather than a path it
        // could re-resolve: the parent comes from the pinned root's own `..`
        // (so it names the real containing directory), and it is passed to
        // the child by fd via /proc/self/fd. The fd is left inheritable (no
        // O_CLOEXEC) precisely so the child tar can open it. The archive's
        // member names keep the `<dirname>/` prefix, so the bundle layout is
        // unchanged.
        // SAFETY: `self.root_fd` is a valid open directory descriptor; `".."`
        // is a valid C string. `openat` returns a fresh owned descriptor.
        let parent_raw =
            unsafe { libc::openat(self.root_fd.as_raw_fd(), c"..".as_ptr(), libc::O_DIRECTORY) };
        if parent_raw < 0 {
            return Err(note(
                "archive",
                format!(
                    "could not open the bundle parent directory: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
        // SAFETY: `parent_raw` is a fresh, valid descriptor this process owns.
        let parent_fd = unsafe { OwnedFd::from_raw_fd(parent_raw) };

        // Create the archive output ourselves, refusing a pre-planted symlink
        // or an existing file (O_EXCL | O_NOFOLLOW), and hand tar the open
        // file as its stdout. That way tar never creates the output by a path
        // it could be redirected through.
        let out_file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(archive)
        {
            Ok(f) => f,
            Err(e) => {
                return Err(note(
                    "archive",
                    format!("could not create {}: {e}", archive.display()),
                ))
            }
        };
        let size_fd = match out_file.try_clone() {
            Ok(f) => f,
            Err(e) => {
                let _ = std::fs::remove_file(archive);
                return Err(note(
                    "archive",
                    format!("could not prepare {}: {e}", archive.display()),
                ));
            }
        };
        let cdir = format!("/proc/self/fd/{}", parent_fd.as_raw_fd());
        let child = Command::new(program)
            .arg("-czf")
            .arg("-")
            .arg("-C")
            .arg(&cdir)
            .arg(dirname)
            .stdout(Stdio::from(out_file))
            .stderr(Stdio::piped())
            .spawn();
        let child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(archive);
                return Err(note("archive", format!("could not run {program}: {e}")));
            }
        };
        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                let _ = std::fs::remove_file(archive);
                return Err(note("archive", format!("could not run {program}: {e}")));
            }
        };
        // The child has its own inherited copy of the parent fd by now.
        drop(parent_fd);
        if !output.status.success() {
            let _ = std::fs::remove_file(archive);
            return Err(note(
                "archive",
                format!(
                    "could not run {program}: exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        let bytes = fstat_fd(size_fd.as_fd())
            .map(|st| st.st_size as u64)
            .map_err(|e| {
                note(
                    "archive",
                    format!("could not read {} after tar: {e}", archive.display()),
                )
            })?;
        chown_created_to_invoker(archive, size_fd.as_raw_fd());
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

    /// Create the bundle root and hand a writer anchored on its pinned fd, the
    /// way `prepare_dir` does in production. The temporary anchor is dropped
    /// here; the writer holds its own dup, so this mirrors the real flow.
    fn writer_at(root: &Path, redactor: Redactor) -> BundleWriter {
        std::fs::create_dir_all(root).unwrap();
        let fd = open_bundle_root(root).unwrap();
        BundleWriter::create(root, fd.as_fd(), redactor).unwrap()
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
        let mut w = writer_at(&root, Redactor::new(Some(Path::new("/home/alice"))));
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
        let mut w = writer_at(&root, Redactor::new(Some(Path::new("/home/alice"))));
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
        let mut w = writer_at(&root, Redactor::new(Some(Path::new("/home/alice"))));
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
        let mut w = writer_at(&root, Redactor::new(None));
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
        let mut w = writer_at(&root, Redactor::new(Some(Path::new("/home/alice"))));
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
        let w = writer_at(&root, Redactor::new(None));
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
        let w = writer_at(&root, Redactor::new(None));
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
        let mut w = writer_at(&root, Redactor::new(None));
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

    /// The load-bearing regression for the finding: with the root fd held, an
    /// intermediate directory of a bundle path is replaced by a symlink
    /// pointing outside the tree. The anchored write must refuse to follow it
    /// (the `openat` of the swapped component fails with `O_NOFOLLOW`), and
    /// nothing must be created at the symlink's target. This holds regardless
    /// of uid, so it is not gated. `O_NOFOLLOW` guards only a final component;
    /// the win here is that we walk `sub` then `inner` one component at a time.
    #[test]
    fn an_intermediate_symlink_is_refused_and_writes_nothing_at_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        std::fs::create_dir(&root).unwrap();
        let root_fd = open_bundle_root(&root).unwrap();

        // A real first component, then `inner` swapped for a link that
        // escapes the bundle entirely.
        std::fs::create_dir(root.join("sub")).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sub/inner")).unwrap();

        let err = write_new_at(
            root_fd.as_fd(),
            "sub/inner/file",
            &root.join("sub/inner/file"),
            b"root-owned bytes",
        )
        .unwrap_err();
        // ELOOP (a symlink where O_NOFOLLOW forbids one) or ENOTDIR.
        assert!(
            matches!(
                err.raw_os_error(),
                Some(e) if e == libc::ELOOP || e == libc::ENOTDIR
            ),
            "expected ELOOP/ENOTDIR, got {err:?}"
        );
        assert!(
            !outside.join("file").exists(),
            "the write escaped through the intermediate symlink"
        );
    }

    /// With the root fd held, the bundle root itself is renamed away and a
    /// symlink is dropped in its place. Writes done relative to the fd still
    /// land on the original (now-renamed) inode, never at the symlink target:
    /// a directory fd pins the inode, so the swap cannot redirect them.
    #[test]
    fn a_root_swapped_to_a_symlink_still_writes_to_the_pinned_inode() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        std::fs::create_dir(&root).unwrap();
        let root_fd = open_bundle_root(&root).unwrap();

        let moved = temp.path().join("bundle-moved");
        std::fs::rename(&root, &moved).unwrap();
        let outside = temp.path().join("attacker");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();

        let n = write_new_at(root_fd.as_fd(), "file", &root.join("file"), b"pinned").unwrap();
        assert_eq!(n, 6);
        assert_eq!(std::fs::read(moved.join("file")).unwrap(), b"pinned");
        assert!(
            !outside.join("file").exists(),
            "the write followed the swapped-in root symlink"
        );
    }

    /// A normal nested write creates the intermediate directories and the
    /// file with exactly the bytes given.
    #[test]
    fn an_anchored_write_creates_nested_dirs_and_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle");
        std::fs::create_dir(&root).unwrap();
        let root_fd = open_bundle_root(&root).unwrap();

        let payload = &[0x12u8, 0x01, 0x00, 0x02, 0xAB, 0xFF];
        let n = write_new_at(
            root_fd.as_fd(),
            "a/b/c.bin",
            &root.join("a/b/c.bin"),
            payload,
        )
        .unwrap();
        assert_eq!(n, payload.len() as u64);
        assert!(root.join("a/b").is_dir());
        assert_eq!(std::fs::read(root.join("a/b/c.bin")).unwrap(), payload);
    }

    /// `split_bundle_rel` refuses empty, `.` and `..` components so no bundle
    /// write can be aimed with a traversal component.
    #[test]
    fn split_bundle_rel_refuses_traversal_and_empty_components() {
        assert!(split_bundle_rel("a/b/c.txt").is_ok());
        for bad in ["", "a//b", "a/./b", "../b", "a/..", ".."] {
            assert!(split_bundle_rel(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}

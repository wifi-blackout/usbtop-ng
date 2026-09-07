use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const CONFIG_DIR_NAME: &str = ".usbtop-ng";
pub const PREFERENCES_FILE_NAME: &str = "preferences.toml";

/// The user a root process is acting on behalf of, resolved from `sudo`'s
/// environment. Every per-user path (preferences, the usb.ids home copy, the
/// internal snapshot, `--create-alias`'s rc file) follows this home instead
/// of root's when it is `Some`.
#[derive(Debug, Clone)]
pub struct Invoker {
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

/// Pure decision logic: is this a root process acting on behalf of another
/// user under `sudo`, and if so, who? `None` unless every one of these holds:
/// `euid` is 0, both `sudo_uid` and `sudo_gid` are set and parse as `u32`,
/// `sudo_uid` is not 0 (root sudo-ing to root changes nothing), and
/// `home_of` knows a non-empty home directory for that uid. `home_of` is the
/// user-database lookup, injected so the decision can be exercised against
/// a synthetic passwd table; production passes [`home_of_uid`].
fn resolve_invoker(
    euid: u32,
    sudo_uid: Option<&str>,
    sudo_gid: Option<&str>,
    home_of: impl Fn(u32) -> Option<PathBuf>,
) -> Option<Invoker> {
    if euid != 0 {
        return None;
    }
    let uid: u32 = sudo_uid?.parse().ok()?;
    let gid: u32 = sudo_gid?.parse().ok()?;
    if uid == 0 {
        return None;
    }
    let home = home_of(uid)?;
    if home.as_os_str().is_empty() {
        return None;
    }
    Some(Invoker { uid, gid, home })
}

/// The home directory of `uid` according to the system's user database,
/// looked up through `getpwuid_r(3)` so that the Name Service Switch is
/// consulted: a user that lives in LDAP, SSSD, or another directory service
/// has no `/etc/passwd` line, and a scan of that file alone would send
/// their preferences into root's home. `None` when there is no such user,
/// the entry has no home, or the lookup fails.
fn home_of_uid(uid: u32) -> Option<PathBuf> {
    // SAFETY: sysconf(3) reads one scalar and touches no memory of ours.
    // -1 means "no limit known"; glibc's own fallback is 1024, and 16 KiB is
    // roomy for any gecos field without being wasteful.
    let capacity = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        n if n > 0 => n as usize,
        _ => 16 * 1024,
    };
    home_of_uid_with_buffer(uid, capacity)
}

/// [`home_of_uid`]'s body with the initial buffer size as a parameter, so a
/// test can start it too small and drive the ERANGE retry loop on the real
/// libc (glibc's suggested size fits root's entry first time, which would
/// otherwise leave the retry unexercised).
fn home_of_uid_with_buffer(uid: u32, mut capacity: usize) -> Option<PathBuf> {
    use std::ffi::CStr;

    loop {
        let mut buffer = vec![0u8; capacity];
        // SAFETY: `passwd` is a plain struct of integers and pointers, for
        // which the all-zero bit pattern is a valid value.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: matches pwd.h, `int getpwuid_r(uid_t, struct passwd *,
        // char *buf, size_t buflen, struct passwd **result)`: `entry` and
        // `result` are valid for writes, `buffer` is valid for `buffer.len()`
        // bytes, and every string pointer the call stores in `entry` points
        // into `buffer`, which outlives the reads below. Returns 0 on success
        // (`result` NULL when there is no such uid), else an errno value;
        // ERANGE means the buffer was too small.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE {
            if capacity >= 1 << 20 {
                return None;
            }
            capacity = (capacity * 2).max(64);
            continue;
        }
        if rc != 0 || result.is_null() || entry.pw_dir.is_null() {
            return None;
        }
        // SAFETY: `pw_dir` is a NUL-terminated string inside `buffer`
        // (see above), still alive here.
        let home = unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes();
        if home.is_empty() {
            return None;
        }
        return Some(PathBuf::from(std::ffi::OsStr::from_bytes(home)));
    }
}

/// Production wrapper around [`resolve_invoker`]: reads the real effective
/// uid and the real `SUDO_UID`/`SUDO_GID` environment, and resolves the home
/// through the system user database ([`home_of_uid`]). These values cannot
/// change mid-process, so the result is cached.
pub fn sudo_invoker() -> Option<Invoker> {
    static INVOKER: OnceLock<Option<Invoker>> = OnceLock::new();
    INVOKER
        .get_or_init(|| {
            // SAFETY: geteuid() takes no arguments, performs no memory access,
            // and cannot fail.
            let euid = unsafe { libc::geteuid() };
            let sudo_uid = std::env::var("SUDO_UID").ok();
            let sudo_gid = std::env::var("SUDO_GID").ok();
            resolve_invoker(euid, sudo_uid.as_deref(), sudo_gid.as_deref(), home_of_uid)
        })
        .clone()
}

/// The raw `fchown(2)` syscall as a testable primitive, separate from the
/// invoker lookup so the syscall itself can be exercised as root without
/// needing a real sudo environment. See [`chown_created_to_invoker`], the
/// production entry point.
///
/// Deliberately fd-based, not path-based: `chown(2)` re-resolves every path
/// component (including a trailing symlink) at the moment it runs, so a
/// path-based chown taken some time after a containment check was passed
/// can be raced -- swap a component for a symlink in the gap and the chown
/// lands wherever that symlink points, including outside the checked
/// directory entirely. `fchown(2)` instead operates on an already-open file
/// descriptor: the kernel resolved that descriptor's target once, at
/// `open(2)` time, and nothing about it changes afterwards no matter what
/// happens to the path that was used to open it. As long as the descriptor
/// passed in was obtained by *this* process creating the file (not by
/// re-opening a path handed in from outside), there is nothing left for an
/// attacker to swap.
fn fchown_fd(fd: RawFd, uid: u32, gid: u32) -> std::io::Result<()> {
    // SAFETY: every caller passes the raw fd of a `File`/handle it still
    // owns and keeps alive at least until this call returns, so `fd` names
    // a valid, open descriptor for the whole call. `fchown` reads no
    // pointers and has no other preconditions; a non-zero return only
    // reflects a kernel-side permission or target error.
    let rc = unsafe { libc::fchown(fd, uid, gid) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Resolve `..`/`.` components without touching the filesystem -- the path
/// need not exist. Used by [`is_within`] so a lexical trick like
/// `/home/alice/../root/x` (which shares a literal component prefix with
/// `/home/alice` but does not resolve inside it) cannot pass a naive
/// component-prefix check.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// True when `path` is `home` itself or lexically nested inside it, after
/// resolving `..`/`.` components. Pure and hermetic -- neither argument need
/// exist on disk. This alone stops the traversal trick above; it does not by
/// itself stop a symlink planted on disk (see [`resolve_for_containment_check`],
/// which callers are expected to run both arguments through first when the
/// paths might exist).
fn is_within(path: &Path, home: &Path) -> bool {
    normalize_lexically(path).starts_with(normalize_lexically(home))
}

/// Resolve `path` against the real filesystem for a containment check, so a
/// symlinked ancestor cannot make a path that is lexically inside `home`
/// actually land somewhere else on disk. Every current
/// [`chown_created_to_invoker`] call site invokes it right after creating
/// the exact file or directory being chowned, so the direct `canonicalize()`
/// below -- which requires the
/// path to exist -- succeeds in practice. The fallback (canonicalize the
/// parent, which must exist for anything to be about to be created inside
/// it, and re-append the file name) keeps the function sound for a
/// not-yet-existing path too: the parent is where a symlink attack would
/// have to live, and a bare, not-yet-created leaf name cannot itself be a
/// symlink. Returns `None` when neither the path nor its parent can be
/// resolved; callers treat that as "not verified in-home" and skip.
fn resolve_for_containment_check(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    let canonical_parent = parent.canonicalize().ok()?;
    Some(canonical_parent.join(name))
}

/// Chown the already-open `fd` -- the handle this process just used to
/// CREATE `path` -- to the invoking user's uid:gid; a no-op when
/// [`sudo_invoker`] is `None`, and, since `--config`/`--usbids` can hand
/// root an arbitrary system path, also a no-op -- silently skipped, not
/// logged -- when `path` does not resolve inside the invoker's own home
/// (see [`resolve_for_containment_check`] and [`is_within`]).
///
/// `path` is consulted only to decide *whether* to chown; the chown itself
/// runs on `fd` via [`fchown_fd`]. That split is what closes the race the
/// old path-based version had: even if something raced the containment
/// check's own path resolution (swapping a component between the check and
/// this call), the worst outcome is a wrong *decision* -- chowning when it
/// should not have, or not chowning when it should have. It can never
/// redirect the chown to a *different* file, because `fchown(2)` does not
/// re-resolve a path -- the check and the act no longer share a
/// TOCTOU-vulnerable path lookup at all.
///
/// Call this on every file or directory this process CREATES under the
/// invoker's home while running as root, right after creating it and while
/// still holding the descriptor open -- appending to an existing,
/// already-user-owned file needs no call. A chown failure (as opposed to an
/// out-of-home skip) logs one warning and continues; ownership drift here
/// must never fail the run.
pub fn chown_created_to_invoker(path: &Path, fd: RawFd) {
    let Some(invoker) = sudo_invoker() else {
        return;
    };
    let Some(resolved_path) = resolve_for_containment_check(path) else {
        return;
    };
    // `invoker.home` comes straight from the user database and is not guaranteed
    // canonical (it could itself sit behind a symlinked ancestor); resolve
    // it the same way so the comparison is apples to apples. A home that
    // cannot be resolved at all falls back to its lexical form -- still
    // correct against the `..`-traversal trick, just not against a symlink,
    // which is the best available answer when the home tree itself does not
    // exist yet.
    let home = resolve_for_containment_check(&invoker.home).unwrap_or_else(|| invoker.home.clone());
    if !is_within(&resolved_path, &home) {
        return;
    }
    if let Err(e) = fchown_fd(fd, invoker.uid, invoker.gid) {
        log::warn!(
            "could not set ownership of {} to uid {} gid {}: {e}",
            path.display(),
            invoker.uid,
            invoker.gid
        );
    }
}

/// What an `lstat` of a directory entry found.
#[derive(Debug, PartialEq, Eq)]
enum EntryKind {
    Missing,
    Symlink,
    RegularFile,
    Other,
}

/// A directory held open for the writes that follow, so that a check made
/// on it and the create, rename, and unlink that trust that check all act
/// on the same inode: swapping the directory for a symlink after the open
/// changes nothing for an `openat(2)` or `renameat(2)` relative to the
/// descriptor. Under sudo, a directory that lexically claims to sit inside
/// the invoker's home is verified, through the very descriptor it was just
/// opened as, to really be there (see [`PinnedDir::for_file`]). This is the
/// support bundle's pinned-root pattern applied to the config directory.
pub struct PinnedDir {
    dir: fs::File,
    path: PathBuf,
}

impl PinnedDir {
    /// Open the directory that holds (or will hold) `file`.
    ///
    /// Ancestors may be symlinks -- a dotfiles checkout inside the home, or
    /// `/home -> /var/home` -- so the open follows them; what is judged is
    /// the directory actually opened. Under sudo, when `file` lexically
    /// claims a place inside the invoker's home, the directory's real
    /// location (read back through `/proc/self/fd`) must lie inside the
    /// resolved home too, else the open is refused: root writes into the
    /// invoker's home and nowhere else, and an `~/.usbtop-ng` swapped for a
    /// link to somewhere else is caught at every write, not only at
    /// startup. An explicit path outside the home (`--config`, `--usbids`)
    /// is the invoker's own choice and is not second-guessed; without sudo
    /// there is no privilege boundary at all.
    pub fn for_file(file: &Path) -> io::Result<PinnedDir> {
        let dir_path = match file.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let home = sudo_invoker().map(|invoker| invoker.home);
        Self::open(&dir_path, file, home.as_deref())
    }

    /// [`PinnedDir::for_file`]'s body with the invoker's home as a
    /// parameter, so the containment rule is testable without root.
    fn open(dir_path: &Path, claimed: &Path, invoker_home: Option<&Path>) -> io::Result<PinnedDir> {
        let dir = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(dir_path)?;
        if let Some(home) = invoker_home {
            if is_within(claimed, home) {
                let actual = fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
                let home_resolved =
                    resolve_for_containment_check(home).unwrap_or_else(|| home.to_path_buf());
                if !is_within(&actual, &home_resolved) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "{} resolves to {}, outside the invoking user's home {}; \
                             refusing to write there as root",
                            dir_path.display(),
                            actual.display(),
                            home.display()
                        ),
                    ));
                }
            }
        }
        Ok(PinnedDir {
            dir,
            path: dir_path.to_path_buf(),
        })
    }

    /// The path this directory was opened as, joined with `name`: for
    /// messages, and for the chown *decision* (see
    /// [`chown_created_to_invoker`], whose act is fd-based).
    pub fn join(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "file name contains a NUL byte")
        })
    }

    /// `lstat` of the entry `name`: a symlink is reported as itself.
    fn entry_kind(&self, name: &OsStr) -> io::Result<EntryKind> {
        let name = Self::c_name(name)?;
        // SAFETY: `stat` is plain data for which all-zero is a valid value.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: matches sys/stat.h, `int fstatat(int dirfd, const char
        // *pathname, struct stat *buf, int flags)`: `name` is NUL-terminated,
        // `st` is valid for the write, `AT_SYMLINK_NOFOLLOW` reports a link
        // as itself. Returns 0, else -1 with errno set.
        let rc = unsafe {
            libc::fstatat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::NotFound {
                Ok(EntryKind::Missing)
            } else {
                Err(e)
            };
        }
        Ok(match st.st_mode & libc::S_IFMT {
            libc::S_IFLNK => EntryKind::Symlink,
            libc::S_IFREG => EntryKind::RegularFile,
            _ => EntryKind::Other,
        })
    }

    fn open_with(
        &self,
        name: &OsStr,
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> io::Result<fs::File> {
        let name = Self::c_name(name)?;
        // SAFETY: matches fcntl.h, `int openat(int dirfd, const char
        // *pathname, int flags, mode_t mode)`: `name` is NUL-terminated and
        // the mode is the variadic argument `O_CREAT` requires. Returns a
        // descriptor nobody else owns (so `from_raw_fd` may take it), else
        // -1 with errno set. `O_NOFOLLOW` refuses a symlink at `name`.
        let fd = unsafe {
            libc::openat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor this call owns.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    /// Create `name` for writing, failing if anything is there already.
    pub fn create_new(&self, name: &OsStr, mode: libc::mode_t) -> io::Result<fs::File> {
        self.open_with(name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, mode)
    }

    /// Open `name` read-write, creating it if absent; never truncates.
    pub fn open_or_create(&self, name: &OsStr, mode: libc::mode_t) -> io::Result<fs::File> {
        self.open_with(name, libc::O_RDWR | libc::O_CREAT, mode)
    }

    /// Remove the entry `name`; a symlink is removed as itself.
    pub fn unlink(&self, name: &OsStr) -> io::Result<()> {
        let name = Self::c_name(name)?;
        // SAFETY: matches unistd.h, `int unlinkat(int dirfd, const char
        // *pathname, int flags)`; flags 0 names a non-directory entry.
        // Returns 0, else -1 with errno set.
        if unsafe { libc::unlinkat(self.dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Rename `from` to `to` inside this directory, atomically replacing
    /// whatever `to` names.
    pub fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        let from = Self::c_name(from)?;
        let to = Self::c_name(to)?;
        // SAFETY: matches stdio.h, `int renameat(int olddirfd, const char
        // *oldpath, int newdirfd, const char *newpath)`, both relative to
        // this one descriptor. Returns 0, else -1 with errno set.
        let rc = unsafe {
            libc::renameat(
                self.dir.as_raw_fd(),
                from.as_ptr(),
                self.dir.as_raw_fd(),
                to.as_ptr(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `fsync` the directory itself, which is what makes a rename durable.
    fn sync(&self) -> io::Result<()> {
        self.dir.sync_all()
    }
}

/// Replace the contents of `path` atomically: write `bytes` to a fresh
/// temporary file in the same directory, fsync it, chown it to the invoking
/// user via [`chown_created_to_invoker`], and rename it over `path`. At no
/// point is `path` truncated, so a full disk, a kill, or a power loss during
/// the write leaves the previous contents in place; the caller sees an
/// error and nothing else changes. `rename(2)` within one directory is
/// atomic -- a reader sees either the old file or the new one, never a
/// half-written one -- and the fsync before it is what makes the new
/// contents durable rather than merely visible.
///
/// Every step is relative to the directory held open by [`PinnedDir`]: the
/// symlink check on `path`, the temp file's create, the rename, and the
/// cleanup all name entries of that one open directory, so a directory
/// swapped for a symlink after the open (or, under sudo, one that never was
/// inside the invoker's home -- see [`PinnedDir::for_file`]) cannot redirect
/// any of them. Refuses a symlink present at `path` when the call starts
/// (the rename would swap the link for a regular file and the link's target
/// would never change, which is not what a caller replacing "the file at
/// `path`" means).
///
/// The temporary file is `<name>.<pid>.tmp`, opened with `O_CREAT|O_EXCL`
/// and mode 0600. The pid is what makes concurrent replacements safe: a CLI
/// `--forget-internal` and the TUI's `S` key running at once each rename
/// only the inode they opened, instead of unlinking each other's in-flight
/// temp file and renaming the wrong one over the target. Nothing belonging
/// to a live writer is ever cleaned up on the way in; the one exception is
/// a *regular file* already at this process's own temp name, which pid
/// reuse makes possible and which can only be a dead run's leftover -- that
/// one is removed and the create retried exactly once. Anything else there
/// (a directory, a planted symlink) fails the call before a single byte is
/// written. The directory is fsynced after the rename on a best-effort
/// basis: the rename is already durable enough for the caller's purposes
/// once it returns, and a failure there is logged, not raised.
pub fn replace_file_owned(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some(file_name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", path.display()),
        ));
    };
    let dir = PinnedDir::for_file(path)?;
    if dir.entry_kind(file_name)? == EntryKind::Symlink {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to replace a symlink at {}", path.display()),
        ));
    }
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));

    let mut file = match dir.create_new(&tmp_name, 0o600) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // Pids are reused, so this name can already be taken: by a run
            // killed after creating its temp file, whose pid this process
            // now carries. No live process shares our pid, so a *regular
            // file* here cannot be a concurrent writer's in-flight temp --
            // it can only be that dead run's leftover -- and is safe to
            // clear before retrying the create exactly once. Anything else
            // is not ours to touch: a directory, a symlink someone planted
            // (which an unlink would happily remove), or an entry we cannot
            // even stat all fail the call, as does a retry that still
            // cannot create the file.
            if dir.entry_kind(&tmp_name)? != EntryKind::RegularFile {
                return Err(e);
            }
            dir.unlink(&tmp_name)?;
            dir.create_new(&tmp_name, 0o600)?
        }
        Err(e) => return Err(e),
    };
    // The chown acts on the fd this call just created, not a re-resolved
    // path (see [`chown_created_to_invoker`]); `rename(2)` renames an entry,
    // it does not touch the inode's ownership, so `path` inherits it.
    let tmp_path = dir.join(&tmp_name);
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map(|()| chown_created_to_invoker(&tmp_path, file.as_raw_fd()));
    drop(file);
    if let Err(e) = written.and_then(|()| dir.rename(&tmp_name, file_name)) {
        let _ = dir.unlink(&tmp_name);
        return Err(e);
    }
    if let Err(e) = dir.sync() {
        log::debug!(
            "could not fsync {} after replacing {}: {e}",
            dir.path.display(),
            path.display()
        );
    }
    Ok(())
}

/// The home directory per-user data resolves against: the invoking user's
/// home under sudo (see [`sudo_invoker`]), else `$HOME` as always.
pub fn config_home() -> Result<PathBuf> {
    if let Some(invoker) = sudo_invoker() {
        return Ok(invoker.home);
    }
    let home = std::env::var("HOME").context("HOME is not set; cannot locate ~/.usbtop-ng")?;
    Ok(PathBuf::from(home))
}

/// Pure decomposition of [`preferences_path`], kept separate so the join can
/// be tested without touching the environment.
fn preferences_path_from(home: &Path) -> PathBuf {
    home.join(CONFIG_DIR_NAME).join(PREFERENCES_FILE_NAME)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// Load the Linux usbmon kernel module automatically when it is missing.
    #[serde(default)]
    pub auto_load_usbmon: bool,
    /// Unload usbmon automatically on exit when usbtop-ng loaded it for this run.
    #[serde(default)]
    pub unload_usbmon_on_exit: bool,
    /// Hide devices that are not transferring. Off by default, so every
    /// connected device shows even at zero bandwidth.
    #[serde(default)]
    pub hide_idle_devices: bool,
    /// Path to a usb.ids database file. Overrides the downloaded and distro
    /// copies; the `--usbids` flag overrides this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usbids_path: Option<String>,
    /// User-given names for physical connectors, shown on the connector
    /// heading in the device table. Keyed by position as the table shows it
    /// (`"3:1"`, `"3:1.4"`, either side's bus) or by the kernel's port object
    /// name (`usb3-port1`, `3-1-port4`). Absent by default and never written
    /// back empty, so the default file keeps its three keys.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connector_names: BTreeMap<String, String>,
}

pub fn preferences_path() -> Result<PathBuf> {
    Ok(preferences_path_from(&config_home()?))
}

pub fn load_or_create_default_at(path: &Path) -> Result<Preferences> {
    if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read preferences from {}", path.display()))?;
        return toml::from_str(&content)
            .with_context(|| format!("failed to parse preferences in {}", path.display()));
    }

    let prefs = Preferences::default();
    write_preferences_at(path, &prefs)?;
    Ok(prefs)
}

/// Writes via [`replace_file_owned`] (an atomic same-directory replacement,
/// so a full disk or a kill mid-write keeps the previous file; `O_NOFOLLOW`, fchown on
/// the fd that created the file -- see its doc comment for the race this
/// closes). Does NOT chown a parent directory it creates here: in
/// production, every default-path caller creates `.usbtop-ng` first via
/// [`ensure_private_config_dir`] (which does chown it), so `create_dir_all`
/// above is a no-op there. It only ever actually creates a directory when
/// `--config` names a path under a not-yet-existing parent -- a location the
/// invoker chose, not the documented layout -- and ownership of that is left
/// to the caller, the same way [`chown_created_to_invoker`]'s containment
/// gate already leaves anything outside the invoker's home alone.
pub fn write_preferences_at(path: &Path, prefs: &Preferences) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create preferences directory {}",
                parent.display()
            )
        })?;
    }

    let content = toml::to_string_pretty(prefs).context("failed to serialize preferences")?;
    replace_file_owned(path, content.as_bytes())
        .with_context(|| format!("failed to write preferences to {}", path.display()))?;
    Ok(())
}

/// Create the default config directory with private (0700) permissions.
/// Only chmods when this call creates the directory; an existing directory
/// (or a user-supplied custom path) is never re-chmodded. The directory is
/// born 0700 (`mkdir(2)` with that mode, so there is no umask-wide window
/// before a later chmod), then reopened once (`O_DIRECTORY | O_NOFOLLOW`)
/// and made private *and* chowned through that single fd -- the same
/// fd-based pattern every creation site in this module uses. A directory
/// that cannot be reopened that way was swapped out from under us between
/// the mkdir and the reopen, and that is an error, not a warning.
pub fn ensure_private_config_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    if dir.exists() {
        if let Some(invoker) = sudo_invoker() {
            refuse_escape_from_home(dir, &invoker.home)?;
        }
        return Ok(());
    }
    // Parents (normally just the home itself) at the default mode; only the
    // leaf is private. A parent this call creates is never chowned, so a
    // 0700 parent owned by root would lock the invoker out of it.
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("failed to create config directory {}", dir.display()))?;
    let handle = set_private_dir_permissions(dir)?;
    chown_created_to_invoker(dir, handle.as_raw_fd());
    Ok(())
}

/// Root acting for a sudo invoker writes into that user's home and nowhere
/// else. An existing config directory is the invoker's to arrange (a
/// symlink into a dotfiles checkout inside the home is fine), but one that
/// resolves *outside* their home -- `~/.usbtop-ng` replaced by a link to a
/// directory they cannot write themselves -- would have every later
/// preferences, snapshot, and usb.ids write land there as root. The chown
/// already skips such a path (see [`chown_created_to_invoker`]); the writes
/// must not proceed either. Both sides are resolved on the real filesystem
/// so a symlinked home (`/home -> /var/home`) compares equal to itself.
/// This startup check fails fast with a clear message; the guarantee that
/// survives a directory swapped *after* startup is [`PinnedDir`], which
/// every write goes through.
fn refuse_escape_from_home(dir: &Path, home: &Path) -> Result<()> {
    let resolved = resolve_for_containment_check(dir)
        .ok_or_else(|| anyhow!("config directory {} could not be resolved", dir.display()))?;
    let home_resolved = resolve_for_containment_check(home).unwrap_or_else(|| home.to_path_buf());
    if is_within(&resolved, &home_resolved) {
        return Ok(());
    }
    bail!(
        "config directory {} resolves to {}, outside the invoking user's home {}; \
         refusing to write there as root",
        dir.display(),
        resolved.display(),
        home.display()
    )
}

/// Make the directory this call just created private, through a descriptor
/// rather than a path, and hand that descriptor back so ownership can be set
/// on the very same one. `O_DIRECTORY` refuses anything that is not (by now)
/// actually a directory; `O_NOFOLLOW` refuses a symlinked final component,
/// so a link swapped in after the mkdir is an error here instead of a
/// `chmod(2)` that follows it and locks down whatever it points at.
/// `fchmod(2)` then acts on the open directory and nothing else.
fn set_private_dir_permissions(path: &Path) -> Result<fs::File> {
    let handle = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("could not reopen {} to make it private", path.display()))?;
    // SAFETY: `fchmod(int fd, mode_t mode)` (sys/stat.h) reads only its two
    // scalar arguments; `handle` keeps the descriptor open for the call.
    // It returns 0 on success, else -1 with errno set.
    if unsafe { libc::fchmod(handle.as_raw_fd(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to set permissions on {}", path.display()));
    }
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
        daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n\
        malformed line without colons\n\
        alice:x:1000:1000:Alice Example:/home/alice:/bin/bash\n";

    /// A synthetic user database for the resolver tests: the home of `uid`
    /// from passwd-format text, so the decision logic is exercised without
    /// the real NSS lookup. (Also used to read root's real home out of
    /// `/etc/passwd` in the live test below.)
    fn home_from_passwd_text(passwd: &str, uid: u32) -> Option<PathBuf> {
        passwd.lines().find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 6 || fields[2].parse::<u32>().ok()? != uid {
                return None;
            }
            Some(PathBuf::from(fields[5]))
        })
    }

    fn table(uid: u32) -> Option<PathBuf> {
        home_from_passwd_text(PASSWD, uid)
    }

    #[test]
    fn resolver_finds_the_invoking_users_home() {
        let inv = resolve_invoker(0, Some("1000"), Some("1000"), table).unwrap();
        assert_eq!(inv.uid, 1000);
        assert_eq!(inv.gid, 1000);
        assert_eq!(inv.home, PathBuf::from("/home/alice"));
    }

    #[test]
    fn resolver_is_none_without_full_sudo_context() {
        assert!(
            resolve_invoker(1000, Some("1000"), Some("1000"), table).is_none(),
            "not root"
        );
        assert!(resolve_invoker(0, None, Some("1000"), table).is_none());
        assert!(resolve_invoker(0, Some("1000"), None, table).is_none());
        assert!(
            resolve_invoker(0, Some("0"), Some("0"), table).is_none(),
            "root sudo root"
        );
        assert!(resolve_invoker(0, Some("abc"), Some("1000"), table).is_none());
        assert!(
            resolve_invoker(0, Some("4242"), Some("4242"), table).is_none(),
            "uid not in the user database"
        );
        assert!(
            resolve_invoker(0, Some("1000"), Some("1000"), |_| None).is_none(),
            "user database unavailable"
        );
    }

    #[test]
    fn resolver_rejects_an_empty_home_field() {
        let text = "ghost:x:1000:1000:g::/bin/bash\n";
        assert!(
            resolve_invoker(0, Some("1000"), Some("1000"), |uid| home_from_passwd_text(
                text, uid
            ))
            .is_none()
        );
    }

    #[test]
    fn home_of_uid_agrees_with_the_passwd_file_for_root() {
        // Root is in every user database; compare the NSS answer with the
        // local file's own line rather than hardcoding /root, so a host
        // that relocates root's home still passes.
        let Ok(passwd) = fs::read_to_string("/etc/passwd") else {
            return;
        };
        let expected = home_from_passwd_text(&passwd, 0).expect("root has a passwd line");
        assert_eq!(home_of_uid(0), Some(expected));
    }

    #[test]
    fn home_of_uid_grows_its_buffer_until_the_entry_fits() {
        // A one-byte buffer cannot hold any entry, so the first call returns
        // ERANGE and the loop must double until root's line fits.
        assert_eq!(home_of_uid_with_buffer(0, 1), home_of_uid(0));
        assert!(home_of_uid_with_buffer(0, 1).is_some());
    }

    #[test]
    fn refuse_escape_from_home_allows_in_home_layouts_and_rejects_an_escape() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let dotfiles = home.join("dotfiles").join("usbtop-ng");
        fs::create_dir_all(&dotfiles).unwrap();
        let plain = home.join(".usbtop-ng-plain");
        fs::create_dir_all(&plain).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();

        assert!(
            refuse_escape_from_home(&plain, &home).is_ok(),
            "a real dir in home"
        );
        let inside_link = home.join(".usbtop-ng");
        symlink(&dotfiles, &inside_link).unwrap();
        assert!(
            refuse_escape_from_home(&inside_link, &home).is_ok(),
            "a symlink that stays inside home is the invoker's own layout"
        );
        let escape = home.join(".usbtop-ng-escape");
        symlink(&outside, &escape).unwrap();
        let err = refuse_escape_from_home(&escape, &home)
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside the invoking user's home"), "{err}");
        assert!(err.contains(&outside.display().to_string()), "{err}");
    }

    #[test]
    fn pinned_dir_refuses_a_directory_that_escapes_the_invokers_home() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(home.join("dotfiles").join("usbtop-ng")).unwrap();
        fs::create_dir_all(home.join(".plain")).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(
            home.join("dotfiles").join("usbtop-ng"),
            home.join(".inside-link"),
        )
        .unwrap();
        symlink(&outside, home.join(".escape")).unwrap();

        let open = |dir: PathBuf| {
            let claimed = dir.join("preferences.toml");
            PinnedDir::open(&dir, &claimed, Some(&home)).map(|_| ())
        };
        open(home.join(".plain")).expect("a real directory in home");
        open(home.join(".inside-link")).expect("a link that stays inside home");
        let err = open(home.join(".escape")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            err.to_string().contains("outside the invoking user's home"),
            "{err}"
        );
        assert!(
            err.to_string().contains(&outside.display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn pinned_dir_has_no_boundary_without_a_sudo_invoker() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let escape = home.join(".escape");
        symlink(&outside, &escape).unwrap();

        // Not root acting for someone else: the user's own layout is theirs.
        PinnedDir::open(&escape, &escape.join("preferences.toml"), None)
            .expect("no privilege boundary, no refusal");
    }

    #[test]
    fn pinned_dir_entries_are_created_renamed_and_unlinked_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let dir = PinnedDir::for_file(&temp.path().join("x")).unwrap();
        assert_eq!(dir.entry_kind(OsStr::new("a")).unwrap(), EntryKind::Missing);
        let mut file = dir.create_new(OsStr::new("a"), 0o600).unwrap();
        file.write_all(b"one").unwrap();
        drop(file);
        assert_eq!(
            dir.entry_kind(OsStr::new("a")).unwrap(),
            EntryKind::RegularFile
        );
        assert!(
            dir.create_new(OsStr::new("a"), 0o600).is_err(),
            "create_new must not clobber"
        );
        dir.rename(OsStr::new("a"), OsStr::new("b")).unwrap();
        assert_eq!(fs::read_to_string(temp.path().join("b")).unwrap(), "one");
        std::os::unix::fs::symlink(temp.path().join("b"), temp.path().join("l")).unwrap();
        assert_eq!(dir.entry_kind(OsStr::new("l")).unwrap(), EntryKind::Symlink);
        dir.unlink(OsStr::new("l")).unwrap();
        assert!(
            temp.path().join("b").exists(),
            "unlinking a link leaves its target"
        );
        assert_eq!(dir.entry_kind(OsStr::new("l")).unwrap(), EntryKind::Missing);
    }

    #[test]
    fn ensure_private_config_dir_leaves_a_created_parent_at_the_default_mode() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        // A sibling made by plain create_dir shows what the umask yields.
        let reference = temp.path().join("reference");
        fs::create_dir(&reference).unwrap();
        let parent = temp.path().join("missing-home");
        let dir = parent.join(".usbtop-ng");

        ensure_private_config_dir(&dir).unwrap();

        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "the leaf is private");
        assert_eq!(
            mode(&parent),
            mode(&reference),
            "a created parent keeps the default mode, not the leaf's 0700"
        );
    }

    #[test]
    fn home_of_uid_is_none_for_an_unknown_uid() {
        // 0xFFFF_FFF0: far above any allocated range but below the
        // sentinel (uid_t)-1 that some libcs treat specially.
        assert_eq!(home_of_uid(0xFFFF_FFF0), None);
    }

    #[test]
    fn is_within_accepts_a_path_nested_inside_home() {
        assert!(is_within(
            Path::new("/home/alice/.usbtop-ng/preferences.toml"),
            Path::new("/home/alice")
        ));
    }

    #[test]
    fn is_within_accepts_home_itself() {
        assert!(is_within(
            Path::new("/home/alice"),
            Path::new("/home/alice")
        ));
    }

    #[test]
    fn is_within_rejects_an_unrelated_path() {
        assert!(!is_within(
            Path::new("/etc/cron.d/evil"),
            Path::new("/home/alice")
        ));
    }

    #[test]
    fn is_within_rejects_dotdot_traversal_that_escapes_home() {
        // Lexically normalizes to /home/root/x -- a sibling of /home/alice
        // under /home, not a descendant of it. A naive string-prefix check
        // (`starts_with` on the raw text) would wrongly accept this, since
        // "/home/alice/../root/x" literally begins with "/home/alice".
        assert!(!is_within(
            Path::new("/home/alice/../root/x"),
            Path::new("/home/alice")
        ));
    }

    #[test]
    fn is_within_rejects_a_sibling_directory_that_shares_a_name_prefix() {
        // /home/alice2 must not count as within /home/alice: guards against
        // a naive string-prefix bug (component-wise starts_with, which
        // Path::starts_with is, gets this right; raw string starts_with
        // would not).
        assert!(!is_within(
            Path::new("/home/alice2/x"),
            Path::new("/home/alice")
        ));
    }

    #[test]
    fn resolve_for_containment_check_accepts_an_existing_in_home_path() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cfg_dir = home.join(".usbtop-ng");
        fs::create_dir_all(&cfg_dir).unwrap();
        let file = cfg_dir.join("preferences.toml");
        fs::write(&file, b"x").unwrap();

        let resolved = resolve_for_containment_check(&file).unwrap();
        assert!(is_within(&resolved, &home));
    }

    #[test]
    fn resolve_for_containment_check_falls_back_to_the_parent_for_a_not_yet_created_file() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let file = home.join("not-written-yet.toml");
        assert!(!file.exists());

        let resolved = resolve_for_containment_check(&file).unwrap();
        assert!(is_within(&resolved, &home));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_for_containment_check_follows_a_symlinked_ancestor_out_of_home() {
        // The attack the containment check exists to stop: a directory
        // inside home is actually a symlink to somewhere outside it (an
        // invoking user fully controls the contents of their own home), so
        // a path that is lexically nested under home resolves, on the real
        // filesystem, to a location that is not. This is also the decision
        // that gates `chown_created_to_invoker`'s call to `fchown_fd`: it
        // resolves the path exactly this way, then returns before ever
        // calling `fchown_fd` when `is_within` comes back false here -- so a
        // `false` result below is "no chown attempted" for the real
        // function, not just for this pure check in isolation.
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let trap = home.join("trap");
        std::os::unix::fs::symlink(&outside, &trap).unwrap();
        let file = trap.join("planted.toml");
        fs::write(&file, b"x").unwrap();

        let resolved = resolve_for_containment_check(&file).unwrap();
        assert!(
            !is_within(&resolved, &home),
            "a symlinked ancestor must resolve to its real target, escaping home -- \
             chown_created_to_invoker returns here without ever calling fchown_fd"
        );
    }

    #[test]
    fn preferences_path_from_joins_config_dir_and_file_name() {
        let home = Path::new("/home/alice");
        assert_eq!(
            preferences_path_from(home),
            PathBuf::from("/home/alice/.usbtop-ng/preferences.toml")
        );
    }

    #[test]
    fn creates_default_preferences_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".usbtop-ng/preferences.toml");

        let prefs = load_or_create_default_at(&path).unwrap();

        assert_eq!(prefs, Preferences::default());
        let written = fs::read_to_string(path).unwrap();
        assert!(written.contains("auto_load_usbmon = false"));
        assert!(written.contains("unload_usbmon_on_exit = false"));
    }

    #[test]
    fn loads_existing_preferences_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".usbtop-ng/preferences.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "auto_load_usbmon = true\nunload_usbmon_on_exit = true\n",
        )
        .unwrap();

        let prefs = load_or_create_default_at(&path).unwrap();

        assert!(prefs.auto_load_usbmon);
        assert!(prefs.unload_usbmon_on_exit);
    }

    #[test]
    fn custom_path_write_does_not_change_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("custom");
        fs::create_dir_all(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();

        let path = parent.join("prefs.toml");
        load_or_create_default_at(&path).unwrap();

        let mode = fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "custom parent dir permissions must be untouched"
        );
    }

    #[test]
    fn ensure_private_config_dir_creates_with_0700() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".usbtop-ng");

        ensure_private_config_dir(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn ensure_private_config_dir_leaves_existing_dir_permissions_alone() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".usbtop-ng");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_config_dir(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "existing dir must not be re-chmodded");
    }

    #[test]
    fn set_private_dir_permissions_refuses_a_symlink_and_leaves_its_target_alone() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("elsewhere");
        fs::create_dir_all(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = temp.path().join(".usbtop-ng");
        symlink(&target, &link).unwrap();

        // The race this guards: root creates the directory, the invoker swaps
        // it for a symlink before the chmod lands. A path-based chmod would
        // follow the link and lock down whatever it points at.
        assert!(
            set_private_dir_permissions(&link).is_err(),
            "a symlink must be refused, not chmodded through"
        );
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the link target must keep its own mode");
    }

    #[test]
    fn ensure_private_config_dir_does_not_create_through_a_dangling_symlink() {
        // Characterization, not a guard: `mkdir(2)` itself refuses a path
        // that is a dangling link (EEXIST), so nothing is ever created at
        // the link's target. The guard against a link to an existing
        // directory is `set_private_dir_permissions_refuses_a_symlink...`.
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("never-made");
        let link = temp.path().join(".usbtop-ng");
        symlink(&target, &link).unwrap();

        assert!(ensure_private_config_dir(&link).is_err());
        assert!(!target.exists(), "nothing may be created through the link");
    }

    #[test]
    fn hide_idle_devices_defaults_to_false() {
        assert!(!Preferences::default().hide_idle_devices);
    }

    #[test]
    fn old_preferences_file_without_the_key_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".usbtop-ng/preferences.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "auto_load_usbmon = true\nunload_usbmon_on_exit = false\n",
        )
        .unwrap();

        let prefs = load_or_create_default_at(&path).unwrap();
        assert!(prefs.auto_load_usbmon);
        assert!(!prefs.hide_idle_devices);
    }

    #[test]
    fn hide_idle_devices_round_trips_and_keeps_the_other_keys() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prefs.toml");
        let prefs = Preferences {
            auto_load_usbmon: true,
            unload_usbmon_on_exit: true,
            hide_idle_devices: true,
            usbids_path: None,
            connector_names: std::collections::BTreeMap::new(),
        };
        write_preferences_at(&path, &prefs).unwrap();

        let read = load_or_create_default_at(&path).unwrap();
        assert_eq!(read, prefs);
    }

    #[test]
    fn connector_names_table_loads_and_survives_the_idle_toggle_rewrite() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prefs.toml");
        fs::write(
            &path,
            "auto_load_usbmon = false\nunload_usbmon_on_exit = false\nhide_idle_devices = false\n\n[connector_names]\n\"3:1\" = \"Left Type-A\"\n\"3-1-port4\" = \"Rear Right Type-C\"\n",
        )
        .unwrap();
        let prefs = load_or_create_default_at(&path).unwrap();
        assert_eq!(
            prefs.connector_names.get("3:1").map(String::as_str),
            Some("Left Type-A")
        );
        assert_eq!(
            prefs.connector_names.get("3-1-port4").map(String::as_str),
            Some("Rear Right Type-C")
        );

        // The `i` toggle rewrites the whole file: the table must survive it.
        let mut toggled = prefs.clone();
        toggled.hide_idle_devices = true;
        write_preferences_at(&path, &toggled).unwrap();
        let again = load_or_create_default_at(&path).unwrap();
        assert_eq!(again, toggled);
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("[connector_names]"));
    }

    /// The `i` toggle rewrites the whole preferences file: a failed rewrite
    /// must leave the previous file, names and all, exactly as it was.
    #[test]
    fn a_failed_preferences_rewrite_leaves_the_previous_file_intact() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prefs.toml");
        let mut prefs = Preferences::default();
        prefs
            .connector_names
            .insert("3:1".to_string(), "Left Type-A".to_string());
        write_preferences_at(&path, &prefs).unwrap();
        let before = fs::read(&path).unwrap();
        // A directory squatting on this process's temp name fails the
        // atomic replacement before a byte of the target is touched.
        fs::create_dir(
            temp.path()
                .join(format!("prefs.toml.{}.tmp", std::process::id())),
        )
        .unwrap();
        prefs.hide_idle_devices = true;
        assert!(write_preferences_at(&path, &prefs).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn preferences_without_names_serialize_without_the_table() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prefs.toml");
        write_preferences_at(&path, &Preferences::default()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("connector_names"), "{text}");
    }

    #[test]
    fn file_without_usbids_path_loads_with_none() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".usbtop-ng/preferences.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "auto_load_usbmon = false\nunload_usbmon_on_exit = false\nhide_idle_devices = false\n",
        )
        .unwrap();

        let prefs = load_or_create_default_at(&path).unwrap();
        assert_eq!(prefs.usbids_path, None);
    }

    #[test]
    fn usbids_path_round_trips_when_set() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prefs.toml");
        let prefs = Preferences {
            usbids_path: Some("/opt/custom/usb.ids".to_string()),
            connector_names: std::collections::BTreeMap::new(),
            ..Preferences::default()
        };
        write_preferences_at(&path, &prefs).unwrap();

        let read = load_or_create_default_at(&path).unwrap();
        assert_eq!(read.usbids_path.as_deref(), Some("/opt/custom/usb.ids"));

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("usbids_path"));
    }

    #[test]
    fn default_preferences_file_omits_usbids_path_entirely() {
        // A None Option<String> has no TOML representation, so the key must
        // be skipped rather than written as some kind of null.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".usbtop-ng/preferences.toml");

        load_or_create_default_at(&path).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("usbids_path"),
            "a None usbids_path must not appear in the written file: {written}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replace_file_owned_replaces_content_privately_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("internal-devices.toml");
        std::fs::write(&path, b"old").unwrap();
        replace_file_owned(&path, b"new contents").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            !names.iter().any(|n| n.to_string_lossy().ends_with(".tmp")),
            "no temp file left behind: {names:?}"
        );
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn replace_file_owned_uses_a_temp_name_unique_to_this_process() {
        // The old fixed `<path>.tmp` let two concurrent replacements unlink
        // each other's in-flight file and rename the wrong inode over the
        // target; each call now writes through `<file_name>.<pid>.tmp`.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("internal-devices.toml");
        replace_file_owned(&path, b"contents").unwrap();
        assert!(
            !temp.path().join("internal-devices.toml.tmp").exists(),
            "the fixed temp name is never used any more"
        );
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            vec![std::ffi::OsString::from("internal-devices.toml")],
            "{names:?}"
        );
    }

    #[test]
    fn replace_file_owned_creates_the_file_when_it_does_not_exist_yet() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fresh.toml");
        replace_file_owned(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[test]
    fn replace_file_owned_ignores_a_stale_temp_file_from_another_run() {
        // What a run killed mid-write leaves behind: a temp file named for
        // *its* pid, not this one's. Nothing cleans it up on the way in and
        // nothing ever picks it up, so the replacement simply succeeds and
        // the leftover stays exactly as it was.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("internal-devices.toml");
        std::fs::write(&path, b"old").unwrap();
        let other_pid = if std::process::id() == 99999 {
            99998
        } else {
            99999
        };
        let stale = temp
            .path()
            .join(format!("internal-devices.toml.{other_pid}.tmp"));
        std::fs::write(&stale, b"half-written garbage").unwrap();
        replace_file_owned(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(
            std::fs::read(&stale).unwrap(),
            b"half-written garbage",
            "another run's temp file is neither read nor removed"
        );
    }

    #[test]
    fn replace_file_owned_replaces_a_stale_temp_file_left_by_a_dead_run_with_our_pid() {
        // Pids are reused: this is the leftover of a run killed after it
        // created its temp file, whose pid we now carry. A regular file
        // there cannot belong to any live process, so it is cleared and the
        // create retried once.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("internal-devices.toml");
        std::fs::write(&path, b"old").unwrap();
        let stale = temp
            .path()
            .join(format!("internal-devices.toml.{}.tmp", std::process::id()));
        std::fs::write(&stale, b"half-written garbage").unwrap();
        replace_file_owned(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            !names.iter().any(|n| n.to_string_lossy().ends_with(".tmp")),
            "the leftover was consumed, not left behind: {names:?}"
        );
    }

    #[test]
    fn replace_file_owned_never_removes_a_symlink_at_its_temp_name() {
        // The retry above must never fire for a symlink: unlinking it and
        // creating a file in its place is exactly the write an attacker who
        // can plant one is after.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("internal-devices.toml");
        std::fs::write(&path, b"precious original").unwrap();
        let decoy = temp.path().join("decoy");
        std::fs::write(&decoy, b"do not touch").unwrap();
        let tmp = temp
            .path()
            .join(format!("internal-devices.toml.{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&decoy, &tmp).unwrap();

        let err = replace_file_owned(&path, b"replacement").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(
            std::fs::read(&decoy).unwrap(),
            b"do not touch",
            "the symlink's target is untouched"
        );
        assert!(
            std::fs::symlink_metadata(&tmp)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself survives"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"precious original",
            "and the target is intact"
        );
    }

    #[test]
    fn replace_file_owned_leaves_the_original_intact_when_the_temp_file_cannot_be_created() {
        // A directory squatting on this process's own temp name: the
        // `create_new` open fails with AlreadyExists, and nothing may have
        // been written.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("internal-devices.toml");
        std::fs::write(&path, b"precious original").unwrap();
        let squatter = temp
            .path()
            .join(format!("internal-devices.toml.{}.tmp", std::process::id()));
        std::fs::create_dir(&squatter).unwrap();
        let err = replace_file_owned(&path, b"replacement").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"precious original",
            "byte-for-byte intact"
        );
        assert!(squatter.is_dir(), "the squatter is not removed");
    }

    #[test]
    fn replace_file_owned_refuses_a_symlinked_target_and_leaves_both_ends_alone() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("elsewhere.toml");
        std::fs::write(&real, b"target contents").unwrap();
        let link = temp.path().join("internal-devices.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = replace_file_owned(&link, b"replacement").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link survives"
        );
        assert_eq!(
            std::fs::read(&real).unwrap(),
            b"target contents",
            "the target is untouched"
        );
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            !names.iter().any(|n| n.to_string_lossy().ends_with(".tmp")),
            "no temp file was created at all: {names:?}"
        );
    }

    #[test]
    fn replace_file_owned_errors_when_the_parent_directory_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing").join("x.toml");
        assert!(replace_file_owned(&path, b"x").is_err());
    }
}

#[cfg(all(test, feature = "integration"))]
mod integration_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// Exercises the real `fchown(2)` primitive, not the invoker lookup
    /// (that half is already covered hermetically above). Requires root,
    /// since only root may chown a file to an arbitrary uid/gid; skips
    /// gracefully otherwise, matching the pattern at `src/usbmon/mod.rs`'s
    /// `debugfs_state_reads_permission_denied`.
    /// Run: cargo test --features integration
    #[test]
    fn fchown_fd_sets_the_files_owning_uid_and_gid() {
        // SAFETY: geteuid() takes no arguments and cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("not running as root; fchown_fd integration check skipped");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned-file");
        let file = fs::File::create(&path).unwrap();

        // `daemon` (uid/gid 1) is present on every Linux system and is not
        // the file's current owner (root, from creating it above), so a
        // successful chown is observable. Kept open across the call, the
        // same way every real caller holds its own handle -- `fchown_fd`
        // never touches the path again.
        fchown_fd(file.as_raw_fd(), 1, 1).unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.uid(), 1);
        assert_eq!(meta.gid(), 1);
    }
}

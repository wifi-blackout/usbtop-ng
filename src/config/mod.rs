use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
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
/// `sudo_uid` is not 0 (root sudo-ing to root changes nothing), and `passwd`
/// is `Some` text with a line whose 3rd colon-separated field equals
/// `sudo_uid` and whose 6th field (the home directory) is non-empty.
/// Malformed lines (too few fields) are skipped, not fatal -- the scan keeps
/// looking rather than aborting on the first bad line.
fn resolve_invoker(
    euid: u32,
    sudo_uid: Option<&str>,
    sudo_gid: Option<&str>,
    passwd: Option<&str>,
) -> Option<Invoker> {
    if euid != 0 {
        return None;
    }
    let uid: u32 = sudo_uid?.parse().ok()?;
    let gid: u32 = sudo_gid?.parse().ok()?;
    if uid == 0 {
        return None;
    }
    let passwd = passwd?;

    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 6 {
            continue;
        }
        let Ok(line_uid) = fields[2].parse::<u32>() else {
            continue;
        };
        if line_uid != uid {
            continue;
        }
        let home = fields[5];
        if home.is_empty() {
            continue;
        }
        return Some(Invoker {
            uid,
            gid,
            home: PathBuf::from(home),
        });
    }
    None
}

/// Production wrapper around [`resolve_invoker`]: reads the real effective
/// uid, the real `SUDO_UID`/`SUDO_GID` environment, and the real
/// `/etc/passwd` (an unreadable file resolves the same as a missing one --
/// `None`). These values cannot change mid-process, so the result is cached.
pub fn sudo_invoker() -> Option<Invoker> {
    static INVOKER: OnceLock<Option<Invoker>> = OnceLock::new();
    INVOKER
        .get_or_init(|| {
            // SAFETY: geteuid() takes no arguments, performs no memory access,
            // and cannot fail.
            let euid = unsafe { libc::geteuid() };
            let sudo_uid = std::env::var("SUDO_UID").ok();
            let sudo_gid = std::env::var("SUDO_GID").ok();
            let passwd = fs::read_to_string("/etc/passwd").ok();
            resolve_invoker(
                euid,
                sudo_uid.as_deref(),
                sudo_gid.as_deref(),
                passwd.as_deref(),
            )
        })
        .clone()
}

/// The raw `chown(2)` syscall as a testable primitive, separate from the
/// invoker lookup so the syscall itself can be exercised as root without
/// needing a real sudo environment. See [`chown_to_invoker`], the production
/// entry point.
fn chown_path(path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains a NUL byte")
    })?;
    // SAFETY: `cstr` is a valid, NUL-terminated C string that outlives this
    // call; chown(2) only reads it and has no other preconditions.
    let rc = unsafe { libc::chown(cstr.as_ptr(), uid, gid) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Resolve `..`/`.` components without touching the filesystem -- the path
/// need not exist. Used by [`is_within`] so a lexical trick like
/// `/home/lylem/../root/x` (which shares a literal component prefix with
/// `/home/lylem` but does not resolve inside it) cannot pass a naive
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
/// actually land somewhere else on disk. Every current [`chown_to_invoker`]
/// call site invokes it right after creating the exact file or directory
/// being chowned, so the direct `canonicalize()` below -- which requires the
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

/// Chown `path` to the invoking user's uid:gid; a no-op when [`sudo_invoker`]
/// is `None`, and, since `--config`/`--usbids` can hand root an arbitrary
/// system path, also a no-op -- silently skipped, not logged -- when `path`
/// does not resolve inside the invoker's own home (see
/// [`resolve_for_containment_check`] and [`is_within`]). Call this on every
/// file or directory this process CREATES under the invoker's home while
/// running as root -- appending to an existing, already-user-owned file
/// needs no call. A chown failure (as opposed to an out-of-home skip) logs
/// one warning and continues; ownership drift here must never fail the run.
pub fn chown_to_invoker(path: &Path) {
    let Some(invoker) = sudo_invoker() else {
        return;
    };
    let Some(resolved_path) = resolve_for_containment_check(path) else {
        return;
    };
    // `invoker.home` comes straight from /etc/passwd and is not guaranteed
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
    // Chown the resolved path, not the original `path`: `chown_path` ->
    // `libc::chown` follows symlinks and re-resolves every component at
    // syscall time, so chowning the original text would let an attacker who
    // controls a component under their own home swap it for a symlink
    // between the check above and this call, landing the chown outside home
    // -- the exact race the containment check exists to close.
    // `resolved_path` has every ancestor already resolved to a real,
    // symlink-free component (or, for a not-yet-created leaf, a real
    // canonical parent plus the plain leaf name), so there is nothing left
    // in it for `chown(2)` to re-resolve elsewhere: the check and the act
    // now run on the same bytes.
    if let Err(e) = chown_path(&resolved_path, invoker.uid, invoker.gid) {
        log::warn!(
            "could not set ownership of {} to uid {} gid {}: {e}",
            resolved_path.display(),
            invoker.uid,
            invoker.gid
        );
    }
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
    fs::write(path, content)
        .with_context(|| format!("failed to write preferences to {}", path.display()))?;
    chown_to_invoker(path);
    Ok(())
}

/// Create the default config directory with private (0700) permissions.
/// Only chmods when this call creates the directory; an existing directory
/// (or a user-supplied custom path) is never re-chmodded.
pub fn ensure_private_config_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create config directory {}", dir.display()))?;
    set_private_dir_permissions(dir)?;
    chown_to_invoker(dir);
    Ok(())
}

fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
        daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n\
        malformed line without colons\n\
        lylem:x:1000:1000:Lyle M:/home/lylem:/bin/bash\n";

    #[test]
    fn resolver_finds_the_invoking_users_home() {
        let inv = resolve_invoker(0, Some("1000"), Some("1000"), Some(PASSWD)).unwrap();
        assert_eq!(inv.uid, 1000);
        assert_eq!(inv.gid, 1000);
        assert_eq!(inv.home, PathBuf::from("/home/lylem"));
    }

    #[test]
    fn resolver_is_none_without_full_sudo_context() {
        assert!(
            resolve_invoker(1000, Some("1000"), Some("1000"), Some(PASSWD)).is_none(),
            "not root"
        );
        assert!(resolve_invoker(0, None, Some("1000"), Some(PASSWD)).is_none());
        assert!(resolve_invoker(0, Some("1000"), None, Some(PASSWD)).is_none());
        assert!(
            resolve_invoker(0, Some("0"), Some("0"), Some(PASSWD)).is_none(),
            "root sudo root"
        );
        assert!(resolve_invoker(0, Some("abc"), Some("1000"), Some(PASSWD)).is_none());
        assert!(
            resolve_invoker(0, Some("4242"), Some("4242"), Some(PASSWD)).is_none(),
            "uid not in passwd"
        );
        assert!(
            resolve_invoker(0, Some("1000"), Some("1000"), None).is_none(),
            "passwd unreadable"
        );
    }

    #[test]
    fn resolver_skips_malformed_passwd_lines() {
        let inv = resolve_invoker(0, Some("1000"), Some("1000"), Some(PASSWD));
        assert!(
            inv.is_some(),
            "the malformed line above lylem's must not abort the scan"
        );
    }

    #[test]
    fn resolver_rejects_an_empty_home_field() {
        let text = "ghost:x:1000:1000:g::/bin/bash\n";
        assert!(resolve_invoker(0, Some("1000"), Some("1000"), Some(text)).is_none());
    }

    #[test]
    fn is_within_accepts_a_path_nested_inside_home() {
        assert!(is_within(
            Path::new("/home/lylem/.usbtop-ng/preferences.toml"),
            Path::new("/home/lylem")
        ));
    }

    #[test]
    fn is_within_accepts_home_itself() {
        assert!(is_within(
            Path::new("/home/lylem"),
            Path::new("/home/lylem")
        ));
    }

    #[test]
    fn is_within_rejects_an_unrelated_path() {
        assert!(!is_within(
            Path::new("/etc/cron.d/evil"),
            Path::new("/home/lylem")
        ));
    }

    #[test]
    fn is_within_rejects_dotdot_traversal_that_escapes_home() {
        // Lexically normalizes to /home/root/x -- a sibling of /home/lylem
        // under /home, not a descendant of it. A naive string-prefix check
        // (`starts_with` on the raw text) would wrongly accept this, since
        // "/home/lylem/../root/x" literally begins with "/home/lylem".
        assert!(!is_within(
            Path::new("/home/lylem/../root/x"),
            Path::new("/home/lylem")
        ));
    }

    #[test]
    fn is_within_rejects_a_sibling_directory_that_shares_a_name_prefix() {
        // /home/lylem2 must not count as within /home/lylem: guards against
        // a naive string-prefix bug (component-wise starts_with, which
        // Path::starts_with is, gets this right; raw string starts_with
        // would not).
        assert!(!is_within(
            Path::new("/home/lylem2/x"),
            Path::new("/home/lylem")
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
        // that gates `chown_to_invoker`'s call to `chown_path`: it resolves
        // the path exactly this way, then returns before ever calling
        // `chown_path` when `is_within` comes back false here -- so a
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
             chown_to_invoker returns here without ever calling chown_path"
        );
    }

    #[test]
    fn preferences_path_from_joins_config_dir_and_file_name() {
        let home = Path::new("/home/lylem");
        assert_eq!(
            preferences_path_from(home),
            PathBuf::from("/home/lylem/.usbtop-ng/preferences.toml")
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
        };
        write_preferences_at(&path, &prefs).unwrap();

        let read = load_or_create_default_at(&path).unwrap();
        assert_eq!(read, prefs);
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
}

#[cfg(all(test, feature = "integration"))]
mod integration_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// Exercises the real `chown(2)` primitive, not the invoker lookup (that
    /// half is already covered hermetically above). Requires root, since
    /// only root may chown a file to an arbitrary uid/gid; skips gracefully
    /// otherwise, matching the pattern at `src/usbmon/mod.rs`'s
    /// `debugfs_state_reads_permission_denied`.
    /// Run: cargo test --features integration
    #[test]
    fn chown_path_sets_the_files_owning_uid_and_gid() {
        // SAFETY: geteuid() takes no arguments and cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("not running as root; chown_path integration check skipped");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned-file");
        fs::write(&path, b"content").unwrap();

        // `daemon` (uid/gid 1) is present on every Linux system and is not
        // the file's current owner (root, from creating it above), so a
        // successful chown is observable.
        chown_path(&path, 1, 1).unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.uid(), 1);
        assert_eq!(meta.gid(), 1);
    }
}

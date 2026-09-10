//! The pinned fixture directory. Every byte the capture core writes -- an
//! attribute file, the `usbN` and `peer` symlinks, a trace, a golden,
//! `meta.toml`, the baseline snapshot -- goes through [`FixtureRoot`],
//! relative to one directory descriptor held for the whole assembly, with
//! every path component resolved `O_NOFOLLOW` and every file created fresh
//! (`O_EXCL`). A directory swapped for a symlink after the pin, at the root
//! or anywhere beneath it, changes nothing about where the bytes land: the
//! descriptor names an inode, not a path. This is the support bundle's own
//! pinned-root pattern ([`crate::diag::bundle`]) extended to the subtree
//! the capturer owns. The read side ([`FixtureRoot::read_base`]) pins the
//! fixture root the same way and reads the tree beneath it as written, by
//! path.

use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
#[cfg(any(feature = "capture-fixture", test))]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::diag::bundle;

/// A fixture directory pinned on a descriptor (see the module doc).
#[derive(Debug)]
pub struct FixtureRoot {
    fd: OwnedFd,
    /// Where reads of the written tree resolve; fixed at construction (see
    /// [`read_base_for`]) so every reader and every message keys on the
    /// same base.
    read_base: PathBuf,
    /// The path the directory was named by: for the ownership pass at the
    /// end (`own_tree`) and the procfs-less read fallback; no write
    /// resolves it.
    logical: PathBuf,
    /// How messages name the directory: the path as the user gave it, or
    /// `fixture` inside a support bundle. Never the descriptor path.
    display: String,
    /// Modes for what this root creates (the umask applies): private
    /// inside a support bundle, readable for a fixture meant to be
    /// committed.
    dir_mode: libc::mode_t,
    file_mode: libc::mode_t,
}

impl FixtureRoot {
    /// For `--capture-fixture <DIR>`: create `dir` when absent and pin it.
    /// The path is the invoker's own choice, so its ancestors are followed
    /// as given, once, here -- but a symlink at `dir` itself is refused, the
    /// same rule `--output` applies -- and from this point on nothing is
    /// followed. A fixture captured this way is meant to be read and
    /// committed, so it is created readable (`0755`/`0644`, the umask
    /// applying) and handed to the sudo invoker by the ownership pass at
    /// the end of the assembly. Only that feature-gated subcommand (and the
    /// tests) name a directory; `--support` pins a child of its bundle root
    /// with [`FixtureRoot::beneath`].
    #[cfg(any(feature = "capture-fixture", test))]
    pub fn create(dir: &Path) -> io::Result<FixtureRoot> {
        std::fs::create_dir_all(dir)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)?;
        let fd: OwnedFd = file.into();
        let read_base = read_base_for(fd.as_fd(), dir, procfs_serves(fd.as_fd()), false)?;
        Ok(FixtureRoot {
            fd,
            read_base,
            logical: dir.to_path_buf(),
            display: dir.display().to_string(),
            dir_mode: 0o755,
            file_mode: 0o644,
        })
    }

    /// For `--support`: the directory `name` directly beneath an already
    /// pinned bundle root, created when absent and refused when a symlink
    /// sits there. `logical` is the bundle directory joined with `name`.
    /// Private like the rest of the bundle (`0700`/`0600`).
    pub fn beneath(parent_fd: BorrowedFd, name: &str, logical: PathBuf) -> io::Result<FixtureRoot> {
        let fd = bundle::open_subdir_at(parent_fd, name, 0o700)?;
        let read_base = read_base_for(fd.as_fd(), &logical, procfs_serves(fd.as_fd()), true)?;
        Ok(FixtureRoot {
            fd,
            read_base,
            logical,
            display: name.to_string(),
            dir_mode: 0o700,
            file_mode: 0o600,
        })
    }

    /// The path this directory was named by (for the ownership pass).
    pub fn logical(&self) -> &Path {
        &self.logical
    }

    /// How messages name this directory (see the field).
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Reword an error that captured the descriptor path (a replay or an
    /// invariant check reads through [`FixtureRoot::read_base`]) so it names
    /// the directory the way the user knows it.
    pub fn describe(&self, err: anyhow::Error) -> anyhow::Error {
        let text = format!("{err:#}");
        anyhow::anyhow!(
            "{}",
            text.replace(&self.read_base().display().to_string(), &self.display)
        )
    }

    /// The read side, fixed at construction: normally `/proc/self/fd/<n>`,
    /// which resolves to the pinned inode whatever the logical path names by
    /// now, so the replay that generates each golden, the SEC-1 and SEC-2
    /// re-checks, and the stale-tree check read the tree beneath the pinned
    /// root as written (by path below that root). See [`read_base_for`] for
    /// the one case without procfs.
    pub fn read_base(&self) -> PathBuf {
        self.read_base.clone()
    }

    /// Create the file `rel` (bundle-relative, `/`-separated) fresh --
    /// an entry already there is an error, never truncated -- creating
    /// directories on the way, and write `bytes`. The file stays root's
    /// until the ownership pass that runs after every re-check and replay
    /// has read the finished tree (`own_tree`, from the `--capture-fixture`
    /// handler or at the end of a support bundle): handing it over here,
    /// while the privileged process still reads it back, would let the
    /// invoker rewrite a trace between its write and its replay.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file =
            bundle::create_new_file_at(self.fd.as_fd(), rel, self.dir_mode, self.file_mode)?;
        file.write_all(bytes)?;
        file.flush()
    }

    /// Create the directory `rel` and every directory on the way.
    pub fn mkdir_all(&self, rel: &str) -> io::Result<()> {
        bundle::mkdir_all_at(self.fd.as_fd(), rel, self.dir_mode)
    }

    /// Create the symlink `rel` -> `target`, `target` stored verbatim.
    pub fn symlink(&self, rel: &str, target: &Path) -> io::Result<()> {
        bundle::symlink_at(self.fd.as_fd(), rel, target, self.dir_mode)
    }
}

/// Whether procfs exposes `fd` at `/proc/self/fd/<n>`.
fn procfs_serves(fd: BorrowedFd) -> bool {
    Path::new(&format!("/proc/self/fd/{}", fd.as_raw_fd())).exists()
}

/// The read base for a root: `/proc/self/fd/<n>` whenever procfs serves it.
/// Without procfs -- a chroot or container that mounts `/sys` and
/// `/dev/usbmon*` but not `/proc` -- a `--capture-fixture` root
/// (`private == false`) reads by the path the invoker named, exactly what
/// the reads used before the pin existed and a directory that is their own
/// choice; a bundle's fixture (`private == true`) is refused instead, since
/// its reads and its cleanup must never resolve the swappable bundle path,
/// and `--support` needs procfs for its archive step anyway. Decided once,
/// here, so readers and messages never disagree on the base.
fn read_base_for(
    fd: BorrowedFd,
    logical: &Path,
    procfs_available: bool,
    private: bool,
) -> io::Result<PathBuf> {
    if procfs_available {
        return Ok(PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd())));
    }
    if private {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "procfs is not mounted; the support bundle's fixture needs /proc/self/fd",
        ));
    }
    Ok(logical.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_directories_files_and_links_relative_to_the_pinned_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = FixtureRoot::create(&temp.path().join("bundle")).unwrap();

        root.mkdir_all("sysfs/ctrl/usb1").unwrap();
        root.write("sysfs/ctrl/usb1/busnum", b"1\n").unwrap();
        root.symlink("sysfs/usb1", Path::new("ctrl/usb1")).unwrap();

        let bundle = temp.path().join("bundle");
        assert_eq!(
            std::fs::read_to_string(bundle.join("sysfs/ctrl/usb1/busnum")).unwrap(),
            "1\n"
        );
        assert_eq!(
            std::fs::read_link(bundle.join("sysfs/usb1")).unwrap(),
            Path::new("ctrl/usb1"),
            "the target is stored verbatim"
        );
        assert_eq!(
            std::fs::read_to_string(root.read_base().join("sysfs/usb1/busnum")).unwrap(),
            "1\n",
            "the read side resolves through the descriptor"
        );
        assert!(
            root.symlink("sysfs/usb1", Path::new("elsewhere")).is_err(),
            "an existing entry is never replaced"
        );
    }

    #[test]
    fn a_symlink_swapped_in_beneath_the_root_is_refused_and_nothing_escapes() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        let root = FixtureRoot::create(&bundle).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, bundle.join("sysfs")).unwrap();

        for result in [
            root.write("sysfs/1-1/busnum", b"1\n"),
            root.mkdir_all("sysfs/1-1"),
            root.symlink("sysfs/usb1", Path::new("ctrl/usb1")),
        ] {
            let err = result.unwrap_err();
            assert!(
                matches!(err.raw_os_error(), Some(e) if e == libc::ELOOP || e == libc::ENOTDIR),
                "expected ELOOP/ENOTDIR, got {err:?}"
            );
        }
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "a write escaped through the swapped-in link"
        );
    }

    #[test]
    fn a_root_renamed_away_still_receives_the_writes() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        let root = FixtureRoot::create(&bundle).unwrap();
        let moved = temp.path().join("moved");
        std::fs::rename(&bundle, &moved).unwrap();
        let outside = temp.path().join("attacker");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &bundle).unwrap();

        root.write("meta.toml", b"x").unwrap();

        assert_eq!(std::fs::read(moved.join("meta.toml")).unwrap(), b"x");
        assert!(!outside.join("meta.toml").exists());
    }

    #[test]
    fn create_refuses_a_symlink_at_the_output_directory() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("out");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = FixtureRoot::create(&link).unwrap_err();
        assert!(
            matches!(err.raw_os_error(), Some(e) if e == libc::ELOOP || e == libc::ENOTDIR),
            "expected ELOOP/ENOTDIR, got {err:?}"
        );
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
    }

    #[test]
    fn a_pre_existing_entry_at_a_write_target_is_refused_and_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        let root = FixtureRoot::create(&bundle).unwrap();
        root.write("meta.toml", b"first").unwrap();

        let err = root.write("meta.toml", b"second").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(bundle.join("meta.toml")).unwrap(), b"first");

        // A hard link planted at an expected name points at an inode
        // elsewhere; a truncating open would have emptied that inode.
        let canary = temp.path().join("canary");
        std::fs::write(&canary, b"precious").unwrap();
        std::fs::hard_link(&canary, bundle.join("trace.bin")).unwrap();
        assert!(root.write("trace.bin", b"x").is_err());
        assert_eq!(std::fs::read(&canary).unwrap(), b"precious");
    }

    #[test]
    fn a_fixture_meant_to_be_committed_asks_for_readable_modes() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        std::fs::create_dir(&bundle).unwrap();
        let root_fd = bundle::open_bundle_root(&bundle).unwrap();

        let committed = FixtureRoot::create(&temp.path().join("out")).unwrap();
        let private =
            FixtureRoot::beneath(root_fd.as_fd(), "fixture", bundle.join("fixture")).unwrap();
        // The policy itself; what lands on disk is that minus the umask,
        // which this test does not control.
        assert_eq!((committed.dir_mode, committed.file_mode), (0o755, 0o644));
        assert_eq!((private.dir_mode, private.file_mode), (0o700, 0o600));

        committed.write("sysfs/1-1/busnum", b"1\n").unwrap();
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(temp.path().join("out/sysfs")) & 0o700, 0o700);
        assert_eq!(
            mode(temp.path().join("out/sysfs/1-1/busnum")) & 0o600,
            0o600
        );
    }

    #[test]
    fn the_read_base_is_the_descriptor_with_procfs_and_never_a_bundle_path_without() {
        let temp = tempfile::tempdir().unwrap();
        let root = FixtureRoot::create(&temp.path().join("out")).unwrap();
        // procfs is mounted on every host that runs this suite.
        assert!(root.read_base().starts_with("/proc/self/fd/"));
        assert!(root.read_base().exists());

        let logical = temp.path().join("out");
        let with = read_base_for(root.fd.as_fd(), &logical, true, true).unwrap();
        assert!(with.starts_with("/proc/self/fd/"));
        // Without procfs: the CLI reads its own directory by name; a
        // bundle's fixture is refused rather than read by a swappable path.
        assert_eq!(
            read_base_for(root.fd.as_fd(), &logical, false, false).unwrap(),
            logical
        );
        let err = read_base_for(root.fd.as_fd(), &logical, false, true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn errors_name_the_directory_as_given_not_the_descriptor() {
        let temp = tempfile::tempdir().unwrap();
        let root = FixtureRoot::create(&temp.path().join("out")).unwrap();
        let base = root.read_base().display().to_string();
        let err = anyhow::anyhow!("payload found in {base}/trace.bin");

        let text = format!("{:#}", root.describe(err));
        assert!(!text.contains("/proc/self/fd"), "{text}");
        assert!(text.ends_with("/out/trace.bin"), "{text}");
    }

    #[test]
    fn beneath_pins_a_child_of_a_pinned_root_and_refuses_a_link_there() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        std::fs::create_dir(&bundle).unwrap();
        let root_fd = bundle::open_bundle_root(&bundle).unwrap();

        let fixture =
            FixtureRoot::beneath(root_fd.as_fd(), "fixture", bundle.join("fixture")).unwrap();
        fixture.write("meta.toml", b"ok").unwrap();
        assert_eq!(
            std::fs::read(bundle.join("fixture/meta.toml")).unwrap(),
            b"ok"
        );
        assert_eq!(fixture.logical(), bundle.join("fixture"));

        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, bundle.join("linked")).unwrap();
        assert!(
            FixtureRoot::beneath(root_fd.as_fd(), "linked", bundle.join("linked")).is_err(),
            "a symlink where the fixture dir should be is refused"
        );
    }
}

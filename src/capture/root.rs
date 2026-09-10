//! The pinned fixture directory. Every byte the capture core writes -- an
//! attribute file, the `usbN` and `peer` symlinks, a trace, a golden,
//! `meta.toml`, the baseline snapshot -- goes through [`FixtureRoot`],
//! relative to one directory descriptor held for the whole assembly, with
//! every path component resolved `O_NOFOLLOW`. A directory swapped for a
//! symlink after the pin, at the root or anywhere beneath it, changes
//! nothing about where the bytes land: the descriptor names an inode, not a
//! path. This is the support bundle's own pinned-root pattern
//! ([`crate::diag::bundle`]) extended to the subtree the capturer owns.

use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
#[cfg(any(feature = "capture-fixture", test))]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::config::chown_created_to_invoker;
use crate::diag::bundle;

/// A fixture directory pinned on a descriptor (see the module doc).
#[derive(Debug)]
pub struct FixtureRoot {
    fd: OwnedFd,
    /// The path the directory was named by: for messages and for the
    /// ownership decision only (see [`chown_created_to_invoker`]); no write
    /// resolves it.
    logical: PathBuf,
}

impl FixtureRoot {
    /// For `--capture-fixture <DIR>`: create `dir` when absent and pin it.
    /// The path is the invoker's own choice, so its ancestors are followed
    /// as given, once, here; from this point on nothing is. Only that
    /// feature-gated subcommand (and the tests) name a directory; `--support`
    /// pins a child of its bundle root with [`FixtureRoot::beneath`].
    #[cfg(any(feature = "capture-fixture", test))]
    pub fn create(dir: &Path) -> io::Result<FixtureRoot> {
        std::fs::create_dir_all(dir)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(dir)?;
        Ok(FixtureRoot {
            fd: file.into(),
            logical: dir.to_path_buf(),
        })
    }

    /// For `--support`: the directory `name` directly beneath an already
    /// pinned bundle root, created when absent and refused when a symlink
    /// sits there. `logical` is the bundle directory joined with `name`.
    pub fn beneath(parent_fd: BorrowedFd, name: &str, logical: PathBuf) -> io::Result<FixtureRoot> {
        Ok(FixtureRoot {
            fd: bundle::open_subdir_at(parent_fd, name)?,
            logical,
        })
    }

    /// The path this directory was named by (messages, ownership decision).
    pub fn logical(&self) -> &Path {
        &self.logical
    }

    /// The read side: `/proc/self/fd/<n>` resolves to the pinned inode
    /// whatever the logical path names by now, so the replay that
    /// generates each golden, the SEC-1 and SEC-2 re-checks, and the
    /// stale-tree check all read what was actually written. procfs is
    /// always mounted on Linux.
    pub fn read_base(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.fd.as_raw_fd()))
    }

    /// Create or truncate the file `rel` (bundle-relative, `/`-separated),
    /// creating directories on the way, and write `bytes`; the file is
    /// handed to the sudo invoker like every other bundle file.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file = bundle::create_file_at(self.fd.as_fd(), rel)?;
        file.write_all(bytes)?;
        file.flush()?;
        chown_created_to_invoker(&self.logical.join(rel), file.as_raw_fd());
        Ok(())
    }

    /// Create the directory `rel` and every directory on the way.
    pub fn mkdir_all(&self, rel: &str) -> io::Result<()> {
        bundle::mkdir_all_at(self.fd.as_fd(), rel)
    }

    /// Create the symlink `rel` -> `target`, `target` stored verbatim.
    pub fn symlink(&self, rel: &str, target: &Path) -> io::Result<()> {
        bundle::symlink_at(self.fd.as_fd(), rel, target)
    }
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

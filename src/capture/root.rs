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
    /// The path the directory was named by; no write resolves it.
    logical: PathBuf,
    /// The topmost directory this root's creation itself made, `None`
    /// when the named directory already existed. With the top-level names
    /// written below (`created`), it defines what the ownership pass at
    /// the end hands over: exactly what the run created, so a date
    /// directory made on the way to `<board>-<date>/stage<N>` goes too, and
    /// nothing that was already in a pre-existing directory does. Only the
    /// `--capture-fixture` handler (and the tests) ask; a bundle runs its
    /// own pass.
    #[cfg(any(feature = "capture-fixture", test))]
    created_top: Option<PathBuf>,
    /// The topmost entry (a bundle-relative prefix) each write through this
    /// root created, recorded the moment it existed.
    #[cfg(any(feature = "capture-fixture", test))]
    created: std::cell::RefCell<std::collections::BTreeSet<String>>,
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
    /// with [`FixtureRoot::beneath`]. Either way procfs must be mounted
    /// (see [`read_base_for`]).
    #[cfg(any(feature = "capture-fixture", test))]
    pub fn create(dir: &Path) -> io::Result<FixtureRoot> {
        // `new/../fixture` would create and record `new` while the fixture
        // lands beside it; the bundle refuses `..` in its own paths, and so
        // does the output directory.
        if dir
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the output directory {} contains `..`; give it as a plain path",
                    dir.display()
                ),
            ));
        }
        let mut created_top = None;
        let root = create_dirs_recording_top(dir, &mut created_top).and_then(|()| {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(dir)?;
            let fd: OwnedFd = file.into();
            let read_base = read_base_for(fd.as_fd(), procfs_serves(fd.as_fd()))?;
            Ok((fd, read_base))
        });
        let (fd, read_base) = match root {
            Ok(parts) => parts,
            Err(e) => {
                // Whatever this call already made is handed over before the
                // failure surfaces, so nothing root-owned is left behind.
                if let Some(top) = &created_top {
                    bundle::own_tree(top);
                }
                return Err(e);
            }
        };
        Ok(FixtureRoot {
            fd,
            read_base,
            logical: dir.to_path_buf(),
            created_top,
            created: Default::default(),
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
        let read_base = read_base_for(fd.as_fd(), procfs_serves(fd.as_fd()))?;
        Ok(FixtureRoot {
            fd,
            read_base,
            #[cfg(any(feature = "capture-fixture", test))]
            created_top: None,
            #[cfg(any(feature = "capture-fixture", test))]
            created: Default::default(),
            logical,
            display: name.to_string(),
            dir_mode: 0o700,
            file_mode: 0o600,
        })
    }

    /// The path this directory was named by.
    pub fn logical(&self) -> &Path {
        &self.logical
    }

    /// What the ownership pass at the end should hand over: the topmost
    /// directory this run created when it created one, else the topmost
    /// entry each write created inside the pre-existing directory (a
    /// directory made on the way, or the file or link itself), with an
    /// entry beneath another recorded one folded into it -- never anything
    /// that was already there.
    #[cfg(any(feature = "capture-fixture", test))]
    pub fn owned_paths(&self) -> Vec<PathBuf> {
        if let Some(top) = &self.created_top {
            return vec![top.clone()];
        }
        let created = self.created.borrow();
        created
            .iter()
            .filter(|rel| {
                !created
                    .iter()
                    .any(|other| other.len() < rel.len() && rel.starts_with(&format!("{other}/")))
            })
            .map(|rel| self.logical.join(rel))
            .collect()
    }

    /// Remember the topmost entry a write of `rel` created: the first
    /// `index + 1` components. Recorded the moment the helper reports the
    /// creation, before any later step that could fail, so a partial
    /// result is handed over too; never called for an entry that already
    /// existed or a path the helper rejected.
    #[cfg(any(feature = "capture-fixture", test))]
    fn note_created(&self, rel: &str, index: usize) {
        let prefix: Vec<&str> = rel.split('/').take(index + 1).collect();
        self.created.borrow_mut().insert(prefix.join("/"));
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

    /// The read side, fixed at construction: `/proc/self/fd/<n>`, which
    /// resolves to the pinned inode whatever the logical path names by now,
    /// so the replay that generates each golden, the SEC-1 and SEC-2
    /// re-checks, and the stale-tree check read the tree beneath the pinned
    /// root as written (by path below that root). A root cannot be built
    /// without it (see [`read_base_for`]).
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
        let mut created = None;
        let opened = bundle::create_new_file_at(
            self.fd.as_fd(),
            rel,
            self.dir_mode,
            self.file_mode,
            &mut created,
        );
        self.record(rel, created);
        let mut file = opened?;
        file.write_all(bytes)?;
        file.flush()
    }

    /// Create the directory `rel` and every directory on the way.
    pub fn mkdir_all(&self, rel: &str) -> io::Result<()> {
        let mut created = None;
        let result = bundle::mkdir_all_at(self.fd.as_fd(), rel, self.dir_mode, &mut created);
        self.record(rel, created);
        result
    }

    /// Create the symlink `rel` -> `target`, `target` stored verbatim.
    pub fn symlink(&self, rel: &str, target: &Path) -> io::Result<()> {
        let mut created = None;
        let result = bundle::symlink_at(self.fd.as_fd(), rel, target, self.dir_mode, &mut created);
        self.record(rel, created);
        result
    }

    /// Record what a helper reported creating along `rel`, whether or not
    /// the helper then succeeded. Present in every build so the callers
    /// read the same in all of them; only the `--capture-fixture` handler
    /// (and the tests) ever ask for the result.
    fn record(&self, rel: &str, created: Option<usize>) {
        #[cfg(any(feature = "capture-fixture", test))]
        if let Some(index) = created {
            self.note_created(rel, index);
        }
        #[cfg(not(any(feature = "capture-fixture", test)))]
        let _ = (rel, created);
    }
}

/// Create `dir` and any missing ancestors, top down. `top` receives the
/// topmost directory this call itself created (left `None` when every
/// ancestor and `dir` already existed), set the moment it is created so a
/// failure further down still reports it. Judged by the outcome of each
/// `mkdir`, not by an existence check beforehand, so a directory someone
/// else creates in between is never counted as ours.
#[cfg(any(feature = "capture-fixture", test))]
fn create_dirs_recording_top(dir: &Path, top: &mut Option<PathBuf>) -> io::Result<()> {
    let mut chain: Vec<&Path> = dir.ancestors().collect();
    chain.reverse();
    for ancestor in chain {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match std::fs::create_dir(ancestor) {
            Ok(()) => {
                top.get_or_insert_with(|| ancestor.to_path_buf());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Whether procfs exposes `fd` at `/proc/self/fd/<n>`.
fn procfs_serves(fd: BorrowedFd) -> bool {
    Path::new(&format!("/proc/self/fd/{}", fd.as_raw_fd())).exists()
}

/// The read base for a root: `/proc/self/fd/<n>`, the one way to read the
/// pinned inode by descriptor. Without procfs -- a chroot or container that
/// mounts `/sys` and `/dev/usbmon*` but not `/proc` -- the alternative
/// would be reading by name, which resolves whatever the path names by
/// then, so the root is refused instead: the capturer needs procfs, as
/// `--support` always did (its archive step hands `tar` a `/proc/self/fd`
/// path). Decided once, here, so readers and messages never disagree on
/// the base.
fn read_base_for(fd: BorrowedFd, procfs_available: bool) -> io::Result<PathBuf> {
    if procfs_available {
        return Ok(PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd())));
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "procfs is not mounted; the fixture capturer reads its output back through /proc/self/fd",
    ))
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
    fn create_refuses_a_parent_component_in_the_output_directory() {
        let temp = tempfile::tempdir().unwrap();
        let err = FixtureRoot::create(&temp.path().join("new/../fixture")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            !temp.path().join("new").exists(),
            "nothing may be created on the way"
        );
        assert!(!temp.path().join("fixture").exists());
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
    fn the_read_base_is_the_descriptor_with_procfs_and_refused_without() {
        let temp = tempfile::tempdir().unwrap();
        let root = FixtureRoot::create(&temp.path().join("out")).unwrap();
        // procfs is mounted on every host that runs this suite.
        assert!(root.read_base().starts_with("/proc/self/fd/"));
        assert!(root.read_base().exists());

        let with = read_base_for(root.fd.as_fd(), true).unwrap();
        assert!(with.starts_with("/proc/self/fd/"));
        // Without procfs there is no way to read the pinned inode by
        // descriptor, and reading by name would resolve whatever the path
        // names by then: refused.
        let err = read_base_for(root.fd.as_fd(), false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn the_ownership_pass_hands_over_exactly_what_the_run_created() {
        let temp = tempfile::tempdir().unwrap();
        // `<board>-<date>/stage2` where the date directory does not exist
        // yet: the pass must start at the date directory, or a root-owned
        // parent would trap the invoker's own fixture.
        let dir = temp.path().join("tgl-2026-09-12").join("stage2");
        let root = FixtureRoot::create(&dir).unwrap();
        assert_eq!(root.owned_paths(), vec![temp.path().join("tgl-2026-09-12")]);
        assert!(dir.is_dir());

        // With the parent there, the named directory is the top.
        let stage3 = temp.path().join("tgl-2026-09-12").join("stage3");
        let again = FixtureRoot::create(&stage3).unwrap();
        assert_eq!(again.owned_paths(), vec![stage3.clone()]);

        // A directory that already existed, with something else in it: only
        // what this run creates is handed over, never the rest -- not a
        // pre-existing (empty) sysfs/ that merely receives entries, only the
        // topmost entry made beneath it; not a file a write refused because
        // it was already there; not a path the helper rejected.
        let existing = temp.path().join("existing");
        std::fs::create_dir_all(existing.join("sysfs")).unwrap();
        std::fs::write(existing.join("other"), "not ours").unwrap();
        std::fs::write(existing.join("trace.bin"), "stale").unwrap();
        let reused = FixtureRoot::create(&existing).unwrap();
        assert!(reused.owned_paths().is_empty(), "nothing written yet");
        reused.mkdir_all("sysfs/1-1/ep").unwrap();
        reused.mkdir_all("sysfs/1-1/ep2").unwrap();
        reused.write("sysfs/1-1/busnum", b"1\n").unwrap();
        reused
            .symlink("sysfs/usb1", Path::new("ctrl/usb1"))
            .unwrap();
        assert!(reused.write("trace.bin", b"x").is_err());
        assert!(reused.write("./meta.toml", b"x").is_err());
        reused.write("meta.toml", b"x").unwrap();
        reused.symlink("link", Path::new("meta.toml")).unwrap();
        assert_eq!(
            reused.owned_paths(),
            vec![
                existing.join("link"),
                existing.join("meta.toml"),
                existing.join("sysfs/1-1"),
                existing.join("sysfs/usb1"),
            ],
            "the topmost entry each write created, with ep, ep2 and busnum folded into sysfs/1-1"
        );
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

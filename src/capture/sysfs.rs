//! Materialize a fixture-owned copy of /sys/bus/usb/devices. Device dirs are
//! real dirs of copied attribute files (never the host's symlinks); the one
//! symlink created is a controlled, relative `usbN` link so the controller
//! resolves the same way it does live (SEC-2).

use std::path::Path;

use anyhow::Context;

/// The attribute files usbtop-ng reads (see `device::read_metadata_from` and
/// `enumerate_present_devices`), except `serial`: a bundle is published, a
/// device serial identifies its owner's hardware, and no replay reads it, so
/// it is never copied. Nothing else is copied either.
const ATTRS: [&str; 8] = [
    "busnum",
    "devnum",
    "speed",
    "idVendor",
    "idProduct",
    "manufacturer",
    "product",
    "version",
];

pub fn materialize_sysfs(src_base: &Path, dst_sysfs: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst_sysfs)
        .with_context(|| format!("create {}", dst_sysfs.display()))?;

    for entry in std::fs::read_dir(src_base)
        .with_context(|| format!("read {}", src_base.display()))?
        .flatten()
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.contains(':') {
            continue; // an interface, not a device
        }
        let src_dir = entry.path();

        if is_root_hub(&name) {
            // Resolve the real controller (canonical parent dir name), exactly
            // as `UsbBus::update_bus_speed` does, and build a fixture-local
            // stand-in `<controller>/usbN/` plus a relative `usbN` symlink.
            match resolve_controller(&src_dir) {
                Some(controller) => {
                    let stand_in = dst_sysfs.join(&controller).join(name.as_ref());
                    copy_attrs(&src_dir, &stand_in)?;
                    let link = dst_sysfs.join(name.as_ref());
                    let target = Path::new(&controller).join(name.as_ref());
                    std::os::unix::fs::symlink(&target, &link)
                        .with_context(|| format!("symlink {}", link.display()))?;
                }
                None => {
                    // Controller unresolved at capture time (canonicalize
                    // failed outright: a dangling `usbN` symlink, ELOOP, or a
                    // path that vanished mid-scan — real sysfs never shapes it
                    // that way): materialize the root hub directly, with no
                    // symlink. On replay, `DeviceManager::update_bus_speed`
                    // still canonicalizes this now-real `usbN` dir; canonicalize
                    // doesn't require a symlink to succeed, so it resolves to
                    // itself and `.parent().file_name()` names the *enclosing
                    // sysfs directory* (e.g. "sysfs"), not the real controller
                    // and not `None`. That synthetic value is visibly not a
                    // PCI/platform id, but it is what both golden generation
                    // and test replay resolve to here, so golden==replay holds.
                    copy_attrs(&src_dir, &dst_sysfs.join(name.as_ref()))?;
                }
            }
        } else {
            copy_attrs(&src_dir, &dst_sysfs.join(name.as_ref()))?;
        }
    }
    Ok(())
}

fn is_root_hub(name: &str) -> bool {
    name.strip_prefix("usb")
        .is_some_and(|rest| rest.parse::<u8>().is_ok())
}

/// The real controller dir name for a root hub entry: canonicalize it (through
/// the host's symlink) and take its parent's file name.
fn resolve_controller(src_dir: &Path) -> Option<String> {
    let real = std::fs::canonicalize(src_dir).ok()?;
    Some(real.parent()?.file_name()?.to_string_lossy().into_owned())
}

/// Copy the known attribute files (those that exist) from `src` into a fresh
/// real dir `dst`. `fs::read` is used, not `fs::copy`, because sysfs files
/// report a 4096-byte size but return fewer bytes; `read` loops to EOF.
fn copy_attrs(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for attr in ATTRS {
        let from = src.join(attr);
        if let Ok(bytes) = std::fs::read(&from) {
            std::fs::write(dst.join(attr), &bytes)
                .with_context(|| format!("write {}", dst.join(attr).display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(dir: &Path, name: &str, attrs: &[(&str, &str)]) -> std::path::PathBuf {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        for (k, v) in attrs {
            std::fs::write(d.join(k), v).unwrap();
        }
        d
    }

    /// Build a source tree shaped like /sys/bus/usb/devices: a controller dir
    /// holding usb1's real files, a top-level `usb1` symlink into it (as sysfs
    /// has), an ordinary device `1-1`, and an interface dir `1-1:1.0`.
    fn build_src(root: &Path) {
        dev(
            &root.join("real"),
            "0000:00:14.0/usb1",
            &[("busnum", "1\n"), ("devnum", "1\n"), ("speed", "480\n")],
        );
        std::os::unix::fs::symlink(
            root.join("real/0000:00:14.0/usb1"),
            root.join("devices/usb1"),
        )
        .unwrap();
        dev(
            &root.join("devices"),
            "1-1",
            &[("busnum", "1\n"), ("devnum", "3\n"), ("idVendor", "0430\n")],
        );
        std::fs::create_dir_all(root.join("devices/1-1:1.0")).unwrap(); // interface, skipped
    }

    #[test]
    fn materializes_devices_and_the_single_relative_controller_symlink() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("devices")).unwrap();
        build_src(temp.path());
        let dst = temp.path().join("bundle").join("sysfs");
        materialize_sysfs(&temp.path().join("devices"), &dst).unwrap();

        // The ordinary device is a real dir of copied attributes.
        assert_eq!(
            std::fs::read_to_string(dst.join("1-1/devnum")).unwrap(),
            "3\n"
        );
        assert!(!dst.join("1-1:1.0").exists(), "interface dir dropped");

        // usb1 is the ONLY symlink, relative, into the controller stand-in.
        let link = std::fs::symlink_metadata(dst.join("usb1")).unwrap();
        assert!(link.file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(dst.join("usb1")).unwrap(),
            Path::new("0000:00:14.0/usb1")
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("0000:00:14.0/usb1/speed")).unwrap(),
            "480\n"
        );

        // No other symlink anywhere under sysfs/.
        assert!(!std::fs::symlink_metadata(dst.join("1-1"))
            .unwrap()
            .file_type()
            .is_symlink());

        // Controller resolves through a manager pointed at the bundle.
        let mut mgr = crate::device::manager::DeviceManager::with_sysfs_base(dst.clone());
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        assert_eq!(mgr.buses[&1].controller.as_deref(), Some("0000:00:14.0"));
    }

    /// `resolve_controller` returns `None` only when canonicalizing `usbN`
    /// itself fails (e.g. a dangling symlink) — a *plain real* `usbN` dir
    /// canonicalizes trivially and takes the `Some` branch instead (with the
    /// enclosing source dir's own name as a bogus "controller"), so a
    /// dangling symlink is the deterministic, portable way to hit the
    /// fallback. A same-bus ordinary device (`1-1`) is included so
    /// `enumerate_present_devices` still discovers bus 1 from *its* attrs —
    /// the broken `usb1` entry itself can carry no attrs, since reading
    /// through a dangling symlink fails the same way canonicalizing it does.
    ///
    /// This pins the *true* replay behavior: `DeviceManager::update_bus_speed`
    /// canonicalizes the now-real, unlinked `usbN` dir at replay time; that
    /// canonicalize needs no symlink to succeed, so it resolves to itself,
    /// and `.parent().file_name()` names the *enclosing sysfs directory*
    /// (here "sysfs") — a visibly synthetic value, not the real controller
    /// and not `None`.
    #[test]
    fn unresolved_controller_falls_back_to_a_real_dir_whose_replayed_controller_is_the_enclosing_dir_name(
    ) {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("devices");
        std::fs::create_dir_all(&src).unwrap();
        // A dangling symlink: canonicalize (and any read through it) fails.
        std::os::unix::fs::symlink(src.join("nonexistent-controller"), src.join("usb1")).unwrap();
        // An ordinary same-bus device so `enumerate_present_devices` still
        // finds bus 1 despite `usb1` itself carrying no readable attrs.
        dev(
            &src,
            "1-1",
            &[("busnum", "1\n"), ("devnum", "3\n"), ("idVendor", "0430\n")],
        );

        // Destination named "sysfs" so the enclosing-dir-name assertion below
        // has a known, checkable value.
        let dst = temp.path().join("sysfs");
        materialize_sysfs(&src, &dst).unwrap();

        // usb1 is a real (empty) dir, not a symlink: the fallback fired.
        let meta = std::fs::symlink_metadata(dst.join("usb1")).unwrap();
        assert!(!meta.file_type().is_symlink());
        assert!(meta.file_type().is_dir());

        // No controller stand-in dir was created: the only entries under dst
        // are the ordinary device and usb1 itself, nothing else.
        let mut entries: Vec<_> = std::fs::read_dir(&dst)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                std::ffi::OsString::from("1-1"),
                std::ffi::OsString::from("usb1")
            ]
        );

        // Replay resolves the controller to the enclosing sysfs dir's own
        // basename, not None and not a real controller id.
        let mut mgr = crate::device::manager::DeviceManager::with_sysfs_base(dst.clone());
        mgr.enumerate_present_devices();
        mgr.update_bus_speeds();
        assert_eq!(mgr.buses[&1].controller.as_deref(), Some("sysfs"));
    }
}

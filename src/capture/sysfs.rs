//! Materialize a fixture-owned copy of /sys/bus/usb/devices. Device dirs are
//! real dirs of copied attribute files (never the host's symlinks); the one
//! symlink created is a controlled, relative `usbN` link so the controller
//! resolves the same way it does live (SEC-2).

use std::path::Path;

use anyhow::Context;

/// The attribute files usbtop-ng reads (see `device::read_metadata_from` and
/// `enumerate_present_devices`). Nothing else is copied.
const ATTRS: [&str; 9] = [
    "busnum",
    "devnum",
    "speed",
    "idVendor",
    "idProduct",
    "manufacturer",
    "product",
    "serial",
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
                    // Controller unresolved (e.g. a source tree with no symlink):
                    // materialize the root hub directly; its controller stays null.
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
}

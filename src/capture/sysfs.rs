//! Materialize a fixture-owned copy of /sys/bus/usb/devices. Device dirs are
//! real dirs of copied attribute files (never the host's symlinks); the
//! symlinks created are the relative `usbN` controller link, so the
//! controller resolves the same way it does live, and the relative hub port
//! `peer` links, both inside the bundle (SEC-2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::capture::FixtureRoot;

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

/// The port attribute files copied per hub port (see `copy_ports`): the
/// firmware's connectability claim and its ACPI position value, neither of
/// which identifies the host. `physical_location/`, `device`, `connector`,
/// `state`, and the rest are not copied; the `peer` link is rebuilt as a
/// relative in-bundle link once every port is known.
const PORT_ATTRS: [&str; 2] = ["connect_type", "location"];

pub fn materialize_sysfs(src_base: &Path, out: &FixtureRoot, dst_rel: &str) -> anyhow::Result<()> {
    out.mkdir_all(dst_rel)
        .with_context(|| format!("create {}", out.logical().join(dst_rel).display()))?;

    // Every copied port's bundle directory (bundle-relative) by port name,
    // and every `peer` seen as (port, peer name); the links are written
    // last, relative, once both ends' bundle paths are known.
    let mut port_dirs: BTreeMap<String, String> = BTreeMap::new();
    let mut peers: Vec<(String, String)> = Vec::new();

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

        let dst_dir = if is_root_hub(&name) {
            // Resolve the real controller (canonical parent dir name), exactly
            // as `UsbBus::update_bus_speed` does, and build a fixture-local
            // stand-in `<controller>/usbN/` plus a relative `usbN` symlink.
            match resolve_controller(&src_dir) {
                Some(controller) => {
                    let stand_in = format!("{dst_rel}/{controller}/{name}");
                    copy_attrs(&src_dir, out, &stand_in)?;
                    let link = format!("{dst_rel}/{name}");
                    let target = Path::new(&controller).join(name.as_ref());
                    out.symlink(&link, &target)
                        .with_context(|| format!("symlink {link}"))?;
                    stand_in
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
                    let plain = format!("{dst_rel}/{name}");
                    copy_attrs(&src_dir, out, &plain)?;
                    plain
                }
            }
        } else {
            let plain = format!("{dst_rel}/{name}");
            copy_attrs(&src_dir, out, &plain)?;
            plain
        };
        copy_ports(&src_dir, &name, out, &dst_dir, &mut port_dirs, &mut peers)?;
    }

    for (port, peer) in peers {
        let (Some(from), Some(to)) = (port_dirs.get(&port), port_dirs.get(&peer)) else {
            continue; // the peer is not in this bundle: no link
        };
        if from == to {
            // A malformed source tree whose `peer` names the port itself:
            // `relative_path` would produce an empty target and the symlink
            // would fail, aborting the whole bundle. The kernel never links
            // a port to itself (`link_peers` pairs two distinct ports), so
            // there is nothing to record -- skip the pair.
            continue;
        }
        let link = format!("{from}/peer");
        out.symlink(&link, &relative_path(Path::new(from), Path::new(to)))
            .with_context(|| format!("symlink {link}"))?;
    }
    Ok(())
}

/// Copy the hub port objects under `src_dev`'s interface directories into
/// the bundle directory `dst_dev`, keeping the `<interface>/<hub>-port<N>/`
/// shape with exactly [`PORT_ATTRS`] inside each (those that exist).
/// Records each port's bundle directory and the name its `peer` link
/// points at.
fn copy_ports(
    src_dev: &Path,
    hub: &str,
    out: &FixtureRoot,
    dst_dev: &str,
    port_dirs: &mut BTreeMap<String, String>,
    peers: &mut Vec<(String, String)>,
) -> anyhow::Result<()> {
    let Ok(children) = std::fs::read_dir(src_dev) else {
        return Ok(());
    };
    let prefix = format!("{hub}-port");
    for interface in children.flatten() {
        let interface_name = interface.file_name();
        let interface_name = interface_name.to_string_lossy();
        if !interface_name.contains(':') {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(interface.path()) else {
            continue;
        };
        for port in entries.flatten() {
            let port_name = port.file_name().to_string_lossy().into_owned();
            if port_name
                .strip_prefix(&prefix)
                .and_then(|n| n.parse::<u32>().ok())
                .is_none()
            {
                continue;
            }
            let dst_port = format!("{dst_dev}/{interface_name}/{port_name}");
            copy_named(&port.path(), out, &dst_port, &PORT_ATTRS)?;
            if let Some(peer) = std::fs::read_link(port.path().join("peer"))
                .ok()
                .and_then(|t| t.file_name().map(|f| f.to_string_lossy().into_owned()))
            {
                peers.push((port_name.clone(), peer));
            }
            port_dirs.insert(port_name, dst_port);
        }
    }
    Ok(())
}

/// `to` relative to the directory `from`, by lexical components: strip the
/// common prefix, one `..` per remaining `from` component, then the rest of
/// `to`. Both are bundle paths this module built, so no symlink is involved
/// and the result is the kernel's own `../../../<hub>/<iface>/<port>` shape.
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut rel = PathBuf::new();
    for _ in common..from.len() {
        rel.push("..");
    }
    for component in &to[common..] {
        rel.push(component);
    }
    rel
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

/// Copy a device's known attribute files (those that exist) into the fresh
/// bundle dir `dst`.
fn copy_attrs(src: &Path, out: &FixtureRoot, dst: &str) -> anyhow::Result<()> {
    copy_named(src, out, dst, &ATTRS)
}

/// Copy the named attribute files (those that exist) from `src` into the
/// fresh bundle dir `dst`, through the pinned root. `fs::read` is used, not
/// `fs::copy`, because sysfs files report a 4096-byte size but return fewer
/// bytes; `read` loops to EOF.
fn copy_named(src: &Path, out: &FixtureRoot, dst: &str, attrs: &[&str]) -> anyhow::Result<()> {
    out.mkdir_all(dst)
        .with_context(|| format!("create {dst}"))?;
    for attr in attrs {
        let from = src.join(attr);
        if let Ok(bytes) = std::fs::read(&from) {
            out.write(&format!("{dst}/{attr}"), &bytes)
                .with_context(|| format!("write {dst}/{attr}"))?;
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
        materialize_sysfs(
            &temp.path().join("devices"),
            &FixtureRoot::create(&temp.path().join("bundle")).unwrap(),
            "sysfs",
        )
        .unwrap();

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
        materialize_sysfs(&src, &FixtureRoot::create(temp.path()).unwrap(), "sysfs").unwrap();

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

    /// `build_src` plus hub port objects: usb1 (bus 1) and usb2 (bus 2) on
    /// the same controller, root ports 1 paired, hub `1-1`/`2-1` halves with
    /// paired port 1, `usb1-port2` alone, and one port whose peer names a
    /// port no bundle will hold.
    fn build_src_with_ports(root: &Path) {
        build_src(root);
        let ctrl = root.join("real/0000:00:14.0");
        dev(
            &ctrl,
            "usb2",
            &[("busnum", "2\n"), ("devnum", "1\n"), ("speed", "5000\n")],
        );
        std::os::unix::fs::symlink(ctrl.join("usb2"), root.join("devices/usb2")).unwrap();
        dev(
            &root.join("devices"),
            "2-1",
            &[("busnum", "2\n"), ("devnum", "3\n")],
        );
        let port = |dir: &Path, hub: &str, n: u32, attrs: &[(&str, &str)]| {
            let p = dir
                .join(format!("{hub}:1.0"))
                .join(format!("{hub}-port{n}"));
            std::fs::create_dir_all(&p).unwrap();
            for (k, v) in attrs {
                std::fs::write(p.join(k), v).unwrap();
            }
            p
        };
        let pair = |a: &Path, b: &Path| {
            std::os::unix::fs::symlink(b, a.join("peer")).unwrap();
            std::os::unix::fs::symlink(a, b.join("peer")).unwrap();
        };
        let u1p1 = port(
            &ctrl.join("usb1"),
            "usb1",
            1,
            &[
                ("connect_type", "hotplug\n"),
                ("location", "0x80000001\n"),
                ("state", "enabled\n"),
            ],
        );
        std::fs::create_dir_all(u1p1.join("physical_location")).unwrap();
        std::fs::write(u1p1.join("physical_location/panel"), "top\n").unwrap();
        let u2p1 = port(
            &ctrl.join("usb2"),
            "usb2",
            1,
            &[("connect_type", "hotplug\n"), ("location", "0x80000001\n")],
        );
        pair(&u1p1, &u2p1);
        port(
            &ctrl.join("usb1"),
            "usb1",
            2,
            &[("connect_type", "hardwired\n")],
        );
        let h1p1 = port(
            &root.join("devices/1-1"),
            "1-1",
            1,
            &[("connect_type", "unknown\n")],
        );
        let h2p1 = port(
            &root.join("devices/2-1"),
            "2-1",
            1,
            &[("connect_type", "unknown\n")],
        );
        pair(&h1p1, &h2p1);
        // A peer that names a port outside this tree: no link must be written.
        let h1p2 = port(&root.join("devices/1-1"), "1-1", 2, &[]);
        std::os::unix::fs::symlink(root.join("elsewhere/9-1:1.0/9-1-port2"), h1p2.join("peer"))
            .unwrap();
    }

    #[test]
    fn copies_hub_port_objects_with_relative_reciprocal_peer_links() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("devices")).unwrap();
        build_src_with_ports(temp.path());
        let dst = temp.path().join("bundle").join("sysfs");
        materialize_sysfs(
            &temp.path().join("devices"),
            &FixtureRoot::create(&temp.path().join("bundle")).unwrap(),
            "sysfs",
        )
        .unwrap();

        // Root-hub ports live under the controller stand-in, exactly the
        // attribute files allowed and nothing else.
        let u1p1 = dst.join("0000:00:14.0/usb1/usb1:1.0/usb1-port1");
        assert_eq!(
            std::fs::read_to_string(u1p1.join("connect_type")).unwrap(),
            "hotplug\n"
        );
        assert_eq!(
            std::fs::read_to_string(u1p1.join("location")).unwrap(),
            "0x80000001\n"
        );
        assert!(!u1p1.join("state").exists());
        assert!(!u1p1.join("physical_location").exists());
        let mut entries: Vec<String> = std::fs::read_dir(&u1p1)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["connect_type", "location", "peer"]);

        // The peer link is relative, in the kernel's own shape, and resolves
        // inside the bundle to the companion's directory.
        assert_eq!(
            std::fs::read_link(u1p1.join("peer")).unwrap(),
            Path::new("../../../usb2/usb2:1.0/usb2-port1")
        );
        assert_eq!(
            std::fs::canonicalize(u1p1.join("peer")).unwrap(),
            std::fs::canonicalize(dst.join("0000:00:14.0/usb2/usb2:1.0/usb2-port1")).unwrap()
        );
        let h1p1 = dst.join("1-1/1-1:1.0/1-1-port1");
        assert_eq!(
            std::fs::read_link(h1p1.join("peer")).unwrap(),
            Path::new("../../../2-1/2-1:1.0/2-1-port1")
        );
        assert_eq!(
            std::fs::read_link(dst.join("2-1/2-1:1.0/2-1-port1/peer")).unwrap(),
            Path::new("../../../1-1/1-1:1.0/1-1-port1")
        );

        // No peer, no link; unknown peer, no link.
        assert!(!dst
            .join("0000:00:14.0/usb1/usb1:1.0/usb1-port2/peer")
            .exists());
        assert!(dst.join("1-1/1-1:1.0/1-1-port2").is_dir());
        assert!(std::fs::symlink_metadata(dst.join("1-1/1-1:1.0/1-1-port2/peer")).is_err());

        // The bundle is still contained (the reciprocal links are a cycle the
        // walk must terminate on), and the connector index pairs through it.
        crate::capture::assert_sysfs_contained(&dst).unwrap();
        let index = crate::connector::PortIndex::scan(&dst);
        let (own, peer) = index.connector_of("1-1").unwrap();
        assert_eq!(own.name, "usb1-port1");
        assert_eq!(peer.map(|p| p.name).as_deref(), Some("usb2-port1"));
        assert_eq!(
            index
                .connector_of("2-1.1")
                .unwrap()
                .1
                .map(|p| p.name)
                .as_deref(),
            Some("1-1-port1")
        );
    }

    #[test]
    fn a_port_whose_peer_names_itself_gets_no_link() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("devices")).unwrap();
        build_src(temp.path());
        // Malformed source: the `peer` link points at the port's own
        // directory, so its last component is the port's own name. Nothing
        // the kernel writes, but a hand-edited or truncated bundle can be
        // shaped this way and it must not abort the materialization.
        let port_dir = temp.path().join("devices/1-1/1-1:1.0/1-1-port1");
        std::fs::create_dir_all(&port_dir).unwrap();
        std::fs::write(port_dir.join("connect_type"), "hotplug\n").unwrap();
        std::os::unix::fs::symlink(&port_dir, port_dir.join("peer")).unwrap();

        let dst = temp.path().join("bundle").join("sysfs");
        materialize_sysfs(
            &temp.path().join("devices"),
            &FixtureRoot::create(&temp.path().join("bundle")).unwrap(),
            "sysfs",
        )
        .unwrap();

        let out = dst.join("1-1/1-1:1.0/1-1-port1");
        assert_eq!(
            std::fs::read_to_string(out.join("connect_type")).unwrap(),
            "hotplug\n",
            "the port and its attributes are still copied"
        );
        let entries: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("connect_type")],
            "no self-peer link is written"
        );
    }

    #[test]
    fn relative_path_walks_up_then_down() {
        assert_eq!(
            relative_path(
                Path::new("/b/3-1/3-1:1.0/3-1-port4"),
                Path::new("/b/4-1/4-1:1.0/4-1-port4")
            ),
            Path::new("../../../4-1/4-1:1.0/4-1-port4")
        );
        assert_eq!(
            relative_path(
                Path::new("/b/c/usb3/3-0:1.0/usb3-port1"),
                Path::new("/b/c/usb4/4-0:1.0/usb4-port1")
            ),
            Path::new("../../../usb4/4-0:1.0/usb4-port1")
        );
        assert_eq!(
            relative_path(Path::new("/b/x"), Path::new("/b/x")),
            Path::new("")
        );
    }
}

//! What the two readers of the PCI side of a Thunderbolt or USB4 tunnel
//! share: the tunneled-device rows (`tunnel`) and the support bundle's
//! `inventory/pci-removable.toml` (`diag::inventory`). The shape of a PCI
//! address, the bridges above a device, and the rule for reading its link.

use std::path::Path;

/// `0000:2e:00.0`: a domain of four hex digits or more (VMD and other
/// synthetic domains go past `ffff`), then a bus, a slot and a function.
pub fn is_address(name: &str) -> bool {
    let hex = |s: &str, min: usize, max: usize| {
        (min..=max).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
    };
    let mut parts = name.split(':');
    let (Some(domain), Some(bus), Some(rest), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Some((slot, func)) = rest.split_once('.') else {
        return false;
    };
    hex(domain, 4, 8) && hex(bus, 2, 2) && hex(slot, 2, 2) && hex(func, 1, 1)
}

/// The PCI addresses above `real` (a resolved device directory) in the
/// device tree, root-first: for a dock's xHCI, its root port and then the
/// dock's two bridges. Empty for a device on the root bus.
pub fn chain(real: &Path) -> Vec<String> {
    let mut chain: Vec<String> = real
        .ancestors()
        .skip(1)
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| is_address(n))
        .collect();
    chain.reverse();
    chain
}

/// `power/runtime_status` of `real`, trimmed; `None` when absent or
/// unreadable. The value [`is_awake`] gates the link reads on; the
/// citation is there.
pub fn runtime_status(real: &Path) -> Option<String> {
    std::fs::read_to_string(real.join("power/runtime_status"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// The wake gate over a `power/runtime_status` value: only `active` is
/// awake; `suspended`, `unsupported`, anything else and an absent file are
/// not. Callers trim the value first.
///
/// `current_link_speed`, `current_link_width` and `max_link_width` are
/// read by the kernel through `pci_config_pm_runtime_get`
/// (drivers/pci/pci.c v7.0: a runtime-PM reference on the parent, a
/// barrier for a suspend in progress, and a resume for a device in
/// D3cold; the sysfs readers in drivers/pci/pci-sysfs.c call it), and
/// `max_link_speed` reads Link Capabilities 2 from config space with no
/// reference at all up to v6.12, which for a device in D3cold is a read of
/// a powered-down device and a meaningless value (v6.13 caches the
/// supported speeds at probe). Both readers open those attributes only
/// while this holds. The check and the read are not atomic: a device that
/// suspends between them is resumed once by the kernel and suspends again
/// on its own. The gate is best-effort, and a reader without it would keep
/// every device it described out of D3cold for as long as it ran.
pub fn is_awake(status: Option<&str>) -> bool {
    status == Some("active")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_lists_the_addresses_above_a_device_root_first() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp
            .path()
            .join("sys/devices/pci0000:00/0000:00:07.1/0000:2c:00.0/0000:2d:00.0/0000:2e:00.0");
        std::fs::create_dir_all(&real).unwrap();
        assert_eq!(
            chain(&real),
            ["0000:00:07.1", "0000:2c:00.0", "0000:2d:00.0"]
        );
        let on_the_root_bus = temp.path().join("sys/devices/pci0000:00/0000:00:0d.2");
        std::fs::create_dir_all(&on_the_root_bus).unwrap();
        assert!(chain(&on_the_root_bus).is_empty());
    }

    #[test]
    fn only_active_is_awake() {
        assert!(is_awake(Some("active")));
        assert!(!is_awake(Some("suspended")));
        assert!(!is_awake(Some("unsupported")));
        assert!(!is_awake(Some("active\n")), "callers trim before asking");
        assert!(!is_awake(None));
    }

    #[test]
    fn runtime_status_is_read_trimmed() {
        let temp = tempfile::tempdir().unwrap();
        let dev = temp.path().join("dev");
        std::fs::create_dir_all(dev.join("power")).unwrap();
        assert_eq!(runtime_status(&dev), None);
        std::fs::write(dev.join("power/runtime_status"), "suspended\n").unwrap();
        assert_eq!(runtime_status(&dev).as_deref(), Some("suspended"));
        assert!(!is_awake(runtime_status(&dev).as_deref()));
        std::fs::write(dev.join("power/runtime_status"), "active\n").unwrap();
        assert!(is_awake(runtime_status(&dev).as_deref()));
    }

    #[test]
    fn pci_addresses_allow_wide_domains_and_nothing_else() {
        for ok in [
            "0000:00:07.1",
            "0001:2e:00.0",
            "10000:e0:1d.0",
            "ffffffff:00:00.7",
        ] {
            assert!(is_address(ok), "{ok}");
        }
        for bad in [
            "pci0000:00",
            "0000:00:07.1:pcie004",
            "usb3",
            "0000:00:07",
            "000:00:07.1",
            "0000:0:07.1",
            "0000:00:07.10",
            "domain0",
        ] {
            assert!(!is_address(bad), "{bad}");
        }
    }
}

//! usb.ids database: VID and PID to name resolution, lsusb parity.
//! Format (see the file's own header): `VVVV  vendor name` at column 0,
//! `\tPPPP  product name` under it. Single-letter section headers (`C `,
//! `AT `, `HID ` and the rest) end the vendor list — everything from the
//! first such line on is class data usbtop-ng does not use.

// Nothing in this module has a production caller yet: Task 2 wires
// `resolve_database` and `UsbIds::{vendor_name,product_name}` into device
// naming. `-D warnings` treats every item below as dead code from main()
// until that lands, so each is `cfg(test)`-gated per the repo's
// SpeedIndicator::get_description idiom (src/device/mod.rs); Task 2 lifts
// these gates.
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
struct Vendor {
    name: String,
    products: HashMap<u16, String>,
}

#[cfg(test)]
pub struct UsbIds {
    vendors: HashMap<u16, Vendor>,
}

#[cfg(test)]
impl UsbIds {
    pub fn parse(text: &str) -> UsbIds {
        let mut vendors: HashMap<u16, Vendor> = HashMap::new();
        let mut current: Option<u16> = None;
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('\t') {
                // A second tab is an interface line under a class section;
                // `current` is None past the vendor list, so both are skipped.
                if rest.starts_with('\t') {
                    continue;
                }
                let (Some(vid), Some((pid, name))) = (current, split_id_name(rest)) else {
                    continue;
                };
                if let Some(vendor) = vendors.get_mut(&vid) {
                    vendor.products.insert(pid, name.to_string());
                }
                continue;
            }
            match split_id_name(line) {
                Some((id, name)) => {
                    current = Some(id);
                    vendors.insert(
                        id,
                        Vendor {
                            name: name.to_string(),
                            products: HashMap::new(),
                        },
                    );
                }
                // A non-hex column-0 line (class and section headers) ends
                // the vendor list: without this, a `C 03` section's product
                // lines would attach to the last real vendor.
                None => current = None,
            }
        }
        UsbIds { vendors }
    }

    pub fn load(path: &Path) -> std::io::Result<UsbIds> {
        Ok(UsbIds::parse(&std::fs::read_to_string(path)?))
    }

    pub fn vendor_count(&self) -> usize {
        self.vendors.len()
    }

    pub fn vendor_name(&self, vid: u16) -> Option<&str> {
        self.vendors.get(&vid).map(|v| v.name.as_str())
    }

    pub fn product_name(&self, vid: u16, pid: u16) -> Option<&str> {
        self.vendors
            .get(&vid)?
            .products
            .get(&pid)
            .map(String::as_str)
    }
}

/// Split `VVVV  name`: exactly 4 hex digits, whitespace, a non-empty name.
#[cfg(test)]
fn split_id_name(line: &str) -> Option<(u16, &str)> {
    let (id, name) = line.split_at_checked(4)?;
    let id = u16::from_str_radix(id, 16).ok()?;
    let name = name.trim();
    (!name.is_empty()).then_some((id, name))
}

/// The distro-packaged locations, Debian and Ubuntu first, then the
/// hwdata path Fedora and openSUSE use (Ubuntu symlinks it to the first).
#[cfg(test)]
const DISTRO_PATHS: [&str; 2] = ["/usr/share/misc/usb.ids", "/usr/share/hwdata/usb.ids"];

/// First source that loads wins: CLI flag, preferences key, the downloaded
/// copy, then the distro files. A source that exists but cannot be read or
/// parsed logs one warning and falls through. None when nothing loads.
#[cfg(test)]
pub fn resolve_database(
    cli_path: Option<&Path>,
    pref_path: Option<&Path>,
    home_copy: &Path,
) -> Option<UsbIds> {
    let distro = DISTRO_PATHS.map(Path::new);
    let mut chain: Vec<&Path> = Vec::new();
    chain.extend(cli_path);
    chain.extend(pref_path);
    chain.push(home_copy);
    chain.extend(distro);
    resolve_from_chain(&chain)
}

#[cfg(test)]
fn resolve_from_chain(paths: &[&Path]) -> Option<UsbIds> {
    for path in paths {
        if !path.exists() {
            continue;
        }
        match UsbIds::load(path) {
            Ok(db) => {
                log::debug!("usb.ids loaded from {}", path.display());
                return Some(db);
            }
            Err(e) => log::warn!("could not read {}: {e}", path.display()),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# Date:    2024-03-18 20:34:02
#
0430  Fujitsu Component Limited
\t0100  3-button Mouse
\t0a02  Keyboard
05e3  Genesys Logic, Inc.
\t0610  Hub
1a6e  Global Unichip Corp.
garbage line that is not a vendor
ffff  Last Vendor
C 03  HID (Human Interface Device)
\t01  Boot Interface Subclass
";

    #[test]
    fn parses_vendors_and_products() {
        let db = UsbIds::parse(FIXTURE);
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
        assert_eq!(db.product_name(0x0430, 0x0100), Some("3-button Mouse"));
        assert_eq!(db.product_name(0x0430, 0x0a02), Some("Keyboard"));
        assert_eq!(db.vendor_name(0x1a6e), Some("Global Unichip Corp."));
        assert_eq!(
            db.product_name(0x1a6e, 0x089a),
            None,
            "vendor listed, product not"
        );
        assert_eq!(db.vendor_name(0x9999), None);
        assert_eq!(db.vendor_count(), 4);
    }

    #[test]
    fn class_sections_and_garbage_are_ignored() {
        let db = UsbIds::parse(FIXTURE);
        // "C 03" must not be read as vendor 0xC03 or 0x03.
        assert_eq!(db.vendor_name(0x0c03), None);
        assert_eq!(db.vendor_name(0x0003), None);
        // The interface line under it must not become a product of any vendor.
        assert_eq!(db.product_name(0xffff, 0x0001), None);
    }

    #[test]
    fn parse_stops_attributing_products_after_the_class_section_starts() {
        let db = UsbIds::parse("0001  V\nC 03  HID\n\t0100  NotAProduct\n");
        assert_eq!(db.product_name(0x0001, 0x0100), None);
    }

    #[test]
    fn chain_returns_the_first_source_that_loads() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.ids");
        let good = temp.path().join("good.ids");
        std::fs::write(&good, "0430  Fujitsu Component Limited\n").unwrap();
        let later = temp.path().join("later.ids");
        std::fs::write(&later, "0001  Wrong Winner\n").unwrap();

        let db = resolve_from_chain(&[&missing, &good, &later]).expect("good.ids loads");
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
        assert_eq!(
            db.vendor_name(0x0001),
            None,
            "later sources must not merge in"
        );
    }

    #[test]
    fn chain_with_no_readable_source_is_none() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve_from_chain(&[&temp.path().join("nope.ids")]).is_none());
    }

    #[test]
    fn resolve_database_prefers_the_cli_path_over_the_rest_of_the_chain() {
        // A hermetic exercise of the public entry point itself (the chain
        // logic is covered above via resolve_from_chain): the CLI path
        // loads and wins on the first iteration, so the distro constants
        // are never `.exists()`-checked and /usr/share is never touched.
        let temp = tempfile::tempdir().unwrap();
        let cli = temp.path().join("cli.ids");
        std::fs::write(&cli, "0430  Fujitsu Component Limited\n").unwrap();
        let home_copy = temp.path().join("home.ids");

        let db = resolve_database(Some(&cli), None, &home_copy).expect("cli path loads");
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
    }
}

//! `--filter` expressions: KEY=VALUE pairs joined by commas AND together,
//! repeated flags OR together, and an empty set matches everything.

use anyhow::{anyhow, Result};

use crate::device::UsbDevice;
use crate::usbmon::parser::{TransferType, UsbPacket};

/// One `--filter` argument, parsed. Every populated key must hold.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterExpr {
    bus: Option<u8>,
    dev: Option<u8>,
    vid: Option<u16>,
    pid: Option<u16>,
    /// Lowercased at parse time; matched against vendor and product.
    name: Option<String>,
    ep: Option<u8>,
    dir_in: Option<bool>,
    transfer: Option<TransferType>,
    internal: Option<bool>,
}

impl FilterExpr {
    fn parse(raw: &str) -> Result<Self> {
        let mut expr = FilterExpr::default();
        for pair in raw.split(',') {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| anyhow!("filter term '{pair}' is not KEY=VALUE"))?;
            match key {
                "bus" => set_once(&mut expr.bus, key, parse_u8(key, value)?)?,
                "dev" => set_once(&mut expr.dev, key, parse_u8(key, value)?)?,
                "vid" => set_once(&mut expr.vid, key, parse_hex16(key, value)?)?,
                "pid" => set_once(&mut expr.pid, key, parse_hex16(key, value)?)?,
                "id" => {
                    let (vid, pid) = value
                        .split_once(':')
                        .ok_or_else(|| anyhow!("filter id '{value}' is not VID:PID"))?;
                    set_once(&mut expr.vid, "vid", parse_hex16("id", vid)?)?;
                    set_once(&mut expr.pid, "pid", parse_hex16("id", pid)?)?;
                }
                "name" => set_once(&mut expr.name, key, value.to_lowercase())?,
                "ep" => {
                    let ep = parse_u8(key, value)?;
                    if ep > 15 {
                        return Err(anyhow!("filter ep is 0 through 15, got {ep}"));
                    }
                    set_once(&mut expr.ep, key, ep)?;
                }
                "dir" => {
                    let dir = match value {
                        "in" => true,
                        "out" => false,
                        _ => return Err(anyhow!("filter dir is 'in' or 'out', got '{value}'")),
                    };
                    set_once(&mut expr.dir_in, key, dir)?;
                }
                "type" => {
                    let t = match value {
                        "control" | "ctrl" => TransferType::Control,
                        "iso" => TransferType::Isochronous,
                        "bulk" => TransferType::Bulk,
                        "interrupt" | "int" => TransferType::Interrupt,
                        _ => {
                            return Err(anyhow!(
                                "filter type is control, iso, bulk, or interrupt, got '{value}'"
                            ))
                        }
                    };
                    set_once(&mut expr.transfer, key, t)?;
                }
                "internal" => {
                    let value = match value {
                        "yes" | "true" => true,
                        "no" | "false" => false,
                        _ => {
                            return Err(anyhow!("filter internal is 'yes' or 'no', got '{value}'"))
                        }
                    };
                    set_once(&mut expr.internal, key, value)?;
                }
                _ => return Err(anyhow!("unknown filter key '{key}'")),
            }
        }
        Ok(expr)
    }

    /// Identity keys only: does this device belong on screen at all.
    fn matches_device(&self, device: &UsbDevice) -> bool {
        self.bus.is_none_or(|bus| bus == device.bus_id)
            && self.dev.is_none_or(|dev| dev == device.device_id)
            && self.vid.is_none_or(|vid| device.vendor_id == Some(vid))
            && self.pid.is_none_or(|pid| device.product_id == Some(pid))
            && self.name.as_ref().is_none_or(|needle| {
                let hit = |field: &Option<String>| {
                    field
                        .as_ref()
                        .is_some_and(|s| s.to_lowercase().contains(needle))
                };
                hit(&device.vendor) || hit(&device.product)
            })
            && self.internal.is_none_or(|want| want == device.is_internal)
    }

    /// Every key: identity against the device, ep/dir/type against the packet.
    fn matches_packet(&self, packet: &UsbPacket, device: &UsbDevice) -> bool {
        self.matches_device(device)
            && self.ep.is_none_or(|ep| ep == packet.endpoint)
            && self.dir_in.is_none_or(|dir| dir == packet.direction)
            && self
                .transfer
                .is_none_or(|t| packet.transfer_type == Some(t))
    }
}

fn set_once<T>(slot: &mut Option<T>, key: &str, value: T) -> Result<()> {
    if slot.is_some() {
        return Err(anyhow!(
            "filter key '{key}' appears twice in one expression"
        ));
    }
    *slot = Some(value);
    Ok(())
}

fn parse_u8(key: &str, value: &str) -> Result<u8> {
    value
        .parse()
        .map_err(|_| anyhow!("filter {key} '{value}' is not a number 0 through 255"))
}

fn parse_hex16(key: &str, value: &str) -> Result<u16> {
    if value.len() != 4 {
        return Err(anyhow!("filter {key} '{value}' is not 4 hex digits"));
    }
    u16::from_str_radix(value, 16)
        .map_err(|_| anyhow!("filter {key} '{value}' is not 4 hex digits"))
}

/// Every `--filter` argument, ORed. Empty means match-all.
#[derive(Debug, Clone, Default)]
pub struct FilterSet {
    pub(crate) exprs: Vec<FilterExpr>,
}

impl FilterSet {
    pub fn parse(args: &[String]) -> Result<Self> {
        let exprs = args
            .iter()
            .map(|raw| FilterExpr::parse(raw))
            .collect::<Result<_>>()?;
        Ok(Self { exprs })
    }

    pub fn is_empty(&self) -> bool {
        self.exprs.is_empty()
    }

    pub fn matches_device(&self, device: &UsbDevice) -> bool {
        self.exprs.is_empty() || self.exprs.iter().any(|e| e.matches_device(device))
    }

    pub fn matches_packet(&self, packet: &UsbPacket, device: &UsbDevice) -> bool {
        self.exprs.is_empty() || self.exprs.iter().any(|e| e.matches_packet(packet, device))
    }

    /// True iff any expression in this set sets the `internal` key. Callers
    /// use this to reject an `internal=` filter up front when no snapshot
    /// file exists, rather than let it silently match nothing.
    pub fn uses_internal(&self) -> bool {
        self.exprs.iter().any(|e| e.internal.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::parser::parse_usbmon_text_line;

    #[test]
    fn parses_every_key() {
        let set = FilterSet::parse(&["bus=1,dev=4,vid=04f2,pid=b71a,ep=1,dir=in,type=iso".into()])
            .unwrap();
        assert!(!set.is_empty());
    }

    #[test]
    fn id_expands_to_vid_and_pid() {
        let set = FilterSet::parse(&["id=04f2:b71a".into()]).unwrap();
        let with_both = FilterSet::parse(&["vid=04f2,pid=b71a".into()]).unwrap();
        assert_eq!(set.exprs, with_both.exprs);
    }

    #[test]
    fn rejects_unknown_keys_duplicates_and_bad_values() {
        assert!(FilterSet::parse(&["speed=480".into()])
            .unwrap_err()
            .to_string()
            .contains("speed"));
        assert!(FilterSet::parse(&["bus=1,bus=2".into()])
            .unwrap_err()
            .to_string()
            .contains("bus"));
        assert!(FilterSet::parse(&["ep=16".into()]).is_err());
        assert!(FilterSet::parse(&["dir=up".into()]).is_err());
        assert!(FilterSet::parse(&["vid=xyz".into()]).is_err());
        assert!(FilterSet::parse(&["bus".into()]).is_err());
    }

    #[test]
    fn type_accepts_short_aliases() {
        assert!(FilterSet::parse(&["type=ctrl".into()]).is_ok());
        assert!(FilterSet::parse(&["type=int".into()]).is_ok());
    }

    #[test]
    fn internal_accepts_yes_no_and_true_false_aliases() {
        let yes = FilterSet::parse(&["internal=yes".into()]).unwrap();
        let tru = FilterSet::parse(&["internal=true".into()]).unwrap();
        assert_eq!(yes.exprs, tru.exprs);

        let no = FilterSet::parse(&["internal=no".into()]).unwrap();
        let fal = FilterSet::parse(&["internal=false".into()]).unwrap();
        assert_eq!(no.exprs, fal.exprs);
    }

    #[test]
    fn internal_rejects_other_values() {
        assert!(FilterSet::parse(&["internal=maybe".into()])
            .unwrap_err()
            .to_string()
            .contains("internal"));
    }

    #[test]
    fn uses_internal_true_iff_any_expression_sets_the_key() {
        assert!(!FilterSet::default().uses_internal());
        assert!(!FilterSet::parse(&["bus=1".into()]).unwrap().uses_internal());
        assert!(FilterSet::parse(&["internal=yes".into()])
            .unwrap()
            .uses_internal());
        assert!(FilterSet::parse(&["bus=1".into(), "internal=no".into()])
            .unwrap()
            .uses_internal());
    }

    fn device(bus: u8, dev: u8, vid: Option<u16>, product: Option<&str>) -> UsbDevice {
        let mut d = UsbDevice::new(bus, dev);
        d.vendor_id = vid;
        d.product = product.map(str::to_string);
        d
    }

    #[test]
    fn empty_set_matches_everything() {
        let set = FilterSet::default();
        assert!(set.matches_device(&device(1, 4, None, None)));
    }

    #[test]
    fn device_match_uses_identity_keys_only() {
        let set = FilterSet::parse(&["bus=1,ep=1,dir=in".into()]).unwrap();
        assert!(
            set.matches_device(&device(1, 4, None, None)),
            "ep/dir do not hide a device"
        );
        assert!(!set.matches_device(&device(2, 4, None, None)));
    }

    #[test]
    fn unread_metadata_does_not_match_identity_keys() {
        let set = FilterSet::parse(&["vid=04f2".into()]).unwrap();
        assert!(!set.matches_device(&device(1, 4, None, None)));
        assert!(set.matches_device(&device(1, 4, Some(0x04f2), None)));
    }

    #[test]
    fn internal_matches_a_device_s_is_internal_flag_both_directions() {
        let mut internal = device(1, 4, None, None);
        internal.is_internal = true;
        let external = device(1, 5, None, None);

        let want_internal = FilterSet::parse(&["internal=yes".into()]).unwrap();
        assert!(want_internal.matches_device(&internal));
        assert!(!want_internal.matches_device(&external));

        let want_external = FilterSet::parse(&["internal=no".into()]).unwrap();
        assert!(!want_external.matches_device(&internal));
        assert!(want_external.matches_device(&external));
    }

    #[test]
    fn name_matches_vendor_or_product_case_insensitively() {
        let set = FilterSet::parse(&["name=camera".into()]).unwrap();
        assert!(set.matches_device(&device(1, 4, None, Some("Integrated IR Camera"))));
        assert!(!set.matches_device(&device(1, 4, None, Some("Keyboard"))));
    }

    #[test]
    fn packet_match_requires_every_key_of_one_expression() {
        let set = FilterSet::parse(&["bus=1,ep=1,dir=in,type=iso".into(), "bus=2".into()]).unwrap();
        let iso_in =
            parse_usbmon_text_line("ffff0000aaaa0001 200 C Zi:1:004:1 0:1:6672:0 32 27000 =")
                .unwrap();
        let bulk_out = parse_usbmon_text_line("ffff0000aaaa0002 300 C Bo:1:004:2 0 512 >").unwrap();
        let other_bus = parse_usbmon_text_line("ffff0000aaaa0003 400 C Bi:2:003:1 0 64 <").unwrap();
        let d1 = device(1, 4, None, None);
        let d2 = device(2, 3, None, None);
        assert!(set.matches_packet(&iso_in, &d1));
        assert!(
            !set.matches_packet(&bulk_out, &d1),
            "second expression is bus=2, first needs iso in ep1"
        );
        assert!(set.matches_packet(&other_bus, &d2), "expressions OR");
    }
}

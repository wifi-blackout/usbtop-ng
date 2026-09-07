//! Where a device row goes in the connector-grouped table: the connector it
//! joins, what the heading says, the user's name for it, and the render
//! order. Pure over the port index and the manager's bus speeds; `sync_from`
//! in the parent module is the one caller. See the connector-rows design's
//! "Sides and label" and "Fallbacks".

use std::collections::BTreeMap;

use super::DeviceRow;
use crate::connector::{PortIndex, PortRef};
use crate::device::UsbDevice;
use crate::usbmon::parser::UsbSpeed;

/// The last component of a device's sysfs path: `3-1.4`, `usb3`.
pub(super) fn sysfs_name(device: &UsbDevice) -> Option<&str> {
    device.sysfs_path.as_deref()?.file_name()?.to_str()
}

/// `1.4.2` for a chain, the Port column's and the connector label's form.
pub(super) fn chain_text(chain: &[u32]) -> String {
    chain
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// Where a device row goes: the connector it joins (by key), what the
/// heading shows, and the render order. See the spec's "Sides and label"
/// and "Fallbacks".
pub(super) struct Placement {
    pub(super) key: String,
    pub(super) label: String,
    pub(super) name: Option<String>,
    pub(super) buses: Vec<u8>,
    pub(super) sort_key: (Vec<u32>, u8),
}

/// The two keys a user may name one side of a connector by: its position as
/// the table shows it (`3:1.4`) and its kernel port object name (`3-1-port4`).
fn connector_name_keys(port: &PortRef) -> [String; 2] {
    [
        format!("{}:{}", port.bus, chain_text(&port.chain)),
        port.name.clone(),
    ]
}

/// The user's name for the first candidate key present in `names`, in the
/// order the candidates are given (the USB2 side's keys come first).
fn connector_name(
    names: &BTreeMap<String, String>,
    candidates: impl IntoIterator<Item = String>,
) -> Option<String> {
    candidates
        .into_iter()
        .find_map(|key| names.get(&key).cloned())
}

/// `None` for a row nothing places on a connector: a root hub (empty chain)
/// or a device whose sysfs entry did not resolve. Otherwise the port pair
/// from the index or, when the port object is absent (a tree without port
/// objects, or a hub still enumerating), a single connector synthesized from
/// the device's own chain and bus.
pub(super) fn connector_placement(
    index: &PortIndex,
    row: &DeviceRow,
    bus_speed: impl Fn(u8) -> Option<UsbSpeed>,
    names: &BTreeMap<String, String>,
) -> Option<Placement> {
    let chain = row.port_chain.as_ref().filter(|chain| !chain.is_empty())?;
    let name = sysfs_name(&row.device)?;
    let bus_id = row.device.bus_id;
    let Some((own, peer)) = index.connector_of(name) else {
        // Without a port object the device's own name still says which port
        // it sits on, so both key forms resolve exactly as they would with one.
        let mut candidates = vec![format!("{bus_id}:{}", chain_text(chain))];
        if let Some((hub, number)) = crate::connector::port_of_device(name) {
            candidates.push(crate::connector::port_name(&hub, number));
        }
        return Some(Placement {
            key: format!("device:{name}"),
            label: format!("Port {}", chain_text(chain)),
            name: connector_name(names, candidates),
            buses: vec![bus_id],
            sort_key: (chain.clone(), bus_id),
        });
    };
    let (primary, secondary) = order_sides(own, peer, bus_speed);
    let mut buses = vec![primary.bus];
    buses.extend(secondary.as_ref().map(|s| s.bus));
    buses.sort_unstable();
    buses.dedup();
    let label = match &secondary {
        Some(s) if s.chain != primary.chain => format!(
            "Port {} (USB3 side: {})",
            chain_text(&primary.chain),
            chain_text(&s.chain)
        ),
        _ => format!("Port {}", chain_text(&primary.chain)),
    };
    let mut port_names = vec![primary.name.clone()];
    port_names.extend(secondary.as_ref().map(|s| s.name.clone()));
    port_names.sort();
    let mut candidates = connector_name_keys(&primary).to_vec();
    if let Some(s) = &secondary {
        candidates.extend(connector_name_keys(s));
    }
    Some(Placement {
        key: port_names.join("+"),
        label,
        name: connector_name(names, candidates),
        sort_key: (primary.chain, buses[0]),
        buses,
    })
}

/// Which port of a pair is the USB2 side: the one whose bus speed is known
/// and at most 480 Mbps (the `side_label` rule; unknown never qualifies).
/// When exactly one qualifies it leads; otherwise the lower bus number does.
fn order_sides(
    own: PortRef,
    peer: Option<PortRef>,
    bus_speed: impl Fn(u8) -> Option<UsbSpeed>,
) -> (PortRef, Option<PortRef>) {
    let Some(peer) = peer else {
        return (own, None);
    };
    let usb2 = |bus: u8| {
        bus_speed(bus).is_some_and(|speed| {
            let mbps = speed.to_mbps();
            mbps > 0.0 && mbps <= 480.0
        })
    };
    let own_first = match (usb2(own.bus), usb2(peer.bus)) {
        (true, false) => true,
        (false, true) => false,
        _ => own.bus <= peer.bus,
    };
    if own_first {
        (own, Some(peer))
    } else {
        (peer, Some(own))
    }
}

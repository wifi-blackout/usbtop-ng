use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum UrbType {
    Submission, // 'S' - Host to device
    Callback,   // 'C' - Device to host
    Error,      // 'E' - Error
}

#[derive(Debug, Clone, PartialEq)]
pub enum UsbSpeed {
    Low,            // 1.5 Mbps
    Full,           // 12 Mbps
    High,           // 480 Mbps
    SuperSpeed,     // 5 Gbps
    SuperSpeedPlus, // 10+ Gbps
    Unknown,
}

impl UsbSpeed {
    pub fn from_speed_str(speed: &str) -> Self {
        match speed {
            "1.5" => UsbSpeed::Low,
            "12" => UsbSpeed::Full,
            "480" => UsbSpeed::High,
            "5000" => UsbSpeed::SuperSpeed,
            "10000" | "20000" => UsbSpeed::SuperSpeedPlus,
            _ => UsbSpeed::Unknown,
        }
    }

    pub fn to_mbps(&self) -> f64 {
        match self {
            UsbSpeed::Low => 1.5,
            UsbSpeed::Full => 12.0,
            UsbSpeed::High => 480.0,
            UsbSpeed::SuperSpeed => 5000.0,
            UsbSpeed::SuperSpeedPlus => 10000.0,
            UsbSpeed::Unknown => 0.0,
        }
    }
}

/// A single parsed usbmon URB event.
///
/// Only the fields the bandwidth aggregator consumes (`urb_type`, `bus_id`,
/// `device_id`, `direction`, `data_length`) are compiled into production
/// builds. The remaining fields (`urb_tag`, `endpoint`, `status`,
/// `setup_packet`, `data`) are still fully parsed and validated by
/// [`parse_usbmon_text_line`] on every build — they are `cfg(test)`-only
/// because nothing downstream reads them yet, but the parser test suite
/// relies on them to verify the full usbmon `Nu` text format is decoded
/// correctly (see Documentation/usb/usbmon.rst).
#[derive(Debug, Clone)]
pub struct UsbPacket {
    pub urb_type: UrbType,
    pub bus_id: u8,
    pub device_id: u8,
    pub direction: bool, // true = IN (device->host), false = OUT (host->device)
    pub data_length: u32,
    #[cfg(test)]
    pub urb_tag: String,
    #[cfg(test)]
    pub endpoint: u8,
    #[cfg(test)]
    pub status: i32,
    #[cfg(test)]
    pub setup_packet: Option<Vec<u8>>,
    #[cfg(test)]
    pub data: Option<Vec<u8>>,
}

/// Parses a single line of usbmon's `Nu` text output.
///
/// Format (see Documentation/usb/usbmon.rst): whitespace-separated words --
/// `URB_TAG TIMESTAMP EVENT_TYPE ADDRESS STATUS_OR_SETUP [ISO_DESCRIPTORS] [LENGTH] [DATA_TAG DATA...]`
///
/// Examples:
///   ffff88007c861a00 2389264913 S Bo:1:001:0 -115 31 = 55534243 ...
///   ffff880067b00300 373151059 S Ci:2:001:0 s a3 00 0000 0003 0004 4 <
///   ffff8800643c5900 3049672848 S Ii:1:001:1 -115:128 4 <
///   ffff88005bd8b100 2189039971 S Zo:1:005:2 -115:1:1810 3 -18:0:2048 ... 12288 >
///   ffff88006fff3800 2453805583 E Bi:1:004:1 -108
pub fn parse_usbmon_text_line(line: &str) -> Result<UsbPacket> {
    let mut tokens = line.split_whitespace().peekable();

    // Word 1: URB tag. Kept only for `cfg(test)` verification (see
    // `UsbPacket` docs), so the parsed value is bound with a leading
    // underscore to stay warning-free in non-test builds.
    let _urb_tag = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: empty line"))?
        .to_string();

    // Word 2: timestamp in microseconds. We don't yet reconstruct wall-clock
    // time from usbmon's boot-relative clock, so just validate the field;
    // nothing downstream needs it.
    let timestamp_token = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: missing timestamp"))?;
    let _timestamp_us: u64 = timestamp_token
        .parse()
        .map_err(|_| anyhow!("Invalid timestamp: {}", timestamp_token))?;

    // Word 3: event type.
    let event_token = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: missing event type"))?;
    let urb_type = match event_token {
        "S" => UrbType::Submission,
        "C" => UrbType::Callback,
        "E" => UrbType::Error,
        _ => return Err(anyhow!("Invalid URB type: {}", event_token)),
    };

    // Word 4: address, e.g. `Bo:1:001:0` or `Ci:2:001:0`.
    let addr_token = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: missing address word"))?;
    let addr_parts: Vec<&str> = addr_token.split(':').collect();
    if addr_parts.len() != 4 {
        return Err(anyhow!("Invalid address format: {}", addr_token));
    }

    let transfer_token = addr_parts[0];
    if transfer_token.len() != 2 {
        return Err(anyhow!(
            "Invalid transfer/address token: {}",
            transfer_token
        ));
    }
    let mut token_chars = transfer_token.chars();
    let transfer_type = token_chars.next().unwrap(); // C=Control, Z=Isochronous, I=Interrupt, B=Bulk
    let direction_char = token_chars.next().unwrap(); // i=IN, o=OUT
    if !matches!(direction_char, 'i' | 'o') {
        return Err(anyhow!(
            "Invalid direction in address token: {}",
            transfer_token
        ));
    }
    let direction = direction_char == 'i';

    let bus_id: u8 = addr_parts[1]
        .parse()
        .map_err(|_| anyhow!("Invalid bus ID: {}", addr_parts[1]))?;
    let device_id: u8 = addr_parts[2]
        .parse()
        .map_err(|_| anyhow!("Invalid device ID: {}", addr_parts[2]))?;
    // cfg(test)-only field (see `UsbPacket` docs); still validated on every build.
    let _endpoint: u8 = addr_parts[3]
        .parse()
        .map_err(|_| anyhow!("Invalid endpoint: {}", addr_parts[3]))?;

    // Word 5: either a captured control setup packet (`s` followed by 5 hex
    // words) or a `STATUS[:INTERVAL[:START_FRAME[:ERROR_COUNT]]]` word.
    let status_token = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: missing status/setup word"))?;
    let is_setup = status_token == "s";

    // `status`/`setup_packet` are cfg(test)-only fields (see `UsbPacket`
    // docs); the parsing and validation below always run so malformed lines
    // are rejected identically in every build.
    let (_status, _setup_packet) = if is_setup {
        let mut setup_bytes = Vec::with_capacity(8);
        let bm_request_type = next_hex_u8(&mut tokens, "bmRequestType")?;
        let b_request = next_hex_u8(&mut tokens, "bRequest")?;
        let w_value = next_hex_u16(&mut tokens, "wValue")?;
        let w_index = next_hex_u16(&mut tokens, "wIndex")?;
        let w_length = next_hex_u16(&mut tokens, "wLength")?;
        setup_bytes.push(bm_request_type);
        setup_bytes.push(b_request);
        setup_bytes.extend_from_slice(&w_value.to_le_bytes());
        setup_bytes.extend_from_slice(&w_index.to_le_bytes());
        setup_bytes.extend_from_slice(&w_length.to_le_bytes());
        (-115, Some(setup_bytes)) // EINPROGRESS: matches in-flight submissions
    } else {
        let status_field = status_token.split(':').next().unwrap_or(status_token);
        let status: i32 = status_field
            .parse()
            .map_err(|_| anyhow!("Invalid status: {}", status_token))?;
        (status, None)
    };

    // Word 6 (isochronous transfers only, when word 5 wasn't a setup):
    // descriptor count followed by up to 5 `status:offset:length` words.
    if transfer_type == 'Z' && !is_setup {
        let ndesc_token = tokens
            .next()
            .ok_or_else(|| anyhow!("Missing isochronous descriptor count"))?;
        let ndesc: usize = ndesc_token
            .parse()
            .map_err(|_| anyhow!("Invalid isochronous descriptor count: {}", ndesc_token))?;
        for _ in 0..ndesc.min(5) {
            match tokens.peek() {
                Some(word) if word.contains(':') => {
                    tokens.next();
                }
                _ => break,
            }
        }
    }

    // Word 7: data length. `E` events may omit it entirely.
    let data_length: u32 = match tokens.next() {
        Some(word) => word
            .parse()
            .map_err(|_| anyhow!("Invalid data length: {}", word))?,
        None if urb_type == UrbType::Error => 0,
        None => {
            return Err(anyhow!(
                "Invalid usbmon text line format: missing data length"
            ))
        }
    };

    // Word 8: data tag (`=` for captured data, `<`/`>` for none) plus data
    // words. cfg(test)-only field (see `UsbPacket` docs).
    let _data = match tokens.next() {
        Some("=") => Some(parse_hex_data(&tokens.collect::<Vec<_>>()).unwrap_or_default()),
        _ => None,
    };

    Ok(UsbPacket {
        urb_type,
        bus_id,
        device_id,
        direction,
        data_length,
        #[cfg(test)]
        urb_tag: _urb_tag,
        #[cfg(test)]
        endpoint: _endpoint,
        #[cfg(test)]
        status: _status,
        #[cfg(test)]
        setup_packet: _setup_packet,
        #[cfg(test)]
        data: _data,
    })
}

/// Pulls the next token from `tokens` and parses it as a 2-hex-digit byte,
/// used for the `bmRequestType`/`bRequest` fields of a captured setup packet.
fn next_hex_u8<'a>(
    tokens: &mut std::iter::Peekable<std::str::SplitWhitespace<'a>>,
    field: &str,
) -> Result<u8> {
    let word = tokens
        .next()
        .ok_or_else(|| anyhow!("Truncated control setup packet: missing {}", field))?;
    u8::from_str_radix(word, 16).map_err(|_| anyhow!("Invalid setup packet {}: {}", field, word))
}

/// Pulls the next token from `tokens` and parses it as a 4-hex-digit word,
/// used for the `wValue`/`wIndex`/`wLength` fields of a captured setup packet.
fn next_hex_u16<'a>(
    tokens: &mut std::iter::Peekable<std::str::SplitWhitespace<'a>>,
    field: &str,
) -> Result<u16> {
    let word = tokens
        .next()
        .ok_or_else(|| anyhow!("Truncated control setup packet: missing {}", field))?;
    u16::from_str_radix(word, 16).map_err(|_| anyhow!("Invalid setup packet {}: {}", field, word))
}

fn parse_hex_data(hex_parts: &[&str]) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    for part in hex_parts {
        // Each part might be multiple hex bytes like "55534243"
        if part.len() % 2 != 0 {
            continue; // Skip malformed hex
        }

        for i in (0..part.len()).step_by(2) {
            if let Ok(byte) = u8::from_str_radix(&part[i..i + 2], 16) {
                data.push(byte);
            }
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_usbmon_text_line() {
        let line = "ffff88007c861a00 2389264913 S Bo:1:001:0 -115 31 = 55534243 1f000000 00000000 00000600 00000000 00000000 00000000 000000";
        let packet = parse_usbmon_text_line(line).unwrap();

        assert_eq!(packet.urb_tag, "ffff88007c861a00");
        assert_eq!(packet.urb_type, UrbType::Submission);
        assert_eq!(packet.bus_id, 1);
        assert_eq!(packet.device_id, 1);
        assert_eq!(packet.endpoint, 0);
        assert!(!packet.direction); // OUT
        assert_eq!(packet.data_length, 31);
        assert_eq!(packet.status, -115);
        assert!(packet.data.is_some());
    }

    #[test]
    fn test_parse_usbmon_text_line_rejects_short_address_type() {
        let line = "ffff88007c861a00 2389264913 S B:1:001:0 -115 31 = 55534243";
        let err = parse_usbmon_text_line(line).unwrap_err();
        assert!(err.to_string().contains("Invalid transfer/address token"));
    }

    #[test]
    fn parses_control_submission_with_setup_packet() {
        // Example line from Documentation/usb/usbmon.rst
        let line = "ffff880067b00300 373151059 S Ci:2:001:0 s a3 00 0000 0003 0004 4 <";
        let p = parse_usbmon_text_line(line).unwrap();
        assert_eq!(p.urb_type, UrbType::Submission);
        assert_eq!(p.bus_id, 2);
        assert_eq!(p.device_id, 1);
        assert_eq!(p.endpoint, 0);
        assert!(p.direction); // IN
        assert_eq!(p.status, -115);
        assert_eq!(p.data_length, 4);
        assert_eq!(
            p.setup_packet.as_deref(),
            Some(&[0xa3, 0x00, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00][..])
        );
        assert!(p.data.is_none());
    }

    #[test]
    fn parses_control_callback_with_data() {
        let line = "ffff880067b00300 373151577 C Ci:2:001:0 0 4 = 01050000";
        let p = parse_usbmon_text_line(line).unwrap();
        assert_eq!(p.urb_type, UrbType::Callback);
        assert_eq!(p.status, 0);
        assert_eq!(p.data_length, 4);
        assert_eq!(p.data.as_deref(), Some(&[0x01, 0x05, 0x00, 0x00][..]));
    }

    #[test]
    fn parses_interrupt_status_with_interval() {
        let line = "ffff8800643c5900 3049672848 S Ii:1:001:1 -115:128 4 <";
        let p = parse_usbmon_text_line(line).unwrap();
        assert_eq!(p.status, -115);
        assert_eq!(p.data_length, 4);
        assert!(p.data.is_none());

        let line = "ffff8800643c5900 3049674955 C Ii:1:001:1 0:128 4 = 40000000";
        let p = parse_usbmon_text_line(line).unwrap();
        assert_eq!(p.status, 0);
        assert_eq!(p.data.as_deref(), Some(&[0x40, 0x00, 0x00, 0x00][..]));
    }

    #[test]
    fn parses_iso_events_with_descriptors() {
        let line = "ffff88005bd8b100 2189039971 S Zo:1:005:2 -115:1:1810 3 -18:0:2048 -18:2048:2048 -18:4096:2048 12288 >";
        let p = parse_usbmon_text_line(line).unwrap();
        assert_eq!(p.urb_type, UrbType::Submission);
        assert_eq!(p.device_id, 5);
        assert_eq!(p.status, -115);
        assert_eq!(p.data_length, 12288);

        let line = "ffff88005bd8b100 2189040992 C Zo:1:005:2 0:1:1810:0 3 0:0:2048 0:2048:2048 0:4096:2048 12288 >";
        let p = parse_usbmon_text_line(line).unwrap();
        assert_eq!(p.urb_type, UrbType::Callback);
        assert_eq!(p.status, 0);
        assert_eq!(p.data_length, 12288);
    }

    #[test]
    fn parses_error_event_without_length() {
        let line = "ffff88006fff3800 2453805583 E Bi:1:004:1 -108";
        let p = parse_usbmon_text_line(line).unwrap();
        assert_eq!(p.urb_type, UrbType::Error);
        assert_eq!(p.status, -108);
        assert_eq!(p.data_length, 0);
        assert!(p.data.is_none());
    }

    #[test]
    fn rejects_garbage_lines() {
        assert!(parse_usbmon_text_line("").is_err());
        assert!(parse_usbmon_text_line("not a usbmon line at all").is_err());
        assert!(parse_usbmon_text_line("ffff 123 X Bo:1:001:0 0 0").is_err());
    }

    #[test]
    fn test_usb_speed_from_str_and_mbps() {
        assert_eq!(UsbSpeed::from_speed_str("480"), UsbSpeed::High);
        assert_eq!(UsbSpeed::SuperSpeed.to_mbps(), 5000.0);
    }
}

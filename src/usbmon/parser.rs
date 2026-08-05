use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

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

    /// Returns theoretical maximum bandwidth in bytes per second
    /// Note: These are raw theoretical maximums, actual usable bandwidth is lower
    /// due to protocol overhead, frame structure, etc.
    pub fn to_bytes_per_second(&self) -> f64 {
        match self {
            UsbSpeed::Low => 1_500_000.0 / 8.0,    // 1.5 Mbps = ~187.5 KB/s
            UsbSpeed::Full => 12_000_000.0 / 8.0,  // 12 Mbps = 1.5 MB/s
            UsbSpeed::High => 480_000_000.0 / 8.0, // 480 Mbps = 60 MB/s
            UsbSpeed::SuperSpeed => 5_000_000_000.0 / 8.0, // 5 Gbps = 625 MB/s
            UsbSpeed::SuperSpeedPlus => 10_000_000_000.0 / 8.0, // 10 Gbps = 1.25 GB/s
            UsbSpeed::Unknown => 0.0,
        }
    }

    /// Returns practical maximum bandwidth in bytes per second
    /// Takes into account typical protocol overhead (~80% efficiency for most speeds)
    pub fn to_practical_bytes_per_second(&self) -> f64 {
        match self {
            UsbSpeed::Low => self.to_bytes_per_second() * 0.7, // ~70% for low speed
            UsbSpeed::Full => self.to_bytes_per_second() * 0.8, // ~80% for full speed
            UsbSpeed::High => self.to_bytes_per_second() * 0.8, // ~80% for high speed
            UsbSpeed::SuperSpeed => self.to_bytes_per_second() * 0.85, // ~85% for super speed
            UsbSpeed::SuperSpeedPlus => self.to_bytes_per_second() * 0.85, // ~85% for super speed+
            UsbSpeed::Unknown => 0.0,
        }
    }

    pub fn color_code(&self) -> (u8, u8, u8) {
        match self {
            UsbSpeed::Low => (255, 100, 100),          // Light red
            UsbSpeed::Full => (255, 165, 0),           // Orange
            UsbSpeed::High => (255, 255, 0),           // Yellow
            UsbSpeed::SuperSpeed => (0, 255, 0),       // Green
            UsbSpeed::SuperSpeedPlus => (0, 255, 255), // Cyan
            UsbSpeed::Unknown => (128, 128, 128),      // Gray
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsbPacket {
    pub timestamp: DateTime<Utc>,
    pub urb_tag: String,
    pub urb_type: UrbType,
    pub bus_id: u8,
    pub device_id: u8,
    pub endpoint: u8,
    pub direction: bool, // true = IN (device->host), false = OUT (host->device)
    pub data_length: u32,
    pub status: i32,
    pub setup_packet: Option<Vec<u8>>,
    pub data: Option<Vec<u8>>,
}

impl UsbPacket {
    pub fn is_data_packet(&self) -> bool {
        self.data_length > 0 && matches!(self.urb_type, UrbType::Submission | UrbType::Callback)
    }

    pub fn bandwidth_bytes(&self) -> u32 {
        if self.is_data_packet() {
            self.data_length
        } else {
            0
        }
    }
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

    // Word 1: URB tag.
    let urb_tag = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: empty line"))?
        .to_string();

    // Word 2: timestamp in microseconds. We don't yet reconstruct wall-clock
    // time from usbmon's boot-relative clock, so just validate the field and
    // stamp the packet with the current time.
    let timestamp_token = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: missing timestamp"))?;
    let _timestamp_us: u64 = timestamp_token
        .parse()
        .map_err(|_| anyhow!("Invalid timestamp: {}", timestamp_token))?;
    let timestamp = Utc::now();

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
    let endpoint: u8 = addr_parts[3]
        .parse()
        .map_err(|_| anyhow!("Invalid endpoint: {}", addr_parts[3]))?;

    // Word 5: either a captured control setup packet (`s` followed by 5 hex
    // words) or a `STATUS[:INTERVAL[:START_FRAME[:ERROR_COUNT]]]` word.
    let status_token = tokens
        .next()
        .ok_or_else(|| anyhow!("Invalid usbmon text line format: missing status/setup word"))?;
    let is_setup = status_token == "s";

    let (status, setup_packet) = if is_setup {
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

    // Word 8: data tag (`=` for captured data, `<`/`>` for none) plus data words.
    let data = match tokens.next() {
        Some("=") => Some(parse_hex_data(&tokens.collect::<Vec<_>>()).unwrap_or_default()),
        _ => None,
    };

    Ok(UsbPacket {
        timestamp,
        urb_tag,
        urb_type,
        bus_id,
        device_id,
        endpoint,
        direction,
        data_length,
        status,
        setup_packet,
        data,
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

pub fn parse_usbmon_binary_packet(buffer: &[u8]) -> Result<UsbPacket> {
    if buffer.len() < 64 {
        return Err(anyhow!("Binary packet too short: {} bytes", buffer.len()));
    }

    // usbmon binary format (64 bytes):
    // Offset 0: urb_id (8 bytes)
    // Offset 8: urb_type (1 byte): 'S', 'C', 'E'
    // Offset 9: transfer_type (1 byte)
    // Offset 10: endpoint (1 byte)
    // Offset 11: device_id (1 byte)
    // Offset 12: bus_id (2 bytes, little endian)
    // Offset 14: flag_setup (1 byte)
    // Offset 15: flag_data (1 byte)
    // Offset 16: ts_sec (8 bytes, little endian)
    // Offset 24: ts_usec (4 bytes, little endian)
    // Offset 28: status (4 bytes, little endian, signed)
    // Offset 32: length (4 bytes, little endian)
    // Offset 36: len_cap (4 bytes, little endian)
    // Rest: setup packet or data

    let urb_id = u64::from_le_bytes([
        buffer[0], buffer[1], buffer[2], buffer[3], buffer[4], buffer[5], buffer[6], buffer[7],
    ]);
    let urb_tag = format!("{:016x}", urb_id);

    let urb_type = match buffer[8] as char {
        'S' => UrbType::Submission,
        'C' => UrbType::Callback,
        'E' => UrbType::Error,
        _ => return Err(anyhow!("Invalid URB type: {}", buffer[8] as char)),
    };

    let _transfer_type = buffer[9];
    let endpoint = buffer[10] & 0x7F; // Lower 7 bits
    let direction = (buffer[10] & 0x80) != 0; // MSB indicates direction
    let device_id = buffer[11];
    let bus_id = u16::from_le_bytes([buffer[12], buffer[13]]) as u8;

    let ts_sec = u64::from_le_bytes([
        buffer[16], buffer[17], buffer[18], buffer[19], buffer[20], buffer[21], buffer[22],
        buffer[23],
    ]);
    let ts_usec = u32::from_le_bytes([buffer[24], buffer[25], buffer[26], buffer[27]]);

    let timestamp =
        DateTime::from_timestamp(ts_sec as i64, ts_usec * 1000).unwrap_or_else(Utc::now);

    let status = i32::from_le_bytes([buffer[28], buffer[29], buffer[30], buffer[31]]);
    let data_length = u32::from_le_bytes([buffer[32], buffer[33], buffer[34], buffer[35]]);

    // TODO: Parse setup packet and data from remaining bytes

    Ok(UsbPacket {
        timestamp,
        urb_tag,
        urb_type,
        bus_id,
        device_id,
        endpoint,
        direction,
        data_length,
        status,
        setup_packet: None,
        data: None,
    })
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
    fn test_usb_speed_color_codes() {
        assert_eq!(UsbSpeed::SuperSpeed.color_code(), (0, 255, 0));
        assert_eq!(UsbSpeed::High.color_code(), (255, 255, 0));
        assert_eq!(UsbSpeed::from_speed_str("480"), UsbSpeed::High);
        assert_eq!(UsbSpeed::SuperSpeed.to_mbps(), 5000.0);
    }

    #[test]
    fn test_bandwidth_calculations() {
        // Test theoretical bandwidth
        assert_eq!(UsbSpeed::High.to_bytes_per_second(), 60_000_000.0);
        assert_eq!(UsbSpeed::SuperSpeed.to_bytes_per_second(), 625_000_000.0);

        // Test practical bandwidth (with overhead)
        let high_practical = UsbSpeed::High.to_practical_bytes_per_second();
        assert!(high_practical < UsbSpeed::High.to_bytes_per_second());
        assert_eq!(high_practical, 48_000_000.0); // 80% of 60MB/s
    }
}

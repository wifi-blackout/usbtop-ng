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

pub fn parse_usbmon_text_line(line: &str) -> Result<UsbPacket> {
    // usbmon text format:
    // URB_TAG TIMESTAMP EVENT_TYPE ADDR:EP:D S URB_STATUS LENGTH DATA...
    // Example: ffff88007c861a00 2389264913 S Bo:1:001:0 -115 31 = 55534243 ...

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 7 {
        return Err(anyhow!("Invalid usbmon text line format: too few fields"));
    }

    let urb_tag = parts[0].to_string();

    // Parse timestamp (microseconds since boot)
    let _timestamp_us: u64 = parts[1]
        .parse()
        .map_err(|_| anyhow!("Invalid timestamp: {}", parts[1]))?;
    let timestamp = Utc::now(); // For now, use current time. TODO: Convert from boot time

    // Parse event type
    let urb_type = match parts[2] {
        "S" => UrbType::Submission,
        "C" => UrbType::Callback,
        "E" => UrbType::Error,
        _ => return Err(anyhow!("Invalid URB type: {}", parts[2])),
    };

    // Parse address field: Bo:1:001:0 or Ci:1:001:0 etc.
    let addr_parts: Vec<&str> = parts[3].split(':').collect();
    if addr_parts.len() != 4 {
        return Err(anyhow!("Invalid address format: {}", parts[3]));
    }

    let transfer_token = addr_parts[0];
    if transfer_token.len() != 2 {
        return Err(anyhow!(
            "Invalid transfer/address token: {}",
            transfer_token
        ));
    }
    let mut token_chars = transfer_token.chars();
    let _transfer_type = token_chars.next().unwrap(); // B=Bulk, C=Control, I=Interrupt, Z=Isochronous
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

    // Parse status
    let status: i32 = parts[4]
        .parse()
        .map_err(|_| anyhow!("Invalid status: {}", parts[4]))?;

    // Parse data length
    let data_length: u32 = parts[5]
        .parse()
        .map_err(|_| anyhow!("Invalid data length: {}", parts[5]))?;

    // Parse data if present (parts[6] should be '=' if data follows)
    let data = if parts.len() > 7 && parts[6] == "=" {
        Some(parse_hex_data(&parts[7..]).unwrap_or_default())
    } else {
        None
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
        setup_packet: None, // TODO: Parse setup packets for control transfers
        data,
    })
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

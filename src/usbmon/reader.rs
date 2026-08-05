use anyhow::{anyhow, Result};
use log::{debug, error, warn};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use tokio::fs::File as TokioFile;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader as TokioBufReader};

use super::parser::{parse_usbmon_binary_packet, parse_usbmon_text_line, UsbPacket};

#[derive(Debug, Clone)]
pub struct UsbmonReader {
    pub bus_id: u8,
    pub use_binary: bool,
    pub path: String,
}

impl UsbmonReader {
    pub fn new(bus_id: u8, use_binary: bool) -> Self {
        let path = Self::get_usbmon_path(bus_id, use_binary);
        Self {
            bus_id,
            use_binary,
            path,
        }
    }

    fn get_usbmon_path(bus_id: u8, use_binary: bool) -> String {
        #[cfg(target_os = "linux")]
        {
            let suffix = if use_binary { "u" } else { "t" };
            format!("/sys/kernel/debug/usb/usbmon/{}{}", bus_id, suffix)
        }

        #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
        {
            // BSD systems might use different paths
            format!("/dev/ugen{}.{}", bus_id, if use_binary { "1" } else { "0" })
        }

        #[cfg(target_os = "macos")]
        {
            // macOS doesn't have usbmon, return a placeholder
            format!("/dev/null")
        }
    }

    pub fn is_available(&self) -> bool {
        Path::new(&self.path).exists()
    }

    pub async fn read_packets<F>(&self, mut callback: F) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        if !self.is_available() {
            return Err(anyhow!("usbmon interface not available: {}", self.path));
        }

        debug!("Starting packet capture from {}", self.path);

        if self.use_binary {
            self.read_binary_packets(callback).await
        } else {
            self.read_text_packets(callback).await
        }
    }

    async fn read_binary_packets<F>(&self, mut callback: F) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        let mut file = TokioFile::open(&self.path)
            .await
            .map_err(|e| anyhow!("Failed to open {}: {}", self.path, e))?;

        let mut buffer = vec![0u8; 64]; // usbmon binary packets are 64 bytes

        loop {
            match file.read_exact(&mut buffer).await {
                Ok(_) => match parse_usbmon_binary_packet(&buffer) {
                    Ok(packet) => {
                        if let Err(e) = callback(packet) {
                            error!("Packet callback error: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse binary packet: {}", e);
                        continue;
                    }
                },
                Err(e) => {
                    error!("Failed to read from {}: {}", self.path, e);
                    break;
                }
            }
        }

        Ok(())
    }

    async fn read_text_packets<F>(&self, mut callback: F) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        let file = TokioFile::open(&self.path)
            .await
            .map_err(|e| anyhow!("Failed to open {}: {}", self.path, e))?;

        let mut reader = TokioBufReader::new(file);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    // EOF reached, continue monitoring
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    continue;
                }
                Ok(_) => match parse_usbmon_text_line(line.trim()) {
                    Ok(packet) => {
                        if let Err(e) = callback(packet) {
                            error!("Packet callback error: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        debug!("Failed to parse text line '{}': {}", line.trim(), e);
                        continue;
                    }
                },
                Err(e) => {
                    error!("Failed to read line from {}: {}", self.path, e);
                    break;
                }
            }
        }

        Ok(())
    }
}

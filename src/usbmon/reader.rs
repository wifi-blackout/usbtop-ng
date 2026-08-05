use anyhow::{anyhow, Result};
use log::{debug, error};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Duration;

use super::parser::{parse_usbmon_text_line, UsbPacket};

/// Reads usbmon's `Nu` text interface (`/sys/kernel/debug/usb/usbmon/{bus}u`
/// on Linux) with blocking I/O. debugfs's `Nu` files ARE the text interface
/// described in Documentation/usb/usbmon.rst — the real binary API is the
/// separate `/dev/usbmonN` character devices, which this codebase does not
/// open.
#[derive(Debug, Clone)]
pub struct UsbmonReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool,
}

impl UsbmonReader {
    pub fn new(bus_id: u8) -> Self {
        Self {
            bus_id,
            path: Self::get_usbmon_path(bus_id),
            follow: true,
        }
    }

    /// Test seam: point the reader at an arbitrary file instead of the real
    /// debugfs path, and optionally disable follow-on-EOF so tests over a
    /// fixed fixture file terminate.
    pub fn with_path(bus_id: u8, path: PathBuf, follow: bool) -> Self {
        Self {
            bus_id,
            path,
            follow,
        }
    }

    fn get_usbmon_path(bus_id: u8) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            PathBuf::from(format!("/sys/kernel/debug/usb/usbmon/{}u", bus_id))
        }

        #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
        {
            // BSD systems might use different paths
            PathBuf::from(format!("/dev/ugen{}.0", bus_id))
        }

        #[cfg(target_os = "macos")]
        {
            // macOS doesn't have usbmon, return a placeholder
            PathBuf::from("/dev/null")
        }
    }

    pub fn is_available(&self) -> bool {
        self.path.exists()
    }

    /// Blocking read loop over the usbmon text interface. Runs to completion
    /// on the calling thread; callers that want this alongside other work
    /// (e.g. a TUI event loop) should spawn it on a dedicated thread.
    ///
    /// Lines that fail to parse are skipped (logged at debug level). A
    /// callback `Err` stops the loop early and still returns `Ok(())`.
    pub fn read_packets<F>(&self, mut callback: F) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        if !self.is_available() {
            return Err(anyhow!(
                "usbmon interface not available: {}",
                self.path.display()
            ));
        }

        debug!("Starting packet capture from {}", self.path.display());

        let file = std::fs::File::open(&self.path)
            .map_err(|e| anyhow!("Failed to open {}: {}", self.path.display(), e))?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    // EOF reached.
                    if self.follow {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    break;
                }
                Ok(_) => match parse_usbmon_text_line(line.trim()) {
                    Ok(packet) => {
                        if let Err(e) = callback(packet) {
                            debug!("Packet callback error: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        debug!("Failed to parse text line '{}': {}", line.trim(), e);
                        continue;
                    }
                },
                Err(e) => {
                    error!("Failed to read line from {}: {}", self.path.display(), e);
                    break;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::parser::UrbType;
    use std::io::Write;

    #[test]
    fn reads_packets_from_fixture_file_skipping_garbage() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("2u");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "ffff880067b00300 373151059 S Ci:2:001:0 s a3 00 0000 0003 0004 4 <"
        )
        .unwrap();
        writeln!(f, "this line is garbage and must be skipped").unwrap();
        writeln!(f, "ffff880067b00300 373151577 C Ci:2:001:0 0 4 = 01050000").unwrap();

        let reader = UsbmonReader::with_path(2, path, false);
        let mut packets = Vec::new();
        reader
            .read_packets(|p| {
                packets.push(p);
                Ok(())
            })
            .unwrap();

        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].urb_type, UrbType::Submission);
        assert_eq!(packets[1].urb_type, UrbType::Callback);
        assert_eq!(packets[1].data_length, 4);
    }

    #[test]
    fn callback_error_stops_reading() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("1u");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "ffff880067b00300 373151577 C Ci:1:001:0 0 4 = 01050000").unwrap();
        writeln!(f, "ffff880067b00301 373151578 C Ci:1:002:0 0 4 = 01050000").unwrap();

        let reader = UsbmonReader::with_path(1, path, false);
        let mut count = 0;
        reader
            .read_packets(|_| {
                count += 1;
                Err(anyhow::anyhow!("stop"))
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}

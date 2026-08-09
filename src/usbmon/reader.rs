use anyhow::{anyhow, Result};
use log::{debug, error};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use super::parser::{parse_usbmon_text_line, UsbPacket};
use super::{open_nonblocking, POLL_INTERVAL};

/// Reads usbmon's `Nu` text interface (`/sys/kernel/debug/usb/usbmon/{bus}u`).
/// debugfs's `Nu` files ARE the text interface described in
/// Documentation/usb/usbmon.rst; the binary API is the separate
/// `/dev/usbmonN` character devices, read by
/// [`BinaryReader`](super::binary::BinaryReader). This reader is the fallback
/// [`start_monitoring`](super::monitor::start_monitoring) picks when those
/// devices cannot be opened.
///
/// The file is opened non-blocking and polled every [`POLL_INTERVAL`], so a
/// silent bus never parks the reader thread inside a `read` where it could
/// neither be joined nor release the debugfs file.
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
    #[cfg(test)]
    pub fn with_path(bus_id: u8, path: PathBuf, follow: bool) -> Self {
        Self {
            bus_id,
            path,
            follow,
        }
    }

    fn get_usbmon_path(bus_id: u8) -> PathBuf {
        PathBuf::from(format!("/sys/kernel/debug/usb/usbmon/{}u", bus_id))
    }

    pub fn is_available(&self) -> bool {
        self.path.exists()
    }

    /// Read loop over the usbmon text interface. Runs to completion on the
    /// calling thread; callers that want this alongside other work (e.g. a TUI
    /// event loop) should spawn it on a dedicated thread.
    ///
    /// `shutdown` is polled between reads and whenever the interface has
    /// nothing to give, so a caller can stop the loop within `POLL_INTERVAL`
    /// and join the thread.
    ///
    /// Lines that fail to parse are skipped (logged at debug level). A
    /// callback `Err` stops the loop early and still returns `Ok(())`.
    pub fn read_packets<F>(&self, shutdown: &AtomicBool, mut callback: F) -> Result<()>
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

        let file = open_nonblocking(&self.path)
            .map_err(|e| anyhow!("Failed to open {}: {}", self.path.display(), e))?;
        let mut reader = BufReader::new(file);
        // Held across iterations on purpose: a `WouldBlock` can land mid-line,
        // and `read_line` appends, so the partial line must survive the retry.
        let mut line = String::new();

        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            match reader.read_line(&mut line) {
                Ok(0) => {
                    // EOF reached.
                    if self.follow {
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(POLL_INTERVAL);
                        continue;
                    }
                    break;
                }
                Ok(_) => match parse_usbmon_text_line(line.trim()) {
                    Ok(packet) => {
                        line.clear();
                        if let Err(e) = callback(packet) {
                            debug!("Packet callback error: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        debug!("Failed to parse text line '{}': {}", line.trim(), e);
                        line.clear();
                        continue;
                    }
                },
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    // Nothing queued on the interface right now; keep whatever
                    // partial line we have and poll again.
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
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
    use std::time::Duration;

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
            .read_packets(&AtomicBool::new(false), |p| {
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
            .read_packets(&AtomicBool::new(false), |_| {
                count += 1;
                Err(anyhow::anyhow!("stop"))
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Covers the `WouldBlock` retry path, which regular fixture files never
    /// reach: a FIFO opened `O_RDONLY | O_NONBLOCK` succeeds with no writer
    /// attached and then answers reads with EAGAIN, just like an idle usbmon
    /// interface.
    #[test]
    fn wouldblock_retry_reassembles_partial_lines() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("5u");
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());

        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let reader = UsbmonReader::with_path(5, path.clone(), true);
        let flag = std::sync::Arc::clone(&shutdown);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            reader
                .read_packets(&flag, |p| {
                    tx.send(p).unwrap();
                    Ok(())
                })
                .unwrap();
        });

        // The writer opens after the reader is already polling, and splits one
        // event across two writes so a WouldBlock lands mid-line.
        let mut w = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        w.write_all(b"ffff0000dddd0005 500 C Bi:5:003:1 0 ")
            .unwrap();
        w.flush().unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        w.write_all(b"32 <\n").unwrap();
        w.flush().unwrap();

        let packet = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            packet.data_length, 32,
            "partial line must survive the WouldBlock retry"
        );

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn preset_shutdown_flag_stops_follow_mode_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("4u");
        std::fs::File::create(&path).unwrap();

        // follow = true would otherwise poll this empty file forever.
        let reader = UsbmonReader::with_path(4, path, true);
        let started = std::time::Instant::now();
        reader
            .read_packets(&AtomicBool::new(true), |_| Ok(()))
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a set shutdown flag must end the loop without polling"
        );
    }
}

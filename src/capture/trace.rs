//! Frame a raw captured usbmon window buffer and sanitize each event/line.
//! The live device open and window timing live in the orchestrator; this
//! module is pure over a `Read` so it is fully unit-tested.

use std::io::{BufRead, Read};

use crate::capture::sanitize::{sanitize_binary_header, sanitize_text_line};
use crate::usbmon::binary::HEADER_LEN;

/// Read concatenated binary usbmon events to EOF and return the sanitized
/// stream: each complete 48-byte header with `len_cap` zeroed and no payload.
/// A trailing partial header (a window cut mid-event) is dropped.
pub fn sanitize_binary_stream<R: Read>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    reader.read_to_end(&mut raw)?;

    let mut out = Vec::new();
    let mut offset = 0;
    while offset + HEADER_LEN <= raw.len() {
        let header: [u8; HEADER_LEN] = raw[offset..offset + HEADER_LEN]
            .try_into()
            .expect("slice is exactly HEADER_LEN");
        let len_cap = u32::from_ne_bytes(header[36..40].try_into().unwrap()) as usize;
        let next = offset + HEADER_LEN + len_cap;
        if next > raw.len() {
            break; // truncated trailing payload: drop it
        }
        out.extend_from_slice(&sanitize_binary_header(&header));
        offset = next;
    }
    Ok(out)
}

/// Read `Nu` text lines to EOF and return the sanitized text: one sanitized
/// line (plus `\n`) per complete line. A trailing line without a newline (a
/// window cut mid-line) is dropped.
pub fn sanitize_text_stream<R: BufRead>(reader: &mut R) -> std::io::Result<String> {
    let mut out = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break; // EOF
        }
        if !line.ends_with('\n') {
            break; // incomplete trailing line: drop it
        }
        out.push_str(&sanitize_text_line(line.trim_end_matches('\n')));
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn event(t: u8, epnum: u8, devnum: u8, busnum: u16, length: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 48];
        b[8] = t;
        b[9] = 3;
        b[10] = epnum;
        b[11] = devnum;
        b[12..14].copy_from_slice(&busnum.to_ne_bytes());
        b[32..36].copy_from_slice(&length.to_ne_bytes());
        b[36..40].copy_from_slice(&(payload.len() as u32).to_ne_bytes());
        b.extend_from_slice(payload);
        b
    }

    #[test]
    fn binary_stream_drops_payload_and_keeps_framing() {
        let mut raw = Vec::new();
        raw.extend(event(b'C', 0x81, 3, 1, 1000, &[0xAB; 16]));
        raw.extend(event(b'C', 0x82, 4, 1, 64, &[]));
        let out = sanitize_binary_stream(&mut Cursor::new(raw)).unwrap();
        assert_eq!(out.len(), 96, "two headers, no payload");
        assert_eq!(&out[36..40], &0u32.to_ne_bytes(), "first len_cap zeroed");
        assert_eq!(&out[32..36], &1000u32.to_ne_bytes(), "first length kept");
        assert_eq!(
            out[48 + 11],
            4,
            "second event's devnum aligned after the drop"
        );
    }

    #[test]
    fn binary_stream_drops_a_truncated_trailing_header() {
        let mut raw = event(b'C', 0x81, 3, 1, 64, &[]);
        raw.extend_from_slice(&[0u8; 20]); // partial next header
        let out = sanitize_binary_stream(&mut Cursor::new(raw)).unwrap();
        assert_eq!(out.len(), 48, "only the one complete event survives");
    }

    #[test]
    fn binary_stream_drops_a_complete_header_whose_claimed_payload_is_truncated() {
        // A complete 48-byte header claims len_cap=64, but only 30 payload
        // bytes actually follow (the window was cut mid-payload). The whole
        // event — header included — must be dropped, not just the payload.
        let mut raw = vec![0u8; 48];
        raw[8] = b'C';
        raw[9] = 3;
        raw[36..40].copy_from_slice(&64u32.to_ne_bytes());
        raw.extend_from_slice(&[0xAB; 30]);
        let out = sanitize_binary_stream(&mut Cursor::new(raw)).unwrap();
        assert!(
            out.is_empty(),
            "the trailing event is dropped, not just truncated"
        );
    }

    #[test]
    fn text_stream_sanitizes_each_complete_line() {
        let raw = "ffff0000aaaa0001 200 C Bi:1:003:1 0 512 = 00 11\n\
                   ffff0000bbbb0002 300 C Bo:1:004:2 0 64 >\n\
                   ffff0000cccc0003 400 C Bi:1:005:1 0 128 = 22"; // no trailing newline
        let out = sanitize_text_stream(&mut Cursor::new(raw)).unwrap();
        assert_eq!(
            out,
            "ffff0000aaaa0001 200 C Bi:1:003:1 0 512 <\n\
             ffff0000bbbb0002 300 C Bo:1:004:2 0 64 <\n",
            "two complete lines sanitized; the newline-less trailing line dropped"
        );
        assert!(!out.contains('='), "SEC-1: no data tag survives");
    }
}

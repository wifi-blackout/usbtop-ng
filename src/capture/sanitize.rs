//! SEC-1: strip captured USB payload from one usbmon event before it is
//! written into a fixture. Byte accounting uses the header `length` field, not
//! `len_cap`/the data field, so this is golden-neutral (see the plan's
//! Global Constraints).

/// One 48-byte binary usbmon header with `len_cap` (bytes 36..40) forced to
/// zero. No payload bytes are ever written after the returned header, so the
/// sanitized event carries no captured data.
pub fn sanitize_binary_header(header: &[u8; 48]) -> [u8; 48] {
    let mut out = *header;
    out[36..40].copy_from_slice(&0u32.to_ne_bytes());
    out
}

/// One `Nu` text line with its data field elided. Every token up to the data
/// tag (`=`, `<`, or `>` — the only single-character tokens that introduce the
/// data field) is kept verbatim (the length column, iso descriptors, and any
/// control SETUP header included); the data tag and its hex are replaced with a
/// bare `<`. A line with no data tag (a bare `E` event) is returned trimmed and
/// unchanged. Hex data words are `[0-9a-f]+`, so none can be mistaken for a
/// tag; the first tag token is always the real data field.
pub fn sanitize_text_line(line: &str) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    match tokens.iter().position(|t| matches!(*t, "=" | "<" | ">")) {
        Some(idx) => {
            let mut out = tokens[..idx].join(" ");
            out.push_str(" <");
            out
        }
        None => tokens.join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_sanitizer_zeroes_len_cap_and_preserves_length() {
        let mut header = [0u8; 48];
        header[8] = b'C';
        header[32..36].copy_from_slice(&1000u32.to_ne_bytes()); // length
        header[36..40].copy_from_slice(&512u32.to_ne_bytes()); // len_cap (payload)

        let out = sanitize_binary_header(&header);
        assert_eq!(&out[36..40], &0u32.to_ne_bytes(), "len_cap zeroed");
        assert_eq!(&out[32..36], &1000u32.to_ne_bytes(), "length preserved");
        assert_eq!(out[8], b'C', "type preserved");
    }

    #[test]
    fn text_sanitizer_elides_captured_data_to_a_bare_marker() {
        let line = "ffff0000aaaa0001 200 C Bi:1:003:1 0 512 = 00 11 22 33";
        assert_eq!(
            sanitize_text_line(line),
            "ffff0000aaaa0001 200 C Bi:1:003:1 0 512 <"
        );
    }

    #[test]
    fn text_sanitizer_leaves_a_no_data_line_semantically_unchanged() {
        let line = "ffff0000aaaa0001 200 C Bi:1:004:1 0 1000 <";
        assert_eq!(sanitize_text_line(line), line);
    }

    #[test]
    fn text_sanitizer_keeps_the_control_setup_header_but_drops_the_data() {
        // Word 5 is the `s`-prefixed control SETUP header (request metadata,
        // not len_cap payload); word 8 is the data field. Only the data goes.
        let line = "ffff880067b00300 373151059 S Ci:2:001:0 s a3 00 0000 0003 0004 4 <";
        assert_eq!(sanitize_text_line(line), line);
    }

    #[test]
    fn text_sanitizer_leaves_a_bare_error_line_unchanged() {
        let line = "ffff88006fff3800 2453805583 E Bi:1:004:1 -108";
        assert_eq!(sanitize_text_line(line), line);
    }
}

//! Synchronized output: asking the terminal whether it can hold a frame back
//! until the frame is whole, and deciding when asking is a bad idea.
//!
//! A frame is a diff, and a terminal paints a diff as it arrives. On anything
//! slower than a local pty that is visible: the top half of the table is the
//! new numbers and the bottom half is still the old ones for as long as the
//! bytes take to land. Mode 2026 fixes it — `ESC [ ? 2026 h` tells the terminal
//! to stop presenting, and `ESC [ ? 2026 l` tells it to present everything
//! since, once, as a unit.
//!
//! Emitting it blind is not an option. A terminal that does not know the mode
//! is *supposed* to ignore it, and most do, but the sequence is young enough
//! that "most" is doing real work in that sentence — and one that mishandles it
//! leaves the user staring at a screen that has stopped updating. So it is
//! asked first, with DECRQM ([`MODE_2026_QUERY`]), and only a terminal that
//! answers yes gets bracketed frames.
//!
//! The asking has its own hazard, which is why [`probe_decision`] exists. A
//! query is only half a round trip: the reply arrives on *stdin*, and anything
//! that does not reply costs the wait before startup gives up. Over ssh the
//! wait is the network's, the reply may be swallowed by a multiplexer in the
//! middle, and any byte that goes unread is a byte the input thread later reads
//! as a keystroke. So a remote session does not probe at all unless its `TERM`
//! is on a list of terminals known to behave — a list that ships empty, which
//! makes "no synchronized output over ssh" today's policy rather than an
//! accident.

#[cfg(unix)]
use std::io;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// Whether the terminal will hold a frame back until it is whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Frames are bracketed with [`BEGIN_SYNCHRONIZED_UPDATE`] and
    /// [`END_SYNCHRONIZED_UPDATE`].
    Supported,
    /// Frames go out bare, as they always did.
    Unsupported,
}

/// Whether a session is one where the DECRQM handshake is worth running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeDecision {
    /// Ask the terminal.
    Probe,
    /// Do not ask; assume [`SyncMode::Unsupported`].
    AssumeUnsupported,
}

/// Begin a synchronized update: stop presenting until the matching end.
pub const BEGIN_SYNCHRONIZED_UPDATE: &str = "\x1b[?2026h";

/// End a synchronized update, presenting everything since the begin at once.
///
/// Also what `lifecycle::restore_terminal` emits unconditionally on the way
/// out, because a session that died mid-update leaves a terminal holding
/// everything back — including the restore sequences.
pub const END_SYNCHRONIZED_UPDATE: &str = "\x1b[?2026l";

/// What [`probe_sync_mode`] writes: "is mode 2026 set?", then "identify
/// yourself".
///
/// The second half is the part that makes the first half safe. DECRQM has no
/// negative answer — a terminal that does not know mode 2026 says nothing at
/// all — so on its own the query is indistinguishable from a slow reply and
/// costs the full timeout every time. DA1 is answered by everything back to a
/// real VT100, so its reply is the marker for "that is all you are getting":
/// it arrives after the DECRQM reply if there was one, and on its own if there
/// was not. Reading up to it is also what keeps the reply bytes out of the
/// input thread's keystrokes.
pub const MODE_2026_QUERY: &str = "\x1b[?2026$p\x1b[c";

/// Longest [`probe_sync_mode`] waits for the terminal to answer.
///
/// It is paid once, before the first frame, and only by terminals that answer
/// nothing at all — everything that answers DA1 ends the wait early. A tenth of
/// a second is long enough for a local pty by three orders of magnitude and
/// short enough not to read as a slow start.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(100);

/// Terminals trusted to answer DECRQM honestly over ssh, by `TERM`.
///
/// Ships empty, and that is the policy rather than a gap: over ssh the reply
/// crosses a network and possibly a multiplexer, and a wrong answer costs
/// either a hundred milliseconds of startup or a screen that stops updating.
/// An entry here is a claim that a given terminal has been checked end to end
/// through ssh, which is a claim nobody has made yet.
const KNOWN_GOOD_OVER_SSH: &[&str] = &[];

/// Whether to run the handshake, given the environment the session started in.
///
/// `ssh_tty` is `SSH_TTY` and `term` is `TERM`, both as the process received
/// them. A variable that is set but empty names nothing and is read as absent,
/// which is what a shell that exported it without a value meant.
pub fn probe_decision(ssh_tty: Option<&str>, term: Option<&str>) -> ProbeDecision {
    let remote = ssh_tty.is_some_and(|tty| !tty.is_empty());
    if !remote || term.is_some_and(|term| KNOWN_GOOD_OVER_SSH.contains(&term)) {
        return ProbeDecision::Probe;
    }
    ProbeDecision::AssumeUnsupported
}

/// What the terminal's reply says about mode 2026.
///
/// `None` means the reply is still arriving — nothing in `buf` yet settles the
/// question. Anything that does settle it is a `Some`, including a DA1 reply
/// with no DECRPM report in front of it: the terminal answered, it just had
/// nothing to say about mode 2026, which is exactly how a terminal that does
/// not know the mode declines.
pub fn parse_decrqm_response(buf: &[u8]) -> Option<SyncMode> {
    let mut identified = false;

    for sequence in csi_sequences(buf) {
        if let Some(mode) = reported_mode_2026(&sequence) {
            return Some(mode);
        }
        identified |= sequence.final_byte == DA1_FINAL;
    }

    // Nothing about mode 2026, but the terminal has finished answering: it
    // ignored the query, which is the only "no" DECRQM has.
    identified.then_some(SyncMode::Unsupported)
}

/// Whether `buf` holds the terminal's identification, and so everything the
/// terminal was ever going to say.
///
/// This is the read loop's stop condition. It is deliberately not "does the
/// buffer contain a `c`" — a `c` the user typed before the UI came up sits in
/// the same buffer, and stopping on it would leave the real reply behind for
/// the input thread to read as keystrokes.
fn identification_complete(buf: &[u8]) -> bool {
    csi_sequences(buf).any(|sequence| sequence.final_byte == DA1_FINAL)
}

/// A complete CSI sequence: `ESC [`, then parameters, then intermediates, then
/// the final byte that says what it was.
struct Csi<'a> {
    parameters: &'a [u8],
    intermediates: &'a [u8],
    final_byte: u8,
}

/// The final byte of a DECRPM mode report, and of a DA1 identification.
const DECRPM_FINAL: u8 = b'y';
const DA1_FINAL: u8 = b'c';

/// The intermediate byte that separates a DECRPM report from every other CSI
/// sequence ending in `y` (`ESC [ Ps ; Ps y` is DECTST).
const DECRPM_INTERMEDIATE: &[u8] = b"$";

/// What a report about mode 2026 begins with: the private-marker `?`, the mode
/// number, and the separator before its setting.
const MODE_2026_REPORT: &[u8] = b"?2026;";

/// Every complete CSI sequence in `buf`, in order.
///
/// Bytes that are not part of one are skipped, so typeahead and a mangled
/// escape are both survivable; a sequence that runs off the end of the buffer
/// ends the scan, because the rest of it has not arrived yet.
fn csi_sequences(buf: &[u8]) -> impl Iterator<Item = Csi<'_>> {
    // Per ECMA-48: parameters are 0x30-0x3F, intermediates 0x20-0x2F, and the
    // final byte 0x40-0x7E.
    fn run(buf: &[u8], range: std::ops::RangeInclusive<u8>) -> usize {
        buf.iter().take_while(|byte| range.contains(byte)).count()
    }

    let mut at = 0;
    std::iter::from_fn(move || {
        while at + 1 < buf.len() {
            if buf[at] != b'\x1b' || buf[at + 1] != b'[' {
                at += 1;
                continue;
            }

            let parameters = at + 2;
            let intermediates = parameters + run(&buf[parameters..], 0x30..=0x3f);
            let final_byte = intermediates + run(&buf[intermediates..], 0x20..=0x2f);

            match buf.get(final_byte) {
                Some(&byte) if (0x40..=0x7e).contains(&byte) => {
                    at = final_byte + 1;
                    return Some(Csi {
                        parameters: &buf[parameters..intermediates],
                        intermediates: &buf[intermediates..final_byte],
                        final_byte: byte,
                    });
                }
                // Not a final byte where one belongs: this `ESC [` began
                // something else (or nothing), so resume the scan just past it.
                Some(_) => at += 1,
                // The end of the buffer, not the end of the sequence. Whatever
                // follows has not been read yet.
                None => return None,
            }
        }
        None
    })
}

/// What a CSI sequence says about mode 2026, if it says anything.
fn reported_mode_2026(sequence: &Csi) -> Option<SyncMode> {
    if sequence.final_byte != DECRPM_FINAL || sequence.intermediates != DECRPM_INTERMEDIATE {
        return None;
    }

    let setting = sequence.parameters.strip_prefix(MODE_2026_REPORT)?;
    Some(match setting {
        // Set or reset: either way the terminal knows the mode and will act on
        // it. Everything else is 0 (not recognized) or 3/4 (recognized but
        // permanently set or reset, so asking for it changes nothing) — and an
        // unknown setting is treated the same way, because a report this code
        // cannot read is not consent.
        b"1" | b"2" => SyncMode::Supported,
        _ => SyncMode::Unsupported,
    })
}

/// Run the handshake on the real terminal and answer for this session.
///
/// The caller owes three things, all of them held by `tui::enter_terminal`:
/// raw mode is on (a canonical-mode terminal would hold the reply back until a
/// newline that is never coming, and echo it), stdout is still blocking (the
/// query must not come back short), and no input thread exists yet (whoever
/// reads stdin next takes the reply). Every failure answers
/// [`SyncMode::Unsupported`], which is the pre-existing behavior.
#[cfg(unix)]
pub fn probe_sync_mode() -> SyncMode {
    use std::io::Write;

    // SAFETY: a plain query about a descriptor this process owns for its whole
    // life; it cannot fail in a way that matters here.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        // Nothing on stdin that could answer, and reading it would eat bytes
        // belonging to whatever is really on the other end.
        return SyncMode::Unsupported;
    }

    let mut stdout = std::io::stdout();
    // Buffered until flushed: the query has no newline in it, and `Stdout` is
    // a `LineWriter`.
    if write!(stdout, "{MODE_2026_QUERY}")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return SyncMode::Unsupported;
    }

    let reply = read_reply(Instant::now() + PROBE_TIMEOUT);
    // A reply that never finished arriving is not consent.
    parse_decrqm_response(&reply).unwrap_or(SyncMode::Unsupported)
}

/// Most bytes the handshake takes off stdin before giving up on it.
///
/// Both replies together are well under fifty. The cap is for the terminal
/// that answers something else entirely, so that a stuck handshake costs a
/// bounded read rather than however much a confused terminal can produce in a
/// tenth of a second.
#[cfg(unix)]
const MAX_REPLY: usize = 512;

/// Accumulate stdin until the terminal has finished identifying itself, or
/// `deadline` passes. Whatever arrived is the answer, complete or not.
#[cfg(unix)]
fn read_reply(deadline: Instant) -> Vec<u8> {
    let mut reply = Vec::new();

    while reply.len() < MAX_REPLY {
        let budget = deadline.saturating_duration_since(Instant::now());
        if budget.is_zero() {
            break;
        }

        match stdin_readable(budget) {
            Ok(true) => {}
            // A wait that expired and a wait a signal cut short are the same
            // thing here: come back for whatever is left of the budget.
            Ok(false) => continue,
            Err(_) => break,
        }

        let mut chunk = [0u8; 64];
        match read_stdin(&mut chunk) {
            // The far end closed. There is no reply coming.
            Ok(0) => break,
            Ok(read) => reply.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }

        // Stopping at the mode report instead would leave the identification
        // on stdin for the input thread to read as keystrokes — and its
        // leading `ESC` is bound to quit.
        if identification_complete(&reply) {
            break;
        }
    }

    reply
}

/// Wait up to `budget` for stdin to have something to read.
///
/// `Ok(false)` is "not yet, and not for a reason worth reporting" — the wait
/// expired, or a signal cut it short. Only a wait that could not be performed
/// at all is an `Err`.
#[cfg(unix)]
fn stdin_readable(budget: Duration) -> io::Result<bool> {
    let mut watch = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // At least a millisecond: a sub-millisecond budget truncated to zero would
    // make this a spin for the rest of the budget.
    let timeout = i32::try_from(budget.as_millis().max(1)).unwrap_or(i32::MAX);

    // SAFETY: `watch` is one initialized `pollfd` and the count says so. The
    // call reads `fd` and `events`, writes `revents`, and reports failure as a
    // negative return.
    let ready = unsafe { libc::poll(&raw mut watch, 1, timeout) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        return match error.kind() {
            io::ErrorKind::Interrupted => Ok(false),
            _ => Err(error),
        };
    }
    Ok(ready > 0)
}

/// One `read(2)` of stdin. The caller has already established that it will not
/// block.
#[cfg(unix)]
fn read_stdin(into: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `STDIN_FILENO` is valid for the life of the process; the pointer
    // and length describe a slice this call only writes into, and the return
    // is checked before it is used as a length.
    let read = unsafe {
        libc::read(
            libc::STDIN_FILENO,
            into.as_mut_ptr().cast::<libc::c_void>(),
            into.len(),
        )
    };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    // Non-negative by the check above, so the conversion cannot fail.
    Ok(usize::try_from(read).unwrap_or(0))
}

/// Off unix there is no handshake to run: this path has no `poll(2)` on stdin,
/// and a query nobody reads the answer to is just bytes on the screen. The
/// answer is the pre-existing behavior, unbracketed frames.
#[cfg(not(unix))]
pub fn probe_sync_mode() -> SyncMode {
    SyncMode::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_session_asks_the_terminal() {
        assert_eq!(
            probe_decision(None, Some("xterm-256color")),
            ProbeDecision::Probe
        );
        assert_eq!(probe_decision(None, None), ProbeDecision::Probe);
    }

    #[test]
    fn an_ssh_session_does_not_ask_whatever_its_term_says() {
        // The allowlist is empty, so every remote session lands here — that is
        // the shipped posture, not a missing case.
        for term in [
            Some("xterm-256color"),
            Some("foot"),
            Some("alacritty"),
            Some("screen-256color"),
            None,
        ] {
            assert_eq!(
                probe_decision(Some("/dev/pts/3"), term),
                ProbeDecision::AssumeUnsupported,
                "TERM={term:?} over ssh"
            );
        }
    }

    #[test]
    fn the_ssh_allowlist_ships_empty() {
        assert!(
            KNOWN_GOOD_OVER_SSH.is_empty(),
            "an entry here is a claim that a terminal was checked through ssh"
        );
    }

    #[test]
    fn a_variable_exported_without_a_value_names_no_tty() {
        assert_eq!(probe_decision(Some(""), Some("foot")), ProbeDecision::Probe);
    }

    #[test]
    fn a_terminal_holding_the_mode_either_way_supports_it() {
        // 1 is "set", 2 is "reset": both mean the terminal knows the mode.
        assert_eq!(
            parse_decrqm_response(b"\x1b[?2026;1$y"),
            Some(SyncMode::Supported)
        );
        assert_eq!(
            parse_decrqm_response(b"\x1b[?2026;2$y"),
            Some(SyncMode::Supported)
        );
    }

    #[test]
    fn a_mode_that_is_unrecognized_or_stuck_is_not_support() {
        // 0 is "the mode is not recognized"; 3 and 4 are recognized but
        // permanently set or reset, so asking for it changes nothing.
        for reply in [
            &b"\x1b[?2026;0$y"[..],
            &b"\x1b[?2026;3$y"[..],
            &b"\x1b[?2026;4$y"[..],
        ] {
            assert_eq!(
                parse_decrqm_response(reply),
                Some(SyncMode::Unsupported),
                "{reply:?}"
            );
        }
    }

    #[test]
    fn a_terminal_that_only_identifies_itself_has_declined() {
        // DA1 came back and the DECRQM query did not: the terminal answered,
        // and its answer about mode 2026 is silence.
        assert_eq!(
            parse_decrqm_response(b"\x1b[?62;1;2;6;9;15;22;29c"),
            Some(SyncMode::Unsupported)
        );
        assert_eq!(
            parse_decrqm_response(b"\x1b[?1;2c"),
            Some(SyncMode::Unsupported)
        );
    }

    #[test]
    fn a_reply_still_arriving_is_not_an_answer() {
        for partial in [
            &b""[..],
            &b"\x1b"[..],
            &b"\x1b["[..],
            &b"\x1b[?2026"[..],
            &b"\x1b[?2026;"[..],
            &b"\x1b[?2026;1"[..],
            &b"\x1b[?2026;1$"[..],
            &b"\x1b[?62;1;2"[..],
        ] {
            assert_eq!(
                parse_decrqm_response(partial),
                None,
                "{partial:?} settles nothing yet"
            );
        }
    }

    #[test]
    fn the_mode_report_wins_over_the_identification_that_follows_it() {
        // The usual shape of a real reply: both queries answered, in order.
        assert_eq!(
            parse_decrqm_response(b"\x1b[?2026;2$y\x1b[?62;1;2c"),
            Some(SyncMode::Supported)
        );
    }

    #[test]
    fn typeahead_in_front_of_the_reply_does_not_hide_it() {
        // Whatever the user typed before the UI came up is in the same buffer.
        // A bare `c` is not a terminal identifying itself, and a bare `$y` is
        // not a mode report.
        assert_eq!(
            parse_decrqm_response(b"abc$y\x1b[?2026;1$y"),
            Some(SyncMode::Supported)
        );
        assert_eq!(parse_decrqm_response(b"abc$y"), None);
    }

    #[test]
    fn a_report_for_some_other_mode_is_not_an_answer_about_this_one() {
        // Bracketed paste, say, reported by a terminal answering a query this
        // program never sent.
        assert_eq!(parse_decrqm_response(b"\x1b[?2004;1$y"), None);
        assert_eq!(
            parse_decrqm_response(b"\x1b[?2004;1$y\x1b[?1;2c"),
            Some(SyncMode::Unsupported)
        );
    }

    #[test]
    fn only_a_real_identification_ends_the_read() {
        // The read loop stops on this and nothing else, so a `c` the user
        // typed before the UI came up must not end it: the real reply would be
        // left on stdin for the input thread, and the `ESC` at its front is
        // bound to quit.
        assert!(!identification_complete(b"c"));
        assert!(!identification_complete(b"cccc"));
        assert!(!identification_complete(b"\x1b[?62;1;2"));
        assert!(!identification_complete(b"\x1b[?2026;1$y"));
        assert!(identification_complete(b"\x1b[?2026;1$y\x1b[?62;1;2c"));
        assert!(identification_complete(b"\x1b[?1;2c"));
    }

    #[test]
    fn a_mangled_sequence_does_not_swallow_the_reply_behind_it() {
        // An 8-bit or truncated escape in the buffer must not stop the scan:
        // the answer may be the next sequence along.
        assert_eq!(
            parse_decrqm_response(b"\x1b[\x1b[?2026;1$y"),
            Some(SyncMode::Supported)
        );
    }
}

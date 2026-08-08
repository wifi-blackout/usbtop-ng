//! The output stage: what stands between ratatui and the terminal's file
//! descriptor.
//!
//! A terminal is a pipe, and a pipe fills up. Writing straight to a blocking
//! stdout means the render loop stops dead whenever the far end stops reading —
//! a scrolled-back tmux pane, a laggy ssh link, a suspended emulator — and a
//! stopped render loop is also a stopped input loop, so the UI hangs on
//! something that has nothing to do with USB.
//!
//! [`ShedWriter`] takes the descriptor non-blocking instead and absorbs the
//! difference. Bytes are staged in memory until ratatui flushes, at which point
//! the staged bytes become *one frame* — one queue entry, written as one burst.
//! The queue is drained as far as the descriptor will take it and no further.
//! When the backlog outgrows [`ShedWriter::new`]'s watermark the queued frames
//! are dropped rather than buffered forever: they are diffs against a screen
//! state that will now never exist, so keeping them would desync the display.
//! What replaces them is a single full repaint, which the loop issues on seeing
//! [`ShedHandles::take_repaint_request`].
//!
//! Nothing here ever reports failure through [`std::io::Write`]. Returning an
//! error to ratatui mid-frame would take its teardown off the healthy path for
//! something the loop can handle better a few microseconds later, so the flags
//! behind [`ShedHandles`] are the signalling channel and both `write` and
//! `flush` are infallible.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long after a shed the writer refuses to shed again.
///
/// The frame that follows a shed is a full repaint — the biggest frame the app
/// ever emits. Without this it would arrive at a queue that is still over its
/// watermark, be shed in turn, and ask for another repaint: a slow terminal
/// would spend its whole life recovering from its own recovery.
pub const SHED_GRACE: Duration = Duration::from_secs(1);

/// The far end of the output stage: somewhere bytes go, one non-blocking write
/// at a time.
///
/// This is the seam that keeps the whole backpressure story testable without a
/// terminal — the tests script `WouldBlock`, short writes and hard errors in
/// whatever order they need.
pub trait RawOut {
    /// Write what will fit right now. Short writes are normal, not an error;
    /// so is [`io::ErrorKind::WouldBlock`] with nothing written at all.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
}

/// Standard output with `O_NONBLOCK` set, written through raw `write(2)`.
///
/// The descriptor is stdout itself rather than a fresh `/dev/tty`: the flags
/// are restored on the way out by `lifecycle::restore_terminal`, and what it
/// saved (in `arm_restore`, before this type exists) is stdout's. A second
/// descriptor would be a second set of flags with nobody to put them back —
/// and non-blocking outlives the process on a shared descriptor, so the shell
/// that started usbtop-ng would inherit it.
#[cfg(unix)]
pub struct StdoutRaw {
    fd: std::os::unix::io::RawFd,
}

#[cfg(unix)]
impl StdoutRaw {
    /// Switch stdout to non-blocking.
    ///
    /// The caller must already have armed the restore (see
    /// `lifecycle::arm_restore`), because the flags worth putting back are the
    /// ones read *before* this runs.
    pub fn new() -> io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let fd = io::stdout().as_raw_fd();
        // SAFETY: F_GETFL only reads the flags of a descriptor this process
        // owns for its whole life, and reports failure as a negative return.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: same descriptor; F_SETFL with the flags just read plus one
        // more bit is a round trip through values the kernel produced.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }
}

#[cfg(unix)]
impl RawOut for StdoutRaw {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Deliberately not `io::Stdout::write`: that is a `LineWriter`, and a
        // frame has no newlines in it, so the escape sequences would sit in
        // std's buffer until something unrelated flushed them — and a partial
        // flush of that buffer is exactly the mid-escape truncation the frame
        // queue exists to prevent.
        //
        // SAFETY: `fd` is stdout, valid for the life of the process; the
        // pointer and length describe a slice this call only reads from, and
        // the return is checked before it is used as a length.
        let written =
            unsafe { libc::write(self.fd, buf.as_ptr().cast::<libc::c_void>(), buf.len()) };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        // Non-negative by the check above, so the conversion cannot fail; a
        // zero fallback would only mean "made no progress", which the drain
        // already handles.
        Ok(usize::try_from(written).unwrap_or(0))
    }
}

/// Standard output, blocking, on platforms with no `fcntl`.
///
/// Nothing here can shed, because nothing here can tell that the terminal is
/// behind: a blocking write returns only once the bytes are gone. That is the
/// pre-existing behavior, kept so the rest of the module compiles and runs
/// unchanged off unix.
#[cfg(not(unix))]
pub struct StdoutRaw {
    stdout: io::Stdout,
}

#[cfg(not(unix))]
impl StdoutRaw {
    /// Never fails; the signature matches the unix one so callers need no
    /// `cfg` of their own.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            stdout: io::stdout(),
        })
    }
}

#[cfg(not(unix))]
impl RawOut for StdoutRaw {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // All-or-nothing: a blocking descriptor has no reason to stop early,
        // so there is no partial write to resume from.
        self.stdout.write_all(buf)?;
        self.stdout.flush()?;
        Ok(buf.len())
    }
}

/// Everything about a live [`ShedWriter`] that the event loop can reach.
///
/// The writer itself disappears into ratatui's backend, which keeps it private,
/// so the shared cells are how the loop hears about a shed and how a resize
/// gets back in. Cheap to hold: it is all `Arc`s.
pub struct ShedHandles {
    shed_frames: Arc<AtomicU64>,
    needs_full_repaint: Arc<AtomicBool>,
    invalidated: Arc<AtomicBool>,
    terminal_dead: Arc<AtomicBool>,
    high_watermark: Arc<AtomicUsize>,
    discard_requested: Arc<AtomicBool>,
}

impl ShedHandles {
    /// The count of frames dropped to backpressure so far, for the header to
    /// render. Shared, so the header reads it live.
    pub fn shed_frames(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.shed_frames)
    }

    /// Whether the screen the writer left behind can still be trusted, and
    /// clear the question.
    ///
    /// True after a shed (frames the terminal never saw are missing from it)
    /// or after a failed write (bytes the terminal never saw are missing from
    /// it). Either way the answer is the same: wipe it and paint the lot.
    /// Both flags are consumed on every call — reading one and leaving the
    /// other set would mean an untrusted screen for a whole extra frame.
    pub fn take_repaint_request(&self) -> bool {
        let shed = self.needs_full_repaint.swap(false, Ordering::SeqCst);
        let failed = self.invalidated.swap(false, Ordering::SeqCst);
        shed || failed
    }

    /// Whether a write found the terminal gone (`EPIPE`/`EIO`). There is
    /// nothing to draw on after this, so the loop's only move is to leave.
    pub fn terminal_dead(&self) -> bool {
        self.terminal_dead.load(Ordering::SeqCst)
    }

    /// Tell the writer how big the screen is now, which is what its watermark
    /// is a function of.
    pub fn set_area(&self, cols: u16, rows: u16) {
        self.high_watermark
            .store(high_watermark(cols, rows), Ordering::SeqCst);
    }

    /// Throw away whatever is still queued, at the next flush.
    ///
    /// For teardown. The frames in the queue are diffs against an alternate
    /// screen that is about to be left; flushing them afterwards would scribble
    /// them across the shell the user just got back. On a healthy terminal the
    /// queue is already empty and this changes nothing.
    pub fn discard_pending(&self) {
        self.discard_requested.store(true, Ordering::SeqCst);
    }
}

/// tmux's rule for "too far behind": a screen's worth of cells at eight bytes
/// each, which is roughly two full repaints' worth of escape sequences.
fn high_watermark(cols: u16, rows: u16) -> usize {
    1 + usize::from(cols) * usize::from(rows) * 8
}

/// A [`Write`] that never blocks the render loop and never lies about it.
///
/// See the module documentation for the shape of the thing; the algorithm is
/// all in [`ShedWriter::flush_at`].
pub struct ShedWriter<R: RawOut> {
    raw: R,
    /// The frame ratatui is still writing. Becomes a queue entry on flush.
    staging: Vec<u8>,
    /// Whole frames, oldest first. Frame granularity is what makes truncation
    /// mid-escape-sequence impossible.
    pending: VecDeque<Vec<u8>>,
    /// How far into `pending.front()` the terminal has got.
    front_cursor: usize,
    /// Bytes queued and not yet written, `front_cursor` accounted for.
    pending_bytes: usize,
    /// Live so a resize can move it; see [`ShedHandles::set_area`].
    high_watermark: Arc<AtomicUsize>,
    /// While set, [`ShedWriter::flush_at`] will not shed. See [`SHED_GRACE`].
    grace_until: Option<Instant>,
    shed_frames: Arc<AtomicU64>,
    needs_full_repaint: Arc<AtomicBool>,
    invalidated: Arc<AtomicBool>,
    terminal_dead: Arc<AtomicBool>,
    discard_requested: Arc<AtomicBool>,
}

impl<R: RawOut> ShedWriter<R> {
    /// Wrap `raw` for a `cols` x `rows` screen.
    pub fn new(raw: R, cols: u16, rows: u16) -> Self {
        Self {
            raw,
            staging: Vec::new(),
            pending: VecDeque::new(),
            front_cursor: 0,
            pending_bytes: 0,
            high_watermark: Arc::new(AtomicUsize::new(high_watermark(cols, rows))),
            grace_until: None,
            shed_frames: Arc::new(AtomicU64::new(0)),
            needs_full_repaint: Arc::new(AtomicBool::new(false)),
            invalidated: Arc::new(AtomicBool::new(false)),
            terminal_dead: Arc::new(AtomicBool::new(false)),
            discard_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The shared cells the event loop watches. Take these before handing the
    /// writer to ratatui, which keeps it to itself from then on.
    pub fn handles(&self) -> ShedHandles {
        ShedHandles {
            shed_frames: Arc::clone(&self.shed_frames),
            needs_full_repaint: Arc::clone(&self.needs_full_repaint),
            invalidated: Arc::clone(&self.invalidated),
            terminal_dead: Arc::clone(&self.terminal_dead),
            high_watermark: Arc::clone(&self.high_watermark),
            discard_requested: Arc::clone(&self.discard_requested),
        }
    }

    /// The body of [`Write::flush`], with the clock as a parameter so the grace
    /// period is testable without sitting out a real second.
    fn flush_at(&mut self, now: Instant) {
        if self.discard_requested.swap(false, Ordering::SeqCst) {
            self.pending.clear();
            self.front_cursor = 0;
            self.pending_bytes = 0;
        }

        self.stage_frame();
        self.shed_if_swamped(now);
        self.drain();
    }

    /// Close off what ratatui just wrote and queue it as one frame.
    fn stage_frame(&mut self) {
        if self.staging.is_empty() {
            return;
        }
        let frame = std::mem::take(&mut self.staging);
        self.pending_bytes += frame.len();
        self.pending.push_back(frame);
    }

    /// Drop the backlog if it has outgrown the watermark, and ask for a repaint
    /// to replace it.
    fn shed_if_swamped(&mut self, now: Instant) {
        if let Some(until) = self.grace_until {
            if now < until {
                return;
            }
            self.grace_until = None;
        }

        if self.pending_bytes <= self.high_watermark.load(Ordering::SeqCst) {
            return;
        }

        // A frame with bytes already on the wire is finished, never dropped:
        // its tail may be the rest of an escape sequence the terminal is in the
        // middle of reading, and truncating that is the one failure a
        // frame-granular queue exists to make impossible. Everything behind it
        // has put nothing on the wire and can go.
        let partial_front = self.front_cursor > 0;
        let sheddable = self.pending.len() - usize::from(partial_front);
        if sheddable == 0 {
            return;
        }

        let retained = if partial_front {
            self.pending.pop_front()
        } else {
            None
        };
        self.pending.clear();
        match retained {
            Some(front) => {
                self.pending_bytes = front.len().saturating_sub(self.front_cursor);
                self.pending.push_back(front);
            }
            None => {
                self.front_cursor = 0;
                self.pending_bytes = 0;
            }
        }

        self.shed_frames.fetch_add(
            u64::try_from(sheddable).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.needs_full_repaint.store(true, Ordering::SeqCst);
        self.grace_until = Some(now + SHED_GRACE);
    }

    /// Write as much of the queue as the descriptor will take.
    fn drain(&mut self) {
        // Whether there was anything to drain decides what an empty queue means
        // at the end: frames that went out, or frames that were shed.
        let had_frames = !self.pending.is_empty();

        while let Some(frame_len) = self.pending.front().map(Vec::len) {
            if self.front_cursor >= frame_len {
                self.pending.pop_front();
                self.front_cursor = 0;
                continue;
            }

            // Indexable because the queue is non-empty by the `while let`, and
            // in range because the cursor is inside the frame by the check
            // above.
            match self.raw.write(&self.pending[0][self.front_cursor..]) {
                // Not progress, and looping on it would spin. A non-blocking
                // descriptor says "not now" with `WouldBlock`, so this is only
                // reachable from an odd `RawOut`; treat it the same way.
                Ok(0) => return,
                Ok(written) => {
                    self.front_cursor += written;
                    self.pending_bytes = self.pending_bytes.saturating_sub(written);
                }
                // A signal landed mid-write; nothing was written, so retry.
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // The terminal is full. The cursor stays where it is and the
                // next flush picks the frame up mid-way.
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if is_terminal_death(&error) => {
                    self.terminal_dead.store(true, Ordering::SeqCst);
                    return;
                }
                // Something else went wrong, so the terminal's contents no
                // longer match ratatui's idea of them. The frame is beyond
                // saving; the loop repaints the screen from scratch.
                Err(_) => {
                    self.invalidated.store(true, Ordering::SeqCst);
                    self.drop_front_frame();
                    return;
                }
            }
        }

        if had_frames {
            // Everything queued reached the terminal, so it is keeping up
            // again and the recovery frame no longer needs protecting.
            self.grace_until = None;
        }
    }

    /// Throw away the frame at the head of the queue, cursor and all.
    fn drop_front_frame(&mut self) {
        if let Some(frame) = self.pending.pop_front() {
            self.pending_bytes = self
                .pending_bytes
                .saturating_sub(frame.len().saturating_sub(self.front_cursor));
        }
        self.front_cursor = 0;
    }

    /// The scripted `RawOut` behind the writer, so tests can read what actually
    /// reached the wire.
    #[cfg(test)]
    fn raw(&self) -> &R {
        &self.raw
    }
}

impl<R: RawOut> Write for ShedWriter<R> {
    /// Stage bytes for the frame in progress. Cannot fail: nothing has been
    /// asked of the terminal yet.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.staging.extend_from_slice(buf);
        Ok(buf.len())
    }

    /// End the frame and push the queue as far as it will go. Never `Err`: see
    /// the module documentation.
    fn flush(&mut self) -> io::Result<()> {
        self.flush_at(Instant::now());
        Ok(())
    }
}

/// Whether a write error means the terminal itself is gone rather than that
/// this one write failed.
///
/// `EPIPE` is the pipe's far end closing; `EIO` is what a pty gives once its
/// master is gone or the session leader has left. Neither is worth retrying —
/// there is no terminal left to retry onto.
#[cfg(unix)]
fn is_terminal_death(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::BrokenPipe || error.raw_os_error() == Some(libc::EIO)
}

#[cfg(not(unix))]
fn is_terminal_death(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::BrokenPipe
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a scripted terminal does once its script runs out.
    enum Exhausted {
        /// Take everything, like a terminal that is keeping up.
        Accept,
        /// Take nothing, ever, like a terminal that has stopped reading.
        Block,
    }

    /// A terminal made of a list of answers. Nothing here touches a real
    /// descriptor, so the whole backpressure story is testable in-process.
    struct ScriptedRaw {
        script: VecDeque<io::Result<usize>>,
        after_script: Exhausted,
        written: Vec<u8>,
        /// How many times the writer asked, script or no script.
        calls: usize,
    }

    impl ScriptedRaw {
        fn scripted(script: Vec<io::Result<usize>>) -> Self {
            Self {
                script: script.into(),
                after_script: Exhausted::Accept,
                written: Vec::new(),
                calls: 0,
            }
        }

        /// A terminal that never takes another byte.
        fn wedged() -> Self {
            Self {
                script: VecDeque::new(),
                after_script: Exhausted::Block,
                written: Vec::new(),
                calls: 0,
            }
        }
    }

    impl RawOut for ScriptedRaw {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            let outcome = self.script.pop_front().unwrap_or(match self.after_script {
                Exhausted::Accept => Ok(buf.len()),
                Exhausted::Block => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            });
            match outcome {
                Ok(count) => {
                    let count = count.min(buf.len());
                    self.written.extend_from_slice(&buf[..count]);
                    Ok(count)
                }
                Err(error) => Err(error),
            }
        }
    }

    fn would_block() -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }

    /// A writer over a scripted terminal, plus the loop's view of it.
    fn writer(raw: ScriptedRaw, cols: u16, rows: u16) -> (ShedWriter<ScriptedRaw>, ShedHandles) {
        let writer = ShedWriter::new(raw, cols, rows);
        let handles = writer.handles();
        (writer, handles)
    }

    /// Put one frame through: everything ratatui writes between two flushes.
    fn frame(writer: &mut ShedWriter<ScriptedRaw>, bytes: &[u8]) {
        writer.write_all(bytes).expect("staging cannot fail");
        writer.flush().expect("flush never reports failure");
    }

    #[test]
    fn a_frame_interrupted_by_a_full_terminal_resumes_where_it_stopped() {
        let (mut writer, _handles) =
            writer(ScriptedRaw::scripted(vec![Ok(10), would_block()]), 80, 24);

        frame(&mut writer, b"0123456789abcdefghij");
        assert_eq!(
            writer.raw().written,
            b"0123456789",
            "only what the terminal took"
        );
        assert_eq!(writer.pending_bytes, 10, "the rest is still owed");

        // Nothing new to say, but the queue still has ten bytes in it.
        writer.flush().expect("flush never reports failure");
        assert_eq!(
            writer.raw().written,
            b"0123456789abcdefghij",
            "in order, once each"
        );
        assert_eq!(writer.pending_bytes, 0);
        assert!(writer.pending.is_empty());
    }

    #[test]
    fn a_backlog_past_the_watermark_is_shed_whole_and_asks_for_a_repaint() {
        // A 1x1 screen: watermark 1 + 1*1*8 = 9 bytes.
        let (mut writer, handles) = writer(ScriptedRaw::wedged(), 1, 1);
        assert_eq!(writer.high_watermark.load(Ordering::SeqCst), 9);

        frame(&mut writer, b"aaaa"); // 4 queued
        frame(&mut writer, b"bbbb"); // 8 queued, still under
        assert_eq!(handles.shed_frames.load(Ordering::Relaxed), 0);
        assert!(!handles.needs_full_repaint.load(Ordering::SeqCst));

        frame(&mut writer, b"cccc"); // 12 queued: over
        assert_eq!(
            handles.shed_frames.load(Ordering::Relaxed),
            3,
            "all three frames go, including the one that tipped it over"
        );
        assert_eq!(writer.pending_bytes, 0);
        assert!(writer.pending.is_empty());
        assert!(
            handles.needs_full_repaint.load(Ordering::SeqCst),
            "the screen is missing frames the terminal never saw"
        );
        assert!(writer.raw().written.is_empty(), "nothing reached the wire");
    }

    #[test]
    fn the_recovery_frame_is_not_shed_in_its_turn() {
        let (mut writer, handles) = writer(ScriptedRaw::wedged(), 1, 1);
        for _ in 0..3 {
            frame(&mut writer, b"aaaa");
        }
        assert_eq!(handles.shed_frames.load(Ordering::Relaxed), 3);

        // A full repaint is the biggest frame there is, and it lands on the
        // same wedged terminal: without the grace it would be shed at once and
        // ask for another repaint, forever.
        frame(&mut writer, &[b'r'; 40]);
        assert_eq!(
            handles.shed_frames.load(Ordering::Relaxed),
            3,
            "the grace period held"
        );
        assert_eq!(writer.pending_bytes, 40, "and the repaint is still queued");
    }

    #[test]
    fn the_grace_period_runs_out() {
        let (mut writer, handles) = writer(ScriptedRaw::wedged(), 1, 1);
        for _ in 0..3 {
            frame(&mut writer, b"aaaa");
        }
        let shed_at = writer.grace_until.expect("a shed starts a grace period");

        // Still behind a whole grace period later: the terminal is not slow,
        // it is gone quiet, and the queue must not grow forever.
        writer.write_all(&[b'r'; 40]).unwrap();
        writer.flush_at(shed_at + Duration::from_millis(1));
        assert_eq!(handles.shed_frames.load(Ordering::Relaxed), 4);
        assert_eq!(writer.pending_bytes, 0);
    }

    #[test]
    fn draining_the_queue_lifts_the_grace_period_early() {
        let (mut writer, _handles) = writer(ScriptedRaw::wedged(), 1, 1);
        for _ in 0..3 {
            frame(&mut writer, b"aaaa");
        }
        assert!(writer.grace_until.is_some(), "the shed armed the grace");

        // The terminal starts reading again and the recovery frame gets out.
        writer.raw.after_script = Exhausted::Accept;
        frame(&mut writer, &[b'r'; 40]);
        assert!(writer.pending.is_empty());
        assert!(
            writer.grace_until.is_none(),
            "a terminal that is keeping up needs no protection"
        );
    }

    #[test]
    fn a_shed_never_truncates_a_frame_the_terminal_has_started_reading() {
        // Watermark 9. The first frame gets two bytes out and then stalls, so
        // its tail may be the rest of an escape sequence.
        let (mut writer, handles) = writer(ScriptedRaw::scripted(vec![Ok(2), would_block()]), 1, 1);
        frame(&mut writer, b"aaaaaa");
        assert_eq!(writer.front_cursor, 2);

        writer.raw.after_script = Exhausted::Block;
        frame(&mut writer, b"bbbbbb"); // 4 + 6 = 10 queued: over
        assert_eq!(
            handles.shed_frames.load(Ordering::Relaxed),
            1,
            "only the frame that put nothing on the wire"
        );
        assert_eq!(
            writer.pending_bytes, 4,
            "the started frame keeps its unwritten tail"
        );
    }

    #[test]
    fn a_failed_write_invalidates_the_screen_and_drops_the_frame() {
        let (mut writer, handles) = writer(
            ScriptedRaw::scripted(vec![Err(io::Error::other("device is confused"))]),
            80,
            24,
        );

        frame(&mut writer, b"frame one");
        assert!(
            handles.take_repaint_request(),
            "the terminal's contents no longer match ratatui's mirror"
        );
        assert!(!handles.terminal_dead(), "one bad write is not a death");
        assert_eq!(writer.pending_bytes, 0, "the failed frame is gone");
        assert!(writer.raw().written.is_empty());

        // And the writer is still usable: the next frame goes out normally.
        frame(&mut writer, b"frame two");
        assert_eq!(writer.raw().written, b"frame two");
    }

    #[test]
    fn a_broken_pipe_is_terminal_death() {
        let (mut writer, handles) = writer(
            ScriptedRaw::scripted(vec![Err(io::Error::from(io::ErrorKind::BrokenPipe))]),
            80,
            24,
        );

        frame(&mut writer, b"one last frame");
        assert!(handles.terminal_dead());
        assert!(
            !handles.take_repaint_request(),
            "there is nothing left to repaint onto"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pty_whose_master_left_is_terminal_death() {
        let (mut writer, handles) = writer(
            ScriptedRaw::scripted(vec![Err(io::Error::from_raw_os_error(libc::EIO))]),
            80,
            24,
        );

        frame(&mut writer, b"one last frame");
        assert!(handles.terminal_dead(), "EIO on a pty is not a hiccup");
    }

    #[test]
    fn an_interrupted_write_is_retried_rather_than_lost() {
        let (mut writer, handles) = writer(
            ScriptedRaw::scripted(vec![Err(io::Error::from(io::ErrorKind::Interrupted))]),
            80,
            24,
        );

        frame(&mut writer, b"signal me");
        assert_eq!(writer.raw().written, b"signal me");
        assert_eq!(writer.raw().calls, 2, "asked again after the interruption");
        assert!(!handles.terminal_dead());
        assert!(!handles.take_repaint_request());
    }

    #[test]
    fn everything_between_two_flushes_is_one_frame() {
        // Mid-escape truncation is impossible because there is no boundary
        // inside a frame to truncate at: the queue entry is the whole thing.
        let (mut writer, _handles) = writer(ScriptedRaw::scripted(vec![would_block()]), 80, 24);

        writer.write_all(b"\x1b[1;1H").unwrap();
        writer.write_all(b"\x1b[38;5;42m").unwrap();
        writer.write_all(b"hello").unwrap();
        writer.flush().unwrap();

        assert_eq!(writer.pending.len(), 1, "one flush, one queue entry");
        assert_eq!(writer.pending[0], b"\x1b[1;1H\x1b[38;5;42mhello");
    }

    #[test]
    fn a_flush_with_nothing_staged_queues_nothing() {
        let (mut writer, _handles) = writer(ScriptedRaw::wedged(), 80, 24);

        writer.flush().unwrap();
        assert!(writer.pending.is_empty());
        assert_eq!(writer.raw().calls, 0, "nothing to ask the terminal for");
    }

    #[test]
    fn a_resize_moves_the_watermark() {
        let (writer, handles) = writer(ScriptedRaw::wedged(), 80, 24);
        assert_eq!(
            writer.high_watermark.load(Ordering::SeqCst),
            1 + 80 * 24 * 8
        );

        handles.set_area(120, 40);
        assert_eq!(
            writer.high_watermark.load(Ordering::SeqCst),
            1 + 120 * 40 * 8,
            "a bigger screen is allowed a bigger backlog"
        );
    }

    #[test]
    fn both_repaint_flags_are_consumed_together() {
        let (_writer, handles) = writer(ScriptedRaw::wedged(), 80, 24);

        handles.needs_full_repaint.store(true, Ordering::SeqCst);
        handles.invalidated.store(true, Ordering::SeqCst);
        assert!(handles.take_repaint_request());
        assert!(
            !handles.take_repaint_request(),
            "one repaint answers both; a leftover flag would cost another"
        );
    }

    #[test]
    fn teardown_drops_frames_the_alternate_screen_took_with_it() {
        let (mut writer, handles) = writer(ScriptedRaw::wedged(), 80, 24);
        frame(&mut writer, b"a stale diff");
        assert_eq!(writer.pending.len(), 1);

        handles.discard_pending();
        // The terminal is readable again by now — it is the shell, not the TUI.
        writer.raw.after_script = Exhausted::Accept;
        writer.flush().unwrap();

        assert!(
            writer.raw().written.is_empty(),
            "the stale frame never reaches the restored screen"
        );
        assert_eq!(writer.pending_bytes, 0);
    }
}

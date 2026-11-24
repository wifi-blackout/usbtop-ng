use anyhow::{anyhow, Result};
use log::{debug, error, warn};
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::ops::ControlFlow;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::parser::{parse_usbmon_text_line, UsbPacket};
use super::tick::Ticker;
use super::{open_nonblocking, POLL_INTERVAL};

/// The longest the reader stays away from the interface while events may be
/// arriving: the cap on the pause between two batches (see [`pause_for`]),
/// and the poll interval of the fallback that cannot wait in `read` (see
/// [`start_tick`]). A fifth of [`POLL_INTERVAL`]. The kernel's queue for a
/// text reader is small (see [`UsbmonReader`]), so this is what bounds the
/// burst that can be lost to one pause: more than the queue holds, inside
/// 10 ms.
const PAUSE_LIMIT: Duration = Duration::from_millis(10);

/// A pause shorter than this is not taken: a bus fast enough to earn one,
/// above 16,000 events a second, is read without stopping. A sleep that
/// short overshoots by a good part of its own length (the kernel's timer
/// slack and the time to be scheduled again) while the queue fills in a few
/// milliseconds. Measured on a bus at 59,000 events a second, where either
/// way the reader is at the edge of keeping up: taking its 270 µs pauses it
/// dropped events in 13 runs of 35, taking none in 13 of 55. That leans
/// this way without settling it.
const PAUSE_FLOOR: Duration = Duration::from_millis(1);

/// The longest first pause after none, and what each later one may add to
/// twice the one before (see [`pause_for`]).
const PAUSE_STEP: Duration = Duration::from_millis(1);

/// How many events the reader aims to find queued after a pause: about a
/// sixth of the 93 the kernel holds at the least, so the rate can rise
/// several times over within one pause before the queue is full.
const BATCH_TARGET: u32 = 16;

/// The longest run of bytes with no newline the reader keeps before it gives
/// the run up as not being a line. The kernel formats a text event into a
/// buffer of 250 bytes (`PRINTF_DFL` in `drivers/usb/mon/mon_text.c`); this
/// is far above that and only bounds a file that is not usbmon's.
const LINE_LIMIT: usize = 64 * 1024;

/// Reads usbmon's `Nu` text interface (`/sys/kernel/debug/usb/usbmon/{bus}u`).
/// debugfs's `Nu` files ARE the text interface described in
/// Documentation/usb/usbmon.rst; the binary API is the separate
/// `/dev/usbmonN` character devices, read by
/// [`BinaryReader`](super::binary::BinaryReader). This reader is the fallback
/// [`start_monitoring`](super::monitor::start_monitoring) picks when those
/// devices cannot be opened.
///
/// The reader waits for events inside `read`, where the kernel wakes it as
/// one arrives. It does not sleep a poll interval when the interface is
/// empty, because the kernel holds few events for a text reader and drops
/// what arrives while that queue is full: `EVENT_MAX` in
/// `drivers/usb/mon/mon_text.c` is four pages of event records, 93 events on
/// a 64-bit kernel with 4 KiB pages and more with larger pages. A reader
/// asleep for a poll interval loses whatever a burst adds beyond that. A
/// [`Ticker`] interrupts the blocked `read` every [`POLL_INTERVAL`], so a
/// silent bus still never parks the thread where it could neither be joined
/// nor release the debugfs file.
#[derive(Debug, Clone)]
pub struct UsbmonReader {
    pub bus_id: u8,
    pub path: PathBuf,
    follow: bool,
}

/// Bytes read from the interface and not yet ended by a newline.
#[derive(Debug, Default)]
struct Lines {
    pending: Vec<u8>,
    /// Set when a run with no newline passed [`LINE_LIMIT`] and was dropped:
    /// what follows, up to the next newline, is the rest of that same run
    /// and is dropped with it.
    discarding: bool,
}

impl Lines {
    /// Adds `bytes` and hands `each` every line they complete, without its
    /// newline, in order. Stops at the first line `each` breaks on; the lines
    /// after it are dropped with the reader.
    fn push(
        &mut self,
        bytes: &[u8],
        mut each: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        self.pending.extend_from_slice(bytes);
        let mut done = 0;
        let mut flow = ControlFlow::Continue(());
        while let Some(length) = self.pending[done..].iter().position(|byte| *byte == b'\n') {
            let line = &self.pending[done..done + length];
            done += length + 1;
            if std::mem::take(&mut self.discarding) {
                // The tail of a run that was given up.
                continue;
            }
            if each(line).is_break() {
                flow = ControlFlow::Break(());
                break;
            }
        }
        self.pending.drain(..done);
        if self.discarding {
            self.pending.clear();
        } else if self.pending.len() > LINE_LIMIT {
            debug!(
                "dropping a run of text with no newline in its first {} bytes",
                self.pending.len()
            );
            self.pending.clear();
            self.discarding = true;
        }
        flow
    }

    /// What is left when the file ends: a last line with no newline, if any.
    fn take_rest(&mut self) -> Option<Vec<u8>> {
        (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
    }
}

/// Parses one line and hands the packet to `callback`. A line that is not
/// UTF-8 or does not parse is skipped. Breaks when the callback fails.
fn deliver<F>(line: &[u8], callback: &mut F) -> ControlFlow<()>
where
    F: FnMut(UsbPacket) -> Result<()>,
{
    let Ok(text) = std::str::from_utf8(line) else {
        debug!(
            "skipping a text line that is not UTF-8 ({} bytes)",
            line.len()
        );
        return ControlFlow::Continue(());
    };
    match parse_usbmon_text_line(text.trim()) {
        Ok(packet) => {
            if let Err(e) = callback(packet) {
                debug!("packet callback error: {}", e);
                return ControlFlow::Break(());
            }
        }
        Err(e) => debug!("could not parse text line '{}': {}", text.trim(), e),
    }
    ControlFlow::Continue(())
}

/// How long to stay away from the interface after a batch, given that
/// `events` arrived over the `cycle` since the batch before and that the
/// pause before this batch was `previous`: the time in which
/// [`BATCH_TARGET`] more arrive at that rate, never more than
/// [`PAUSE_LIMIT`], and never more than twice the pause before plus
/// [`PAUSE_STEP`].
///
/// Reading each event as it arrives costs a wake-up per event, which is most
/// of what a reader costs at a few hundred events a second. Pausing lets a
/// handful collect and takes them in one wake-up. The pause follows the rate
/// at both ends, and comes on gradually:
///
/// - A slow bus earns none. Below 200 events a second not even two would
///   collect in the longest pause, so there is nothing to save, and the
///   reader goes straight back to waiting in `read`. That is also what keeps
///   a pause away from the start of a burst: the first event after a quiet
///   spell is one event over a long cycle.
/// - A fast bus earns none either: above 16,000 events a second the pause
///   would be under [`PAUSE_FLOOR`], and the reader does not stop.
/// - After no pause the first is at most [`PAUSE_STEP`], the next at most
///   three times that, and so on: a rate read off the first two events of a
///   burst is weak evidence, and the queue is small.
fn pause_for(events: u32, cycle: Duration, previous: Duration) -> Duration {
    if events == 0 {
        return Duration::ZERO;
    }
    // Fewer than two events per longest pause.
    if cycle / events > PAUSE_LIMIT / 2 {
        return Duration::ZERO;
    }
    let pause = (cycle.saturating_mul(BATCH_TARGET) / events)
        .min(PAUSE_LIMIT)
        .min(previous.saturating_mul(2).saturating_add(PAUSE_STEP));
    if pause < PAUSE_FLOOR {
        return Duration::ZERO;
    }
    pause
}

/// The file's status flags (`F_GETFL`, fcntl(2)), to be handed back to
/// [`set_nonblocking`] with `O_NONBLOCK` on or off.
fn status_flags(file: &File) -> io::Result<libc::c_int> {
    // SAFETY: `F_GETFL` takes no argument and only returns the status flags
    // of the descriptor, which `file` keeps open for the length of the call.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags)
}

/// Sets or clears `O_NONBLOCK` on `file`, whose other status flags are
/// `flags`. The text interface looks at the flag on every read
/// (`file->f_flags & O_NONBLOCK` in `mon_text_read_wait`), and `F_SETFL` may
/// change it (fcntl(2)).
fn set_nonblocking(file: &File, flags: libc::c_int, nonblocking: bool) -> io::Result<()> {
    let wanted = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    // SAFETY: `F_SETFL` takes the new status flags as an int; these are the
    // descriptor's own flags with one bit set or cleared. `file` keeps the
    // descriptor open for the length of the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, wanted) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// What the reader needs in order to wait for an event inside `read`: the
/// tick that bounds the wait, and the file's status flags, for switching it
/// between blocking and non-blocking.
#[derive(Debug)]
struct ReadWait {
    /// Never read: it is here to keep ticking until the reader is done.
    _ticker: Ticker,
    flags: libc::c_int,
}

/// Starts the tick for the calling thread and reads `file`'s flags.
///
/// `None` when either fails, with one warning that names the cause. The
/// reader then never blocks: it polls the file every [`PAUSE_LIMIT`].
fn start_tick(file: &File, path: &Path) -> Option<ReadWait> {
    let started = Ticker::start(POLL_INTERVAL)
        .and_then(|ticker| status_flags(file).map(|flags| (ticker, flags)));
    match started {
        Ok((ticker, flags)) => Some(ReadWait {
            _ticker: ticker,
            flags,
        }),
        Err(e) => {
            warn!(
                "could not wait for events in read on {}: {}; polling every {} ms instead, \
                 so a burst of more events than the kernel queues between two polls is lost",
                path.display(),
                e,
                PAUSE_LIMIT.as_millis()
            );
            None
        }
    }
}

/// What one read of the interface came to.
enum Step {
    /// Bytes arrived, completing this many lines.
    Lines(u32),
    /// A non-blocking read found nothing queued.
    Empty,
    /// A signal ended the read before anything arrived: the tick, when the
    /// read was waiting.
    Interrupted,
    /// The end of a file: a regular file read through, or a FIFO with no
    /// writer. The usbmon interface never reports it.
    End,
    /// The callback asked to stop, or the read failed.
    Stop,
}

/// What the read loop did, for the debug log and for the tests that hold the
/// loop to waiting rather than spinning.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct LoopCounts {
    /// Calls to `read`, whatever they returned.
    reads: u64,
    /// Pauses taken between batches.
    pauses: u64,
}

impl UsbmonReader {
    pub fn new(bus_id: u8) -> Self {
        Self {
            bus_id,
            path: Self::get_usbmon_path(bus_id),
            follow: true,
        }
    }

    /// Test/capture seam: point the reader at an arbitrary file instead of the
    /// real debugfs path, and optionally disable follow-on-EOF so reads over a
    /// fixed fixture file terminate.
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
    /// `shutdown` is looked at between reads and every time a wait is
    /// interrupted, so a caller can stop the loop within `POLL_INTERVAL` and
    /// join the thread. While the loop follows a live interface, the calling
    /// thread receives a signal every `POLL_INTERVAL` (see [`Ticker`]), which
    /// the callback may see as an `EINTR` from a blocking call of its own.
    ///
    /// Lines that fail to parse are skipped (logged at debug level). A
    /// callback `Err` stops the loop early and still returns `Ok(())`.
    pub fn read_packets<F>(&self, shutdown: &AtomicBool, callback: F) -> Result<()>
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        if !self.is_available() {
            return Err(anyhow!(
                "usbmon interface not available: {}",
                self.path.display()
            ));
        }

        debug!("starting packet capture from {}", self.path.display());

        let file = open_nonblocking(&self.path)
            .map_err(|e| anyhow!("could not open {}: {}", self.path.display(), e))?;
        // A file read once to its end never waits, so it needs no tick. The
        // tick is dropped when the loop returns, which stops it.
        let wait = if self.follow {
            start_tick(&file, &self.path)
        } else {
            None
        };
        let counts = self.read_from(file, wait.as_ref(), shutdown, callback);
        debug!(
            "finished packet capture from {}: {} reads, {} pauses",
            self.path.display(),
            counts.reads,
            counts.pauses
        );
        Ok(())
    }

    /// One read, sorted into what the loop does next.
    fn step<F>(&self, file: &mut File, lines: &mut Lines, callback: &mut F) -> Step
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        // The kernel returns one event's line per read, 250 bytes at most; a
        // fixture file returns as much as fits.
        let mut chunk = [0u8; 4096];
        // `File::read`, not `BufRead::read_line`: the latter retries an
        // interrupted read itself, and a reader waiting on a silent bus would
        // never come back to look at its shutdown flag.
        match file.read(&mut chunk) {
            Ok(0) => Step::End,
            Ok(read) => {
                let mut completed = 0u32;
                let flow = lines.push(&chunk[..read], |line| {
                    completed = completed.saturating_add(1);
                    deliver(line, callback)
                });
                if flow.is_break() {
                    Step::Stop
                } else {
                    Step::Lines(completed)
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => Step::Empty,
            Err(e) if e.kind() == ErrorKind::Interrupted => Step::Interrupted,
            Err(e) => {
                error!("could not read from {}: {}", self.path.display(), e);
                Step::Stop
            }
        }
    }

    /// The loop itself, over a file that is open and non-blocking.
    ///
    /// Each pass takes everything that is queued without waiting. If that
    /// was nothing, it waits for the next event: inside a blocking `read`
    /// when `wait` is there, where the kernel wakes it the moment one
    /// arrives, and by sleeping [`PAUSE_LIMIT`] when it is not. Then it
    /// pauses for as long as [`pause_for`] allows at the rate of the batch
    /// it just took, which is not at all on a slow bus or after a quiet
    /// spell, and goes round.
    ///
    /// A reader on a silent bus is in `read` when a burst starts. It is
    /// still woken twenty times a second there, by the tick, to look at
    /// `shutdown`.
    fn read_from<F>(
        &self,
        mut file: File,
        wait: Option<&ReadWait>,
        shutdown: &AtomicBool,
        mut callback: F,
    ) -> LoopCounts
    where
        F: FnMut(UsbPacket) -> Result<()>,
    {
        let mut lines = Lines::default();
        let mut counts = LoopCounts::default();
        // When the last batch was taken. Only a batch moves it: a tick, an
        // empty poll and the end of a file leave it alone, so the first
        // event after a quiet spell is measured over the whole spell.
        let mut batch_taken = Instant::now();
        let mut pause = Duration::ZERO;

        'reading: while !shutdown.load(Ordering::Relaxed) {
            // Everything queued, without waiting.
            let mut events = 0u32;
            let mut ended = false;
            loop {
                counts.reads += 1;
                match self.step(&mut file, &mut lines, &mut callback) {
                    Step::Lines(completed) => events = events.saturating_add(completed),
                    Step::Empty => break,
                    // Only a descriptor left blocking by a failed switch
                    // waits here. Back to the flag before anything else.
                    Step::Interrupted => continue 'reading,
                    Step::End => {
                        ended = true;
                        break;
                    }
                    Step::Stop => break 'reading,
                }
                // A file that never runs dry must not keep the loop from its
                // flag.
                if shutdown.load(Ordering::Relaxed) {
                    break 'reading;
                }
            }

            if events == 0 && !ended {
                // Nothing queued: wait for the next event.
                let Some(wait) = wait else {
                    std::thread::sleep(PAUSE_LIMIT);
                    continue;
                };
                if let Err(e) = set_nonblocking(&file, wait.flags, false) {
                    // Still non-blocking: poll this once, and try again on
                    // the next pass.
                    debug!("could not make {} blocking: {}", self.path.display(), e);
                    std::thread::sleep(PAUSE_LIMIT);
                    continue;
                }
                // Stay in the blocking read across ticks: each one brings
                // the loop to its flag and nothing else.
                let woken_by = loop {
                    counts.reads += 1;
                    match self.step(&mut file, &mut lines, &mut callback) {
                        Step::Interrupted if !shutdown.load(Ordering::Relaxed) => {}
                        other => break other,
                    }
                };
                if let Err(e) = set_nonblocking(&file, wait.flags, true) {
                    // Left blocking: every read now waits for its event, and
                    // the tick still brings the loop back to its flag.
                    debug!(
                        "could not make {} non-blocking again: {}",
                        self.path.display(),
                        e
                    );
                }
                match woken_by {
                    Step::Lines(completed) => events = completed,
                    // The shutdown flag is set; the loop condition sees it.
                    Step::Interrupted => continue,
                    Step::End => ended = true,
                    Step::Empty => {
                        // A blocking read that did not wait. Never the
                        // usbmon interface; whatever this is, do not spin.
                        std::thread::sleep(PAUSE_LIMIT);
                        continue;
                    }
                    Step::Stop => break,
                }
            }

            if ended {
                if !self.follow {
                    if let Some(rest) = lines.take_rest() {
                        let _ = deliver(&rest, &mut callback);
                    }
                    break;
                }
                // When following, a line is a line once its newline arrives:
                // what is pending is kept, since the end of a growing file
                // can fall in the middle of one.
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            if events == 0 {
                // Bytes that completed no line yet.
                continue;
            }

            let now = Instant::now();
            pause = pause_for(events, now.duration_since(batch_taken), pause);
            batch_taken = now;
            if !pause.is_zero() {
                counts.pauses += 1;
                std::thread::sleep(pause);
            }
        }
        counts
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

    /// A FIFO stands in for the interface: opened non-blocking it opens with
    /// no writer attached, reports end of file until one appears, and then
    /// blocks while it is empty, as an idle usbmon interface does. One event
    /// is split across two writes several ticks apart, so the wait for its
    /// second half is interrupted more than once with half a line held.
    #[test]
    fn a_line_split_across_reads_survives_the_interrupted_waits_between() {
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

        // The writer opens after the reader is already running.
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
            "the first half must still be there when the second arrives"
        );

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    /// The defect the benchmark found: a reader that sleeps a poll interval
    /// whenever the interface is empty leaves the kernel's 93-event queue
    /// to overflow behind it. A reader that waits in `read` is handed each
    /// line as it arrives. Fifteen lines, each written after an idle spell
    /// longer than a poll and at a different point in the poll cycle: a
    /// sleeping reader's delays spread over 0 to 50 ms with a median near
    /// 27, a waiting reader's are all near zero. The median keeps a few slow
    /// schedulings out of it, and the limit leaves a loaded single core room.
    /// (That the reader waits in `read` and does not merely poll faster is
    /// `a_silent_interface_is_waited_on_in_read_and_not_polled`'s to show.)
    #[test]
    fn a_line_written_after_an_idle_spell_arrives_without_waiting_for_a_poll() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("6u");
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());

        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let reader = UsbmonReader::with_path(6, path.clone(), true);
        let flag = std::sync::Arc::clone(&shutdown);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            reader
                .read_packets(&flag, |_| {
                    tx.send(std::time::Instant::now()).unwrap();
                    Ok(())
                })
                .unwrap();
        });

        // Held open for the whole test: with a writer attached an empty FIFO
        // is an idle interface, not the end of a file.
        let mut w = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let mut delays = Vec::new();
        for i in 0..15u32 {
            std::thread::sleep(POLL_INTERVAL + Duration::from_millis(u64::from(10 + 7 * i)));
            let written = std::time::Instant::now();
            w.write_all(b"ffff0000dddd0006 600 C Bi:6:003:1 0 32 <\n")
                .unwrap();
            let delivered = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            delays.push(delivered.duration_since(written));
        }
        delays.sort();
        assert!(
            delays[7] < Duration::from_millis(20),
            "the median delay from write to delivery was {:?}; all fifteen: {delays:?}",
            delays[7]
        );

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    fn fifo(directory: &std::path::Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        path
    }

    /// The property the tick exists for: a reader waiting in `read` on an
    /// interface that stays silent still stops when asked. With the writer
    /// held open nothing but the tick can end that read.
    #[test]
    fn a_reader_waiting_on_a_silent_interface_stops_when_asked() {
        let temp = tempfile::tempdir().unwrap();
        let path = fifo(temp.path(), "7u");

        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let reader = UsbmonReader::with_path(7, path.clone(), true);
        let flag = std::sync::Arc::clone(&shutdown);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let outcome = reader.read_packets(&flag, |_| Ok(()));
            done_tx.send(outcome.is_ok()).unwrap();
        });

        let _writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        // Long enough for the reader to be inside the blocking read.
        std::thread::sleep(POLL_INTERVAL * 3);
        assert!(
            done_rx.try_recv().is_err(),
            "nothing has asked the reader to stop yet"
        );

        shutdown.store(true, Ordering::Relaxed);
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)),
            Ok(true),
            "the reader must come out of its read within a tick or two of the request"
        );
        handle.join().unwrap();
    }

    /// The fallback for a host where the tick cannot be set up: the file
    /// stays non-blocking and the loop polls it. Driven directly, since on
    /// the test host the tick does start.
    #[test]
    fn a_polled_non_blocking_file_still_delivers_and_stops() {
        let temp = tempfile::tempdir().unwrap();
        let path = fifo(temp.path(), "8u");

        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let reader = UsbmonReader::with_path(8, path.clone(), true);
        let file = open_nonblocking(&path).unwrap();
        let flag = std::sync::Arc::clone(&shutdown);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            reader.read_from(file, None, &flag, |p| {
                tx.send(p).unwrap();
                Ok(())
            });
        });

        let mut w = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        // Several polls of an empty file first, each answered with EAGAIN.
        std::thread::sleep(PAUSE_LIMIT * 5);
        w.write_all(b"ffff0000dddd0008 800 C Bi:8:003:1 0 64 <\n")
            .unwrap();
        let packet = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(packet.data_length, 64);

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    /// Runs `read_from` on its own thread over a FIFO, with the tick when
    /// `wait_in_read`, handing every packet's length to the returned channel
    /// and the loop's counts to the second one when it returns.
    struct Running {
        shutdown: std::sync::Arc<AtomicBool>,
        lengths: std::sync::mpsc::Receiver<u32>,
        counts: std::sync::mpsc::Receiver<LoopCounts>,
        handle: std::thread::JoinHandle<()>,
        writer: File,
        _temp: tempfile::TempDir,
    }

    impl Running {
        /// `leave_blocking` hands the loop a descriptor that is already
        /// blocking, as a failed switch back would leave it.
        fn start(name: &str, wait_in_read: bool, leave_blocking: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let path = fifo(temp.path(), name);
            let shutdown = std::sync::Arc::new(AtomicBool::new(false));
            let reader = UsbmonReader::with_path(12, path.clone(), true);
            let flag = std::sync::Arc::clone(&shutdown);
            let (length_tx, lengths) = std::sync::mpsc::channel();
            let (count_tx, counts) = std::sync::mpsc::channel();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
            let handle = std::thread::spawn(move || {
                let file = open_nonblocking(&reader.path).unwrap();
                let wait = if wait_in_read {
                    start_tick(&file, &reader.path)
                } else {
                    None
                };
                assert_eq!(
                    wait.is_some(),
                    wait_in_read,
                    "the tick starts on the test host"
                );
                // The writer has to be there before the loop starts: with
                // none, a read of a FIFO reports its end and does not wait.
                ready_rx.recv().unwrap();
                if leave_blocking {
                    set_nonblocking(&file, status_flags(&file).unwrap(), false).unwrap();
                }
                let counts = reader.read_from(file, wait.as_ref(), &flag, |p| {
                    length_tx.send(p.data_length).unwrap();
                    Ok(())
                });
                count_tx.send(counts).unwrap();
            });
            let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            ready_tx.send(()).unwrap();
            Self {
                shutdown,
                lengths,
                counts,
                handle,
                writer,
                _temp: temp,
            }
        }

        fn write_event(&mut self, length: u32) {
            writeln!(
                self.writer,
                "ffff0000dddd000c {length} C Bi:12:003:1 0 {length} <"
            )
            .unwrap();
        }

        /// Asks the loop to stop and returns its counts. A loop that does
        /// not stop fails here, where a bare `join` would hang.
        fn stop(self) -> LoopCounts {
            self.shutdown.store(true, Ordering::Relaxed);
            let counts = self
                .counts
                .recv_timeout(Duration::from_secs(5))
                .expect("the loop must return within a few ticks of the request");
            self.handle.join().unwrap();
            counts
        }
    }

    /// What tells waiting in `read` from polling, and from spinning: how
    /// often the loop reads while nothing arrives. Waiting, it reads once
    /// per tick, twenty times a second. Polling every 10 ms it would read a
    /// hundred times a second, and a blocking read that did not block would
    /// read without end.
    #[test]
    fn a_silent_interface_is_waited_on_in_read_and_not_polled() {
        let running = Running::start("12u", true, false);
        let started = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(400));
        let silent_for = started.elapsed();
        let counts = running.stop();

        // Waiting, the loop reads once per tick (fewer if the scheduler let
        // ticks run together). Twice the ticks and a few for the start and
        // the stop is still under half of what polling every 10 ms comes to.
        let ticks = u64::try_from(silent_for.as_millis()).unwrap() / 50;
        assert!(
            counts.reads <= 2 * ticks + 6,
            "no more than a read or two per tick: {counts:?} over {silent_for:?}"
        );
        assert_eq!(counts.pauses, 0, "nothing arrived, so nothing to pause by");
    }

    /// The fallback polls, and the same count shows it: about a read every
    /// 10 ms while nothing arrives.
    #[test]
    fn without_the_tick_a_silent_interface_is_polled() {
        let running = Running::start("12u", false, false);
        let started = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        let counts = running.stop();
        let polls = u64::try_from(started.elapsed().as_millis()).unwrap() / 10;
        assert!(
            (5..=polls + 6).contains(&counts.reads),
            "a poll every 10 ms, give or take the scheduler: {counts:?} against {polls}"
        );
        assert_eq!(counts.pauses, 0);
    }

    /// The burst the benchmark lost events to starts on a quiet bus. The
    /// first event after a quiet spell must be followed by another read and
    /// not by a pause, wherever in the tick's cycle it falls: twenty events,
    /// each after a spell a little longer than a tick, so that they step
    /// through the cycle some 3 ms at a time and one lands in every part of
    /// it. (A reader that measured the quiet from the last tick would pause
    /// after each event that came within 5 ms of one.)
    #[test]
    fn the_first_event_after_a_quiet_spell_is_followed_by_no_pause() {
        let mut running = Running::start("12u", true, false);
        for number in 1..=20u32 {
            std::thread::sleep(POLL_INTERVAL + Duration::from_millis(3));
            running.write_event(number);
            assert_eq!(
                running.lengths.recv_timeout(Duration::from_secs(5)),
                Ok(number)
            );
        }
        let counts = running.stop();
        assert_eq!(counts.pauses, 0, "{counts:?}");
    }

    /// A descriptor the loop could not switch back to non-blocking: every
    /// read then waits for its event, and only the tick brings the loop back
    /// to its shutdown flag. Set up directly, since the switch does not fail
    /// on the test host.
    #[test]
    fn a_descriptor_left_blocking_still_delivers_and_stops() {
        let mut running = Running::start("12u", true, true);
        // Several ticks of a read that has nothing to return.
        std::thread::sleep(POLL_INTERVAL * 3);
        running.write_event(96);
        assert_eq!(running.lengths.recv_timeout(Duration::from_secs(5)), Ok(96));
        running.stop();
    }

    /// The same with no tick-bounded wait to fall back on, as when the tick
    /// could not be started but the descriptor is blocking all the same: a
    /// tick started by hand stands in for whatever interrupts the read.
    #[test]
    fn a_blocking_descriptor_without_a_wait_still_delivers_and_stops() {
        let temp = tempfile::tempdir().unwrap();
        let path = fifo(temp.path(), "10u");

        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let reader = UsbmonReader::with_path(10, path.clone(), true);
        let file = open_nonblocking(&path).unwrap();
        let flag = std::sync::Arc::clone(&shutdown);
        let (tx, rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ticker = Ticker::start(POLL_INTERVAL).unwrap();
            ready_rx.recv().unwrap();
            set_nonblocking(&file, status_flags(&file).unwrap(), false).unwrap();
            reader.read_from(file, None, &flag, |p| {
                tx.send(p.data_length).unwrap();
                Ok(())
            });
            done_tx.send(()).unwrap();
        });

        let mut w = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        w.write_all(b"ffff0000dddd000a 1000 C Bi:10:003:1 0 96 <\n")
            .unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(96));

        shutdown.store(true, Ordering::Relaxed);
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)), Ok(()));
        handle.join().unwrap();
    }

    /// Four hundred events half a millisecond apart, each carrying its
    /// number as its length. At that pace the loop goes through every state
    /// it has: waiting for an event, taking a batch, pausing by the rate it
    /// saw. Every event must come out, once and in order, and the loop must
    /// have paused: at some 2,000 events a second it is meant to take them
    /// in batches, not one wake-up each.
    fn a_paced_stream_arrives_whole_and_in_order(wait_in_read: bool) {
        let mut running = Running::start("12u", wait_in_read, false);
        for number in 1..=400u32 {
            running.write_event(number);
            std::thread::sleep(Duration::from_micros(500));
        }
        let delivered: Vec<_> = (0..400)
            .map(|_| {
                running
                    .lengths
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
            })
            .collect();
        assert_eq!(delivered, (1..=400).collect::<Vec<_>>());
        assert!(running.lengths.try_recv().is_err(), "and nothing twice");

        let counts = running.stop();
        assert!(counts.pauses > 0, "{counts:?}");
    }

    #[test]
    fn a_paced_stream_arrives_whole_and_in_order_when_waiting_in_read() {
        a_paced_stream_arrives_whole_and_in_order(true);
    }

    #[test]
    fn a_paced_stream_arrives_whole_and_in_order_when_polled() {
        a_paced_stream_arrives_whole_and_in_order(false);
    }

    /// With the pause before it already at the limit, so that only the rate
    /// decides.
    fn settled_pause(events: u32, cycle: Duration) -> Duration {
        pause_for(events, cycle, PAUSE_LIMIT)
    }

    #[test]
    fn the_pause_is_the_time_for_a_batch_at_the_rate_seen_and_never_past_the_limit() {
        // A webcam's 500 events a second: sixteen would take 32 ms, so the
        // limit applies.
        assert_eq!(settled_pause(5, Duration::from_millis(10)), PAUSE_LIMIT);
        // 1,600 a second is where the limit stops applying.
        assert_eq!(
            settled_pause(16, Duration::from_millis(10)),
            Duration::from_millis(10)
        );
        assert_eq!(
            settled_pause(32, Duration::from_millis(10)),
            Duration::from_millis(5)
        );
        // 16,000 a second: sixteen events take a millisecond.
        assert_eq!(
            settled_pause(16, Duration::from_millis(1)),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn a_slow_bus_and_a_quiet_spell_earn_no_pause() {
        // 200 a second, an event every 5 ms, is the slowest that pauses:
        // two events would collect in the longest pause.
        assert_eq!(settled_pause(1, Duration::from_millis(5)), PAUSE_LIMIT);
        assert_eq!(settled_pause(2, Duration::from_millis(10)), PAUSE_LIMIT);
        // Anything slower does not.
        assert_eq!(
            settled_pause(1, Duration::from_micros(5001)),
            Duration::ZERO
        );
        assert_eq!(settled_pause(1, Duration::from_millis(40)), Duration::ZERO);
        // The first event after a quiet spell is one event over the spell,
        // however long: a minute, or more than a clock can say.
        assert_eq!(settled_pause(1, Duration::from_secs(60)), Duration::ZERO);
        assert_eq!(settled_pause(1, Duration::MAX), Duration::ZERO);
        // No events is no rate.
        assert_eq!(settled_pause(0, Duration::from_millis(1)), Duration::ZERO);
    }

    #[test]
    fn a_fast_bus_is_read_without_stopping() {
        // 16,000 a second earns exactly the floor, and takes it.
        assert_eq!(settled_pause(16, Duration::from_millis(1)), PAUSE_FLOOR);
        // Faster earns less, and takes none.
        assert_eq!(settled_pause(17, Duration::from_millis(1)), Duration::ZERO);
        assert_eq!(settled_pause(59, Duration::from_millis(1)), Duration::ZERO);
        assert_eq!(settled_pause(160, Duration::from_millis(1)), Duration::ZERO);
        // A batch read in the first microseconds after a quiet spell.
        assert_eq!(settled_pause(4, Duration::from_micros(40)), Duration::ZERO);
    }

    /// The pause comes on gradually: at a webcam's rate, which earns the
    /// limit once settled, the first four pauses after none are 1, 3, 7 and
    /// 10 ms.
    #[test]
    fn the_pause_grows_from_a_millisecond_and_not_all_at_once() {
        let webcam = |previous| pause_for(5, Duration::from_millis(10), previous);
        let first = webcam(Duration::ZERO);
        let second = webcam(first);
        let third = webcam(second);
        let fourth = webcam(third);
        assert_eq!(
            [first, second, third, fourth],
            [
                Duration::from_millis(1),
                Duration::from_millis(3),
                Duration::from_millis(7),
                Duration::from_millis(10)
            ]
        );
        assert_eq!(webcam(fourth), PAUSE_LIMIT, "and there it stays");
        // The growth never lifts a pause above what the rate earns.
        assert_eq!(
            pause_for(16, Duration::from_millis(1), Duration::from_millis(7)),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn the_last_line_of_a_file_counts_without_its_newline() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("3u");
        std::fs::write(
            &path,
            b"ffff880067b00300 1 C Ci:3:001:0 0 4 = 01050000\n\
              ffff880067b00301 2 C Ci:3:002:0 0 8 = 0105000001050000",
        )
        .unwrap();

        let reader = UsbmonReader::with_path(3, path, false);
        let mut lengths = Vec::new();
        reader
            .read_packets(&AtomicBool::new(false), |p| {
                lengths.push(p.data_length);
                Ok(())
            })
            .unwrap();
        assert_eq!(lengths, [4, 8]);
    }

    /// A line that is not UTF-8 is skipped like any other line that does not
    /// parse, and the lines after it are still read.
    #[test]
    fn a_line_that_is_not_utf8_is_skipped_and_reading_goes_on() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("9u");
        let mut bytes = b"ffff880067b00300 1 C Ci:9:001:0 0 4 = 01050000\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0x80, b'\n']);
        bytes.extend_from_slice(b"ffff880067b00301 2 C Ci:9:002:0 0 8 = 0105000001050000\n");
        std::fs::write(&path, bytes).unwrap();

        let reader = UsbmonReader::with_path(9, path, false);
        let mut lengths = Vec::new();
        reader
            .read_packets(&AtomicBool::new(false), |p| {
                lengths.push(p.data_length);
                Ok(())
            })
            .unwrap();
        assert_eq!(lengths, [4, 8]);
    }

    /// Collects what `Lines` hands out for a sequence of reads.
    fn lines_from(reads: &[&[u8]]) -> (Vec<String>, Lines) {
        let mut lines = Lines::default();
        let mut seen = Vec::new();
        for read in reads {
            let flow = lines.push(read, |line| {
                seen.push(String::from_utf8_lossy(line).into_owned());
                ControlFlow::Continue(())
            });
            assert!(flow.is_continue());
        }
        (seen, lines)
    }

    #[test]
    fn one_read_can_hold_several_lines_and_the_start_of_another() {
        let (seen, mut lines) = lines_from(&[b"one\ntwo\nthr"]);
        assert_eq!(seen, ["one", "two"]);
        assert_eq!(lines.take_rest(), Some(b"thr".to_vec()));
        assert_eq!(lines.take_rest(), None, "the rest is taken once");
    }

    #[test]
    fn a_line_is_put_together_from_as_many_reads_as_it_takes() {
        let (seen, mut lines) = lines_from(&[b"al", b"", b"ph", b"a\nbe", b"ta\n"]);
        assert_eq!(seen, ["alpha", "beta"]);
        assert_eq!(lines.take_rest(), None);
    }

    #[test]
    fn an_empty_line_is_handed_out_as_an_empty_line() {
        let (seen, _) = lines_from(&[b"\n\nx\n"]);
        assert_eq!(seen, ["", "", "x"]);
    }

    #[test]
    fn a_break_stops_at_that_line_and_keeps_nothing_before_it() {
        let mut lines = Lines::default();
        let mut seen = Vec::new();
        let flow = lines.push(b"one\ntwo\nthree\nfou", |line| {
            seen.push(line.to_vec());
            if line == b"two" {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        assert!(flow.is_break());
        assert_eq!(seen, [b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(
            lines.take_rest(),
            Some(b"three\nfou".to_vec()),
            "what came after the break is left as it was"
        );
    }

    /// A run of bytes with no newline is given up once it passes the limit,
    /// so a file that is not usbmon's cannot grow the buffer without bound.
    /// The rest of that run, up to its newline, goes with it, and the line
    /// after is read normally.
    #[test]
    fn bytes_with_no_newline_are_dropped_past_the_limit() {
        let mut lines = Lines::default();
        let mut seen = Vec::new();
        let mut each = |line: &[u8]| {
            seen.push(line.to_vec());
            ControlFlow::Continue(())
        };
        let block = vec![b'a'; LINE_LIMIT];
        assert!(lines.push(&block, &mut each).is_continue());
        assert_eq!(lines.pending.len(), LINE_LIMIT, "at the limit it is kept");
        assert!(lines.push(b"a", &mut each).is_continue());
        assert!(lines.pending.is_empty(), "one byte past it, it is dropped");
        // More of the same run, in two reads, neither kept.
        assert!(lines.push(b"more of it", &mut each).is_continue());
        assert!(lines.pending.is_empty());
        assert!(lines
            .push(b" and its tail\nnext\nla", &mut each)
            .is_continue());
        assert_eq!(seen, [b"next".to_vec()]);
        assert_eq!(lines.take_rest(), Some(b"la".to_vec()));
    }

    /// A run at the limit that then gets its newline was never given up.
    #[test]
    fn a_line_of_exactly_the_limit_is_still_a_line() {
        let mut lines = Lines::default();
        let mut lengths = Vec::new();
        let mut each = |line: &[u8]| {
            lengths.push(line.len());
            ControlFlow::Continue(())
        };
        assert!(lines.push(&vec![b'a'; LINE_LIMIT], &mut each).is_continue());
        assert!(lines.push(b"\n", &mut each).is_continue());
        assert_eq!(lengths, [LINE_LIMIT]);
    }

    #[test]
    fn preset_shutdown_flag_stops_follow_mode_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("4u");
        std::fs::File::create(&path).unwrap();

        // follow = true would otherwise wait on this empty file forever.
        let reader = UsbmonReader::with_path(4, path, true);
        let started = std::time::Instant::now();
        reader
            .read_packets(&AtomicBool::new(true), |_| Ok(()))
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a set shutdown flag must end the loop before it reads"
        );
    }
}

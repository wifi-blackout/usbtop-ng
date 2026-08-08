//! Lifecycle: giving the terminal back, and what an exit is allowed to ask.
//!
//! Three things here outlive the event loop. [`restore_terminal`] undoes the
//! terminal setup and is safe to call from anywhere, any number of times —
//! teardown and the panic hook both race for it and only the first one does any
//! work. [`spawn_signal_thread`] turns the signals that mean "stop" into
//! ordinary [`UiEvent`]s, so a signal leaves through the same door as `q`. And
//! [`unload_policy`] decides what an exit may still ask the user, which after a
//! hangup or a dead terminal is nothing.

use std::io::{self, Write};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::{
    cursor::Show,
    event::{Event, KeyCode, KeyEvent, KeyEventKind},
    queue,
    terminal::{disable_raw_mode, LeaveAlternateScreen},
};

use super::events::UiEvent;
use super::ExitReason;

/// Longest [`prompt_via_events`] waits for an answer before deciding for the
/// user. The answer it picks is always the one that changes nothing.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// End a synchronized update. Emitted unconditionally on restore: if a frame
/// was interrupted mid-update the terminal is holding everything back until it
/// sees this — including the restore sequences that follow it.
const END_SYNCHRONIZED_UPDATE: &str = "\x1b[?2026l";

/// How long [`restore_with`] waits between attempts to get the restore
/// sequences onto a terminal that is not taking them.
const RESTORE_POLL: Duration = Duration::from_millis(10);

/// How many attempts it makes. With [`RESTORE_POLL`] between them the whole
/// budget is a quarter of a second — long enough for a link that is merely
/// congested to catch up, short enough that a terminal which is never going to
/// read again does not read as a hung process.
const RESTORE_ATTEMPTS: u32 = 25;

/// Whether the terminal is currently in TUI mode, and so has something to
/// restore. This is what makes [`restore_terminal`] idempotent.
static ARMED: AtomicBool = AtomicBool::new(false);

/// The live output stage's abandon latch, for [`restore_terminal`] to trip.
///
/// Process-global for the same reason [`ARMED`] and [`SAVED_FD`] are: the
/// restore runs from the panic hook, and a panic hook is handed nothing. This
/// is how it reaches a writer it was never given. `OnceLock` rather than a
/// mutex because the hook must not be able to wait on anything — a read here is
/// a load, and usbtop-ng puts up one TUI per process.
static OUTPUT_LATCH: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// Whether the last restore got its sequences onto the terminal.
///
/// A terminal that would not take twenty-odd bytes in [`RESTORE_ATTEMPTS`]
/// tries is not reading, which is the one thing an exit path needs to know
/// before it decides to ask the user a question. Starts `true`, so a process
/// that never put up a TUI is never treated as having a broken one.
static RESTORE_LANDED: AtomicBool = AtomicBool::new(true);

/// The output descriptor whose flags [`restore_terminal`] puts back, and the
/// flags it puts back. A negative descriptor means nothing was saved.
#[cfg(unix)]
static SAVED_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
#[cfg(unix)]
static SAVED_FD_FLAGS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Record that the terminal is now in TUI mode, so the next
/// [`restore_terminal`] — from teardown, from the panic hook, whichever gets
/// there first — has something to undo.
///
/// It also saves the output descriptor's current flags. That is deliberately
/// not the writer's job: the flags worth restoring are the ones stdout had
/// *before* the TUI touched it, this is the moment they are still true, and a
/// writer that later switches stdout to non-blocking then has nothing to
/// remember. Non-blocking stdout outlives the process on a shared descriptor,
/// so the shell that started usbtop-ng inherits it if this is missed.
pub fn arm_restore() {
    save_output_flags();
    ARMED.store(true, Ordering::SeqCst);
}

/// Hand [`restore_terminal`] the latch that stops the output stage writing.
///
/// Called once, from `tui::enter_terminal`, with
/// `output::ShedHandles::abandon_latch`. Without it the restore still restores;
/// what it cannot then do is stop a write that comes *after* it, which on the
/// panic path is ratatui's destructor writing into a descriptor the restore has
/// just made blocking again.
pub fn arm_output_latch(latch: Arc<AtomicBool>) {
    // A second TUI in one process would keep the first latch. There is no such
    // thing here — `run_ui` is called once — and a hook that cannot wait is
    // worth more than handling a case that does not arise.
    let _ = OUTPUT_LATCH.set(latch);
}

/// Whether the terminal took the last restore.
///
/// The exit path asks before it asks the *user* anything: see
/// [`prompt_via_events`].
fn restore_landed() -> bool {
    RESTORE_LANDED.load(Ordering::SeqCst)
}

/// Give the terminal back: leave raw mode, close any open synchronized update,
/// leave the alternate screen, show the cursor, and put the output
/// descriptor's original flags back last of all.
///
/// Idempotent, infallible and bounded by design. It is called from the panic
/// hook and from teardown, and neither has anywhere to report a failure to — a
/// terminal that will not take the restore sequences will not take an error
/// message either. Nor can either afford to wait: the sequences go out on a
/// budget (see [`write_within_budget`]), because a terminal that has stopped
/// reading must not be able to hold the process here.
pub fn restore_terminal() {
    restore_to(&mut RawStdout);
}

/// Standard output with nothing buffered in front of it.
///
/// [`io::Stdout`] cannot be used here, and the reason is the whole point of the
/// budget. It is a `LineWriter`; the restore sequences contain no newline, so
/// they would sit in its buffer while this module concluded they had been
/// written — and then be flushed by std's own exit handler, after
/// [`restore_output_flags`] had put the descriptor back to blocking, into the
/// terminal that had just refused them. That flush answers to nothing and
/// cannot give up. It is the hang the budget exists to prevent, moved one frame
/// later, and a pty-driven check found it there.
#[cfg(unix)]
struct RawStdout;

#[cfg(unix)]
impl Write for RawStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: stdout is valid for the life of the process; the pointer and
        // length describe a slice this call only reads from, and the return is
        // checked before it is used as a length.
        let written = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                buf.as_ptr().cast::<libc::c_void>(),
                buf.len(),
            )
        };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(usize::try_from(written).unwrap_or(0))
    }

    /// Nothing is held back, so there is nothing to push.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Standard output on platforms with no `write(2)` to reach for.
///
/// The descriptor is never switched to non-blocking off unix, so a write there
/// cannot come back `WouldBlock` and the budget never has anything to spend.
#[cfg(not(unix))]
struct RawStdout;

#[cfg(not(unix))]
impl Write for RawStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut out = io::stdout();
        out.write_all(buf)?;
        out.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

/// The body of [`restore_terminal`], writing to `out` so tests can read the
/// sequences instead of the developer's terminal. Reports whether it found
/// anything to do.
///
/// `out` must be unbuffered: see [`RawStdout`] for what a buffered one costs.
fn restore_to(out: &mut impl Write) -> bool {
    restore_with(out, thread::sleep)
}

/// The body of [`restore_to`], with the wait as a parameter so tests can watch
/// the budget being spent without sitting out a real quarter of a second.
fn restore_with(out: &mut impl Write, sleep: impl FnMut(Duration)) -> bool {
    // The one place the "is there anything to undo" question is asked, and it
    // answers "no" to everyone who arrives after the first caller.
    if !ARMED.swap(false, Ordering::SeqCst) {
        return false;
    }

    // First, before anything here writes and long before the flags go back: the
    // terminal is the shell's again from this moment, so the output stage has
    // nothing left to paint on. On the ordinary exit path this is free — the
    // terminal was dropped before the call. On the panic path it is the whole
    // point: unwinding drops ratatui's `Terminal` *after* this hook returns,
    // and its destructor writes.
    if let Some(latch) = OUTPUT_LATCH.get() {
        latch.store(true, Ordering::SeqCst);
    }

    let _ = disable_raw_mode();

    // Assembled in memory first, so that what follows is one bounded push of a
    // known number of bytes rather than a sequence of writes each of which
    // could stall half-done somewhere this function cannot see.
    let mut sequence = Vec::new();
    let _ = write!(sequence, "{END_SYNCHRONIZED_UPDATE}");
    let _ = queue!(sequence, LeaveAlternateScreen, Show);

    // Written while the descriptor is still non-blocking, and so on a budget.
    // The alternative is to put the original flags back first and write into a
    // blocking descriptor, which reads as the safer order and is not: a
    // terminal that has stopped reading takes the process with it, on the one
    // path that exists to end the session. That used to be unreachable —
    // nothing survived a wedged terminal this far — and the output stage is
    // exactly what changed that.
    // Recorded, not just discarded: a terminal that would not take these bytes
    // is not reading, and the exit path has a question it must not ask a
    // terminal that is not reading. See [`prompt_via_events`].
    RESTORE_LANDED.store(write_within_budget(out, &sequence, sleep), Ordering::SeqCst);
    // Last, once nothing else here is going to write and nothing anywhere is
    // holding bytes for this descriptor: from here on stdout is whatever it was
    // before the TUI, which is what the shell inherits.
    restore_output_flags();
    true
}

/// Push `bytes` out, waiting for a terminal that is merely behind but giving up
/// on one that is not reading at all. Reports whether they all landed.
///
/// A healthy terminal takes twenty-odd bytes on the first attempt and never
/// sees the wait. A wedged one costs [`RESTORE_ATTEMPTS`] waits of
/// [`RESTORE_POLL`] and then gets its shell back anyway, minus a repaint the
/// user can ask for with `reset`. Only a wait spends the budget: a short write
/// is progress, and progress is allowed to continue.
fn write_within_budget(
    out: &mut impl Write,
    mut bytes: &[u8],
    mut sleep: impl FnMut(Duration),
) -> bool {
    let mut waits = 0;

    while !bytes.is_empty() {
        match out.write(bytes) {
            // No progress and no reason given; looping on it would spin.
            Ok(0) => return false,
            // Cannot exceed the slice by the `write` contract, but a `min` is
            // cheaper than trusting it.
            Ok(written) => bytes = &bytes[written.min(bytes.len())..],
            // The terminal is full, or a signal landed mid-write. Both say
            // "not yet" rather than "never", so they are worth a wait.
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted =>
            {
                waits += 1;
                if waits >= RESTORE_ATTEMPTS {
                    return false;
                }
                sleep(RESTORE_POLL);
            }
            // Anything else is a terminal that is gone rather than behind, and
            // waiting out the budget for it would be waiting for nothing.
            Err(_) => return false,
        }
    }

    true
}

/// Save the output descriptor's file status flags for [`restore_terminal`].
fn save_output_flags() {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;

        let fd = io::stdout().as_raw_fd();
        // SAFETY: F_GETFL only reads the flags of a descriptor this process
        // owns for its whole life, and reports failure as a negative return
        // rather than through errno alone.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            SAVED_FD_FLAGS.store(flags, Ordering::SeqCst);
            // Published last: the restore reads the descriptor first and only
            // then trusts the flags.
            SAVED_FD.store(fd, Ordering::SeqCst);
        }
    }
}

/// Put back what [`save_output_flags`] saved. Nothing saved, nothing to do.
fn restore_output_flags() {
    #[cfg(unix)]
    {
        let fd = SAVED_FD.swap(-1, Ordering::SeqCst);
        if fd < 0 {
            return;
        }
        let flags = SAVED_FD_FLAGS.load(Ordering::SeqCst);
        // SAFETY: `fd` is stdout, saved by `save_output_flags` from this same
        // process; F_SETFL with flags read out of F_GETFL is a round trip.
        let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    }
}

/// Put the terminal back before the panic message prints, then let the hook
/// that was already installed print it.
///
/// Without this a panic mid-frame prints its trace onto an alternate screen
/// that disappears at exit, in raw mode, with no cursor — the trace is lost and
/// the shell it drops back to is unusable.
pub fn install_panic_hook() {
    static INSTALLED: Once = Once::new();

    INSTALLED.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
    });
}

/// Turn the signals that mean "stop" into [`UiEvent`]s.
///
/// The loop then treats a signal exactly like a quit key: it unwinds through
/// the normal teardown instead of dying inside a frame with the alternate
/// screen still up. Note that raw mode disables ISIG, so a `^C` typed at the UI
/// is a key event and never reaches here; this thread covers the signals that
/// come from elsewhere (`kill`, a closing terminal emulator, a logout).
///
/// The thread is detached, like the input thread: it is parked in the signal
/// iterator, and process exit reaps it.
#[cfg(unix)]
pub fn spawn_signal_thread(tx: std::sync::mpsc::Sender<UiEvent>) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = match Signals::new([SIGHUP, SIGINT, SIGTERM]) {
        Ok(signals) => signals,
        Err(e) => {
            // Not fatal: the UI runs fine, it just cannot leave cleanly on a
            // signal, which is what the panic hook and the restore latch are
            // the second line of defense for.
            log::warn!("Cannot watch for signals, so a signal will not restore the terminal: {e}");
            return;
        }
    };

    std::thread::spawn(move || {
        for signal in signals.forever() {
            // A closed channel means the loop is gone, and with it the only
            // reason to report anything.
            if tx.send(UiEvent::from(signal)).is_err() {
                return;
            }
        }
    });
}

/// What an exit path may do about a usbmon module usbtop-ng loaded itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnloadPolicy {
    /// Ask the user, or unload outright if preferences already said to.
    PromptFlow,
    /// Unload only because preferences said to; there is nobody to ask.
    AutoOnly,
    /// Leave usbmon alone: this run did not load it.
    Skip,
}

/// `SIGHUP` on unix; an inert placeholder elsewhere, where no signal can reach
/// [`unload_policy`] because no signal thread runs.
#[cfg(unix)]
const HANGUP: i32 = libc::SIGHUP;
#[cfg(not(unix))]
const HANGUP: i32 = 1;

/// What this exit may still do about usbmon.
///
/// The rule is about who is left to answer a question. A hangup means the
/// terminal is already gone, and a dead terminal says so directly; prompting
/// either one parks the process forever on an answer nobody can type, holding
/// usbmon loaded and the reader files open. Every other exit still has a user
/// in front of it.
pub fn unload_policy(reason: &ExitReason, loaded_this_run: bool) -> UnloadPolicy {
    if !loaded_this_run {
        return UnloadPolicy::Skip;
    }

    match reason {
        ExitReason::TerminalDead | ExitReason::Signal(HANGUP) => UnloadPolicy::AutoOnly,
        ExitReason::UserQuit | ExitReason::Signal(_) => UnloadPolicy::PromptFlow,
    }
}

/// Ask a yes/no question after the TUI is down, reading the answer from the UI
/// event channel rather than from stdin.
///
/// The input thread is parked on `event::read()` for the life of the process —
/// a blocked terminal read cannot be portably called off — so it, not this
/// thread, owns stdin. A `read_line` here would race it for every keystroke and
/// usually lose, dropping the user's answer into a UI that no longer exists.
///
/// The terminal is back in cooked mode by now, so the keystrokes arrive in a
/// burst when the user presses Enter. The first `y` or `n` in that burst is the
/// answer; anything else, including no answer at all, leaves things as they
/// are.
///
/// A terminal that would not take the restore does not get asked at all. By
/// then stdout is blocking again — [`restore_output_flags`] has run — so the
/// question would be an unbounded write to something that has stopped reading,
/// on the last path of a process that is trying to leave. This is
/// [`unload_policy`]'s rule one layer down: the question is not whether anyone
/// is *there*, but whether anything said to them would arrive.
pub fn prompt_via_events(question: &str, rx: &Receiver<UiEvent>) -> bool {
    prompt_within(question, rx, PROMPT_TIMEOUT, restore_landed())
}

/// The body of [`prompt_via_events`], with the wait and the terminal's state as
/// parameters so tests need neither a real minute nor a real terminal.
fn prompt_within(
    question: &str,
    rx: &Receiver<UiEvent>,
    timeout: Duration,
    terminal_reachable: bool,
) -> bool {
    if !terminal_reachable {
        // "No" is the answer that changes nothing, which is the same answer an
        // unread question gets — reached without spending a minute of the
        // exit's time waiting for it.
        return false;
    }

    // Whatever is already queued was typed at the UI, not at a question that
    // had not been asked yet. Answering with it would unload a module the user
    // never agreed to unload.
    while rx.try_recv().is_ok() {}

    ask(&mut io::stdout(), question);

    await_answer(rx, Instant::now() + timeout)
}

/// Put the question on the terminal, dropping a write failure on the floor.
///
/// `print!` would panic on that failure, and everything this module does is for
/// terminals that may already have gone away — the answer to an unwritable
/// question is the same as the answer to an unanswered one, not a crash.
fn ask(out: &mut impl Write, question: &str) {
    let _ = write!(out, "{question}");
    let _ = out.flush();
}

/// Read events until one of them answers the question, or `deadline` passes.
fn await_answer(rx: &Receiver<UiEvent>, deadline: Instant) -> bool {
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(UiEvent::Input(Event::Key(key))) => {
                if let Some(answer) = answer_for(key) {
                    return answer;
                }
            }
            // Resizes, focus changes and paste land here and mean nothing to a
            // question that is only asking for a letter.
            Ok(UiEvent::Input(_)) => {}
            // A signal or a dead terminal is not an answer; it is a reason to
            // stop asking. So is running out of time, or running out of anyone
            // who could still send. "No" is the answer that changes nothing.
            Ok(UiEvent::Signal(_) | UiEvent::TerminalDead) | Err(_) => {
                // Closes the prompt line for whatever prints next. Ignored on
                // failure for the same reason as the question itself.
                let _ = writeln!(io::stdout());
                return false;
            }
        }
    }
}

/// Whether a key answers a yes/no question, and how.
fn answer_for(key: KeyEvent) -> Option<bool> {
    // Terminals that report releases send several events per press; only the
    // press is an answer.
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match key.code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N') => Some(false),
        // An empty line accepts the default, and the default is "no".
        KeyCode::Enter => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventState, KeyModifiers};
    use std::sync::{mpsc, Mutex, MutexGuard};

    /// The armed flag is process-global, so the tests that touch it take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serialized() -> MutexGuard<'static, ()> {
        // A test that panics on purpose must not take the rest of the suite
        // down with it.
        SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A terminal that has already gone away: every write fails, as writes to a
    /// pty whose master closed do (EIO). This is the state the SIGHUP and
    /// terminal-death paths run in.
    struct GoneTerminal;

    impl Write for GoneTerminal {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    /// A terminal that has stopped reading: every write comes straight back
    /// `WouldBlock`, forever, which is what a non-blocking write to a full pty
    /// does. This is the state a session now survives long enough to exit from.
    #[derive(Default)]
    struct WedgedTerminal {
        attempts: u32,
    }

    impl Write for WedgedTerminal {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.attempts += 1;
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A terminal that takes one byte per call: progress, but slowly. Nothing
    /// here may read as a stall, because it is not one.
    #[derive(Default)]
    struct ByteAtATime {
        written: Vec<u8>,
    }

    impl Write for ByteAtATime {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match buf.first() {
                Some(&byte) => {
                    self.written.push(byte);
                    Ok(1)
                }
                None => Ok(0),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_event(code: KeyCode) -> UiEvent {
        UiEvent::Input(Event::Key(key(code)))
    }

    #[test]
    fn sighup_and_dead_terminal_never_prompt() {
        assert!(matches!(
            unload_policy(&ExitReason::Signal(HANGUP), true),
            UnloadPolicy::AutoOnly
        ));
        assert!(matches!(
            unload_policy(&ExitReason::TerminalDead, true),
            UnloadPolicy::AutoOnly
        ));
        assert!(matches!(
            unload_policy(&ExitReason::UserQuit, true),
            UnloadPolicy::PromptFlow
        ));
        assert!(matches!(
            unload_policy(&ExitReason::UserQuit, false),
            UnloadPolicy::Skip
        ));
    }

    #[test]
    fn a_run_that_loaded_nothing_leaves_usbmon_alone() {
        for reason in [
            ExitReason::UserQuit,
            ExitReason::TerminalDead,
            ExitReason::Signal(HANGUP),
        ] {
            assert_eq!(unload_policy(&reason, false), UnloadPolicy::Skip);
        }
    }

    #[test]
    fn a_terminating_signal_still_has_a_user_to_ask() {
        // SIGTERM and SIGINT say "stop", not "your terminal is gone".
        assert_eq!(
            unload_policy(&ExitReason::Signal(15), true),
            UnloadPolicy::PromptFlow
        );
        assert_eq!(
            unload_policy(&ExitReason::Signal(2), true),
            UnloadPolicy::PromptFlow
        );
    }

    #[cfg(unix)]
    #[test]
    fn hangup_is_the_real_sighup() {
        assert_eq!(HANGUP, libc::SIGHUP);
    }

    #[test]
    fn restore_runs_once_however_often_it_is_asked() {
        let _serial = serialized();
        let mut out = Vec::new();

        arm_restore();
        assert!(restore_to(&mut out), "an armed terminal has work to undo");
        let restored = String::from_utf8(out.clone()).expect("restore emits utf-8");
        assert!(
            restored.contains(END_SYNCHRONIZED_UPDATE),
            "closes any open synchronized update: {restored:?}"
        );
        assert!(
            restored.contains("\x1b[?1049l"),
            "leaves the alternate screen: {restored:?}"
        );
        assert!(
            restored.contains("\x1b[?25h"),
            "shows the cursor: {restored:?}"
        );

        let after_first = out.len();
        assert!(
            !restore_to(&mut out),
            "a restored terminal has nothing left to undo"
        );
        assert_eq!(out.len(), after_first, "the second restore writes nothing");
    }

    #[test]
    fn a_terminal_that_is_already_gone_is_still_restored() {
        let _serial = serialized();

        arm_restore();
        // Nothing here may panic: this is the path a hangup takes, and a panic
        // would abandon the rest of the exit — including the unload.
        assert!(restore_to(&mut GoneTerminal), "the restore still ran");
        assert!(
            !ARMED.load(Ordering::SeqCst),
            "and it still disarmed, so the panic hook will not try again"
        );
    }

    #[test]
    fn a_wedged_terminal_cannot_hold_the_exit_open() {
        let _serial = serialized();
        arm_restore();

        let mut out = WedgedTerminal::default();
        let mut waits = Vec::new();
        // The restore is written on a descriptor that is still non-blocking, so
        // this returns rather than parking the process forever inside a write
        // nobody is reading.
        assert!(restore_with(&mut out, |wait| waits.push(wait)), "it ran");

        assert_eq!(out.attempts, RESTORE_ATTEMPTS, "it tried, and then stopped");
        let waited: Duration = waits.iter().sum();
        assert!(
            waited <= RESTORE_POLL * RESTORE_ATTEMPTS,
            "and it waited a bounded quarter of a second: {waited:?}"
        );
        assert!(
            !ARMED.load(Ordering::SeqCst),
            "and it disarmed, so the panic hook will not try the same wait again"
        );
    }

    #[test]
    fn the_restore_silences_the_output_stage_before_it_restores_the_flags() {
        let _serial = serialized();
        // The one test that may arm a latch: the slot behind `arm_output_latch`
        // is set once per process, so a second one anywhere would be ignored
        // and this assertion would go quiet rather than fail loudly.
        let latch = Arc::new(AtomicBool::new(false));
        arm_output_latch(Arc::clone(&latch));
        arm_restore();

        assert!(restore_to(&mut Vec::new()), "the restore ran");
        assert!(
            latch.load(Ordering::SeqCst),
            "the writer is told to stop before stdout goes back to blocking, \
             because on the panic path ratatui's destructor still has a write in it"
        );
    }

    #[test]
    fn a_terminal_that_took_the_restore_may_still_be_asked() {
        let _serial = serialized();

        arm_restore();
        restore_to(&mut Vec::new());
        assert!(restore_landed(), "a `Vec` takes everything, first time");

        arm_restore();
        restore_with(&mut WedgedTerminal::default(), |_| {});
        assert!(
            !restore_landed(),
            "and a terminal that spent the whole budget refusing 22 bytes is \
             not one to put a question to"
        );
    }

    #[test]
    fn the_restore_budget_is_a_quarter_of_a_second() {
        // Pinned, because the number that matters is the product: a terminal
        // that is never coming back holds the exit for exactly this long.
        assert_eq!(RESTORE_POLL * RESTORE_ATTEMPTS, Duration::from_millis(250));
    }

    #[test]
    fn a_terminal_that_is_gone_is_not_waited_for_at_all() {
        // `GoneTerminal` fails with `BrokenPipe`, which is not "not yet".
        // Spending the budget on it would be spending it on nothing.
        let mut waits = Vec::new();
        assert!(!write_within_budget(
            &mut GoneTerminal,
            b"\x1b[?25h",
            |wait| {
                waits.push(wait);
            }
        ));
        assert!(waits.is_empty(), "nothing to wait for: {waits:?}");
    }

    #[test]
    fn a_healthy_terminal_pays_nothing_for_the_budget() {
        let mut out = Vec::new();
        let mut waits = Vec::new();
        assert!(write_within_budget(&mut out, b"\x1b[?25h", |wait| {
            waits.push(wait);
        }));
        assert_eq!(out, b"\x1b[?25h", "and all of it went out");
        assert!(waits.is_empty(), "it landed first time: {waits:?}");
    }

    #[test]
    fn a_short_write_is_progress_and_does_not_spend_the_budget() {
        // A terminal taking the sequences a byte at a time is slow, not
        // stalled. Counting its writes against the budget would abandon it
        // partway through an escape sequence, which is worse than not writing
        // at all: the tail would arrive on the shell as stray characters.
        let sequence = b"\x1b[?2026l\x1b[?1049l\x1b[?25h";
        let mut out = ByteAtATime::default();
        let mut waits = Vec::new();
        assert!(write_within_budget(&mut out, sequence, |wait| {
            waits.push(wait);
        }));
        assert_eq!(out.written, sequence, "every byte, in order");
        assert!(waits.is_empty(), "nothing stalled: {waits:?}");
    }

    #[test]
    fn a_question_nobody_can_read_is_not_fatal() {
        ask(&mut GoneTerminal, "unload? ");
    }

    #[test]
    fn the_panic_hook_gives_the_terminal_back() {
        let _serial = serialized();
        install_panic_hook();
        // Installing twice must not stack two hooks (or two restores).
        install_panic_hook();
        arm_restore();

        let panicked = panic::catch_unwind(|| panic!("mid-frame"));

        assert!(panicked.is_err(), "the panic still propagates");
        assert!(
            !ARMED.load(Ordering::SeqCst),
            "the hook restored the terminal before the trace printed"
        );
    }

    #[test]
    fn yes_and_no_are_the_only_real_answers() {
        assert_eq!(answer_for(key(KeyCode::Char('y'))), Some(true));
        assert_eq!(answer_for(key(KeyCode::Char('Y'))), Some(true));
        assert_eq!(answer_for(key(KeyCode::Char('n'))), Some(false));
        assert_eq!(answer_for(key(KeyCode::Char('N'))), Some(false));
        // A bare Enter is an empty answer, and the default is "no".
        assert_eq!(answer_for(key(KeyCode::Enter)), Some(false));
        assert_eq!(answer_for(key(KeyCode::Char('x'))), None);
        assert_eq!(answer_for(key(KeyCode::Up)), None);
    }

    #[test]
    fn a_key_going_back_up_is_not_a_second_answer() {
        let release = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        assert_eq!(answer_for(release), None);
    }

    /// A deadline far enough out that only an event can end the wait.
    fn unhurried() -> Instant {
        Instant::now() + PROMPT_TIMEOUT
    }

    #[test]
    fn the_first_yes_answers_the_question() {
        let (tx, rx) = mpsc::channel();
        tx.send(key_event(KeyCode::Char('y'))).unwrap();
        tx.send(key_event(KeyCode::Enter)).unwrap();
        assert!(await_answer(&rx, unhurried()));
    }

    #[test]
    fn events_that_are_not_answers_are_waited_through() {
        let (tx, rx) = mpsc::channel();
        tx.send(UiEvent::Input(Event::Resize(80, 24))).unwrap();
        tx.send(UiEvent::Input(Event::FocusGained)).unwrap();
        tx.send(key_event(KeyCode::Char('x'))).unwrap();
        tx.send(key_event(KeyCode::Char('n'))).unwrap();
        assert!(!await_answer(&rx, unhurried()));
    }

    #[test]
    fn keys_typed_at_the_ui_do_not_answer_a_later_question() {
        let (tx, rx) = mpsc::channel();
        // Typed while the TUI was up, and still queued when it exited: the
        // whole prompt runs here, so the drain has to come before the wait.
        tx.send(key_event(KeyCode::Char('y'))).unwrap();
        drop(tx);
        assert!(
            !prompt_within("unload? ", &rx, PROMPT_TIMEOUT, true),
            "the stale keystroke was drained, and a channel nobody can send on declines"
        );
    }

    #[test]
    fn a_terminal_that_refused_the_restore_is_not_asked_anything() {
        let (tx, rx) = mpsc::channel();
        // A "yes" is waiting, and a live sender means the wait would otherwise
        // run to the full timeout rather than ending on a disconnect.
        tx.send(key_event(KeyCode::Char('y'))).unwrap();

        let answered = prompt_within("unload? ", &rx, PROMPT_TIMEOUT, false);

        assert!(!answered, "nothing to ask, so the answer is the safe one");
        assert!(
            rx.try_recv().is_ok(),
            "and it returned without even draining the channel, let alone writing \
             the question into a descriptor that is blocking again"
        );
        drop(tx);
    }

    #[test]
    fn a_restore_that_landed_still_asks() {
        // The guard must not turn every prompt off: a healthy exit still runs
        // the whole flow. Which path it took is visible in the drain — the
        // short-circuit above leaves the queue untouched, this one empties it.
        let (tx, rx) = mpsc::channel();
        tx.send(key_event(KeyCode::Char('y'))).unwrap();

        // A tiny wait rather than the real minute: what is under test is the
        // path, not the answer, and an unanswered question declines either way.
        assert!(!prompt_within(
            "unload? ",
            &rx,
            Duration::from_millis(10),
            true
        ));
        assert!(
            rx.try_recv().is_err(),
            "the stale keystroke was drained, so the question really was asked"
        );
        drop(tx);
    }

    #[test]
    fn a_signal_at_the_prompt_declines() {
        let (tx, rx) = mpsc::channel();
        tx.send(UiEvent::Signal(15)).unwrap();
        // The sender stays alive, so the answer is the signal and not a
        // disconnect.
        assert!(!await_answer(&rx, unhurried()));
        drop(tx);
    }

    #[test]
    fn a_terminal_that_died_mid_prompt_declines() {
        let (tx, rx) = mpsc::channel();
        tx.send(UiEvent::TerminalDead).unwrap();
        assert!(!await_answer(&rx, unhurried()));
        drop(tx);
    }

    #[test]
    fn an_unanswered_question_decides_itself() {
        let (tx, rx) = mpsc::channel();
        // A live sender that never sends: only the deadline can end this.
        assert!(!await_answer(
            &rx,
            Instant::now() + Duration::from_millis(10)
        ));
        // And a deadline already gone waits not at all.
        assert!(!await_answer(&rx, Instant::now()));
        drop(tx);
    }
}

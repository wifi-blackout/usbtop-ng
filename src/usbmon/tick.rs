//! A tick for one thread: a signal delivered to the calling thread at a fixed
//! interval, so that a system call the thread is blocked in returns `EINTR`
//! and the thread gets to look at its shutdown flag.
//!
//! The text reader ([`reader`](super::reader)) is what needs it. usbmon's
//! text interface has no `poll`, so the only way to be woken when an event
//! arrives is to block in `read`, and a thread blocked in `read` on a silent
//! bus would never see a shutdown request. A [`Ticker`] bounds that wait to
//! one interval without the reader giving up the blocking read.
//!
//! Verified against timer_create(2), timer_settime(2), timer_delete(2),
//! sigevent(3type), sigaction(2), pthread_sigmask(3) and signal(7), and
//! against `good_sigevent` in `kernel/time/posix-timers.c`. It runs on glibc
//! and on musl, 64-bit and 32-bit.

use std::io;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::OnceLock;
use std::time::Duration;

/// `SIGEV_THREAD_ID` from `include/uapi/asm-generic/siginfo.h`: deliver the
/// timer's signal to the thread named in `sigev_notify_thread_id`. The libc
/// crate exports it for glibc and not for musl, whose `timer_create` takes it
/// all the same; the value is the kernel's (a test holds it to libc's where
/// libc has it).
const SIGEV_THREAD_ID: libc::c_int = 4;

/// The handler. A signal that is ignored interrupts nothing, so there has to
/// be one, and it has nothing to do: being delivered is the whole effect.
extern "C" fn on_tick(_signal: libc::c_int) {}

/// The signal the tick uses, once its handler is installed, or why it could
/// not be. Decided once for the process.
static TICK_SIGNAL: OnceLock<Result<libc::c_int, String>> = OnceLock::new();

/// Whether a signal with this disposition is free for the tick to take: only
/// the default one is. A handler belongs to whoever installed it. "Ignore"
/// belongs to whoever chose it, which can be the program that launched this
/// one, since an ignored signal stays ignored across `execve`: replacing it
/// with a handler, even one that does nothing, would let that signal
/// interrupt system calls in every thread of a process that was told it
/// could not.
fn is_unclaimed(disposition: libc::sighandler_t) -> bool {
    disposition == libc::SIG_DFL
}

/// Installs [`on_tick`] for the first real-time signal the C library leaves
/// to programs, and returns that signal.
///
/// `sa_flags` is 0, so no `SA_RESTART`: a call the signal interrupts fails
/// with `EINTR` rather than being restarted behind the caller's back, which
/// is the point (signal(7), "Interruption of system calls and library
/// functions by signal handlers").
///
/// A real-time signal's default action ends the process, so the handler is
/// installed before any timer can send one, and it is never removed: it
/// outlives every [`Ticker`], including one that failed to start.
///
/// The signal's disposition is read before it is written, and written only
/// when it is the default one (see [`is_unclaimed`]); otherwise it is left
/// exactly as it was and the tick is refused. Nothing in this program
/// installs a handler for it, so the read and the write are not made atomic
/// against a third party doing so in between.
fn install_handler() -> Result<libc::c_int, String> {
    let signal = libc::SIGRTMIN();

    let mut current = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: with a null `act`, `sigaction` changes nothing and writes the
    // current action to `current`, which is writable storage of that type.
    if unsafe { libc::sigaction(signal, std::ptr::null(), current.as_mut_ptr()) } != 0 {
        return Err(format!(
            "could not read the disposition of signal {signal}: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: `sigaction` returned 0, so it wrote the current action.
    let current = unsafe { current.assume_init() };
    if !is_unclaimed(current.sa_sigaction) {
        return Err(format!(
            "signal {signal} is already handled or ignored in this process"
        ));
    }

    // SAFETY: an all-zero `sigaction` is a valid value of the type: integer
    // fields, a signal set, and an `Option` of a function pointer for which
    // zero is `None`.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_tick as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
    action.sa_flags = 0;
    // SAFETY: `sigemptyset` writes the empty set to the `sigset_t` it is
    // given, which is a field of a live local.
    if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0 {
        return Err(format!(
            "could not build an empty signal set: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: `action` is a fully initialised `sigaction` whose handler is an
    // `extern "C" fn(c_int)` that touches nothing, which is all a handler
    // installed without `SA_SIGINFO` has to be; a null `oldact` asks for
    // nothing back.
    if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
        return Err(format!(
            "could not install a handler for signal {signal}: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(signal)
}

/// Changes whether `signal` is blocked in the calling thread, and returns
/// whether it was before.
fn set_blocked(signal: libc::c_int, blocked: bool) -> io::Result<bool> {
    let mut set = MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: `sigemptyset` initialises the set it is given and `sigaddset`
    // then adds one signal to that initialised set; both get a pointer to
    // writable storage of the right type.
    let built = unsafe {
        libc::sigemptyset(set.as_mut_ptr()) == 0 && libc::sigaddset(set.as_mut_ptr(), signal) == 0
    };
    if !built {
        return Err(io::Error::last_os_error());
    }
    let how = if blocked {
        libc::SIG_BLOCK
    } else {
        libc::SIG_UNBLOCK
    };
    let mut before = MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: `set` was initialised just above and `before` is writable
    // storage for the old mask. The call changes only the calling thread's
    // mask.
    let error = unsafe { libc::pthread_sigmask(how, set.as_ptr(), before.as_mut_ptr()) };
    // pthread_sigmask returns the error number itself, not -1 and errno.
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    // SAFETY: `pthread_sigmask` returned 0, so it wrote the old mask, and
    // `sigismember` only reads it.
    match unsafe { libc::sigismember(before.as_ptr(), signal) } {
        1 => Ok(true),
        0 => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

/// The tick's signal, unblocked in the calling thread for as long as this
/// lives. A thread starts with a copy of its creator's mask
/// (pthread_sigmask(3)), and a tick that is blocked would never interrupt
/// anything. Dropping this blocks the signal again if it was blocked before,
/// so a caller's thread is handed back as it was lent.
#[derive(Debug)]
struct Unblocked {
    signal: libc::c_int,
    was_blocked: bool,
}

impl Unblocked {
    fn new(signal: libc::c_int) -> io::Result<Self> {
        Ok(Self {
            signal,
            was_blocked: set_blocked(signal, false)?,
        })
    }
}

impl Drop for Unblocked {
    fn drop(&mut self) {
        if self.was_blocked {
            if let Err(e) = set_blocked(self.signal, true) {
                log::debug!("could not block signal {} again: {}", self.signal, e);
            }
        }
    }
}

/// `interval` as the `timespec` a timer takes.
fn timespec(interval: Duration) -> io::Result<libc::timespec> {
    let too_long = |_| io::Error::new(io::ErrorKind::InvalidInput, "the interval is too long");
    // SAFETY: an all-zero `timespec` is a valid value: two integers, and on
    // some 32-bit targets padding the crate keeps private, which is why the
    // fields are assigned rather than the struct written out.
    let mut spec: libc::timespec = unsafe { std::mem::zeroed() };
    // The width of each field differs between targets, so each conversion
    // takes its type from the field. Under a second in nanoseconds fits them
    // all.
    spec.tv_sec = interval.as_secs().try_into().map_err(too_long)?;
    spec.tv_nsec = u64::from(interval.subsec_nanos())
        .try_into()
        .map_err(too_long)?;
    Ok(spec)
}

/// A running tick for the thread that started it. Dropping it stops the tick.
///
/// It names one thread, so it is neither `Send` nor `Sync`: it stays on the
/// thread it ticks, and is dropped there.
#[derive(Debug)]
pub(super) struct Ticker {
    timer: libc::timer_t,
    /// Dropped after `drop` has deleted the timer, which is when the signal
    /// may be blocked again.
    _unblocked: Unblocked,
    _this_thread_only: PhantomData<*const ()>,
}

impl Ticker {
    /// Starts delivering a signal to the calling thread every `interval`,
    /// the first one `interval` from now.
    ///
    /// Only the calling thread is interrupted. In that thread any blocking
    /// system call can now fail with `EINTR`, not only the one the tick is
    /// wanted for.
    ///
    /// Fails when the handler cannot be installed, the signal cannot be
    /// unblocked, or the kernel refuses the timer (a sandbox that filters
    /// `timer_create`, or the per-user limit on queued signals,
    /// `RLIMIT_SIGPENDING`, which every timer counts against). A failed
    /// start leaves the thread's signal mask as it found it and no timer
    /// behind; the handler, once installed, stays.
    pub(super) fn start(interval: Duration) -> io::Result<Self> {
        if interval.is_zero() {
            // A zero `it_value` disarms a timer (timer_settime(2)): it would
            // be a ticker that never ticks.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a tick needs an interval above zero",
            ));
        }
        let period = timespec(interval)?;
        let signal = TICK_SIGNAL
            .get_or_init(install_handler)
            .clone()
            .map_err(io::Error::other)?;
        // Dropped on every early return below, which puts the mask back.
        let unblocked = Unblocked::new(signal)?;

        // SAFETY: an all-zero `sigevent` is a valid value: integers and a
        // union of an integer and a pointer.
        let mut event: libc::sigevent = unsafe { std::mem::zeroed() };
        // `SIGEV_SIGNAL` is 0, so this is the kernel's
        // `SIGEV_SIGNAL | SIGEV_THREAD_ID`.
        event.sigev_notify = SIGEV_THREAD_ID;
        event.sigev_signo = signal;
        // SAFETY: `gettid` takes nothing and returns the calling thread's
        // kernel ID; it cannot fail.
        event.sigev_notify_thread_id = unsafe { libc::gettid() };

        let mut timer = MaybeUninit::<libc::timer_t>::uninit();
        // SAFETY: `event` is initialised and names a thread of this process,
        // the caller, which is what the kernel requires of a
        // `SIGEV_THREAD_ID` target; `timer` points at writable storage for
        // the handle.
        if unsafe { libc::timer_create(libc::CLOCK_MONOTONIC, &raw mut event, timer.as_mut_ptr()) }
            != 0
        {
            return Err(io::Error::last_os_error());
        }
        let ticker = Self {
            // SAFETY: `timer_create` returned 0, so it wrote the handle.
            timer: unsafe { timer.assume_init() },
            _unblocked: unblocked,
            _this_thread_only: PhantomData,
        };

        let schedule = libc::itimerspec {
            it_interval: period,
            it_value: period,
        };
        // SAFETY: `ticker.timer` is the handle `timer_create` just returned
        // and has not been deleted; `schedule` is initialised; a null
        // `old_value` asks for nothing back. Flags 0 makes `it_value`
        // relative to now.
        if unsafe {
            libc::timer_settime(ticker.timer, 0, &raw const schedule, std::ptr::null_mut())
        } != 0
        {
            // Read before `ticker` drops: deleting the timer may set errno.
            let error = io::Error::last_os_error();
            drop(ticker);
            return Err(error);
        }
        Ok(ticker)
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        // SAFETY: `self.timer` came from a successful `timer_create` and is
        // deleted nowhere else. Deleting disarms it first (timer_delete(2)).
        // A tick already on its way is still delivered to the handler, which
        // does nothing, or stays pending if the signal is blocked again.
        if unsafe { libc::timer_delete(self.timer) } != 0 {
            log::debug!(
                "could not delete the tick timer: {}",
                io::Error::last_os_error()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{ErrorKind, Read, Write};
    use std::sync::mpsc;
    use std::time::Instant;

    /// Holds the write end of a pipe open and writes one byte when told to,
    /// or after `patience` if never told: the byte is what turns a read that
    /// would hang forever into one that returns and fails an assertion.
    fn writer_that_writes_when_told(
        mut writer: std::io::PipeWriter,
        patience: Duration,
    ) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (tell, told) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            // A message means write now; a closed channel means never mind.
            if !matches!(
                told.recv_timeout(patience),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ) {
                writer.write_all(b"x").unwrap();
            }
        });
        (tell, handle)
    }

    /// Reads from an empty pipe whose writer stays open, under a 20 ms
    /// ticker started by `start`, and returns how the read ended.
    fn read_under_a_ticker(start: impl FnOnce() -> Ticker) -> Result<usize, ErrorKind> {
        let (mut reader, writer) = std::io::pipe().unwrap();
        let (tell, rescue) = writer_that_writes_when_told(writer, Duration::from_secs(3));
        let ticker = start();
        let outcome = reader.read(&mut [0u8; 8]);
        drop(ticker);
        drop(tell);
        rescue.join().unwrap();
        outcome.map_err(|e| e.kind())
    }

    #[test]
    fn a_read_blocked_under_a_ticker_fails_with_interrupted() {
        let started = Instant::now();
        let outcome = read_under_a_ticker(|| Ticker::start(Duration::from_millis(20)).unwrap());
        assert_eq!(
            outcome,
            Err(ErrorKind::Interrupted),
            "an empty pipe with its writer open blocks; only the tick ends the read"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "and it ended at a tick, not when the rescue byte was due"
        );
    }

    #[test]
    fn a_dropped_ticker_interrupts_nothing() {
        let (mut reader, writer) = std::io::pipe().unwrap();
        // Ten ticks' worth of waiting before the byte is written.
        let (_tell, helper) = writer_that_writes_when_told(writer, Duration::from_millis(200));

        drop(Ticker::start(Duration::from_millis(20)).unwrap());
        let mut byte = [0u8; 8];
        let outcome = reader.read(&mut byte);
        helper.join().unwrap();

        assert_eq!(
            outcome.map_err(|e| e.kind()),
            Ok(1),
            "with the ticker gone the read waits for the byte"
        );
        assert_eq!(byte[0], b'x');
    }

    /// A sleep is interrupted by every tick and resumes, and a read blocked
    /// on another thread meanwhile is not disturbed. (That the tick reaches
    /// the thread that asked for it is the first test's to show; this one
    /// would catch a tick sent to every thread.)
    #[test]
    fn a_sleep_under_a_ticker_lasts_its_length_and_another_threads_read_is_left_alone() {
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        let other = std::thread::spawn(move || reader.read(&mut [0u8; 8]).map_err(|e| e.kind()));

        let ticker = Ticker::start(Duration::from_millis(10)).unwrap();
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(150));
        assert!(started.elapsed() >= Duration::from_millis(150));
        drop(ticker);

        writer.write_all(b"x").unwrap();
        assert_eq!(other.join().unwrap(), Ok(1));
    }

    /// A thread that starts with the signal blocked, as one spawned by a
    /// thread that blocks it would: the tick still gets through while the
    /// ticker lives, and the thread has the signal blocked again once the
    /// ticker is dropped. The guard that does it is also dropped on its own,
    /// which is all a start that fails after unblocking does with it; the
    /// kernel refusing a timer cannot be arranged here.
    #[test]
    fn a_thread_that_had_the_signal_blocked_gets_it_back_blocked() {
        std::thread::spawn(|| {
            let signal = *TICK_SIGNAL
                .get_or_init(install_handler)
                .as_ref()
                .expect("the handler installs on the test host");
            // Whatever mask the test runner handed down, this thread's
            // starts with the signal blocked.
            set_blocked(signal, true).unwrap();

            let outcome = read_under_a_ticker(|| Ticker::start(Duration::from_millis(20)).unwrap());
            assert_eq!(outcome, Err(ErrorKind::Interrupted));
            assert!(
                set_blocked(signal, true).unwrap(),
                "blocked again once the ticker is dropped"
            );

            let guard = Unblocked::new(signal).unwrap();
            assert!(guard.was_blocked);
            assert!(
                !set_blocked(signal, false).unwrap(),
                "unblocked while the guard lives"
            );
            drop(guard);
            assert!(
                set_blocked(signal, true).unwrap(),
                "and blocked again when it goes"
            );
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_thread_that_had_the_signal_unblocked_keeps_it_unblocked() {
        std::thread::spawn(|| {
            let signal = *TICK_SIGNAL
                .get_or_init(install_handler)
                .as_ref()
                .expect("the handler installs on the test host");
            // Whatever mask the test runner handed down, this thread's
            // starts with the signal unblocked.
            set_blocked(signal, false).unwrap();
            drop(Ticker::start(Duration::from_millis(20)).unwrap());
            assert!(!set_blocked(signal, false).unwrap());
        })
        .join()
        .unwrap();
    }

    /// Only the default disposition is the tick's to replace: not "ignore",
    /// which a launcher may have chosen for this process, and not a handler,
    /// the tick's own included.
    #[test]
    fn only_a_signal_left_at_its_default_is_taken() {
        assert!(is_unclaimed(libc::SIG_DFL));
        assert!(!is_unclaimed(libc::SIG_IGN));
        let handler = on_tick as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
        assert!(!is_unclaimed(handler));
    }

    #[test]
    fn a_zero_interval_is_refused() {
        let error = Ticker::start(Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "a tick needs an interval above zero");
    }

    #[test]
    fn an_interval_becomes_seconds_and_nanoseconds() {
        let spec = timespec(Duration::new(3, 250_000_000)).unwrap();
        assert_eq!((spec.tv_sec, spec.tv_nsec), (3, 250_000_000));
        let spec = timespec(Duration::from_millis(50)).unwrap();
        assert_eq!((spec.tv_sec, spec.tv_nsec), (0, 50_000_000));
        let error = timespec(Duration::MAX).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "the interval is too long");
    }

    /// The constant this module defines for the sake of musl is the one the
    /// libc crate has for glibc.
    #[cfg(target_env = "gnu")]
    #[test]
    fn the_thread_id_notification_constant_is_libcs() {
        assert_eq!(SIGEV_THREAD_ID, libc::SIGEV_THREAD_ID);
    }
}

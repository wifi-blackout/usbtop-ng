//! Committed PTY harness for the wedged-terminal checks (spec §3). These ran
//! by hand for every review before this file existed; they are now part of
//! the default suite.
//!
//! Each test spawns the real binary with a `libc::openpty`-allocated slave
//! wired to its stdin, stdout, and stderr, `--force` (so the TUI opens with
//! no usbmon needed) and `--refresh 100` (so frames keep coming), with `HOME`
//! pointed at a tempdir so the child never touches the real `~/.usbtop-ng`.
//! Every test enforces a bounded exit deadline: that bound is the whole
//! point, since the thing under test is the terminal-restore path described
//! in `src/tui/lifecycle.rs` (`restore_with`, `write_within_budget`) staying
//! inside its own 250ms write budget even when the far end will not read.
//!
//! Zombie hygiene: [`PtySession`]'s `Drop` kills and reaps the child, and its
//! `master`/`retained_slave` fields close their descriptors on the way out,
//! on every path out of a test -- including a panicking assertion, which
//! unwinds (this crate does not build with `panic = "abort"`) and runs
//! `Drop` before the test binary reports the failure.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// How long a test waits for the first frame (the alternate-screen-enter
/// sequence) to show up on the master. Generous: on a healthy pty this
/// lands within milliseconds, but a loaded CI box must never flake here.
const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(10);

/// How long a test waits, after asking the child to leave (a `q` keystroke
/// or a signal), for it to actually exit and for the restore bytes to show
/// up. Generous next to the internal 250ms restore budget
/// (`RESTORE_POLL` * `RESTORE_ATTEMPTS` in `src/tui/lifecycle.rs`).
const EXIT_DEADLINE: Duration = Duration::from_secs(10);

/// Same idea as [`EXIT_DEADLINE`], longer: the wedged test spends real time
/// first filling the pty's kernel buffer before it can even send the signal
/// that starts this clock.
const WEDGED_EXIT_DEADLINE: Duration = Duration::from_secs(15);

/// How long the wedged test lets the child free-run, undrained, before
/// sending `SIGTERM`. Several refresh ticks (`--refresh 100`) at this
/// length is enough for the pty's kernel buffer to fill.
const WEDGE_FILL_PAUSE: Duration = Duration::from_secs(2);

/// Poll interval for [`PtySession::wait_for_exit`]'s `try_wait` loop.
const POLL: Duration = Duration::from_millis(30);

/// The pty's window size. `openpty`'s `winp` is given this explicitly
/// (rather than null, which leaves the pty at 0x0) so the TUI renders a
/// real frame every tick -- load-bearing for the wedged test, which needs
/// genuine bytes accumulating in the kernel buffer, not empty diffs.
const PTY_ROWS: u16 = 24;
const PTY_COLS: u16 = 80;

/// Alternate-screen-enter, the sequence crossterm's `EnterAlternateScreen`
/// writes as part of the first frame. Its presence on the master is "the
/// TUI is up".
const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";
/// Leave-alternate-screen, the first half of `restore_with`'s teardown pair
/// (src/tui/lifecycle.rs).
const ALT_SCREEN_LEAVE: &[u8] = b"\x1b[?1049l";
/// Cursor-show, the second half of that pair.
const CURSOR_SHOW: &[u8] = b"\x1b[?25h";

/// A distinctive substring of `usbmon::UNLOAD_QUESTION` (src/usbmon/mod.rs).
/// This crate has no lib target -- only a bin -- so the harness mirrors the
/// text instead of importing the constant.
const UNLOAD_QUESTION_MARKER: &[u8] = b"Unload usbmon now?";

/// Open a pty pair. Returns the master (kept for I/O) and two slave
/// descriptors: one to wire up the child's stdio, one retained by the
/// caller purely to query termios after the child exits. The child's own
/// slave copies are its stdio and go away with it, so somebody on the
/// parent side has to keep a handle open for `tcgetattr` to have anything
/// left to ask -- see test 1, the only test that uses it.
fn open_pty() -> (OwnedFd, OwnedFd, OwnedFd) {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let winsize = libc::winsize {
        ws_row: PTY_ROWS,
        ws_col: PTY_COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: `master` and `slave` are valid `&mut c_int` out-params for the
    // call. `name` and `termp` are null, accepting the kernel's own pty name
    // and its default termios (cooked: ICANON|ECHO on, exactly the freshly
    // opened state test 1 checks the terminal comes back to after restore).
    // `winp` points at a `winsize` this function owns for the length of the
    // call. A negative return means neither descriptor was allocated.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &winsize,
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());

    // SAFETY: `master` and `slave` are the fresh, valid, exclusively-owned
    // descriptors `openpty` just returned on success.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let retained_slave = slave
        .try_clone()
        .expect("dup the slave for the parent's retained copy");
    (master, slave, retained_slave)
}

/// Drain `master` into `tx` until it errors, closes, or `stop` is set.
///
/// The `stop` check happens before every read, not just once: the wedged
/// test (3) sets it right after the first frame arrives, and from that
/// moment this thread must stop pulling bytes off the master entirely, or
/// the pty's kernel buffer this test needs to fill would just keep
/// draining.
fn spawn_reader(mut master: File, tx: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match master.read(&mut buf) {
                Ok(0) => return, // master closed (every slave fd is gone)
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        return; // the test has what it needs and moved on
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
        }
    });
}

/// Accumulate chunks from `rx` until `needle` appears in the total or
/// `deadline` passes. Returns whatever was collected either way, so a
/// timing-out caller can still report what (if anything) arrived.
fn read_until(rx: &mpsc::Receiver<Vec<u8>>, needle: &[u8], deadline: Instant) -> Vec<u8> {
    let mut collected = Vec::new();
    loop {
        if contains(&collected, needle) {
            return collected;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return collected;
        }
        match rx.recv_timeout(remaining) {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(_) => return collected, // timed out or the sender hung up
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// One pty-backed child process and everything needed to drive it: the
/// master for reading frames and typing keystrokes, a retained slave for
/// termios, and a background thread feeding `rx` from the master.
struct PtySession {
    child: Child,
    master: File,
    retained_slave: OwnedFd,
    rx: mpsc::Receiver<Vec<u8>>,
    stop_reading: Arc<AtomicBool>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Best-effort: a child that already exited (or is already gone) must
        // not turn a test failure into a panic-during-unwind abort. `master`
        // and `retained_slave` close on their own `Drop` right after this
        // runs, once the struct's fields are torn down in turn -- that also
        // unblocks the reader thread's own master read, if it is still
        // parked in one.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PtySession {
    /// Spawn `usbtop-ng --force --refresh 100` on a fresh pty, with `HOME`
    /// pointed at `home` so the run never touches the real
    /// `~/.usbtop-ng`.
    fn spawn(home: &Path) -> Self {
        let (master_fd, slave_fd, retained_slave) = open_pty();
        let master = File::from(master_fd);
        // Cloned, and the reader thread started, before anything fallible
        // touches the child: once `cmd.spawn()` below succeeds, the very
        // next thing built is the `PtySession` itself, whose `Drop` is the
        // zombie guard. Nothing fallible sits between those two points.
        let reader_master = master
            .try_clone()
            .expect("dup master for the reader thread");
        let stdin_fd = slave_fd.try_clone().expect("dup slave for stdin");
        let stdout_fd = slave_fd.try_clone().expect("dup slave for stdout");

        let (tx, rx) = mpsc::channel();
        let stop_reading = Arc::new(AtomicBool::new(false));
        spawn_reader(reader_master, tx, Arc::clone(&stop_reading));

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_usbtop-ng"));
        cmd.args(["--force", "--refresh", "100"])
            .env("HOME", home)
            .stdin(Stdio::from(stdin_fd))
            .stdout(Stdio::from(stdout_fd))
            .stderr(Stdio::from(slave_fd));
        let child = cmd.spawn().expect("spawn the pty-backed child");
        // Closes the parent's three copies of the slave (stdin/stdout/stderr)
        // now that exec has dup2'd its own onto 0/1/2 -- only `master` and
        // `retained_slave` stay open on this side from here on.
        drop(cmd);

        PtySession {
            child,
            master,
            retained_slave,
            rx,
            stop_reading,
        }
    }

    /// Wait (up to `deadline` from now) for `needle` to show up on the
    /// master, returning everything collected in the meantime.
    fn wait_for(&self, needle: &[u8], deadline: Duration) -> Vec<u8> {
        read_until(&self.rx, needle, Instant::now() + deadline)
    }

    /// Type `bytes` at the child, as if a user had.
    fn type_at_child(&mut self, bytes: &[u8]) {
        self.master
            .write_all(bytes)
            .expect("write a keystroke to the pty master");
    }

    /// Send `signal` to the child directly, the way an external `kill`
    /// would -- not through the pty's own signal-generation (`^C`, a
    /// hangup), which raw mode disables or which needs a controlling
    /// terminal this harness never establishes.
    fn signal(&self, signal: libc::c_int) {
        // SAFETY: `self.child` keeps this pid alive and owned by this
        // process for as long as `self` exists, so it names a real process
        // this test is allowed to signal.
        let rc = unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
        assert_eq!(
            rc,
            0,
            "kill({signal}) failed: {}",
            std::io::Error::last_os_error()
        );
    }

    /// Poll for the child's exit, up to `deadline` from now.
    fn wait_for_exit(&mut self, deadline: Duration) -> Option<ExitStatus> {
        let until = Instant::now() + deadline;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= until {
                return None;
            }
            thread::sleep(POLL);
        }
    }

    /// Stop the reader thread from draining the master, so the pty's
    /// kernel buffer can fill. Test 3 only.
    fn stop_draining(&self) {
        self.stop_reading.store(true, Ordering::SeqCst);
    }

    /// Whether the retained slave's termios has canonical mode and echo
    /// on -- the state a freshly opened terminal starts in, and the state
    /// `disable_raw_mode` (called from `restore_with`) is supposed to put
    /// it back to.
    fn slave_is_cooked(&self) -> bool {
        let mut term: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `self.retained_slave` is the parent's own open copy of
        // the pty slave, valid for the life of `self`; `tcgetattr` writes
        // into `term` and reads nothing from it.
        let rc = unsafe { libc::tcgetattr(self.retained_slave.as_raw_fd(), &mut term) };
        assert_eq!(
            rc,
            0,
            "tcgetattr failed: {}",
            std::io::Error::last_os_error()
        );
        term.c_lflag & (libc::ICANON | libc::ECHO) == (libc::ICANON | libc::ECHO)
    }
}

#[test]
fn quit_restores_the_terminal_and_returns_it_to_cooked_mode() {
    let home = tempfile::tempdir().expect("tempdir for a hermetic HOME");
    let mut session = PtySession::spawn(home.path());

    let first_frame = session.wait_for(ALT_SCREEN_ENTER, FIRST_FRAME_DEADLINE);
    assert!(
        contains(&first_frame, ALT_SCREEN_ENTER),
        "the first frame's alternate-screen-enter never arrived within \
         {FIRST_FRAME_DEADLINE:?}: {:?}",
        String::from_utf8_lossy(&first_frame)
    );

    session.type_at_child(b"q");

    let status = session
        .wait_for_exit(EXIT_DEADLINE)
        .unwrap_or_else(|| panic!("child did not exit within {EXIT_DEADLINE:?} after 'q'"));
    assert!(status.success(), "quit should exit cleanly: {status:?}");

    let restore = session.wait_for(ALT_SCREEN_LEAVE, EXIT_DEADLINE);
    let text = String::from_utf8_lossy(&restore);
    assert!(
        contains(&restore, ALT_SCREEN_LEAVE),
        "leave-alternate-screen never arrived within {EXIT_DEADLINE:?}: {text:?}"
    );
    assert!(
        contains(&restore, CURSOR_SHOW),
        "cursor-show never arrived within {EXIT_DEADLINE:?}: {text:?}"
    );

    assert!(
        session.slave_is_cooked(),
        "the slave's termios should have ICANON and ECHO back on after restore"
    );
}

#[test]
fn sighup_restores_the_terminal_without_a_prompt() {
    let home = tempfile::tempdir().expect("tempdir for a hermetic HOME");
    let mut session = PtySession::spawn(home.path());

    let first_frame = session.wait_for(ALT_SCREEN_ENTER, FIRST_FRAME_DEADLINE);
    assert!(
        contains(&first_frame, ALT_SCREEN_ENTER),
        "the first frame's alternate-screen-enter never arrived within \
         {FIRST_FRAME_DEADLINE:?}: {:?}",
        String::from_utf8_lossy(&first_frame)
    );

    session.signal(libc::SIGHUP);

    let status = session
        .wait_for_exit(EXIT_DEADLINE)
        .unwrap_or_else(|| panic!("child did not exit within {EXIT_DEADLINE:?} after SIGHUP"));
    assert!(status.success(), "a hangup should exit cleanly: {status:?}");

    let restore = session.wait_for(ALT_SCREEN_LEAVE, EXIT_DEADLINE);
    let text = String::from_utf8_lossy(&restore);
    assert!(
        contains(&restore, ALT_SCREEN_LEAVE),
        "leave-alternate-screen never arrived within {EXIT_DEADLINE:?}: {text:?}"
    );
    assert!(
        contains(&restore, CURSOR_SHOW),
        "cursor-show never arrived within {EXIT_DEADLINE:?}: {text:?}"
    );
    assert!(
        !contains(&restore, UNLOAD_QUESTION_MARKER),
        "a hangup must never print a prompt -- there is nobody left to \
         answer one (see UnloadPolicy::AutoOnly, src/tui/lifecycle.rs): {text:?}"
    );

    assert!(
        session.slave_is_cooked(),
        "the slave's termios should have ICANON and ECHO back on after restore"
    );
}

#[test]
fn a_wedged_terminal_still_exits_within_the_deadline_on_sigterm() {
    let home = tempfile::tempdir().expect("tempdir for a hermetic HOME");
    let mut session = PtySession::spawn(home.path());

    let first_frame = session.wait_for(ALT_SCREEN_ENTER, FIRST_FRAME_DEADLINE);
    assert!(
        contains(&first_frame, ALT_SCREEN_ENTER),
        "the first frame's alternate-screen-enter never arrived within \
         {FIRST_FRAME_DEADLINE:?}: {:?}",
        String::from_utf8_lossy(&first_frame)
    );

    // Stop reading right after the first frame: with nobody draining the
    // master, every 100ms-refresh frame the child writes stays queued in
    // the pty's kernel buffer instead of being read out from under it.
    session.stop_draining();
    thread::sleep(WEDGE_FILL_PAUSE);

    session.signal(libc::SIGTERM);

    // The assertion is the bounded exit itself: `restore_with`'s write
    // budget (see `write_within_budget`, src/tui/lifecycle.rs) gives up on
    // a terminal that will not take the restore bytes rather than blocking
    // the process on them, so the child must still leave promptly even
    // though this test never reads another byte off the master.
    let status = session
        .wait_for_exit(WEDGED_EXIT_DEADLINE)
        .unwrap_or_else(|| {
            panic!(
                "child did not exit within {WEDGED_EXIT_DEADLINE:?} of SIGTERM \
             on a wedged terminal -- the restore path hung"
            )
        });
    // `spawn_signal_thread` (src/tui/lifecycle.rs) turns SIGTERM into an
    // ordinary `UiEvent`, so this leaves through the normal teardown and
    // exits 0 -- the same path a `q` keypress takes, not one where the raw
    // signal kills the process.
    assert!(
        status.success(),
        "a SIGTERM'd session should still exit cleanly: {status:?}"
    );
}

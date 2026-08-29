//! Proves that `tui::lifecycle::restore_terminal`'s bytes reach fd 1 through
//! the raw `libc::write` path while the process is still alive, rather than
//! sitting in a buffered writer until process exit.
//!
//! The regression this guards: if the restore path ever went back through a
//! buffered `Write` (`io::stdout()`'s `LineWriter`, say — see the comment on
//! `RawStdout` in `src/tui/lifecycle.rs` for why that was tried and reverted),
//! the restore sequence would sit unflushed until the process exited. A pipe
//! reader with a bounded deadline, checking the child is *still running* the
//! moment the bytes show up, tells the two apart: raw bytes arrive promptly,
//! mid-life; buffered bytes would arrive only at exit — after this harness's
//! deadline expires and it has already killed the child.
//!
//! The real binary carries a hidden hook for exactly this (see `main.rs`):
//! set `USBTOP_NG_RESTORE_PROBE=1` and it arms the restore latch, runs
//! `restore_terminal()` — the same call teardown and the panic hook make —
//! and then parks for 60s, far past this file's 5s read deadline, so the
//! child is always still alive when the assertions run.

use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long the harness waits for the restore sequence to show up on the
/// pipe. Generous relative to the internal 250ms restore budget (see
/// `RESTORE_POLL` / `RESTORE_ATTEMPTS` in `src/tui/lifecycle.rs`) so a slow
/// CI box cannot flake it, but far short of the probe's own 60s park.
const READ_DEADLINE: Duration = Duration::from_secs(5);

/// Owns the probe child and guarantees it is killed and reaped on every path
/// out of a test, including a failed assertion: a panic unwinds the stack
/// (this crate does not build with `panic = "abort"`), which runs `Drop`
/// here before the test binary reports the failure. Without this, a failed
/// assertion would leave the parked probe process behind.
struct ProbeChild(Child);

impl Drop for ProbeChild {
    fn drop(&mut self) {
        // Best-effort: a child that already exited (or is already gone) must
        // not turn a test failure into a panic-during-unwind abort.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl std::ops::Deref for ProbeChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for ProbeChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

/// Spawn the real binary with the hidden restore-probe hook armed. Stdin and
/// stderr are discarded so the harness owns exactly one descriptor's worth of
/// output; stdout is piped so this test is the terminal the restore bytes
/// have to reach.
///
/// `SUDO_UID`/`SUDO_GID` are stripped from the child's environment the same
/// way `PtySession::spawn` (`tests/pty.rs`) strips them: this probe never
/// creates any config file, so under the root test flow (`sudo cargo test
/// --features integration`) there is no chown-target home for a leaked
/// invoker identity to redirect -- lower risk than the pty harness, not
/// zero, so it strips them too for the same reason and to keep both
/// harnesses' child environments consistent with each other.
fn spawn_probe() -> ProbeChild {
    let child = Command::new(env!("CARGO_BIN_EXE_usbtop-ng"))
        .env("USBTOP_NG_RESTORE_PROBE", "1")
        .env_remove("SUDO_UID")
        .env_remove("SUDO_GID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the probe binary spawns");
    ProbeChild(child)
}

/// Read `stdout` on a helper thread, forwarding every chunk through `tx`
/// until the pipe closes or the read errors. Detached: the test only ever
/// waits on the channel with its own deadline, never on this thread — a
/// pipe that never produces anything must not be able to hang the test, only
/// time it out.
fn spawn_reader(mut stdout: ChildStdout, tx: mpsc::Sender<Vec<u8>>) {
    thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => return, // pipe closed
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        return; // the test has what it needs and moved on
                    }
                }
                Err(_) => return,
            }
        }
    });
}

/// Accumulate chunks from `rx` until `needle` appears in the total or
/// `READ_DEADLINE` elapses. Returns whatever was collected either way, so a
/// timing-out caller can still report what (if anything) arrived.
fn read_until_contains(rx: &mpsc::Receiver<Vec<u8>>, needle: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut collected = Vec::new();
    loop {
        if collected
            .windows(needle.len())
            .any(|window| window == needle)
        {
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

#[test]
fn restore_bytes_reach_a_piped_stdout_while_the_process_is_still_alive() {
    let mut probe = spawn_probe();
    let stdout = probe.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    spawn_reader(stdout, tx);

    // Leave-alternate-screen: the byte-for-byte sequence `restore_to` queues
    // via crossterm's `LeaveAlternateScreen`, asserted the same way the unit
    // test `restore_runs_once_however_often_it_is_asked` in
    // src/tui/lifecycle.rs asserts it.
    let needle = b"\x1b[?1049l";
    let collected = read_until_contains(&rx, needle);
    let text = String::from_utf8_lossy(&collected);

    // Checked immediately once the bytes are in hand, before any assertion
    // that could panic and before any kill: this is the moment that
    // distinguishes the raw write from a buffered one. `try_wait` never
    // blocks, so this adds no slack a buffered-writer regression could hide
    // behind.
    let still_running = probe
        .try_wait()
        .expect("try_wait does not fail on a live child")
        .is_none();

    assert!(
        text.contains("\x1b[?1049l"),
        "leave-alternate-screen never arrived on the pipe within {READ_DEADLINE:?}: {text:?}"
    );
    assert!(
        still_running,
        "restore bytes arrived only after the process had already exited -- \
         that is buffered stdio flushing at exit, not the raw write this \
         harness guards against regressing to"
    );
}

#[test]
fn restore_sequence_also_shows_the_cursor() {
    let mut probe = spawn_probe();
    let stdout = probe.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    spawn_reader(stdout, tx);

    // Cursor-show: the other half of the pair `restore_to` queues alongside
    // leave-alternate-screen (`queue!(sequence, LeaveAlternateScreen, Show)`
    // in `restore_with`, src/tui/lifecycle.rs).
    let needle = b"\x1b[?25h";
    let collected = read_until_contains(&rx, needle);
    let text = String::from_utf8_lossy(&collected);

    let still_running = probe
        .try_wait()
        .expect("try_wait does not fail on a live child")
        .is_none();

    assert!(
        text.contains("\x1b[?25h"),
        "cursor-show never arrived on the pipe within {READ_DEADLINE:?}: {text:?}"
    );
    assert!(
        still_running,
        "restore bytes arrived only after the process had already exited -- \
         that is buffered stdio flushing at exit, not the raw write this \
         harness guards against regressing to"
    );
}

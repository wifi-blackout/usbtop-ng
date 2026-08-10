use anyhow::Result;
use crossterm::{
    event::Event,
    execute,
    terminal::{enable_raw_mode, size as terminal_size, EnterAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    env, io, iter,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    time::{Duration, Instant},
};

use crate::device::manager::DeviceManager;
use crate::ui::{self, KeyOutcome, UsbTopApp};
use crate::usbmon::parser::UsbPacket;

pub(crate) mod events;
pub(crate) mod lifecycle;
pub(crate) mod output;
pub(crate) mod sync;

use events::UiEvent;
use output::{ShedHandles, ShedWriter, StdoutRaw};
use sync::{probe_decision, probe_sync_mode, ProbeDecision, SyncMode};

/// The terminal as this program has it: ratatui, over crossterm, over the
/// non-blocking output stage.
type TuiTerminal = Terminal<CrosstermBackend<ShedWriter<StdoutRaw>>>;

/// Floor for `--refresh`, in milliseconds. Below this the poll/redraw loop
/// spends more time spinning than the terminal can usefully repaint, so
/// requests under the floor are clamped rather than honored literally.
const REFRESH_FLOOR_MS: u64 = 100;

/// Clamp a requested `--refresh` value (milliseconds) to the floor.
pub fn effective_refresh_ms(requested: u64) -> u64 {
    requested.max(REFRESH_FLOOR_MS)
}

/// Why the event loop stopped. [`lifecycle::unload_policy`] turns this into
/// what the exit path is still allowed to ask the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The user pressed `q`, Esc or Ctrl-C.
    UserQuit,
    /// The terminal is gone; there is nothing left to draw on.
    TerminalDead,
    /// A signal arrived, by number.
    Signal(i32),
}

/// A finished TUI session: why it ended, and the means to ask the user one
/// more thing on the way out.
pub struct UiSession {
    /// Why the event loop stopped.
    pub reason: ExitReason,
    /// The input thread's channel. It is still running — a blocked terminal
    /// read cannot be called off — so this, not stdin, is where a keystroke
    /// arrives after teardown.
    events: Receiver<UiEvent>,
}

impl UiSession {
    /// Ask the user a yes/no question now that the TUI is down.
    ///
    /// Reading stdin directly here would race the input thread for every
    /// keystroke and usually lose; see [`lifecycle::prompt_via_events`].
    pub fn confirm(&self, question: &str) -> bool {
        lifecycle::prompt_via_events(question, &self.events)
    }
}

pub fn run_ui(
    mut app: UsbTopApp,
    mut manager: DeviceManager,
    packets: Receiver<UsbPacket>,
) -> Result<UiSession> {
    // First, so that everything below has a way back out of TUI mode even if
    // it dies mid-frame.
    lifecycle::install_panic_hook();

    let (tx, ui_events) = mpsc::channel();
    // Signals arrive as ordinary events, so a SIGTERM leaves through the same
    // teardown as `q` instead of dropping the process on an alternate screen.
    // Spawned before the terminal changes state, so that a signal landing
    // during setup is caught rather than defaulting to a kill that would leave
    // the terminal raw.
    lifecycle::spawn_signal_thread(tx.clone());

    let (mut terminal, shed) = enter_terminal().inspect_err(|_| {
        // Raw mode is already on if it was the alternate screen that failed.
        lifecycle::restore_terminal();
    })?;

    // A session that has to shed frames is showing the user less than it
    // measured, which the header says out loud — like `dropped:` does for
    // packets nobody counted.
    app.shed_counter = Some(shed.shed_frames());

    // Only now: a reader started before raw mode would be handed whole lines
    // instead of keys.
    events::spawn_input_thread(tx);

    let result = run_app(
        &mut terminal,
        &mut app,
        &mut manager,
        &packets,
        &ui_events,
        &shed,
    );

    // Whatever is still queued is a diff against an alternate screen that is
    // about to be gone. Flushing it after the restore below would paint it
    // across the shell the user just got back, so it is dropped here instead.
    // On a terminal that kept up there is nothing queued and nothing to drop.
    shed.discard_pending();
    // And no more synchronized updates. The last thing ratatui writes is the
    // `show_cursor` in its destructor, on a terminal already on its way out; a
    // bracket around it buys nothing and can cost everything, because a write
    // that stalls after the opening half leaves the terminal holding its screen
    // back until something closes the update — and the only thing left to do
    // that is the restore below, whose own writes are now on a budget and may
    // never land either.
    shed.set_sync_mode(SyncMode::Unsupported);

    // Before the restore, deliberately. ratatui's destructor shows the cursor
    // again, which is a write through the shed writer, and the shed writer only
    // stays non-blocking until `restore_terminal` puts stdout's original flags
    // back — after which that same write would park the process on a terminal
    // that has stopped reading. Here it either goes out or is shed, and it
    // cannot fail: the writer answers through its flags rather than through
    // `io::Result`, so ratatui's "failed to show the cursor" branch — which
    // `eprintln!`s, and so would panic mid-destructor — is unreachable and
    // needs no guarding. `restore_terminal` shows the cursor too, so nothing is
    // lost if this one is shed.
    //
    // The panic path cannot be ordered like this — the hook runs, and unwinding
    // drops the terminal afterwards — which is what the abandon latch armed in
    // `enter_terminal` is for. Two mechanisms because there are two orders; do
    // not retire either on the strength of the other.
    drop(terminal);

    // Explicit teardown; the panic hook is the safety net, not the plan.
    lifecycle::restore_terminal();

    // The receiver outlives the loop: after teardown it is the only way to
    // read the keyboard, and the exit path may still have a question.
    result.map(|reason| UiSession {
        reason,
        events: ui_events,
    })
}

/// Put the terminal into TUI mode: raw input, the mode-2026 handshake,
/// alternate screen, the non-blocking output stage, ratatui on top.
///
/// Order matters three times here. `arm_restore` runs before [`StdoutRaw::new`]
/// switches stdout to non-blocking, so what it saves — and what
/// `restore_terminal` puts back — is stdout's *pre-TUI* flags. The alternate
/// screen is entered on the still-blocking descriptor, so that write cannot
/// come back short. And the handshake sits where it does because it is the only
/// window in the program's life with all three of its preconditions true at
/// once: raw mode is on (so the reply arrives a byte at a time and is not
/// echoed), stdout is still blocking (so the query cannot come back short), and
/// [`events::spawn_input_thread`] has not run (so the reply is still this
/// thread's to read — afterwards stdin belongs to the input thread, which would
/// deliver the reply to the event loop as keystrokes). It runs before the
/// alternate screen for the same reason it runs at all: a terminal that hangs
/// on the query has not yet been given a screen to hang on.
fn enter_terminal() -> Result<(TuiTerminal, ShedHandles)> {
    enable_raw_mode()?;
    // From here on there is something to undo, whoever gets to it first — the
    // handshake below included. This is also where stdout's original
    // file-status flags are saved.
    lifecycle::arm_restore();

    let sync_mode = match probe_decision(
        env::var("SSH_TTY").ok().as_deref(),
        env::var("SSH_CONNECTION").ok().as_deref(),
        env::var("SSH_CLIENT").ok().as_deref(),
        env::var("TERM").ok().as_deref(),
    ) {
        ProbeDecision::Probe => probe_sync_mode(),
        ProbeDecision::AssumeUnsupported => SyncMode::Unsupported,
    };

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let (cols, rows) = terminal_size()?;
    let writer = ShedWriter::new(StdoutRaw::new()?, cols, rows);
    // Taken before ratatui swallows the writer: the backend keeps it private,
    // so these shared cells are the only way back in.
    let shed = writer.handles();
    shed.set_sync_mode(sync_mode);
    // The restore has to be able to silence this writer before it makes stdout
    // blocking again, and on the panic path it is handed nothing to silence it
    // with — so it is given the latch now, while there is somewhere to put it.
    lifecycle::arm_output_latch(shed.abandon_latch());

    Ok((Terminal::new(CrosstermBackend::new(writer))?, shed))
}

/// Wipe the screen and make the next draw a whole frame rather than a diff.
///
/// Deliberately not `Terminal::clear`, which snapshots the cursor position
/// first — a DECRQM-style round trip that writes `ESC [ 6 n` and then waits for
/// the terminal to answer on stdin. Both halves of that fail exactly when this
/// function is needed most. The write goes to a descriptor that is refusing
/// writes, and the answer would have to come from a terminal that has stopped
/// reading; crossterm gives up after two seconds and reports an error, which
/// would end the session on the one path that exists to rescue it. (The answer
/// is not even reliably ours: the input thread owns stdin, so it can take the
/// reply first — crossterm documents `position()` as unreliable while another
/// thread is in `read()`.)
///
/// `resize` to the size already in force does the two things that matter —
/// clear the screen, reset the diff's baseline — and asks the terminal nothing.
fn force_full_repaint(terminal: &mut TuiTerminal) -> Result<()> {
    // An ioctl, not a terminal round trip; `Terminal::draw` makes the same call
    // on every frame anyway.
    let size = terminal.size()?;
    terminal.resize(size.into())?;
    Ok(())
}

/// The event loop.
///
/// It sleeps until the earliest deadline it owes — the next data tick, or the
/// next frame when something is waiting to be painted — rather than drawing on
/// a fixed interval. A burst of events costs one repaint instead of one per
/// event, and nothing repaints at all unless something changed.
///
/// The one thing that cannot wake it is a packet: the readers push onto a
/// bounded channel and drop once it fills. So the wait is capped at
/// [`events::PACKET_DRAIN_INTERVAL`] — a wake that only drains, never draws.
fn run_app(
    terminal: &mut TuiTerminal,
    app: &mut UsbTopApp,
    manager: &mut DeviceManager,
    packets: &Receiver<UsbPacket>,
    ui_events: &Receiver<UiEvent>,
    shed: &ShedHandles,
) -> Result<ExitReason> {
    let start = Instant::now();
    // Start owing a frame, and owing it immediately: the first pass refreshes
    // and paints instead of leaving an empty screen up for a whole interval.
    let mut dirty = true;
    let mut last_draw = start
        .checked_sub(events::MIN_FRAME_INTERVAL)
        .unwrap_or(start);
    let mut next_tick = start;
    let mut packet_backlog = false;

    loop {
        let now = Instant::now();
        let timeout = if packet_backlog {
            // The last drain filled its batch, so the channel may still hold
            // more: come straight back for the rest instead of spending a
            // drain interval on what is already queued.
            Duration::ZERO
        } else {
            events::next_wait(now, dirty, next_tick, last_draw)
        };

        match ui_events.recv_timeout(timeout) {
            Ok(event) => {
                // Fold the whole queued batch, not just the event that woke
                // us: that is what turns a burst of resizes into one repaint.
                // `ok()` reads a mid-batch disconnect as "nothing more
                // queued"; the next pass's `recv_timeout` reports it properly.
                let batch = iter::once(event).chain(iter::from_fn(|| ui_events.try_recv().ok()));
                let fold = fold_events(app, batch);
                if let Some(reason) = fold.exit {
                    return Ok(reason);
                }
                // The writer's shed threshold is a function of the screen's
                // area, so a resize has to reach it before the frames drawn at
                // the new size do.
                if let Some((cols, rows)) = fold.resize {
                    shed.set_area(cols, rows);
                }
                if fold.clear {
                    force_full_repaint(terminal)?;
                }
                dirty |= fold.dirty;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Every sender is gone, so no event can ever wake the loop again.
            Err(RecvTimeoutError::Disconnected) => return Ok(ExitReason::TerminalDead),
        }

        packet_backlog = ui::drain_packets(manager, packets, ui::DRAIN_BATCH) == ui::DRAIN_BATCH;

        // The wait above may have consumed the whole timeout, so the clock is
        // re-read before deciding what this pass owes.
        let now = Instant::now();
        if now >= next_tick {
            // `sync_from` rebuilds the whole snapshot, so the list of devices
            // dropped by this refresh needs no separate handling.
            manager.enumerate_present_devices();
            let _ = manager.refresh();
            app.sync_from(manager);
            app.update_bandwidth_history();
            // Measured from now, not from the missed deadline: a slow pass
            // shifts the next tick instead of queueing catch-up ticks.
            next_tick = now + app.refresh_rate;
            dirty = true;
        }

        if events::should_draw(now, dirty, last_draw) {
            terminal.draw(|f| ui::draw_ui(f, app))?;
            last_draw = now;
            dirty = false;
        }

        // The output stage reports through flags rather than errors — a write
        // failure must not take ratatui's teardown off its healthy path — so
        // this is where the writes issued above are actually answered for. It
        // sits outside the draw gate because `terminal.clear()` writes too.
        if shed.terminal_dead() {
            return Ok(ExitReason::TerminalDead);
        }
        if shed.take_repaint_request() {
            // Frames were shed, or bytes were lost to a failed write: either
            // way the screen no longer matches ratatui's mirror of it, and
            // only a wipe plus a full frame can put that right.
            force_full_repaint(terminal)?;
            dirty = true;
        }
    }
}

/// What a batch of drained events leaves the loop owing.
///
/// Folding a whole batch into one of these is the coalescing: a hundred queued
/// resizes cost one repaint, not a hundred.
#[derive(Debug, Default, PartialEq, Eq)]
struct Fold {
    /// Something changed what the screen should show.
    dirty: bool,
    /// The screen itself is suspect and has to be wiped before the repaint.
    clear: bool,
    /// The size the screen ended the batch at, when the batch resized it. Only
    /// the last one in a drag matters, which is the whole point of folding.
    resize: Option<(u16, u16)>,
    /// The batch ended the session; whatever is still queued no longer matters.
    exit: Option<ExitReason>,
}

/// Apply a batch of events to `app` in arrival order, stopping at the first
/// one that ends the session.
fn fold_events(app: &mut UsbTopApp, batch: impl Iterator<Item = UiEvent>) -> Fold {
    let mut fold = Fold::default();

    for event in batch {
        match event {
            UiEvent::Input(Event::Key(key)) => match ui::apply_key(app, key) {
                KeyOutcome::Quit => {
                    fold.exit = Some(ExitReason::UserQuit);
                    return fold;
                }
                KeyOutcome::ClearAndRedraw => {
                    fold.clear = true;
                    fold.dirty = true;
                }
                KeyOutcome::Redraw => fold.dirty = true,
                KeyOutcome::None => {}
            },
            // A resize invalidates the frame and nothing else: the next draw
            // re-reads the terminal's size on its own, so only the last one in
            // a drag matters and all of them fold into a single repaint. The
            // dimensions are carried out because the output stage sizes its
            // backlog allowance from them.
            UiEvent::Input(Event::Resize(cols, rows)) => {
                fold.dirty = true;
                fold.resize = Some((cols, rows));
            }
            // Focus, paste and (were they enabled) mouse events change nothing
            // on screen.
            UiEvent::Input(_) => {}
            UiEvent::TerminalDead => {
                fold.exit = Some(ExitReason::TerminalDead);
                return fold;
            }
            UiEvent::Signal(signal) => {
                fold.exit = Some(ExitReason::Signal(signal));
                return fold;
            }
        }
    }

    fold
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn test_app() -> UsbTopApp {
        UsbTopApp::new(Duration::from_millis(100))
    }

    fn key_event(code: KeyCode) -> UiEvent {
        UiEvent::Input(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn refresh_floor_clamps_low_values() {
        assert_eq!(effective_refresh_ms(0), 100);
        assert_eq!(effective_refresh_ms(99), 100);
        assert_eq!(effective_refresh_ms(100), 100);
        assert_eq!(effective_refresh_ms(1000), 1000);
    }

    #[test]
    fn an_empty_batch_owes_nothing() {
        let fold = fold_events(&mut test_app(), iter::empty());
        assert_eq!(fold, Fold::default());
    }

    #[test]
    fn a_resize_burst_folds_into_one_repaint() {
        let burst = (1..=5).map(|n| UiEvent::Input(Event::Resize(80 + n, 24)));
        let fold = fold_events(&mut test_app(), burst);
        assert_eq!(
            fold,
            Fold {
                dirty: true,
                clear: false,
                // The size the drag finished at, not the five it passed
                // through: that is the one the writer has to be told about.
                resize: Some((85, 24)),
                exit: None
            }
        );
    }

    #[test]
    fn quit_key_stops_the_loop() {
        let fold = fold_events(&mut test_app(), iter::once(key_event(KeyCode::Char('q'))));
        assert_eq!(fold.exit, Some(ExitReason::UserQuit));
    }

    #[test]
    fn events_after_a_quit_are_not_applied() {
        let mut app = test_app();
        let batch = [key_event(KeyCode::Char('q')), key_event(KeyCode::Char('h'))];
        let fold = fold_events(&mut app, batch.into_iter());
        assert_eq!(fold.exit, Some(ExitReason::UserQuit));
        assert!(!app.show_help, "the loop is leaving; state stops mattering");
    }

    #[test]
    fn ctrl_l_wipes_the_screen_and_repaints() {
        let ctrl_l = UiEvent::Input(Event::Key(KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
        )));
        let fold = fold_events(&mut test_app(), iter::once(ctrl_l));
        assert_eq!(
            fold,
            Fold {
                dirty: true,
                clear: true,
                resize: None,
                exit: None
            }
        );
    }

    #[test]
    fn terminal_death_stops_the_loop() {
        let fold = fold_events(&mut test_app(), iter::once(UiEvent::TerminalDead));
        assert_eq!(fold.exit, Some(ExitReason::TerminalDead));
    }

    #[test]
    fn a_signal_stops_the_loop_and_carries_its_number() {
        let fold = fold_events(&mut test_app(), iter::once(UiEvent::from(15)));
        assert_eq!(fold.exit, Some(ExitReason::Signal(15)));
    }

    #[test]
    fn unbound_input_leaves_the_screen_alone() {
        let batch = [
            key_event(KeyCode::Char('x')),
            UiEvent::Input(Event::FocusGained),
        ];
        let fold = fold_events(&mut test_app(), batch.into_iter());
        assert_eq!(fold, Fold::default());
    }
}

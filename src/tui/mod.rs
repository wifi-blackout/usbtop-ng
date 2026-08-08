use anyhow::Result;
use crossterm::{
    event::Event,
    execute,
    terminal::{enable_raw_mode, size as terminal_size, EnterAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io, iter, mem,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    time::{Duration, Instant},
};

use crate::device::manager::DeviceManager;
use crate::ui::{self, KeyOutcome, UsbTopApp};
use crate::usbmon::parser::UsbPacket;

pub(crate) mod events;
pub(crate) mod lifecycle;
pub(crate) mod output;

use events::UiEvent;
use output::{ShedHandles, ShedWriter, StdoutRaw};

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
    #[cfg(unix)]
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

    // Explicit teardown; the panic hook is the safety net, not the plan.
    lifecycle::restore_terminal();

    // ratatui's `Terminal` shows the cursor again when it drops, and if that
    // write fails it `eprintln!`s — which panics, because the print macros
    // unwrap. On a dead terminal the write always fails, so the destructor
    // would panic here, on the way out of the one exit path that most needs to
    // finish (`monitor.stop()` and the unload both still have to run). Doing
    // ratatui's job here first reports whether its destructor is safe to run at
    // all: if the write lands there is nothing left for it to do, and if it
    // does not, the value must not be dropped. `restore_terminal` has already
    // shown the cursor either way, so nothing is lost by skipping it.
    if terminal.show_cursor().is_err() {
        mem::forget(terminal);
    }

    // The receiver outlives the loop: after teardown it is the only way to
    // read the keyboard, and the exit path may still have a question.
    result.map(|reason| UiSession {
        reason,
        events: ui_events,
    })
}

/// Put the terminal into TUI mode: raw input, alternate screen, the
/// non-blocking output stage, ratatui on top.
///
/// Order matters twice here. `arm_restore` runs before [`StdoutRaw::new`]
/// switches stdout to non-blocking, so what it saves — and what
/// `restore_terminal` puts back — is stdout's *pre-TUI* flags. And the
/// alternate screen is entered on the still-blocking descriptor, so that write
/// cannot come back short.
fn enter_terminal() -> Result<(TuiTerminal, ShedHandles)> {
    enable_raw_mode()?;
    // From here on there is something to undo, whoever gets to it first. This
    // is also where stdout's original file-status flags are saved.
    lifecycle::arm_restore();
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let (cols, rows) = terminal_size()?;
    let writer = ShedWriter::new(StdoutRaw::new()?, cols, rows);
    // Taken before ratatui swallows the writer: the backend keeps it private,
    // so these shared cells are the only way back in.
    let shed = writer.handles();

    Ok((Terminal::new(CrosstermBackend::new(writer))?, shed))
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
                    terminal.clear()?;
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
            terminal.clear()?;
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

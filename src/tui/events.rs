//! Wake sources and redraw scheduling for the event loop.
//!
//! The loop never polls: it sleeps until the earliest instant at which it owes
//! the user something, and everything that can make it owe something arrives
//! here as a [`UiEvent`].

use std::{
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event};

/// Shortest gap between two rendered frames (~30 FPS). Events that land inside
/// one interval coalesce into the single frame that closes it.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Longest the loop may sleep regardless of what it owes the screen.
///
/// Nothing wakes the loop when a packet arrives — the readers push onto their
/// own bounded channel and discard once it is full — so the wait doubles as
/// the drain cadence. Left uncapped it would stretch to a whole `--refresh`
/// interval (1s by default), which at the channel's bound is a few thousand
/// packets silently dropped from the numbers this tool exists to report. This
/// is a bare wake: it touches neither the dirty flag nor the frame cap, so an
/// idle session still repaints only on its tick.
pub const PACKET_DRAIN_INTERVAL: Duration = Duration::from_millis(50);

/// Anything that can wake the event loop.
pub enum UiEvent {
    /// A terminal event: key press, resize, focus change.
    Input(Event),
    /// A caught signal, by number.
    Signal(i32),
    /// The terminal went away: reading it failed or hit EOF.
    TerminalDead,
}

impl From<i32> for UiEvent {
    /// Signals reach the loop as raw numbers from the signal thread; this is
    /// the one place that says what a raw signal number means to the UI.
    fn from(signal: i32) -> Self {
        UiEvent::Signal(signal)
    }
}

/// Park a detached thread on the terminal's input and forward what it reads.
///
/// The thread is deliberately never joined: it spends its life blocked inside
/// `event::read()`, and a blocked read on a tty cannot be portably interrupted
/// (there is no cross-platform "cancel this read" and no timeout on `read`
/// itself). It ends on its own when the read fails or when the loop drops the
/// receiver and the next send has nowhere to go; otherwise process exit reaps
/// it.
pub fn spawn_input_thread(tx: Sender<UiEvent>) {
    thread::spawn(move || loop {
        match event::read() {
            Ok(event) => {
                // A closed channel means the loop is gone, and so is the only
                // reason to keep reading.
                if tx.send(UiEvent::Input(event)).is_err() {
                    return;
                }
            }
            Err(_) => {
                // A tty read only fails once the terminal is unusable (EOF on
                // a closed pty, EIO after the session leader left), so this is
                // terminal death, not a hiccup to retry: retrying would spin.
                let _ = tx.send(UiEvent::TerminalDead);
                return;
            }
        }
    });
}

/// Whether the loop owes the screen a frame right now: something changed
/// *and* a frame's worth of time has passed since the last one.
///
/// The `dirty` half is what keeps an idle session from repainting at all; the
/// interval half is what keeps a busy one from repainting faster than anyone
/// can read.
pub fn should_draw(now: Instant, dirty: bool, last_draw: Instant) -> bool {
    dirty && now.saturating_duration_since(last_draw) >= MIN_FRAME_INTERVAL
}

/// The earliest instant at which the loop has work to do, and so the longest
/// it may sleep waiting for events.
///
/// With a clean screen the only thing on the calendar is the next data tick.
/// With a dirty one the pending frame also comes due, one
/// [`MIN_FRAME_INTERVAL`] after the last frame — which is what holds a burst
/// of events back into a single coalesced repaint. The result is never in the
/// past, so callers can subtract `now` from it directly.
pub fn next_deadline(now: Instant, dirty: bool, next_tick: Instant, last_draw: Instant) -> Instant {
    let deadline = if dirty {
        next_tick.min(last_draw + MIN_FRAME_INTERVAL)
    } else {
        next_tick
    };
    deadline.max(now)
}

/// How long the loop may wait for events before its next pass: whatever it
/// owes the screen, but never longer than [`PACKET_DRAIN_INTERVAL`], because
/// the packet channel has no way to wake it.
pub fn next_wait(now: Instant, dirty: bool, next_tick: Instant, last_draw: Instant) -> Duration {
    next_deadline(now, dirty, next_tick, last_draw)
        .saturating_duration_since(now)
        .min(PACKET_DRAIN_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_draw_when_clean() {
        let t0 = Instant::now();
        assert!(!should_draw(t0 + Duration::from_secs(5), false, t0));
    }

    #[test]
    fn draw_rate_capped_at_min_frame_interval() {
        let t0 = Instant::now();
        assert!(!should_draw(t0 + Duration::from_millis(20), true, t0));
        assert!(should_draw(t0 + Duration::from_millis(33), true, t0));
    }

    #[test]
    fn deadline_is_tick_when_clean_and_frame_when_dirty_sooner() {
        let t0 = Instant::now();
        let tick = t0 + Duration::from_millis(1000);
        assert_eq!(next_deadline(t0, false, tick, t0), tick);
        let d = next_deadline(t0 + Duration::from_millis(20), true, tick, t0);
        assert_eq!(d, t0 + Duration::from_millis(33));
    }

    #[test]
    fn wait_never_outlasts_the_packet_drain_interval() {
        // An idle session's next deadline is its tick, which at the default
        // --refresh is a second away; the readers' channel would overflow long
        // before then, so the wait is capped whatever the screen is doing.
        let t0 = Instant::now();
        let far_tick = t0 + Duration::from_secs(10);
        assert_eq!(next_wait(t0, false, far_tick, t0), PACKET_DRAIN_INTERVAL);
        assert_eq!(
            next_wait(t0, true, far_tick, far_tick),
            PACKET_DRAIN_INTERVAL
        );
    }

    #[test]
    fn wait_is_the_pending_frame_when_that_comes_first() {
        let t0 = Instant::now();
        let tick = t0 + Duration::from_secs(1);
        // Dirty, 20ms since the last frame: 13ms left on the frame cap.
        assert_eq!(
            next_wait(t0 + Duration::from_millis(20), true, tick, t0),
            Duration::from_millis(13)
        );
    }

    #[test]
    fn an_overdue_deadline_waits_not_at_all() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_millis(500);
        assert_eq!(next_wait(now, true, t0, t0), Duration::ZERO);
        assert_eq!(next_wait(now, false, t0, t0), Duration::ZERO);
    }

    #[test]
    fn deadline_is_never_in_the_past() {
        // A pass that overran its frame deadline must not ask for a negative
        // wait; the loop subtracts `now` from this straight away.
        let t0 = Instant::now();
        let now = t0 + Duration::from_millis(500);
        assert_eq!(next_deadline(now, true, t0, t0), now);
        assert_eq!(next_deadline(now, false, t0, t0), now);
    }
}

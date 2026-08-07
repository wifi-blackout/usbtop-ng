use anyhow::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, sync::mpsc::Receiver};

use crate::device::manager::DeviceManager;
use crate::ui::{self, UsbTopApp};
use crate::usbmon::parser::UsbPacket;

/// Floor for `--refresh`, in milliseconds. Below this the poll/redraw loop
/// spends more time spinning than the terminal can usefully repaint, so
/// requests under the floor are clamped rather than honored literally.
const REFRESH_FLOOR_MS: u64 = 100;

/// Clamp a requested `--refresh` value (milliseconds) to the floor.
pub fn effective_refresh_ms(requested: u64) -> u64 {
    requested.max(REFRESH_FLOOR_MS)
}

pub fn run_ui(
    mut app: UsbTopApp,
    mut manager: DeviceManager,
    packets: Receiver<UsbPacket>,
) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &mut app, &mut manager, &packets);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut UsbTopApp,
    manager: &mut DeviceManager,
    packets: &Receiver<UsbPacket>,
) -> Result<()> {
    loop {
        ui::drain_packets(manager, packets, ui::DRAIN_BATCH);

        if app.last_update.elapsed() >= app.refresh_rate {
            // `sync_from` rebuilds the whole snapshot, so the list of devices
            // dropped by this refresh needs no separate handling.
            let _ = manager.refresh();
            app.sync_from(manager);
            app.update_bandwidth_history();
        }

        terminal.draw(|f| ui::draw_ui(f, app))?;

        if app.handle_input()? {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_floor_clamps_low_values() {
        assert_eq!(effective_refresh_ms(0), 100);
        assert_eq!(effective_refresh_ms(99), 100);
        assert_eq!(effective_refresh_ms(100), 100);
        assert_eq!(effective_refresh_ms(1000), 1000);
    }
}

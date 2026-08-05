use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Clear, Dataset, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{
    collections::BTreeMap,
    io,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use crate::device::manager::{DeviceManager, UsbBus};
use crate::device::UsbDevice;
use crate::usbmon::parser::{UsbPacket, UsbSpeed};

pub mod colors;

use colors::*;

/// Group name for buses whose host controller could not be resolved.
const UNKNOWN_CONTROLLER: &str = "unknown";

/// One device as rendered: its physical port chain plus a snapshot of the
/// device itself, taken once per tick from the `DeviceManager`.
pub struct DeviceRow {
    pub port_chain: Option<Vec<u32>>,
    pub device: UsbDevice,
}

/// One bus (root hub) and its devices in port order.
pub struct BusView {
    pub bus_id: u8,
    pub speed: UsbSpeed,
    pub side_label: &'static str,
    pub devices: Vec<DeviceRow>,
}

/// One host controller and its buses in bus-number order.
pub struct ControllerView {
    pub id: String,
    pub buses: Vec<BusView>,
}

pub struct UsbTopApp {
    pub controllers: Vec<ControllerView>,
    pub bandwidth_history: Vec<(f64, f64)>, // (timestamp, total_bandwidth)
    pub selected_device: Option<String>,
    pub show_help: bool,
    pub last_update: Instant,
    pub start_time: Instant,
    pub refresh_rate: Duration,
    pub total_bandwidth: f64,
    pub peak_bandwidth: f64,
}

impl UsbTopApp {
    pub fn new(refresh_rate: Duration) -> Self {
        Self {
            controllers: Vec::new(),
            bandwidth_history: Vec::new(),
            selected_device: None,
            show_help: false,
            last_update: Instant::now(),
            start_time: Instant::now(),
            refresh_rate,
            total_bandwidth: 0.0,
            peak_bandwidth: 0.0,
        }
    }

    /// Rebuild the whole render snapshot from the manager: controller ->
    /// bus -> port-ordered devices, plus totals and selection validity.
    pub fn sync_from(&mut self, manager: &DeviceManager) {
        let mut buses: Vec<&UsbBus> = manager.buses.values().collect();
        buses.sort_by_key(|bus| bus.bus_id);

        let mut grouped: BTreeMap<String, Vec<BusView>> = BTreeMap::new();
        for bus in buses {
            let controller = bus
                .controller
                .clone()
                .unwrap_or_else(|| UNKNOWN_CONTROLLER.to_string());
            grouped.entry(controller).or_default().push(bus_view(bus));
        }

        // Named controllers first (alphabetically), the catch-all group last.
        let unknown = grouped.remove(UNKNOWN_CONTROLLER);
        self.controllers = grouped
            .into_iter()
            .map(|(id, buses)| ControllerView { id, buses })
            .chain(unknown.map(|buses| ControllerView {
                id: UNKNOWN_CONTROLLER.to_string(),
                buses,
            }))
            .collect();

        self.total_bandwidth = self
            .controllers
            .iter()
            .flat_map(|controller| controller.buses.iter())
            .flat_map(|bus| bus.devices.iter())
            .map(|row| row.device.bandwidth_stats.current_bps)
            .sum();
        if self.total_bandwidth > self.peak_bandwidth {
            self.peak_bandwidth = self.total_bandwidth;
        }

        if let Some(selected) = &self.selected_device {
            if !self.device_keys().iter().any(|key| key == selected) {
                self.selected_device = None;
            }
        }
    }

    /// Device keys ("bus:dev") flattened in render order.
    fn device_keys(&self) -> Vec<String> {
        self.controllers
            .iter()
            .flat_map(|controller| controller.buses.iter())
            .flat_map(|bus| {
                bus.devices
                    .iter()
                    .map(move |row| format!("{}:{}", bus.bus_id, row.device.device_id))
            })
            .collect()
    }

    pub fn update_bandwidth_history(&mut self) {
        let now = self.start_time.elapsed().as_secs_f64();
        self.bandwidth_history.push((now, self.total_bandwidth));

        // Keep only last 60 seconds of data
        if self.bandwidth_history.len() > 60 {
            self.bandwidth_history
                .drain(0..self.bandwidth_history.len() - 60);
        }

        self.last_update = Instant::now();
    }

    pub fn handle_input(&mut self) -> Result<bool> {
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
                        KeyCode::Char('h') => self.show_help = !self.show_help,
                        KeyCode::Up => self.select_previous_device(),
                        KeyCode::Down => self.select_next_device(),
                        _ => {}
                    }
                }
            }
        }
        Ok(false)
    }

    fn select_previous_device(&mut self) {
        let device_keys = self.device_keys();
        let Some(last_index) = device_keys.len().checked_sub(1) else {
            return;
        };

        // Nothing selected yet wraps onto the last row, as does the first row.
        let new_index = match self.current_selection_index(&device_keys) {
            Some(0) | None => last_index,
            Some(index) => index - 1,
        };

        self.selected_device = Some(device_keys[new_index].clone());
    }

    fn select_next_device(&mut self) {
        let device_keys = self.device_keys();
        if device_keys.is_empty() {
            return;
        }

        // Nothing selected yet lands on the first row.
        let new_index = match self.current_selection_index(&device_keys) {
            Some(index) => (index + 1) % device_keys.len(),
            None => 0,
        };

        self.selected_device = Some(device_keys[new_index].clone());
    }

    fn current_selection_index(&self, device_keys: &[String]) -> Option<usize> {
        let selected = self.selected_device.as_ref()?;
        device_keys.iter().position(|key| key == selected)
    }
}

/// Snapshot one bus: its devices in physical port order.
fn bus_view(bus: &UsbBus) -> BusView {
    let mut devices: Vec<DeviceRow> = bus
        .devices
        .values()
        .map(|device| DeviceRow {
            port_chain: device.port_chain(),
            device: device.clone(),
        })
        .collect();
    // Unresolved chains sort last; resolved ones compare numerically level by
    // level, so a root hub ([]) leads and 1.4.1 precedes 2.
    devices.sort_by_key(|row| {
        (
            row.port_chain.is_none(),
            row.port_chain.clone().unwrap_or_default(),
            row.device.device_id,
        )
    });

    BusView {
        bus_id: bus.bus_id,
        speed: bus.speed.clone(),
        side_label: side_label(&bus.speed),
        devices,
    }
}

/// Which physical side of a shared xHCI controller a bus lives on: USB2 root
/// hubs top out at 480 Mbps, everything faster is the USB3 side. An unknown
/// bus speed gets no label.
fn side_label(speed: &UsbSpeed) -> &'static str {
    let mbps = speed.to_mbps();
    if mbps <= 0.0 {
        ""
    } else if mbps <= 480.0 {
        "USB2 side"
    } else {
        "USB3 side"
    }
}

pub fn run_ui(
    mut app: UsbTopApp,
    mut manager: DeviceManager,
    packets: Receiver<UsbPacket>,
) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &mut app, &mut manager, &packets);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
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
        // Drain everything the reader threads produced since the last pass.
        // Any `try_recv` error (empty or disconnected) means "nothing to drain",
        // which keeps the UI alive in --force mode with no usbmon readers.
        while let Ok(packet) = packets.try_recv() {
            manager.apply_packet(&packet);
        }

        if app.last_update.elapsed() >= app.refresh_rate {
            // `sync_from` rebuilds the whole snapshot, so the list of devices
            // dropped by this refresh needs no separate handling.
            let _ = manager.refresh();
            app.sync_from(manager);
            app.update_bandwidth_history();
        }

        terminal.draw(|f| draw_ui(f, app))?;

        if app.handle_input()? {
            break;
        }
    }
    Ok(())
}

fn draw_ui(f: &mut Frame, app: &UsbTopApp) {
    if app.show_help {
        draw_help_overlay(f);
        return;
    }

    let size = f.area();

    // Create main layout
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(8), // Bandwidth graph
            Constraint::Min(10),   // Device list
            Constraint::Length(3), // Controls
        ])
        .split(size);

    draw_header(f, chunks[0], app);
    draw_bandwidth_graph(f, chunks[1], app);
    draw_device_list(f, chunks[2], app);
    draw_color_reference(f, chunks[3]);
}

fn draw_header(f: &mut Frame, area: Rect, app: &UsbTopApp) {
    let header_text = vec![
        Line::from(vec![
            Span::styled(
                "usbtop-ng",
                Style::default()
                    .fg(ACCENT_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" - Next-Gen USB Traffic Monitor"),
        ]),
        Line::from(vec![
            Span::raw("Total: "),
            Span::styled(
                format!("{:.1} MB/s", app.total_bandwidth / 1_000_000.0),
                Style::default()
                    .fg(PRIMARY_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | Peak: "),
            Span::styled(
                format!("{:.1} MB/s", app.peak_bandwidth / 1_000_000.0),
                Style::default()
                    .fg(SECONDARY_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | Devices: "),
            Span::styled(
                app.device_keys().len().to_string(),
                Style::default()
                    .fg(SUCCESS_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL).title(" usbtop-ng "));

    f.render_widget(header, area);
}

fn draw_bandwidth_graph(f: &mut Frame, area: Rect, app: &UsbTopApp) {
    if app.bandwidth_history.is_empty() {
        let empty_graph = Paragraph::new("No bandwidth data yet...").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Bandwidth History "),
        );
        f.render_widget(empty_graph, area);
        return;
    }

    // History carries raw bytes/s against session seconds; the chart is drawn in
    // MB/s over a 60-second sliding window, so convert once here.
    let data: Vec<(f64, f64)> = app
        .bandwidth_history
        .iter()
        .map(|(t, bps)| (*t, bps / 1_000_000.0))
        .collect();
    let latest_t = data.last().map(|(t, _)| *t).unwrap_or(0.0);
    let x_min = (latest_t - 60.0).max(0.0);
    let x_max = latest_t.max(60.0);
    let max_mbps = data.iter().map(|(_, m)| *m).fold(0.0, f64::max).max(1.0);

    let datasets = vec![Dataset::default()
        .marker(symbols::Marker::Braille)
        .style(Style::default().fg(PRIMARY_COLOR))
        .data(&data)];

    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Bandwidth History (MB/s) "),
        )
        .x_axis(
            Axis::default()
                .title("Time (s)")
                .style(Style::default().fg(TEXT_COLOR))
                .bounds([x_min, x_max]),
        )
        .y_axis(
            Axis::default()
                .title("MB/s")
                .style(Style::default().fg(TEXT_COLOR))
                .bounds([0.0, max_mbps]),
        );

    f.render_widget(chart, area);
}

/// Column widths for the device rows: Port, Device, Speed, Vendor, Product,
/// Bw↓, Bw↑, Status. Controller and bus headings span the whole line, which a
/// `Table` cannot do, so the list is laid out as pre-padded text lines.
const DEVICE_COLUMNS: [usize; 8] = [8, 8, 10, 14, 18, 10, 10, 12];

fn device_columns(cells: [&str; 8]) -> String {
    cells
        .iter()
        .zip(DEVICE_COLUMNS)
        .map(|(cell, width)| format!("{cell:<width$.width$}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Port column text: "1.4.2" for a hub chain, "-" for a root hub, "?" when the
/// device could not be located in sysfs.
fn port_label(port_chain: Option<&Vec<u32>>) -> String {
    match port_chain.map(Vec::as_slice) {
        None => "?".to_string(),
        Some([]) => "-".to_string(),
        Some(ports) => ports
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("."),
    }
}

fn draw_device_list(f: &mut Frame, area: Rect, app: &UsbTopApp) {
    let heading_style = Style::default()
        .fg(ACCENT_COLOR)
        .add_modifier(Modifier::BOLD);

    let mut lines = vec![Line::styled(
        device_columns([
            "Port", "Device", "Speed", "Vendor", "Product", "Bw↓", "Bw↑", "Status",
        ]),
        heading_style,
    )];

    for controller in &app.controllers {
        lines.push(Line::styled(
            format!("═ {} ═", controller.id),
            heading_style,
        ));

        for bus in &controller.buses {
            lines.push(Line::raw(format!(
                "▶ Bus {:02} ({})  {:.1} Mbps",
                bus.bus_id,
                bus.side_label,
                bus.speed.to_mbps()
            )));

            for row in &bus.devices {
                let device = &row.device;
                let device_key = format!("{}:{}", bus.bus_id, device.device_id);
                let is_selected = app.selected_device.as_ref() == Some(&device_key);

                let status_style = if device.is_disconnected {
                    Style::default().bg(Color::Gray).fg(Color::White)
                } else if is_selected {
                    Style::default().bg(ACCENT_COLOR).fg(Color::Black)
                } else {
                    Style::default().fg(TEXT_COLOR)
                };

                lines.push(Line::styled(
                    device_columns([
                        &port_label(row.port_chain.as_ref()),
                        &format!("{:03}:{:03}", device.bus_id, device.device_id),
                        &format!("{:.1} Mbps", device.speed.to_mbps()),
                        device.vendor.as_deref().unwrap_or("Unknown"),
                        device.product.as_deref().unwrap_or("Unknown"),
                        &format!("{:.1} KB/s", device.bandwidth_stats.rx_bps / 1000.0),
                        &format!("{:.1} KB/s", device.bandwidth_stats.tx_bps / 1000.0),
                        if device.is_disconnected {
                            "Disconnected"
                        } else {
                            "Connected"
                        },
                    ]),
                    status_style,
                ));
            }
        }
    }

    let list = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" USB Devices "),
    );

    f.render_widget(list, area);
}

fn draw_color_reference(f: &mut Frame, area: Rect) {
    let reference_text = vec![Line::from(vec![
        Span::raw("Controls: "),
        Span::styled(
            "↑↓",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Navigate  "),
        Span::styled(
            "h",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Help  "),
        Span::styled(
            "q/Esc",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Quit"),
    ])];

    let reference = Paragraph::new(reference_text)
        .block(Block::default().borders(Borders::ALL).title(" Controls "));

    f.render_widget(reference, area);
}

fn draw_help_overlay(f: &mut Frame) {
    let area = centered_rect(60, 70, f.area());

    let help_text = vec![
        Line::from(vec![Span::styled(
            "usbtop-ng Help",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from("Controls:"),
        Line::from(vec![
            Span::styled("  ↑/↓", Style::default().fg(ACCENT_COLOR)),
            Span::raw("      Navigate device list"),
        ]),
        Line::from(vec![
            Span::styled("  h", Style::default().fg(ACCENT_COLOR)),
            Span::raw("        Toggle this help"),
        ]),
        Line::from(vec![
            Span::styled("  q/Esc", Style::default().fg(ACCENT_COLOR)),
            Span::raw("    Quit application"),
        ]),
        Line::from(""),
        Line::from("Features:"),
        Line::from("  • Real-time USB bandwidth monitoring"),
        Line::from("  • Device disconnect detection"),
        Line::from("  • Bandwidth history graphs"),
        Line::from("  • Multi-platform support (Linux/BSD/macOS)"),
        Line::from(""),
        Line::from("Press 'h' to close this help"),
    ];

    let help = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .wrap(Wrap { trim: true });

    f.render_widget(Clear, area); // Clear background
    f.render_widget(help, area);
}

// Helper function to create centered rectangle
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbmon::parser::parse_usbmon_text_line;

    fn feed(mgr: &mut DeviceManager, lines: &[&str]) {
        for l in lines {
            mgr.apply_packet(&parse_usbmon_text_line(l).unwrap());
        }
        mgr.refresh();
    }

    /// A manager holding hand-built devices with fixed rates, so totals are
    /// exact instead of depending on the sliding-window rate calculation.
    fn manager_with_rates(entries: &[(u8, u8, f64)]) -> (tempfile::TempDir, DeviceManager) {
        let temp = tempfile::tempdir().unwrap();
        let mut mgr = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        for &(bus_id, device_id, current_bps) in entries {
            let mut device = UsbDevice::new(bus_id, device_id);
            device.bandwidth_stats.current_bps = current_bps;
            mgr.get_or_create_bus(bus_id)
                .devices
                .insert(device_id, device);
        }
        (temp, mgr)
    }

    #[cfg(target_os = "linux")]
    fn topology_fixture() -> (tempfile::TempDir, DeviceManager) {
        topology_fixture_named("0000:00:14.0")
    }

    /// Fake sysfs: a PCI controller directory holding the real root hubs, with
    /// the flat `devices/` directory symlinking to them, so `canonicalize`
    /// resolves a root hub back to its controller exactly like real sysfs.
    #[cfg(target_os = "linux")]
    fn topology_fixture_named(controller: &str) -> (tempfile::TempDir, DeviceManager) {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("devices");
        let ctrl = temp.path().join(controller);
        std::fs::create_dir_all(&base).unwrap();
        for (hub, bus, dev, speed) in [("usb3", 3u8, 1u8, "480"), ("usb4", 4, 1, "5000")] {
            let real = ctrl.join(hub);
            std::fs::create_dir_all(&real).unwrap();
            std::fs::write(real.join("busnum"), format!("{bus}\n")).unwrap();
            std::fs::write(real.join("devnum"), format!("{dev}\n")).unwrap();
            std::fs::write(real.join("speed"), format!("{speed}\n")).unwrap();
            symlink(&real, base.join(hub)).unwrap();
        }
        let dev = |name: &str, bus: u8, devnum: u8, speed: &str| {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("busnum"), format!("{bus}\n")).unwrap();
            std::fs::write(d.join("devnum"), format!("{devnum}\n")).unwrap();
            std::fs::write(d.join("speed"), format!("{speed}\n")).unwrap();
        };
        dev("3-2", 3, 3, "480"); // root port 2
        dev("3-1.4.1", 3, 6, "480"); // behind hub at port 1.4
        dev("4-1.4.4", 4, 2, "5000");
        let mgr = DeviceManager::with_sysfs_base(base);
        (temp, mgr)
    }

    fn rows(app: &UsbTopApp) -> Vec<&DeviceRow> {
        app.controllers
            .iter()
            .flat_map(|c| c.buses.iter())
            .flat_map(|b| b.devices.iter())
            .collect()
    }

    #[test]
    fn packets_flow_from_parser_through_manager_into_app_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        feed(
            &mut manager,
            &[
                "ffff0000eeee0001 100 C Bi:1:003:1 0 4096 <",
                "ffff0000eeee0002 200 C Bi:1:003:1 0 4096 <",
                "ffff0000eeee0003 300 C Bo:1:003:2 0 1024 >",
            ],
        );

        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&manager);

        assert_eq!(app.device_keys(), vec!["1:3".to_string()]);
        let row = rows(&app)
            .into_iter()
            .find(|r| r.device.bus_id == 1 && r.device.device_id == 3)
            .expect("device visible in app state");
        assert_eq!(row.device.bandwidth_stats.total_rx_bytes, 8192);
        assert_eq!(row.device.bandwidth_stats.total_tx_bytes, 1024);
        assert!(app.total_bandwidth > 0.0);
        assert_eq!(app.total_bandwidth, row.device.bandwidth_stats.current_bps);
    }

    #[test]
    fn sync_from_recomputes_totals_instead_of_drifting() {
        let (_t, mut mgr) = manager_with_rates(&[(1, 3, 1000.0)]);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        assert_eq!(app.total_bandwidth, 1000.0);
        assert_eq!(app.peak_bandwidth, 1000.0);

        mgr.get_or_create_bus(1)
            .devices
            .get_mut(&3)
            .unwrap()
            .bandwidth_stats
            .current_bps = 400.0;
        app.sync_from(&mgr);
        assert_eq!(app.total_bandwidth, 400.0);
        assert_eq!(app.peak_bandwidth, 1000.0, "peak retains the max");

        app.selected_device = Some("1:3".to_string());
        mgr.get_or_create_bus(1).remove_device(3);
        app.sync_from(&mgr);
        assert_eq!(app.total_bandwidth, 0.0);
        assert_eq!(
            app.selected_device, None,
            "selection drops when the device vanishes"
        );
    }

    #[test]
    fn totals_do_not_accumulate_float_error_across_syncs() {
        let (_t, mut mgr) = manager_with_rates(&[(1, 3, 0.1), (1, 4, 0.2)]);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        mgr.get_or_create_bus(1).remove_device(4);
        app.sync_from(&mgr);
        assert_eq!(
            app.total_bandwidth, 0.1,
            "total must be recomputed from the snapshot, not patched incrementally"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sync_from_groups_by_controller_and_orders_by_port() {
        let (_t, mut mgr) = topology_fixture();
        feed(
            &mut mgr,
            &[
                "f1 100 C Bi:3:006:1 0 64 <", // 3-1.4.1
                "f2 200 C Bi:3:003:1 0 64 <", // 3-2
                "f3 300 C Bi:4:002:1 0 64 <", // 4-1.4.4
                "f4 400 C Bi:3:009:1 0 64 <", // unresolved devnum 9
            ],
        );
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        assert_eq!(app.controllers.len(), 1);
        let ctrl = &app.controllers[0];
        assert_eq!(ctrl.id, "0000:00:14.0");
        assert_eq!(ctrl.buses.len(), 2);
        assert_eq!(ctrl.buses[0].bus_id, 3);
        assert_eq!(ctrl.buses[0].side_label, "USB2 side");
        assert_eq!(ctrl.buses[1].side_label, "USB3 side");
        let order: Vec<(Option<Vec<u32>>, u8)> = ctrl.buses[0]
            .devices
            .iter()
            .map(|r| (r.port_chain.clone(), r.device.device_id))
            .collect();
        // port 1.4.1 before port 2 (numeric per level), unresolved (None) last
        assert_eq!(
            order,
            vec![(Some(vec![1, 4, 1]), 6), (Some(vec![2]), 3), (None, 9),]
        );
        assert!(app.total_bandwidth > 0.0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_hub_sorts_first_and_unknown_controller_sorts_last() {
        // Controller id deliberately sorts after "unknown" alphabetically, so
        // only an explicit move-to-end puts the unknown group last.
        let (_t, mut mgr) = topology_fixture_named("zzzz:00:14.0");
        feed(
            &mut mgr,
            &[
                "f1 100 C Bi:3:003:1 0 64 <", // 3-2
                "f2 200 C Bi:3:001:1 0 64 <", // root hub usb3, empty port chain
                "f3 300 C Bi:9:005:1 0 64 <", // bus 9 has no root hub -> unknown controller
            ],
        );
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        let ids: Vec<&str> = app.controllers.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["zzzz:00:14.0", "unknown"]);
        let chains: Vec<Option<Vec<u32>>> = app.controllers[0].buses[0]
            .devices
            .iter()
            .map(|r| r.port_chain.clone())
            .collect();
        assert_eq!(
            chains,
            vec![Some(vec![]), Some(vec![2])],
            "root hub sorts before port 2"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selection_walks_device_rows_across_groups() {
        let (_t, mut mgr) = topology_fixture();
        feed(
            &mut mgr,
            &["f1 100 C Bi:3:006:1 0 64 <", "f2 300 C Bi:4:002:1 0 64 <"],
        );
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        assert_eq!(
            app.device_keys(),
            vec!["3:6".to_string(), "4:2".to_string()]
        );
        app.select_next_device();
        assert_eq!(app.selected_device.as_deref(), Some("3:6"));
        app.select_next_device();
        assert_eq!(app.selected_device.as_deref(), Some("4:2"));
        app.select_next_device(); // wraps
        assert_eq!(app.selected_device.as_deref(), Some("3:6"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selection_walks_backwards_and_wraps() {
        let (_t, mut mgr) = topology_fixture();
        feed(
            &mut mgr,
            &["f1 100 C Bi:3:006:1 0 64 <", "f2 300 C Bi:4:002:1 0 64 <"],
        );
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.select_previous_device();
        assert_eq!(app.selected_device.as_deref(), Some("4:2"));
        app.select_previous_device();
        assert_eq!(app.selected_device.as_deref(), Some("3:6"));
        app.select_previous_device(); // wraps
        assert_eq!(app.selected_device.as_deref(), Some("4:2"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn device_list_renders_headings_above_port_ordered_rows() {
        let (_t, mut mgr) = topology_fixture();
        feed(
            &mut mgr,
            &[
                "f1 100 C Bi:3:006:1 0 64 <", // 3-1.4.1
                "f2 200 C Bi:3:003:1 0 64 <", // 3-2
                "f3 300 C Bi:3:001:1 0 64 <", // root hub
                "f4 400 C Bi:3:009:1 0 64 <", // unresolved
            ],
        );
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 10)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_device_list(f, area, &app);
            })
            .unwrap();
        let screen = terminal.backend().to_string();

        assert!(screen.contains("═ 0000:00:14.0 ═"), "{screen}");
        assert!(
            screen.contains("▶ Bus 03 (USB2 side)  480.0 Mbps"),
            "{screen}"
        );
        // First column of every device row, top to bottom.
        let ports: Vec<&str> = screen
            .lines()
            .filter_map(|line| line.trim_matches('"').strip_prefix('│'))
            .filter_map(|line| line.split_whitespace().next())
            .filter(|cell| ["-", "1.4.1", "2", "?"].contains(cell))
            .collect();
        assert_eq!(ports, vec!["-", "1.4.1", "2", "?"], "{screen}");
    }
}

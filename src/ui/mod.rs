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
    text::{Line, Span, Text},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, Gauge, List, ListItem, Paragraph, Row, Table,
        Wrap,
    },
    Frame, Terminal,
};
use std::{
    collections::HashMap,
    io,
    time::{Duration, Instant},
};

use crate::device::UsbDevice;
use crate::stats::BandwidthStats;
use crate::usbmon::parser::UsbSpeed;

pub mod colors;
pub mod widgets;

use colors::*;
use widgets::*;

pub struct UsbTopApp {
    pub devices: HashMap<String, UsbDevice>,
    pub bandwidth_history: Vec<(f64, f64)>, // (timestamp, total_bandwidth)
    pub selected_device: Option<String>,
    pub show_help: bool,
    pub last_update: Instant,
    pub refresh_rate: Duration,
    pub total_bandwidth: f64,
    pub peak_bandwidth: f64,
}

impl UsbTopApp {
    pub fn new(refresh_rate: Duration) -> Self {
        Self {
            devices: HashMap::new(),
            bandwidth_history: Vec::new(),
            selected_device: None,
            show_help: false,
            last_update: Instant::now(),
            refresh_rate,
            total_bandwidth: 0.0,
            peak_bandwidth: 0.0,
        }
    }

    pub fn update_device(&mut self, device: UsbDevice) {
        let device_key = format!("{}:{}", device.bus_id, device.device_id);

        // Update total bandwidth
        if let Some(existing_device) = self.devices.get(&device_key) {
            self.total_bandwidth -= existing_device.bandwidth_stats.current_bps;
        }

        self.total_bandwidth += device.bandwidth_stats.current_bps;
        if self.total_bandwidth > self.peak_bandwidth {
            self.peak_bandwidth = self.total_bandwidth;
        }

        self.devices.insert(device_key, device);
    }

    pub fn remove_device(&mut self, bus_id: u8, device_id: u8) {
        let device_key = format!("{}:{}", bus_id, device_id);
        if let Some(device) = self.devices.remove(&device_key) {
            self.total_bandwidth -= device.bandwidth_stats.current_bps;
        }
    }

    pub fn update_bandwidth_history(&mut self) {
        let now = self.last_update.elapsed().as_secs_f64();
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
        let device_keys: Vec<String> = self.devices.keys().cloned().collect();
        if device_keys.is_empty() {
            return;
        }

        let current_index = self
            .selected_device
            .as_ref()
            .and_then(|selected| device_keys.iter().position(|k| k == selected))
            .unwrap_or(0);

        let new_index = if current_index == 0 {
            device_keys.len() - 1
        } else {
            current_index - 1
        };

        self.selected_device = Some(device_keys[new_index].clone());
    }

    fn select_next_device(&mut self) {
        let device_keys: Vec<String> = self.devices.keys().cloned().collect();
        if device_keys.is_empty() {
            return;
        }

        let current_index = self
            .selected_device
            .as_ref()
            .and_then(|selected| device_keys.iter().position(|k| k == selected))
            .unwrap_or(0);

        let new_index = (current_index + 1) % device_keys.len();
        self.selected_device = Some(device_keys[new_index].clone());
    }
}

pub fn run_ui(mut app: UsbTopApp) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &mut app);

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
) -> Result<()> {
    loop {
        terminal.draw(|f| draw_ui(f, app))?;

        if app.handle_input()? {
            break;
        }

        // Update bandwidth history periodically
        if app.last_update.elapsed() >= app.refresh_rate {
            app.update_bandwidth_history();
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
            Constraint::Length(6), // Color reference
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
                app.devices.len().to_string(),
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

    let max_bandwidth = app
        .bandwidth_history
        .iter()
        .map(|(_, bw)| *bw)
        .fold(0.0, f64::max)
        .max(1.0); // Minimum scale

    let data: Vec<(f64, f64)> = app.bandwidth_history.clone();

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
                .bounds([0.0, 60.0]),
        )
        .y_axis(
            Axis::default()
                .title("MB/s")
                .style(Style::default().fg(TEXT_COLOR))
                .bounds([0.0, max_bandwidth / 1_000_000.0]),
        );

    f.render_widget(chart, area);
}

fn draw_device_list(f: &mut Frame, area: Rect, app: &UsbTopApp) {
    let header = Row::new(vec![
        "Device",
        "Speed",
        "Vendor",
        "Product",
        "Bandwidth ↓",
        "Bandwidth ↑",
        "Status",
    ])
    .style(
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);

    let mut devices: Vec<_> = app.devices.values().collect();
    devices.sort_by(|a, b| {
        b.bandwidth_stats
            .current_bps
            .partial_cmp(&a.bandwidth_stats.current_bps)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let rows: Vec<Row> = devices
        .iter()
        .enumerate()
        .map(|(i, device)| {
            let device_key = format!("{}:{}", device.bus_id, device.device_id);
            let is_selected = app.selected_device.as_ref() == Some(&device_key);

            let speed_color = Color::Rgb(
                device.speed.color_code().0,
                device.speed.color_code().1,
                device.speed.color_code().2,
            );

            let status_style = if device.is_disconnected {
                Style::default().bg(Color::Gray).fg(Color::White)
            } else if is_selected {
                Style::default().bg(ACCENT_COLOR).fg(Color::Black)
            } else {
                Style::default().fg(TEXT_COLOR)
            };

            Row::new(vec![
                format!("{:03}:{:03}", device.bus_id, device.device_id),
                format!("{:.1} Mbps", device.speed.to_mbps()),
                device
                    .vendor
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string()),
                device
                    .product
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string()),
                format!("{:.1} KB/s", device.bandwidth_stats.rx_bps / 1000.0),
                format!("{:.1} KB/s", device.bandwidth_stats.tx_bps / 1000.0),
                if device.is_disconnected {
                    "Disconnected"
                } else {
                    "Connected"
                }
                .to_string(),
            ])
            .style(status_style)
            .height(1)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),  // Device
            Constraint::Length(12), // Speed
            Constraint::Length(15), // Vendor
            Constraint::Length(20), // Product
            Constraint::Length(12), // RX Bandwidth
            Constraint::Length(12), // TX Bandwidth
            Constraint::Length(12), // Status
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" USB Devices "),
    )
    .widths([
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(15),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(12),
    ]);

    f.render_widget(table, area);
}

fn draw_color_reference(f: &mut Frame, area: Rect) {
    let reference_text = vec![
        Line::from(vec![
            Span::styled("●", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::raw(" Low Speed (1.5 Mbps)  "),
            Span::styled("●", Style::default().fg(Color::Rgb(255, 165, 0))),
            Span::raw(" Full Speed (12 Mbps)  "),
            Span::styled("●", Style::default().fg(Color::Rgb(255, 255, 0))),
            Span::raw(" High Speed (480 Mbps)"),
        ]),
        Line::from(vec![
            Span::styled("●", Style::default().fg(Color::Rgb(0, 255, 0))),
            Span::raw(" SuperSpeed (5 Gbps)  "),
            Span::styled("●", Style::default().fg(Color::Rgb(0, 255, 255))),
            Span::raw(" SuperSpeed+ (10+ Gbps)  "),
            Span::styled("●", Style::default().fg(Color::Gray)),
            Span::raw(" Unknown/Disconnected"),
        ]),
        Line::from(vec![
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
        ]),
    ];

    let reference = Paragraph::new(reference_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Legend & Controls "),
    );

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
        Line::from("  • Color-coded USB speeds"),
        Line::from("  • Device disconnect detection"),
        Line::from("  • Bandwidth history graphs"),
        Line::from("  • Multi-platform support (Linux/BSD/macOS)"),
        Line::from(""),
        Line::from("Speed Colors:"),
        Line::from(vec![
            Span::styled("  Red", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::raw("     Low Speed (1.5 Mbps)"),
        ]),
        Line::from(vec![
            Span::styled("  Orange", Style::default().fg(Color::Rgb(255, 165, 0))),
            Span::raw("  Full Speed (12 Mbps)"),
        ]),
        Line::from(vec![
            Span::styled("  Yellow", Style::default().fg(Color::Rgb(255, 255, 0))),
            Span::raw("  High Speed (480 Mbps)"),
        ]),
        Line::from(vec![
            Span::styled("  Green", Style::default().fg(Color::Rgb(0, 255, 0))),
            Span::raw("   SuperSpeed (5 Gbps)"),
        ]),
        Line::from(vec![
            Span::styled("  Cyan", Style::default().fg(Color::Rgb(0, 255, 255))),
            Span::raw("    SuperSpeed+ (10+ Gbps)"),
        ]),
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

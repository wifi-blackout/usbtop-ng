use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Clear, Dataset, Paragraph, Wrap},
    Frame,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Receiver,
        Arc,
    },
    time::{Duration, Instant},
};

use crate::device::manager::{DeviceManager, UsbBus};
use crate::device::UsbDevice;
use crate::usbmon::parser::{UsbPacket, UsbSpeed};

pub mod colors;

use colors::*;

/// Group name for buses whose host controller could not be resolved.
const UNKNOWN_CONTROLLER: &str = "unknown";

/// How much of `bandwidth_history` is kept and plotted, in seconds. Eviction
/// and the chart's x-axis read the same number so the window the chart claims
/// is the window the data actually covers.
const HISTORY_WINDOW_SECS: f64 = 60.0;

/// Most packets applied in one pass of the event loop. The channel's bound is
/// what caps memory; this caps how long a single frame can spend catching up,
/// so a burst can never stall input handling or the redraw. Anything left over
/// stays queued, and a pass that fills its batch tells the loop to come
/// straight back for the rest instead of sleeping until the next tick.
pub(crate) const DRAIN_BATCH: usize = 8_192;

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
    /// Aggregate %busy across the bus's devices; `None` when the bus speed
    /// is unknown (see `UsbBus::busy_percentage`).
    pub busy_percentage: Option<f64>,
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
    pub start_time: Instant,
    /// How often the loop takes a fresh snapshot of the devices; the schedule
    /// itself lives in the loop (see `tui::run_app`), not here.
    pub refresh_rate: Duration,
    pub total_bandwidth: f64,
    pub peak_bandwidth: f64,
    /// Vertical scroll offset for the device list, in lines. Follows the
    /// selected device's row so `select_next_device`/`select_previous_device`
    /// can't walk the selection off-screen; see `follow_selection_in_list`.
    pub list_scroll: u16,
    /// Shared count of packets the reader threads had to discard because the
    /// channel was full (see `usbmon::monitor`). `None` when no monitor is
    /// attached; the header surfaces it once it goes above zero, so a lossy
    /// session never reads like a complete one.
    pub dropped_counter: Option<Arc<AtomicU64>>,
    /// Shared count of frames the output stage had to discard because the
    /// terminal stopped reading (see `tui::output`). `None` outside a TUI
    /// session. Same bargain as [`Self::dropped_counter`]: a session that is
    /// showing less than it measured has to say so.
    pub shed_counter: Option<Arc<AtomicU64>>,
}

impl UsbTopApp {
    pub fn new(refresh_rate: Duration) -> Self {
        Self {
            controllers: Vec::new(),
            bandwidth_history: Vec::new(),
            selected_device: None,
            show_help: false,
            start_time: Instant::now(),
            refresh_rate,
            total_bandwidth: 0.0,
            peak_bandwidth: 0.0,
            list_scroll: 0,
            dropped_counter: None,
            shed_counter: None,
        }
    }

    /// Attach the monitor's dropped-packet counter (see
    /// [`Self::dropped_counter`]).
    pub fn with_dropped_counter(mut self, dropped: Arc<AtomicU64>) -> Self {
        self.dropped_counter = Some(dropped);
        self
    }

    /// Packets discarded so far, or 0 when no counter is attached.
    fn dropped_packets(&self) -> u64 {
        self.dropped_counter
            .as_ref()
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
    }

    /// Frames discarded so far, or 0 when no counter is attached.
    fn shed_frames(&self) -> u64 {
        self.shed_counter
            .as_ref()
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
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

        // Keep the last 60 seconds of data, by age rather than by sample
        // count: the tick rate is the user's `--refresh` choice, so a fixed
        // count would mean 15s at 250ms and 120s at 2000ms while the chart
        // keeps claiming a 60-second window. Samples are appended in time
        // order, so the expired ones are exactly the leading run.
        let cutoff = now - HISTORY_WINDOW_SECS;
        let expired = self.bandwidth_history.partition_point(|(t, _)| *t < cutoff);
        self.bandwidth_history.drain(0..expired);
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

    /// Keep `list_scroll` following the selected device's line: scroll up if
    /// the selection is above the visible window, down if it's below, and
    /// leave it untouched otherwise (so it doesn't chase when nothing is
    /// selected). Always clamped to the current content length afterwards,
    /// so a shrunk list or a stale offset can't scroll past its end.
    ///
    /// `total_lines`/`selected_line` come from `device_list_lines_with_selection`
    /// (headings count toward both); `visible_height` is the render area's
    /// height minus its block's borders, computed by the caller since only it
    /// knows the area.
    fn follow_selection_in_list(
        &mut self,
        total_lines: usize,
        selected_line: Option<usize>,
        visible_height: u16,
    ) {
        if let Some(index) = selected_line {
            let index = index as u16;
            if index < self.list_scroll {
                self.list_scroll = index;
            } else if visible_height > 0 && index >= self.list_scroll.saturating_add(visible_height)
            {
                self.list_scroll = index.saturating_sub(visible_height.saturating_sub(1));
            }
        }

        let max_scroll = (total_lines as u16).saturating_sub(visible_height);
        self.list_scroll = self.list_scroll.min(max_scroll);
    }
}

/// What a key press leaves the event loop owing the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// The session is over.
    Quit,
    /// App state changed; the screen is stale until the next frame.
    Redraw,
    /// The screen itself is suspect (Ctrl-L): wipe it, then repaint.
    ClearAndRedraw,
    /// The key means nothing here; the screen is still correct.
    None,
}

/// Apply one key event to `app` and report what the loop owes the screen.
///
/// Kept apart from the loop so every binding is testable without a terminal.
pub(crate) fn apply_key(app: &mut UsbTopApp, key: KeyEvent) -> KeyOutcome {
    // Terminals that report repeat and release (kitty protocol, Windows) send
    // several events per physical press; only the press acts.
    if key.kind != KeyEventKind::Press {
        return KeyOutcome::None;
    }

    match key.code {
        // Checked before the bare letters so Ctrl-L stays a redraw request.
        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyOutcome::ClearAndRedraw
        }
        // Raw mode turns off ISIG, so the terminal never turns ^C into a
        // SIGINT. It arrives as this key event, and it still means quit.
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyOutcome::Quit,
        KeyCode::Char('q') | KeyCode::Esc => KeyOutcome::Quit,
        KeyCode::Char('h') => {
            app.show_help = !app.show_help;
            KeyOutcome::Redraw
        }
        KeyCode::Up => {
            app.select_previous_device();
            KeyOutcome::Redraw
        }
        KeyCode::Down => {
            app.select_next_device();
            KeyOutcome::Redraw
        }
        _ => KeyOutcome::None,
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
        busy_percentage: bus.busy_percentage(),
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

/// Apply up to `batch` queued packets to the manager and report how many were
/// applied. Any `try_recv` error (empty or disconnected) means "nothing to
/// drain", which keeps the UI alive in --force mode with no usbmon readers.
pub(crate) fn drain_packets(
    manager: &mut DeviceManager,
    packets: &Receiver<UsbPacket>,
    batch: usize,
) -> usize {
    let mut applied = 0;
    while applied < batch {
        match packets.try_recv() {
            Ok(packet) => {
                manager.apply_packet(&packet);
                applied += 1;
            }
            Err(_) => break,
        }
    }
    applied
}

pub(crate) fn draw_ui(f: &mut Frame, app: &mut UsbTopApp) {
    if app.show_help {
        draw_help_overlay(f);
        return;
    }

    let size = f.area();

    // Create main layout
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            // Four, not three: the header is a title line and a stats line
            // inside a border, and a row short of that clips the stats line —
            // which is where `dropped:` and `shed:` are reported.
            Constraint::Length(4), // Header
            Constraint::Length(8), // Bandwidth graph
            Constraint::Min(10),   // Device list
            Constraint::Length(4), // Controls
        ])
        .split(size);

    let chart_chunks = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    draw_header(f, chunks[0], app);
    draw_bandwidth_graph(f, chart_chunks[0], app);
    draw_device_chart(f, chart_chunks[1], app);
    draw_device_list(f, chunks[2], app);
    draw_color_reference(f, chunks[3]);
}

fn draw_header(f: &mut Frame, area: Rect, app: &UsbTopApp) {
    let mut stats_line = vec![
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
    ];

    // Only shown once something was actually lost: the figures above are then
    // an undercount, and silence about that would be the real bug.
    let dropped = app.dropped_packets();
    if dropped > 0 {
        stats_line.push(Span::raw(" | dropped: "));
        stats_line.push(Span::styled(
            dropped.to_string(),
            Style::default()
                .fg(SECONDARY_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Same bargain one layer out: these numbers were measured, but the screen
    // showing them is behind by this many frames.
    let shed = app.shed_frames();
    if shed > 0 {
        stats_line.push(Span::raw(" | shed: "));
        stats_line.push(Span::styled(
            shed.to_string(),
            Style::default()
                .fg(SECONDARY_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
    }

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
        Line::from(stats_line),
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
    let x_min = (latest_t - HISTORY_WINDOW_SECS).max(0.0);
    let x_max = latest_t.max(HISTORY_WINDOW_SECS);
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

/// Find the currently selected device's row (plus its bus id, since
/// `DeviceRow` doesn't carry one) by matching `app.selected_device` against
/// the same "bus:dev" key used to build it in `device_keys`.
fn find_selected_device(app: &UsbTopApp) -> Option<(u8, &DeviceRow)> {
    let selected = app.selected_device.as_ref()?;
    app.controllers
        .iter()
        .flat_map(|controller| controller.buses.iter())
        .find_map(|bus| {
            bus.devices
                .iter()
                .find(|row| format!("{}:{}", bus.bus_id, row.device.device_id) == *selected)
                .map(|row| (bus.bus_id, row))
        })
}

/// Right-hand chart of the strip: the selected device's rx/tx rate history
/// over the last 60 seconds, or a placeholder when nothing is selected (or
/// the selection vanished, e.g. the device was unplugged).
fn draw_device_chart(f: &mut Frame, area: Rect, app: &UsbTopApp) {
    let Some((bus_id, row)) = find_selected_device(app) else {
        let placeholder = Paragraph::new("Select a device with ↑/↓").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Device rx/tx "),
        );
        f.render_widget(placeholder, area);
        return;
    };

    let now = Instant::now();
    // rate_history carries raw bytes/s against sample instants; plot MB/s
    // against seconds-ago (0 = now, -60 = a minute back), matching the
    // aggregate chart's units but anchored to "now" instead of session time.
    let to_series = |pick: fn(&(Instant, f64, f64)) -> f64| -> Vec<(f64, f64)> {
        row.device
            .bandwidth_stats
            .rate_history
            .iter()
            .map(|sample| (-(now - sample.0).as_secs_f64(), pick(sample) / 1_000_000.0))
            .collect()
    };
    let rx_data = to_series(|(_, rx, _)| *rx);
    let tx_data = to_series(|(_, _, tx)| *tx);

    let max_mbps = rx_data
        .iter()
        .chain(tx_data.iter())
        .map(|(_, mbps)| *mbps)
        .fold(0.0, f64::max)
        .max(0.001);

    let datasets = vec![
        Dataset::default()
            .name("rx")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(PRIMARY_COLOR))
            .data(&rx_data),
        Dataset::default()
            .name("tx")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(SECONDARY_COLOR))
            .data(&tx_data),
    ];

    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {}:{} rx/tx ", bus_id, row.device.device_id)),
        )
        .x_axis(
            Axis::default()
                .title("Time (s)")
                .style(Style::default().fg(TEXT_COLOR))
                .bounds([-60.0, 0.0]),
        )
        .y_axis(
            Axis::default()
                .title("MB/s")
                .style(Style::default().fg(TEXT_COLOR))
                .bounds([0.0, max_mbps]),
        );

    f.render_widget(chart, area);
}

/// Column widths, in terminal cells, for the device rows: Port, Device, Speed,
/// Vendor, Product, Bw↓, Bw↑, %busy, !. Controller and bus headings span the
/// whole line, which a `Table` cannot do, so the list is laid out as lines of
/// pre-padded per-cell spans instead.
const DEVICE_COLUMNS: [usize; 9] = [8, 8, 10, 14, 18, 10, 10, 7, 3];

/// One padded cell per column, separated by single-space spans. Columns are
/// measured in terminal cells, not chars: a CJK vendor string is twice as wide
/// as its char count, and padding by chars would shove every later column
/// rightwards on that row only.
fn device_columns(cells: [&str; 9]) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(DEVICE_COLUMNS.len() * 2 - 1);
    for (index, (cell, width)) in cells.iter().zip(DEVICE_COLUMNS).enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::raw(fit_to_display_width(cell, width)));
    }
    spans
}

/// Clip `text` to at most `width` terminal cells, then pad it to exactly that
/// many. A wide character that would straddle the edge is dropped in favour of
/// a padding space, so the column always ends where it should.
fn fit_to_display_width(text: &str, width: usize) -> String {
    let mut fitted = String::with_capacity(width);
    let mut used = 0;
    let mut buffer = [0u8; 4];
    for character in text.chars() {
        let cells = Span::raw(&*character.encode_utf8(&mut buffer)).width();
        if used + cells > width {
            break;
        }
        fitted.push(character);
        used += cells;
    }
    fitted.push_str(&" ".repeat(width - used));
    fitted
}

/// Speed is the 3rd column (index 2) of `DEVICE_COLUMNS`; the `!` indicator is
/// the last (index 8). `device_columns` emits one separator span before every
/// column after the first, so a cell at column index `i` lands at span index
/// `2 * i` in its output.
const SPEED_SPAN_INDEX: usize = 2 * 2;
const INDICATOR_SPAN_INDEX: usize = 2 * 8;

/// Style that paints text in a speed's reference color (see
/// `UsbSpeed::color_code`), used for both the bus header's Mbps figure and
/// the device row's Speed cell.
fn speed_style(speed: &UsbSpeed) -> Style {
    let (r, g, b) = speed.color_code();
    Style::default().fg(Color::Rgb(r, g, b))
}

/// %busy cell text for a device row: a numeric percentage normally, or a
/// width-matched "--" when the device's speed is unknown and therefore has
/// no meaningful bandwidth denominator. Mirrors `BusView::busy_percentage`'s
/// `None` case (see `UsbBus::busy_percentage`) — without this, an
/// Unknown-speed device with real traffic renders a misleading "0.0" instead
/// of the bus row's honest "--".
fn busy_cell(device: &UsbDevice) -> String {
    if device.speed == UsbSpeed::Unknown {
        format!("{:>5}", "--")
    } else {
        format!("{:5.1}", device.get_busy_percentage())
    }
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

/// Rendered lines minus the block's top and bottom border rows.
fn inner_height(area: Rect) -> u16 {
    area.height.saturating_sub(2)
}

fn draw_device_list(f: &mut Frame, area: Rect, app: &mut UsbTopApp) {
    let (lines, selected_line) = device_list_lines_with_selection(app);
    app.follow_selection_in_list(lines.len(), selected_line, inner_height(area));

    let list = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" USB Devices "),
        )
        .scroll((app.list_scroll, 0));

    f.render_widget(list, area);
}

/// The device list as rendered: a column header, then per controller a
/// heading, per bus a header, and one line per device in port order. Also
/// returns the line index of the selected device's row (headings count
/// toward it), so the caller can keep it inside the visible scroll window.
fn device_list_lines_with_selection(app: &UsbTopApp) -> (Vec<Line<'static>>, Option<usize>) {
    let heading_style = Style::default()
        .fg(ACCENT_COLOR)
        .add_modifier(Modifier::BOLD);

    let mut lines = vec![Line::from(device_columns([
        "Port", "Device", "Speed", "Vendor", "Product", "Bw↓", "Bw↑", "%busy", "!",
    ]))
    .style(heading_style)];
    let mut selected_line = None;

    for controller in &app.controllers {
        lines.push(Line::styled(
            format!("═ {} ═", controller.id),
            heading_style,
        ));

        for bus in &controller.buses {
            let busy_suffix = match bus.busy_percentage {
                Some(pct) => format!(" · {pct:.1}% busy"),
                None => " · -- busy".to_string(),
            };
            lines.push(Line::from(vec![
                Span::raw(format!("▶ Bus {:02} ({})  ", bus.bus_id, bus.side_label)),
                Span::styled(
                    format!("{:.1} Mbps", bus.speed.to_mbps()),
                    speed_style(&bus.speed),
                ),
                Span::raw(busy_suffix),
            ]));

            for row in &bus.devices {
                let device = &row.device;
                let device_key = format!("{}:{}", bus.bus_id, device.device_id);
                let is_selected = app.selected_device.as_ref() == Some(&device_key);
                if is_selected {
                    selected_line = Some(lines.len());
                }
                let indicator = device.get_speed_indicator(&bus.speed);

                let status_style = if device.is_disconnected {
                    Style::default().bg(Color::Gray).fg(Color::White)
                } else if is_selected {
                    Style::default().bg(ACCENT_COLOR).fg(Color::Black)
                } else {
                    Style::default().fg(TEXT_COLOR)
                };

                let mut spans = device_columns([
                    &port_label(row.port_chain.as_ref()),
                    &format!("{:03}:{:03}", device.bus_id, device.device_id),
                    &format!("{:.1} Mbps", device.speed.to_mbps()),
                    device.vendor.as_deref().unwrap_or("Unknown"),
                    device.product.as_deref().unwrap_or("Unknown"),
                    &format!("{:.1} KB/s", device.bandwidth_stats.rx_bps / 1000.0),
                    &format!("{:.1} KB/s", device.bandwidth_stats.tx_bps / 1000.0),
                    &busy_cell(device),
                    indicator.get_symbol(),
                ]);

                // Selected/disconnected rows stay uniformly styled for
                // readability; only a plain, connected row gets its Speed
                // and indicator cells tinted by their reference colors.
                if !is_selected && !device.is_disconnected {
                    spans[SPEED_SPAN_INDEX] = spans[SPEED_SPAN_INDEX]
                        .clone()
                        .style(speed_style(&device.speed));
                    let (r, g, b) = indicator.get_color();
                    spans[INDICATOR_SPAN_INDEX] = spans[INDICATOR_SPAN_INDEX]
                        .clone()
                        .style(Style::default().fg(Color::Rgb(r, g, b)));
                }

                lines.push(Line::from(spans).style(status_style));
            }
        }
    }

    (lines, selected_line)
}

fn draw_color_reference(f: &mut Frame, area: Rect) {
    let reference_text = vec![
        Line::from(vec![
            Span::raw("Legend: "),
            Span::styled("●", speed_style(&UsbSpeed::Low)),
            Span::raw(" 1.5M  "),
            Span::styled("●", speed_style(&UsbSpeed::Full)),
            Span::raw(" 12M  "),
            Span::styled("●", speed_style(&UsbSpeed::High)),
            Span::raw(" 480M  "),
            Span::styled("●", speed_style(&UsbSpeed::SuperSpeed)),
            Span::raw(" 5G  "),
            Span::styled("●", speed_style(&UsbSpeed::SuperSpeedPlus)),
            Span::raw(" 10G+  "),
            Span::styled("●", speed_style(&UsbSpeed::Unknown)),
            Span::raw(" ?"),
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
            Span::raw("      Select a device (list scrolls to keep it visible)"),
        ]),
        Line::from(vec![
            Span::styled("  h", Style::default().fg(ACCENT_COLOR)),
            Span::raw("        Toggle this help"),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl-L", Style::default().fg(ACCENT_COLOR)),
            Span::raw("   Wipe the screen and repaint it from scratch"),
        ]),
        Line::from(vec![
            Span::styled("  q/Esc", Style::default().fg(ACCENT_COLOR)),
            Span::raw("    Quit application"),
        ]),
        Line::from(vec![
            Span::styled("  Ctrl-C", Style::default().fg(ACCENT_COLOR)),
            Span::raw("   Quit application"),
        ]),
        Line::from(""),
        Line::from("Features:"),
        Line::from("  • Controller-grouped, port-ordered device list (USB2/USB3 sibling buses)"),
        Line::from("  • Per-device and per-bus %busy"),
        Line::from("  • ⚡ high-utilization indicator (>80% of practical bandwidth)"),
        Line::from("  • 🔺 device declares USB 3.x support but linked slower — best-effort signal"),
        Line::from("  • Header shows 'dropped: N' if packets were lost to a full queue"),
        Line::from("  • Header shows 'shed: N' if frames were dropped to keep up with a slow"),
        Line::from("    terminal — the numbers are current, the screen is N frames behind"),
        Line::from("  • Color-coded USB link speeds"),
        Line::from("  • Split charts: aggregate total, plus the selected device's rx/tx"),
        Line::from("  • Device disconnect detection"),
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
    use ratatui::Terminal;

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

    /// A plain, unmodified key press.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The same key press with Control held.
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn quit_keys_end_the_session() {
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Char('q'))),
            KeyOutcome::Quit
        );
        assert_eq!(apply_key(&mut app, key(KeyCode::Esc)), KeyOutcome::Quit);
    }

    #[test]
    fn ctrl_c_ends_the_session_too() {
        // Raw mode turns off ISIG, so ^C never becomes a SIGINT: it arrives
        // here as an ordinary key press and has to be honored as one.
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        assert_eq!(
            apply_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            KeyOutcome::Quit
        );
    }

    #[test]
    fn ctrl_l_asks_for_a_wipe_and_a_repaint() {
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        assert_eq!(
            apply_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)
            ),
            KeyOutcome::ClearAndRedraw
        );
        // Bare "l" is an unbound letter, not a redraw request.
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Char('l'))),
            KeyOutcome::None
        );
    }

    #[test]
    fn help_key_toggles_the_overlay() {
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Char('h'))),
            KeyOutcome::Redraw
        );
        assert!(app.show_help);
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Char('h'))),
            KeyOutcome::Redraw
        );
        assert!(!app.show_help);
    }

    /// The overlay is the only place the bindings are written down, so what it
    /// says has to be what `apply_key` does — and it has to survive the layout,
    /// which is the half a text-only assertion would miss.
    #[test]
    fn the_help_overlay_lists_the_bindings_that_exist() {
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.show_help = true;

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(200, 60)).unwrap();
        terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
        let screen = terminal.backend().to_string();

        // Distinctive strings, not bare letters: a lone "h" would match
        // anywhere on the screen and assert nothing.
        for binding in ["↑/↓", "Toggle this help", "Ctrl-L", "q/Esc", "Ctrl-C"] {
            assert!(screen.contains(binding), "{binding} missing from {screen}");
        }
        // And both counters the header can spring on the user are explained.
        assert!(screen.contains("dropped: N"), "{screen}");
        assert!(screen.contains("shed: N"), "{screen}");

        assert_eq!(
            apply_key(&mut app, ctrl(KeyCode::Char('l'))),
            KeyOutcome::ClearAndRedraw
        );
        assert_eq!(
            apply_key(&mut app, ctrl(KeyCode::Char('c'))),
            KeyOutcome::Quit
        );
    }

    #[test]
    fn unbound_keys_leave_the_screen_alone() {
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        assert_eq!(
            apply_key(&mut app, key(KeyCode::Char('x'))),
            KeyOutcome::None
        );
    }

    #[test]
    fn only_presses_act() {
        // Terminals that report key repeat and release (kitty protocol,
        // Windows) would otherwise fire a binding up to three times per press.
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            let event = KeyEvent::new_with_kind(KeyCode::Char('q'), KeyModifiers::NONE, kind);
            assert_eq!(apply_key(&mut app, event), KeyOutcome::None);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn arrow_keys_move_the_selection() {
        let (_t, mut mgr) = topology_fixture();
        feed(
            &mut mgr,
            &["f1 100 C Bi:3:006:1 0 64 <", "f2 300 C Bi:4:002:1 0 64 <"],
        );
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        assert_eq!(apply_key(&mut app, key(KeyCode::Down)), KeyOutcome::Redraw);
        assert_eq!(app.selected_device.as_deref(), Some("3:6"));
        assert_eq!(apply_key(&mut app, key(KeyCode::Up)), KeyOutcome::Redraw);
        assert_eq!(app.selected_device.as_deref(), Some("4:2"), "wraps to last");
    }

    /// Total display width of a device row: every column plus one space between.
    fn device_row_width() -> usize {
        DEVICE_COLUMNS.iter().sum::<usize>() + DEVICE_COLUMNS.len() - 1
    }

    #[test]
    fn device_columns_pad_and_truncate_by_display_width() {
        // "東京デバイス" is 6 chars but 12 terminal cells, so the 14-cell Vendor
        // column takes 2 spaces of padding, not 8. The "⚡" indicator is a
        // 2-cell glyph inside the 3-cell `!` column, exercising the same
        // display-width padding there.
        let wide = Line::from(device_columns([
            "?",
            "001:004",
            "0.0 Mbps",
            "東京デバイス",
            "プローブ",
            "0.0 KB/s",
            "0.0 KB/s",
            " 91.6",
            "⚡",
        ]));
        assert_eq!(wide.width(), device_row_width());

        // Over-long cells are clipped to their column, again by display width.
        // "🔺" is likewise a 2-cell glyph.
        let clipped = Line::from(device_columns([
            "?",
            "001:004",
            "0.0 Mbps",
            "東京デバイスカンパニー",
            "Product",
            "0.0 KB/s",
            "0.0 KB/s",
            "100.0",
            "🔺",
        ]));
        assert_eq!(clipped.width(), device_row_width());
    }

    #[test]
    fn device_rows_stay_aligned_with_wide_characters() {
        let (_t, mut mgr) = manager_with_rates(&[(1, 3, 0.0), (1, 4, 0.0), (1, 5, 1_100_000.0)]);
        {
            let bus = mgr.get_or_create_bus(1);
            let ascii = bus.devices.get_mut(&3).unwrap();
            ascii.vendor = Some("Acme".to_string());
            ascii.product = Some("Widget".to_string());
            let wide = bus.devices.get_mut(&4).unwrap();
            wide.vendor = Some("東京デバイス".to_string());
            wide.product = Some("プローブ".to_string());
            // Device 5: practical max for Full speed is 1.2 MB/s, so
            // 1.1 MB/s crosses the 80% HighUtilization threshold and renders
            // the 2-cell "⚡" glyph in the `!` column.
            let indicator = bus.devices.get_mut(&5).unwrap();
            indicator.speed = UsbSpeed::Full;
        }
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        let (lines, _selected_line) = device_list_lines_with_selection(&app);
        let widths: Vec<usize> = lines.iter().map(Line::width).collect();
        // Column header plus all three device rows occupy exactly the same
        // cells; the controller heading and bus header are free-form.
        assert_eq!(
            widths,
            vec![
                device_row_width(),
                widths[1],
                widths[2],
                device_row_width(),
                device_row_width(),
                device_row_width(),
            ]
        );

        // Lock the ASCII geometry so column offsets cannot drift silently.
        // Device 3 keeps the default UsbSpeed::Unknown (never overridden
        // above), so its %busy cell is the width-7 "--" fallback, not "0.0".
        assert_eq!(
            lines[3].to_string(),
            "?        001:003  0.0 Mbps   Acme           Widget             0.0 KB/s   0.0 KB/s      --      "
        );

        // The wide (2-cell) "⚡" indicator glyph must not push the row's
        // total width off alignment with the others.
        assert!(lines[5].to_string().contains('⚡'), "{}", lines[5]);
    }

    #[test]
    fn bus_header_shows_busy_percentage_or_dashes() {
        let (_t, mut mgr) = manager_with_rates(&[(1, 3, 0.0), (2, 4, 600_000.0)]);
        {
            // Bus 1 keeps the default UsbSpeed::Unknown -> no meaningful
            // denominator, so its header shows "-- busy".
            let bus2 = mgr.get_or_create_bus(2);
            bus2.speed = UsbSpeed::Full; // practical max 1_200_000 bytes/s
        }
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        let text: String = device_list_lines_with_selection(&app)
            .0
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("· -- busy"), "{text}");
        assert!(text.contains("· 50.0% busy"), "{text}");
    }

    #[test]
    fn device_row_shows_dashes_when_device_speed_is_unknown() {
        // %busy is device-row column index 7; device_columns emits one
        // separator span before every column after the first, so its content
        // lands at span index 2 * 7 (see SPEED_SPAN_INDEX/INDICATOR_SPAN_INDEX
        // above for the same pattern).
        const BUSY_SPAN_INDEX: usize = 2 * 7;

        // The device keeps UsbDevice::new's default UsbSpeed::Unknown, but
        // has real traffic (nonzero current_bps): without the fix this
        // renders a misleading "0.0" instead of the bus header's honest "--".
        let (_t, mgr) = manager_with_rates(&[(1, 3, 600_000.0)]);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);

        let (lines, _selected_line) = device_list_lines_with_selection(&app);
        // [0] column header, [1] controller heading, [2] bus header, [3] device row.
        let busy_span = &lines[3].spans[BUSY_SPAN_INDEX];
        assert_eq!(
            busy_span.content, "   --  ",
            "unknown-speed device's %busy cell must be a width-7 '--', not '{}'",
            lines[3]
        );
    }

    #[test]
    fn speed_span_is_colored_unless_the_row_is_selected() {
        let (_t, mut mgr) = manager_with_rates(&[(1, 3, 0.0), (1, 4, 0.0)]);
        {
            let bus = mgr.get_or_create_bus(1);
            bus.devices.get_mut(&3).unwrap().speed = UsbSpeed::High;
            bus.devices.get_mut(&4).unwrap().speed = UsbSpeed::SuperSpeed;
        }
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.selected_device = Some("1:4".to_string());

        let (lines, selected_line) = device_list_lines_with_selection(&app);
        // [0] column header, [1] controller heading, [2] bus header,
        // [3] device 3 (unselected), [4] device 4 (selected).
        assert_eq!(selected_line, Some(4), "selected row's line index");
        let unselected_speed = &lines[3].spans[SPEED_SPAN_INDEX];
        assert_eq!(
            unselected_speed.style.fg,
            Some(Color::Rgb(255, 255, 0)), // UsbSpeed::High
            "unselected row's Speed span carries its speed color"
        );

        let selected_speed = &lines[4].spans[SPEED_SPAN_INDEX];
        assert_ne!(
            selected_speed.style.fg,
            Some(Color::Rgb(0, 255, 0)), // UsbSpeed::SuperSpeed
            "selected row keeps the uniform highlight instead of the speed color"
        );
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
                draw_device_list(f, area, &mut app);
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

    /// A single bus with `count` devices (no sysfs, so every port chain is
    /// `None` and rows sort by device id ascending), used to build a device
    /// list longer than a small `TestBackend` can show at once.
    fn manager_with_n_devices(count: u8) -> (tempfile::TempDir, DeviceManager) {
        let entries: Vec<(u8, u8, f64)> = (1..=count).map(|dev| (1u8, dev, 0.0)).collect();
        manager_with_rates(&entries)
    }

    #[test]
    fn selecting_last_device_scrolls_it_into_view() {
        let (_t, mgr) = manager_with_n_devices(8);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.selected_device = app.device_keys().last().cloned();

        // 8 visible rows would need height 10+; this backend only shows 6.
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 8)).unwrap();
        terminal
            .draw(|f| draw_device_list(f, f.area(), &mut app))
            .unwrap();
        let screen = terminal.backend().to_string();

        assert!(
            screen.contains("001:008"),
            "selected (last) device's row must be visible: {screen}"
        );
        assert!(
            !screen.contains("═ unknown ═"),
            "first controller heading must have scrolled out: {screen}"
        );
    }

    #[test]
    fn scroll_stays_put_when_nothing_is_selected() {
        let (_t, mgr) = manager_with_n_devices(8);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.list_scroll = 2; // as if a prior selection had scrolled the list
        assert_eq!(app.selected_device, None);

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 8)).unwrap();
        terminal
            .draw(|f| draw_device_list(f, f.area(), &mut app))
            .unwrap();

        assert_eq!(
            app.list_scroll, 2,
            "offset must not chase a selection when there isn't one"
        );
    }

    #[test]
    fn scroll_clamps_to_content_length_when_list_shrinks() {
        let (_t, mgr) = manager_with_n_devices(8);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.list_scroll = 50; // stale offset from a much longer list

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 8)).unwrap();
        terminal
            .draw(|f| draw_device_list(f, f.area(), &mut app))
            .unwrap();

        // 11 total lines (header + heading + bus header + 8 devices), 6 visible.
        assert_eq!(app.list_scroll, 5, "clamped to the last full page");
    }

    #[test]
    fn wraparound_selection_pulls_scroll_back_toward_top() {
        let (_t, mgr) = manager_with_n_devices(8);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.selected_device = app.device_keys().last().cloned(); // start at the last device

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 8)).unwrap();
        terminal
            .draw(|f| draw_device_list(f, f.area(), &mut app))
            .unwrap();
        assert_eq!(
            app.list_scroll, 5,
            "scrolled down to reveal the last device"
        );

        app.select_next_device(); // wraps from the last device back to the first
        assert_eq!(app.selected_device.as_deref(), Some("1:1"));
        terminal
            .draw(|f| draw_device_list(f, f.area(), &mut app))
            .unwrap();

        assert!(
            app.list_scroll < 5,
            "scroll must move back toward the top after the wrap, was {}",
            app.list_scroll
        );
        let screen = terminal.backend().to_string();
        assert!(screen.contains("001:001"), "{screen}");
    }

    /// One pass may not stall the frame behind an unbounded backlog: whatever
    /// is left over stays queued for the next pass, ~50ms later.
    #[test]
    fn drain_stops_at_the_batch_limit_and_leaves_the_rest_queued() {
        let temp = tempfile::tempdir().unwrap();
        let mut manager = DeviceManager::with_sysfs_base(temp.path().to_path_buf());
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        for device_id in 1..=5u8 {
            tx.send(
                parse_usbmon_text_line(&format!(
                    "ffff0000eeee000{device_id} 100 C Bi:1:00{device_id}:1 0 4096 <"
                ))
                .unwrap(),
            )
            .unwrap();
        }

        assert_eq!(drain_packets(&mut manager, &rx, 2), 2);
        assert_eq!(manager.buses[&1].devices.len(), 2);

        // The leftovers are still there for the following pass.
        assert_eq!(drain_packets(&mut manager, &rx, 8), 3);
        assert_eq!(manager.buses[&1].devices.len(), 5);
        assert_eq!(drain_packets(&mut manager, &rx, 8), 0, "empty channel");
    }

    /// A lossy session must never look like a clean one, but a clean session
    /// must not carry a permanent "dropped: 0" either.
    #[test]
    fn header_reports_dropped_packets_only_once_some_were_dropped() {
        let render = |app: &UsbTopApp| {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(90, 4)).unwrap();
            terminal.draw(|f| draw_header(f, f.area(), app)).unwrap();
            terminal.backend().to_string()
        };

        let plain = UsbTopApp::new(Duration::from_millis(100));
        assert!(!render(&plain).contains("dropped"), "no counter wired up");

        let counter = Arc::new(AtomicU64::new(0));
        let app =
            UsbTopApp::new(Duration::from_millis(100)).with_dropped_counter(Arc::clone(&counter));
        let screen = render(&app);
        assert!(!screen.contains("dropped"), "nothing dropped yet: {screen}");

        counter.store(42, Ordering::Relaxed);
        let screen = render(&app);
        assert!(screen.contains("dropped: 42"), "{screen}");
    }

    /// A session whose terminal could not keep up is showing stale numbers,
    /// and the header is the only place that can admit it.
    #[test]
    fn header_reports_shed_frames_only_once_some_were_shed() {
        let render = |app: &UsbTopApp| {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(90, 4)).unwrap();
            terminal.draw(|f| draw_header(f, f.area(), app)).unwrap();
            terminal.backend().to_string()
        };

        let mut app = UsbTopApp::new(Duration::from_millis(100));
        assert!(!render(&app).contains("shed"), "no counter wired up");

        let counter = Arc::new(AtomicU64::new(0));
        app.shed_counter = Some(Arc::clone(&counter));
        let screen = render(&app);
        assert!(!screen.contains("shed"), "nothing shed yet: {screen}");

        counter.store(7, Ordering::Relaxed);
        let screen = render(&app);
        assert!(screen.contains("shed: 7"), "{screen}");
    }

    /// The two tests above draw the header into a rect of their own choosing,
    /// which is exactly the blind spot this one closes: the header is two
    /// content lines inside a border, so a layout that hands it any less than
    /// four rows clips the stats line away — and every counter this program has
    /// for admitting it is behind lives on that line.
    #[test]
    fn the_whole_ui_leaves_room_for_the_header_stats_line() {
        let dropped = Arc::new(AtomicU64::new(42));
        let shed = Arc::new(AtomicU64::new(7));
        let mut app =
            UsbTopApp::new(Duration::from_millis(100)).with_dropped_counter(Arc::clone(&dropped));
        app.shed_counter = Some(Arc::clone(&shed));

        // Drawn through `draw_ui`, not `draw_header`: the layout is the thing
        // under test.
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
        terminal.draw(|f| draw_ui(f, &mut app)).unwrap();
        let screen = terminal.backend().to_string();

        assert!(screen.contains("Total: "), "{screen}");
        assert!(screen.contains("Peak: "), "{screen}");
        assert!(screen.contains("dropped: 42"), "{screen}");
        assert!(screen.contains("shed: 7"), "{screen}");
    }

    /// The chart's x-axis is 60 seconds wide, so the history it plots is
    /// trimmed by age. A 60-sample cap would mean 15s at `--refresh 250`.
    #[test]
    fn bandwidth_history_keeps_sixty_seconds_not_sixty_samples() {
        let mut app = UsbTopApp::new(Duration::from_millis(250));
        app.start_time = Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("monotonic clock has at least 120s of history");
        app.bandwidth_history.push((0.0, 1.0)); // ~120s before now
        app.bandwidth_history.push((100.0, 2.0)); // ~20s before now

        app.update_bandwidth_history();

        let times: Vec<f64> = app.bandwidth_history.iter().map(|(t, _)| *t).collect();
        assert_eq!(times.len(), 2, "only the out-of-window sample is dropped");
        assert_eq!(times[0], 100.0);
        assert!(times[1] >= 119.0, "this tick's sample, at ~120s: {times:?}");
    }

    #[test]
    fn bandwidth_history_retains_more_than_sixty_recent_samples() {
        let mut app = UsbTopApp::new(Duration::from_millis(250));
        for _ in 0..100 {
            app.update_bandwidth_history();
        }
        assert_eq!(
            app.bandwidth_history.len(),
            100,
            "samples inside the 60s window are all kept, however fast the tick"
        );
    }

    #[test]
    fn device_chart_shows_placeholder_when_nothing_selected() {
        let app = UsbTopApp::new(Duration::from_millis(100));

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 8)).unwrap();
        terminal
            .draw(|f| draw_device_chart(f, f.area(), &app))
            .unwrap();
        let screen = terminal.backend().to_string();

        assert!(screen.contains("Select a device with"), "{screen}");
        assert!(screen.contains("Device rx/tx"), "{screen}");
    }

    #[test]
    fn device_chart_shows_placeholder_when_selection_vanishes() {
        let (_t, mgr) = manager_with_rates(&[(1, 3, 0.0)]);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.selected_device = Some("9:9".to_string()); // no such device

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 8)).unwrap();
        terminal
            .draw(|f| draw_device_chart(f, f.area(), &app))
            .unwrap();
        let screen = terminal.backend().to_string();

        assert!(screen.contains("Select a device with"), "{screen}");
    }

    #[test]
    fn device_chart_titles_the_selected_device_when_present() {
        let (_t, mgr) = manager_with_rates(&[(1, 3, 0.0)]);
        let mut app = UsbTopApp::new(Duration::from_millis(100));
        app.sync_from(&mgr);
        app.selected_device = Some("1:3".to_string());

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 8)).unwrap();
        terminal
            .draw(|f| draw_device_chart(f, f.area(), &app))
            .unwrap();
        let screen = terminal.backend().to_string();

        assert!(screen.contains(" 1:3 rx/tx "), "{screen}");
        assert!(!screen.contains("Select a device with"), "{screen}");
    }
}

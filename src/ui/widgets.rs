use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame,
};

use super::colors::*;

pub fn create_bandwidth_gauge(current: f64, max: f64, width: u16) -> Gauge<'static> {
    let ratio = if max > 0.0 {
        (current / max).min(1.0)
    } else {
        0.0
    };

    let color = match ratio {
        r if r < 0.25 => BANDWIDTH_LOW,
        r if r < 0.5 => BANDWIDTH_MEDIUM,
        r if r < 0.75 => BANDWIDTH_HIGH,
        _ => BANDWIDTH_CRITICAL,
    };

    Gauge::default()
        .ratio(ratio)
        .style(Style::default().fg(color))
        .label(format!("{:.1} MB/s", current / 1_000_000.0))
}

pub fn format_bandwidth(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1_000_000_000.0 {
        format!("{:.1} GB/s", bytes_per_sec / 1_000_000_000.0)
    } else if bytes_per_sec >= 1_000_000.0 {
        format!("{:.1} MB/s", bytes_per_sec / 1_000_000.0)
    } else if bytes_per_sec >= 1_000.0 {
        format!("{:.1} KB/s", bytes_per_sec / 1_000.0)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}

pub fn create_sparkline_data(history: &[(f64, f64)], max_points: usize) -> Vec<u64> {
    if history.is_empty() {
        return vec![0; max_points];
    }

    let max_value = history.iter().map(|(_, v)| *v).fold(0.0, f64::max).max(1.0);

    history
        .iter()
        .take(max_points)
        .map(|(_, v)| ((v / max_value) * 64.0) as u64)
        .collect()
}

pub fn create_device_status_indicator(is_connected: bool, is_active: bool) -> Span<'static> {
    if !is_connected {
        Span::styled("●", Style::default().fg(Color::Gray))
    } else if is_active {
        Span::styled("●", Style::default().fg(SUCCESS_COLOR))
    } else {
        Span::styled("●", Style::default().fg(WARNING_COLOR))
    }
}

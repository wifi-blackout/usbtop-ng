use ratatui::style::Color;

// Color palette inspired by bashtop
pub const PRIMARY_COLOR: Color = Color::Rgb(0, 191, 255); // Bright blue
pub const SECONDARY_COLOR: Color = Color::Rgb(255, 140, 0); // Orange
pub const ACCENT_COLOR: Color = Color::Rgb(50, 205, 50); // Lime green
pub const SUCCESS_COLOR: Color = Color::Rgb(0, 255, 0); // Green
pub const WARNING_COLOR: Color = Color::Rgb(255, 255, 0); // Yellow
pub const ERROR_COLOR: Color = Color::Rgb(255, 69, 0); // Red orange
pub const TEXT_COLOR: Color = Color::Rgb(255, 255, 255); // White
pub const BACKGROUND_COLOR: Color = Color::Rgb(40, 44, 52); // Dark gray

// USB speed colors (matching parser.rs)
pub const USB_LOW_SPEED: Color = Color::Rgb(255, 100, 100); // Light red
pub const USB_FULL_SPEED: Color = Color::Rgb(255, 165, 0); // Orange
pub const USB_HIGH_SPEED: Color = Color::Rgb(255, 255, 0); // Yellow
pub const USB_SUPER_SPEED: Color = Color::Rgb(0, 255, 0); // Green
pub const USB_SUPER_SPEED_PLUS: Color = Color::Rgb(0, 255, 255); // Cyan
pub const USB_UNKNOWN: Color = Color::Rgb(128, 128, 128); // Gray

// Bandwidth visualization colors
pub const BANDWIDTH_LOW: Color = Color::Rgb(0, 255, 0); // Green (low usage)
pub const BANDWIDTH_MEDIUM: Color = Color::Rgb(255, 255, 0); // Yellow (medium usage)
pub const BANDWIDTH_HIGH: Color = Color::Rgb(255, 165, 0); // Orange (high usage)
pub const BANDWIDTH_CRITICAL: Color = Color::Rgb(255, 0, 0); // Red (critical usage)

// Disconnected device styling
pub const DISCONNECTED_BG: Color = Color::Gray;
pub const DISCONNECTED_FG: Color = Color::White;

use ratatui::style::Color;

// Color palette inspired by bashtop
pub const PRIMARY_COLOR: Color = Color::Rgb(0, 191, 255); // Bright blue
pub const SECONDARY_COLOR: Color = Color::Rgb(255, 140, 0); // Orange
pub const ACCENT_COLOR: Color = Color::Rgb(50, 205, 50); // Lime green
pub const SUCCESS_COLOR: Color = Color::Rgb(0, 255, 0); // Green
pub const TEXT_COLOR: Color = Color::Rgb(255, 255, 255); // White
/// The header's `dropped:` and `shed:` counters -- both admissions that the
/// screen is lossy or behind, not a measurement like Peak, so they get a red
/// no other palette entry uses instead of sharing `SECONDARY_COLOR`.
pub const WARNING_COLOR: Color = Color::Rgb(255, 82, 82);
/// Device row Port cell for a device the internal-device snapshot matches
/// (see `UsbDevice::is_internal`). A readable mid blue, distinct from
/// `PRIMARY_COLOR`'s brighter cyan-blue so the two never read as the same
/// signal.
pub const INTERNAL_COLOR: Color = Color::Rgb(80, 140, 255); // Mid blue
/// Background of a device row's Product text, under black: the product
/// string is what names a device, so it is the cell the eye should land on.
/// A pale blue no other palette entry comes near, so it reads as a
/// highlight and not as `PRIMARY_COLOR` or `INTERNAL_COLOR`.
pub const PRODUCT_HIGHLIGHT_COLOR: Color = Color::Rgb(173, 216, 230); // Light blue

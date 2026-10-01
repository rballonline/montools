//! MCCS input-source codes (VCP 0x60) and their friendly names.

pub const VCP_INPUT_SELECT: u8 = 0x60;

const NAMES: &[(u8, &str)] = &[
    (0x01, "VGA1"),
    (0x02, "VGA2"),
    (0x03, "DVI1"),
    (0x04, "DVI2"),
    (0x0F, "DP1"),
    (0x10, "DP2"),
    (0x11, "HDMI1"),
    (0x12, "HDMI2"),
    (0x1B, "USB-C"),
];

pub fn input_name(code: u8) -> String {
    NAMES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| format!("0x{code:02X}"))
}

/// Parses "hdmi1", "HDMI-1", "dp", "usbc", "usb-c", "0x11" or "17".
pub fn parse_input(s: &str) -> Option<u8> {
    let norm: String = s
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let alias = match norm.as_str() {
        "dp" | "dp1" | "displayport" | "displayport1" => Some(0x0F),
        "dp2" | "displayport2" => Some(0x10),
        "hdmi" | "hdmi1" => Some(0x11),
        "hdmi2" => Some(0x12),
        "usbc" | "typec" | "c" => Some(0x1B),
        "vga" | "vga1" => Some(0x01),
        "dvi" | "dvi1" => Some(0x03),
        _ => None,
    };
    if alias.is_some() {
        return alias;
    }
    if let Some(hex) = norm.strip_prefix("0x") {
        return u8::from_str_radix(hex, 16).ok();
    }
    norm.parse().ok()
}

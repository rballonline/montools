//! Vendor-specific KVM (USB upstream ↔ video input) mappings.
//!
//! Dell U4919DW, reverse-engineered by diffing VCP dumps around Dell Display
//! Manager changes: VCP 0xE7 low byte holds one 2-bit slot per video input,
//! MSB first DP · USB-C · HDMI2 · HDMI1. Slot value is the USB upstream port
//! that input uses: 0 = USB-B1, 1 = USB-B2, 2 = USB-C. The high byte (0xFF)
//! has never been seen to change and is preserved on write.

use serde::Serialize;

use crate::inputs::input_name;

pub struct KvmProfile {
    pub models: &'static [&'static str],
    pub vcp: u8,
    /// (input code, bit shift of its 2-bit slot)
    pub slots: &'static [(u8, u8)],
    pub ports: &'static [(u8, &'static str)],
}

pub const DELL_U4919DW: KvmProfile = KvmProfile {
    models: &["U4919DW"],
    vcp: 0xE7,
    slots: &[(0x0F, 6), (0x1B, 4), (0x12, 2), (0x11, 0)],
    ports: &[(0, "USB-B1"), (1, "USB-B2"), (2, "USB-C")],
};

const PROFILES: &[&KvmProfile] = &[&DELL_U4919DW];

pub fn profile_for(model: &str) -> Option<&'static KvmProfile> {
    let model = model.to_ascii_uppercase();
    PROFILES
        .iter()
        .copied()
        .find(|p| p.models.iter().any(|m| model.contains(m)))
}

#[derive(Debug, Clone, Serialize)]
pub struct KvmSlot {
    pub input: u8,
    pub input_name: String,
    pub port: u8,
    pub port_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KvmPort {
    pub port: u8,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KvmState {
    pub raw: u16,
    pub ports: Vec<KvmPort>,
    pub slots: Vec<KvmSlot>,
}

impl KvmProfile {
    pub fn port_name(&self, port: u8) -> String {
        self.ports
            .iter()
            .find(|(p, _)| *p == port)
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| format!("port {port}"))
    }

    /// Parses "b1", "usb-b2", "usbc", or the raw slot value.
    pub fn parse_port(&self, s: &str) -> Option<u8> {
        let norm: String = s
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        let norm = norm.strip_prefix("usb").unwrap_or(&norm).to_string();
        self.ports
            .iter()
            .find(|(_, n)| {
                let n: String = n.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                n.strip_prefix("usb").unwrap_or(&n) == norm
            })
            .map(|(p, _)| *p)
            .or_else(|| norm.parse().ok().filter(|p| self.ports.iter().any(|(q, _)| q == p)))
    }

    pub fn decode(&self, raw: u16) -> KvmState {
        KvmState {
            raw,
            ports: self
                .ports
                .iter()
                .map(|(p, n)| KvmPort { port: *p, name: n.to_string() })
                .collect(),
            slots: self
                .slots
                .iter()
                .map(|(input, shift)| {
                    let port = ((raw >> shift) & 0b11) as u8;
                    KvmSlot {
                        input: *input,
                        input_name: input_name(*input),
                        port,
                        port_name: self.port_name(port),
                    }
                })
                .collect(),
        }
    }

    pub fn encode(&self, raw: u16, input: u8, port: u8) -> Result<u16, String> {
        let (_, shift) = self
            .slots
            .iter()
            .find(|(i, _)| *i == input)
            .ok_or_else(|| format!("{} has no USB mapping slot on this monitor", input_name(input)))?;
        if !self.ports.iter().any(|(p, _)| *p == port) {
            return Err(format!("unknown USB port value {port}"));
        }
        Ok((raw & !(0b11 << shift)) | ((port as u16) << shift))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_observed_values() {
        // 0xFF21 = DP→B1 · USB-C→USB-C · HDMI2→B1 · HDMI1→B2 (verified 2026-10-01)
        let s = DELL_U4919DW.decode(0xFF21);
        let ports: Vec<(u8, u8)> = s.slots.iter().map(|x| (x.input, x.port)).collect();
        assert_eq!(ports, vec![(0x0F, 0), (0x1B, 2), (0x12, 0), (0x11, 1)]);
    }

    #[test]
    fn encodes_single_slot_preserving_rest() {
        // DDM DP/B2 change: 0xFF25 → 0xFF65
        assert_eq!(DELL_U4919DW.encode(0xFF25, 0x0F, 1), Ok(0xFF65));
        assert_eq!(DELL_U4919DW.encode(0xFF21, 0x12, 1), Ok(0xFF25));
    }

    #[test]
    fn parses_ports() {
        assert_eq!(DELL_U4919DW.parse_port("b2"), Some(1));
        assert_eq!(DELL_U4919DW.parse_port("USB-B1"), Some(0));
        assert_eq!(DELL_U4919DW.parse_port("usbc"), Some(2));
    }
}

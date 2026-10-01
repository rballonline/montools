//! Settings shared by the tray app and CLI, stored as JSON in the OS config dir
//! (e.g. %APPDATA%\dev.montools.app\config.json, ~/.config/dev.montools.app/config.json).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::inputs::input_name;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Monitor key (see `monitors.rs`); None = auto-pick.
    pub monitor: Option<String>,
    /// Input this computer is connected to.
    pub this_input: Option<u8>,
    /// Input the other computer is connected to.
    pub other_input: Option<u8>,
    /// Friendly names shown instead of "This PC" / "Other PC".
    pub this_name: Option<String>,
    pub other_name: Option<String>,
    /// Global shortcut that toggles between computers, e.g. "Ctrl+Alt+K".
    pub hotkey: Option<String>,
    /// List monitors without a known KVM profile too.
    pub show_all_monitors: bool,
}

pub fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("dev.montools.app")
        .join("config.json")
}

impl Config {
    pub fn load() -> Config {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", p.display()))
    }

    /// Friendly name of the computer on `input`, if it's one of the two configured ones.
    pub fn computer_name(&self, input: u8) -> Option<String> {
        let named = |name: &Option<String>, fallback: &str| {
            name.as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(fallback)
                .to_string()
        };
        if Some(input) == self.this_input {
            Some(named(&self.this_name, "This PC"))
        } else if Some(input) == self.other_input {
            Some(named(&self.other_name, "Other PC"))
        } else {
            None
        }
    }

    /// "Work laptop (HDMI1)", or just "HDMI1" for an input with no computer assigned.
    pub fn input_label(&self, input: u8) -> String {
        match self.computer_name(input) {
            Some(name) => format!("{name} ({})", input_name(input)),
            None => input_name(input),
        }
    }

    /// The input to switch to from `current`: other PC if we're on this one, otherwise back here.
    pub fn toggle_target(&self, current: u8) -> Option<u8> {
        match (self.this_input, self.other_input) {
            (Some(this), Some(other)) => Some(if current == this { other } else { this }),
            _ => None,
        }
    }
}

//! Synchronous DDC/CI access to attached monitors.
//!
//! Monitors are identified by a portable key: the model name with any leading
//! "DELL " stripped (Windows reports it from the capabilities string, Linux and
//! macOS from EDID), suffixed with "#2", "#3"… for duplicates. The same key works
//! on every OS, so one config can be shared between machines.

use anyhow::{anyhow, bail, Context, Result};
use ddc_hi::{Ddc, Display};
use serde::Serialize;

use crate::inputs::{input_name, VCP_INPUT_SELECT};
use crate::kvm::{profile_for, KvmProfile, KvmState};

#[derive(Debug, Clone, Serialize)]
pub struct InputOption {
    pub code: u8,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorStatus {
    pub key: String,
    pub model: String,
    pub inputs: Vec<InputOption>,
    pub current_input: Option<u8>,
    pub kvm: Option<KvmState>,
}

struct Entry {
    display: Display,
    key: String,
    model: String,
    inputs: Vec<u8>,
    kvm: Option<&'static KvmProfile>,
}

pub struct Monitors {
    entries: Vec<Entry>,
}

fn normalize_model(name: &str) -> String {
    let t = name.trim();
    match t.get(..5) {
        Some(p) if p.eq_ignore_ascii_case("dell ") => t[5..].trim().to_string(),
        _ => t.to_string(),
    }
}

impl Monitors {
    /// Enumerates monitors and reads their capabilities. Slow (~1s per monitor).
    pub fn enumerate() -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for mut display in Display::enumerate() {
            let caps = display.handle.capabilities().ok();
            let model = display
                .info
                .model_name
                .clone()
                .or_else(|| caps.as_ref().and_then(|c| c.model.clone()))
                .map(|m| normalize_model(&m))
                .unwrap_or_else(|| display.info.id.clone());
            let mut inputs: Vec<u8> = caps
                .as_ref()
                .and_then(|c| c.vcp_features.get(&VCP_INPUT_SELECT))
                .map(|d| d.values().copied().collect())
                .unwrap_or_default();
            inputs.sort_unstable();

            let dupes = entries.iter().filter(|e| e.model == model).count();
            let key = if dupes == 0 { model.clone() } else { format!("{model}#{}", dupes + 1) };
            entries.push(Entry {
                display,
                kvm: profile_for(&model),
                key,
                model,
                inputs,
            });
        }
        Monitors { entries }
    }

    pub fn keys(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.key.clone()).collect()
    }

    /// Picks the configured monitor, else the first one with a known KVM profile, else the first.
    pub fn resolve(&self, key: Option<&str>) -> Result<String> {
        if let Some(k) = key {
            if self.entries.iter().any(|e| e.key == k) {
                return Ok(k.to_string());
            }
            bail!("monitor '{k}' not found (attached: {})", self.keys().join(", "));
        }
        self.entries
            .iter()
            .find(|e| e.kvm.is_some())
            .or(self.entries.first())
            .map(|e| e.key.clone())
            .ok_or_else(|| anyhow!("no DDC/CI-capable monitors found"))
    }

    fn entry(&mut self, key: &str) -> Result<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| e.key == key)
            .ok_or_else(|| anyhow!("monitor '{key}' not found"))
    }

    pub fn status(&mut self, key: &str) -> Result<MonitorStatus> {
        let e = self.entry(key)?;
        let current_input = e
            .display
            .handle
            .get_vcp_feature(VCP_INPUT_SELECT)
            .ok()
            .map(|v| (v.value() & 0xFF) as u8);
        let kvm = match e.kvm {
            Some(p) => Some(p.decode(e.display.handle.get_vcp_feature(p.vcp)?.value())),
            None => None,
        };
        Ok(MonitorStatus {
            key: e.key.clone(),
            model: e.model.clone(),
            inputs: e.inputs.iter().map(|&code| InputOption { code, name: input_name(code) }).collect(),
            current_input,
            kvm,
        })
    }

    pub fn all_status(&mut self) -> Vec<MonitorStatus> {
        let keys = self.keys();
        keys.iter().filter_map(|k| self.status(k).ok()).collect()
    }

    pub fn current_input(&mut self, key: &str) -> Result<u8> {
        let e = self.entry(key)?;
        let v = e
            .display
            .handle
            .get_vcp_feature(VCP_INPUT_SELECT)
            .context("reading current input")?;
        Ok((v.value() & 0xFF) as u8)
    }

    pub fn set_input(&mut self, key: &str, input: u8) -> Result<()> {
        let e = self.entry(key)?;
        if !e.inputs.is_empty() && !e.inputs.contains(&input) {
            bail!(
                "{} is not an input on {} (has: {})",
                input_name(input),
                e.model,
                e.inputs.iter().map(|&i| input_name(i)).collect::<Vec<_>>().join(", ")
            );
        }
        e.display
            .handle
            .set_vcp_feature(VCP_INPUT_SELECT, input as u16)
            .with_context(|| format!("switching {} to {}", e.model, input_name(input)))
    }

    pub fn kvm_profile(&mut self, key: &str) -> Result<&'static KvmProfile> {
        let e = self.entry(key)?;
        e.kvm
            .ok_or_else(|| anyhow!("no known KVM mapping for {} (only input switching is supported)", e.model))
    }

    pub fn set_kvm_port(&mut self, key: &str, input: u8, port: u8) -> Result<KvmState> {
        self.set_kvm_ports(key, &[(input, port)])
    }

    /// Applies several (input, port) changes in a single write.
    pub fn set_kvm_ports(&mut self, key: &str, changes: &[(u8, u8)]) -> Result<KvmState> {
        let profile = self.kvm_profile(key)?;
        let e = self.entry(key)?;
        let raw = e.display.handle.get_vcp_feature(profile.vcp)?.value();
        let mut new = raw;
        for &(input, port) in changes {
            new = profile.encode(new, input, port).map_err(|s| anyhow!(s))?;
        }
        if new != raw {
            e.display.handle.set_vcp_feature(profile.vcp, new)?;
        }
        Ok(profile.decode(e.display.handle.get_vcp_feature(profile.vcp)?.value()))
    }
}

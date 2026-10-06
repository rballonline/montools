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
    /// Linux /dev/i2c-N bus number.
    bus: Option<u32>,
    /// Supported input codes from the capabilities string; None until first needed.
    inputs: Option<Vec<u8>>,
    kvm: Option<&'static KvmProfile>,
}

/// Reads the capabilities string, which takes several seconds over DDC/CI.
fn read_inputs(display: &mut Display) -> Vec<u8> {
    let mut inputs: Vec<u8> = display
        .handle
        .capabilities()
        .ok()
        .and_then(|c| c.vcp_features.get(&VCP_INPUT_SELECT).map(|d| d.values().copied().collect()))
        .unwrap_or_default();
    inputs.sort_unstable();
    inputs
}

/// ddc-hi identifies I2C displays by the device number of /dev/i2c-N; N is its minor.
fn i2c_bus(display: &Display) -> Option<u32> {
    if display.info.backend != ddc_hi::Backend::I2cDevice {
        return None;
    }
    let rdev: u64 = display.info.id.parse().ok()?;
    Some(((rdev & 0xff) | ((rdev >> 12) & 0xfff00)) as u32)
}

/// Opens a single /dev/i2c-N bus as a display, if a monitor answers with an EDID there.
#[cfg(target_os = "linux")]
fn open_i2c_bus(bus: u32) -> Option<Display> {
    use ddc::Edid;
    use std::os::unix::fs::MetadataExt;
    let mut ddc = ddc_i2c::from_i2c_device(format!("/dev/i2c-{bus}")).ok()?;
    let rdev = ddc.inner_ref().inner_ref().metadata().ok()?.rdev();
    let mut edid = vec![0u8; 0x100];
    ddc.read_edid(0, &mut edid).ok()?;
    let info = ddc_hi::DisplayInfo::from_edid(ddc_hi::Backend::I2cDevice, rdev.to_string(), edid).ok()?;
    Some(Display::new(ddc_hi::Handle::I2cDevice(ddc), info))
}

impl Entry {
    fn inputs(&mut self) -> &[u8] {
        self.inputs.get_or_insert_with(|| read_inputs(&mut self.display))
    }
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
    /// Enumerates monitors. The capabilities string (supported inputs) is read lazily,
    /// except where the model name has to come from it (Windows has no EDID name).
    pub fn enumerate() -> Self {
        Self::from_displays(Display::enumerate())
    }

    /// Opens the monitor `key`, trying the bus it was last seen on (`bus_hint`) before
    /// scanning every bus. Use `bus()` afterwards to learn where it actually was.
    pub fn open(key: Option<&str>, bus_hint: Option<u32>) -> Self {
        #[cfg(target_os = "linux")]
        if let (Some(key), Some(bus)) = (key, bus_hint) {
            if let Some(display) = open_i2c_bus(bus) {
                let m = Self::from_displays(vec![display]);
                if m.entries.iter().any(|e| e.key == key) {
                    return m;
                }
            }
        }
        let _ = bus_hint;
        Self::enumerate()
    }

    fn from_displays(displays: Vec<Display>) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for mut display in displays {
            let mut inputs = None;
            let mut model = display.info.model_name.clone();
            if model.is_none() {
                if let Ok(caps) = display.handle.capabilities() {
                    model = caps.model.clone();
                    let mut v: Vec<u8> = caps
                        .vcp_features
                        .get(&VCP_INPUT_SELECT)
                        .map(|d| d.values().copied().collect())
                        .unwrap_or_default();
                    v.sort_unstable();
                    inputs = Some(v);
                }
            }
            let model = model.map(|m| normalize_model(&m)).unwrap_or_else(|| display.info.id.clone());

            let dupes = entries.iter().filter(|e| e.model == model).count();
            let key = if dupes == 0 { model.clone() } else { format!("{model}#{}", dupes + 1) };
            entries.push(Entry {
                bus: i2c_bus(&display),
                display,
                kvm: profile_for(&model),
                key,
                model,
                inputs,
            });
        }
        Monitors { entries }
    }

    /// The Linux I2C bus number of monitor `key`, if known.
    pub fn bus(&self, key: &str) -> Option<u32> {
        self.entries.iter().find(|e| e.key == key).and_then(|e| e.bus)
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
        let inputs = e.inputs().iter().map(|&code| InputOption { code, name: input_name(code) }).collect();
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
            inputs,
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

    /// Supported input codes (empty if the monitor doesn't report them). Slow on first call.
    pub fn inputs(&mut self, key: &str) -> Result<Vec<u8>> {
        Ok(self.entry(key)?.inputs().to_vec())
    }

    /// Switches input. Validates against the supported inputs only if they've already been
    /// read, so a switch to a known-good input doesn't pay for the capabilities string.
    pub fn set_input(&mut self, key: &str, input: u8) -> Result<()> {
        let e = self.entry(key)?;
        if let Some(inputs) = &e.inputs {
            if !inputs.is_empty() && !inputs.contains(&input) {
                bail!(
                    "{} is not an input on {} (has: {})",
                    input_name(input),
                    e.model,
                    inputs.iter().map(|&i| input_name(i)).collect::<Vec<_>>().join(", ")
                );
            }
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

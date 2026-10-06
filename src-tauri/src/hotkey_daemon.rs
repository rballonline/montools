//! Linux hotkey listener that reads keyboards straight from /dev/input, so it works
//! outside a desktop session: at the login screen, the lock screen and on a VT, and
//! under Wayland compositors that don't let apps grab global shortcuts.
//!
//! Run as a system service with access to the `input` and `i2c` groups (see README).
//! Keyboards and the configured hotkey are rechecked every second: the monitor's KVM
//! unplugs and replugs keyboards on every switch, and the hotkey can change in Settings.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};

use crate::config::Config;
use crate::monitors::Monitors;

const EV_KEY: u16 = 0x01;
const RESCAN: Duration = Duration::from_secs(1);

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;
const SUPER: u8 = 8;

/// Linux key codes (input-event-codes.h) of the modifiers, left and right.
const MODIFIER_KEYS: &[(u16, u8)] = &[
    (29, CTRL),   // KEY_LEFTCTRL
    (97, CTRL),   // KEY_RIGHTCTRL
    (56, ALT),    // KEY_LEFTALT
    (100, ALT),   // KEY_RIGHTALT
    (42, SHIFT),  // KEY_LEFTSHIFT
    (54, SHIFT),  // KEY_RIGHTSHIFT
    (125, SUPER), // KEY_LEFTMETA
    (126, SUPER), // KEY_RIGHTMETA
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    modifiers: u8,
    key: u16,
}

/// Parses the shortcut format the settings window records, e.g. "Alt+Shift+KeyZ",
/// "Ctrl+Alt+K", "Super+F12".
pub fn parse_hotkey(s: &str) -> Result<Hotkey> {
    let mut modifiers = 0;
    let mut key = None;
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let m = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" | "cmdorcontrol" | "commandorctrl" => CTRL,
            "alt" | "option" => ALT,
            "shift" => SHIFT,
            "super" | "meta" | "cmd" | "command" => SUPER,
            _ => 0,
        };
        if m != 0 {
            modifiers |= m;
        } else if key.is_some() {
            bail!("hotkey '{s}' has more than one non-modifier key");
        } else {
            key = Some(key_code(part).ok_or_else(|| anyhow!("unsupported key '{part}' in hotkey '{s}'"))?);
        }
    }
    let key = key.ok_or_else(|| anyhow!("hotkey '{s}' has no key, only modifiers"))?;
    Ok(Hotkey { modifiers, key })
}

/// Maps a key name ("KeyZ", "Z", "Digit1", "F5", "Space"…) to its Linux key code.
fn key_code(name: &str) -> Option<u16> {
    const LETTERS: &[u16; 26] = &[
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
    ];
    const DIGITS: &[u16; 10] = &[11, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    const NUMPAD: &[u16; 10] = &[82, 79, 80, 81, 75, 76, 77, 71, 72, 73];

    let n = name.to_ascii_lowercase();
    let n = n.strip_prefix("key").filter(|r| r.len() == 1).unwrap_or(&n);
    let single = |s: &str| {
        let mut c = s.chars();
        c.next().filter(|_| c.next().is_none())
    };
    if let Some(c) = single(n) {
        if c.is_ascii_lowercase() {
            return Some(LETTERS[(c as u8 - b'a') as usize]);
        }
        if c.is_ascii_digit() {
            return Some(DIGITS[(c as u8 - b'0') as usize]);
        }
    }
    if let Some(d) = n.strip_prefix("digit").and_then(single).and_then(|c| c.to_digit(10)) {
        return Some(DIGITS[d as usize]);
    }
    if let Some(d) = n.strip_prefix("numpad").and_then(single).and_then(|c| c.to_digit(10)) {
        return Some(NUMPAD[d as usize]);
    }
    if let Some(f) = n.strip_prefix('f').and_then(|r| r.parse::<u16>().ok()) {
        return match f {
            1..=10 => Some(58 + f),
            11 | 12 => Some(76 + f),
            13..=24 => Some(170 + f),
            _ => None,
        };
    }
    Some(match n {
        "escape" | "esc" => 1,
        "minus" | "-" => 12,
        "equal" | "=" => 13,
        "backspace" => 14,
        "tab" => 15,
        "bracketleft" | "[" => 26,
        "bracketright" | "]" => 27,
        "enter" | "return" => 28,
        "semicolon" | ";" => 39,
        "quote" | "'" => 40,
        "backquote" | "`" => 41,
        "backslash" | "\\" => 43,
        "comma" | "," => 51,
        "period" | "." => 52,
        "slash" | "/" => 53,
        "space" => 57,
        "capslock" => 58,
        "scrolllock" => 70,
        "printscreen" => 99,
        "home" => 102,
        "arrowup" | "up" => 103,
        "pageup" => 104,
        "arrowleft" | "left" => 105,
        "arrowright" | "right" => 106,
        "end" => 107,
        "arrowdown" | "down" => 108,
        "pagedown" => 109,
        "insert" => 110,
        "delete" => 111,
        "pause" => 119,
        _ => return None,
    })
}

/// Whether the input device at /dev/input/eventN can send `key`, per its sysfs key bitmap
/// (hex words of `usize` bits, most significant word first).
fn has_key(event_node: &Path, key: u16) -> bool {
    let Some(name) = event_node.file_name() else { return false };
    let caps = Path::new("/sys/class/input").join(name).join("device/capabilities/key");
    let Ok(s) = std::fs::read_to_string(caps) else { return false };
    let bits = usize::BITS as usize;
    let words: Vec<&str> = s.split_whitespace().rev().collect();
    let k = key as usize;
    words
        .get(k / bits)
        .and_then(|w| usize::from_str_radix(w, 16).ok())
        .is_some_and(|w| (w >> (k % bits)) & 1 == 1)
}

enum Msg {
    Key { dev: PathBuf, code: u16, pressed: bool },
    Gone(PathBuf),
    Hotkey(Option<Hotkey>),
}

/// Reads key events from one device until it disappears.
fn read_device(path: PathBuf, mut file: File, tx: mpsc::Sender<Msg>) {
    // struct input_event { struct timeval time; __u16 type; __u16 code; __s32 value; }
    let tv = 2 * std::mem::size_of::<libc_long>();
    let mut buf = vec![0u8; tv + 8];
    while file.read_exact(&mut buf).is_ok() {
        let ty = u16::from_ne_bytes([buf[tv], buf[tv + 1]]);
        let code = u16::from_ne_bytes([buf[tv + 2], buf[tv + 3]]);
        let value = i32::from_ne_bytes(buf[tv + 4..tv + 8].try_into().unwrap());
        // value: 1 press, 0 release, 2 autorepeat (ignored).
        if ty == EV_KEY && value != 2 && tx.send(Msg::Key { dev: path.clone(), code, pressed: value == 1 }).is_err() {
            return;
        }
    }
    let _ = tx.send(Msg::Gone(path));
}

/// `long` in the kernel's `struct timeval`.
#[allow(non_camel_case_types)]
type libc_long = isize;

/// Follows the hotkey (`fixed`, else the one in the config) and opens every keyboard that
/// can type its key, now and as they're plugged in.
fn watch(fixed: Option<String>, tx: mpsc::Sender<Msg>, open: Arc<Mutex<HashSet<PathBuf>>>) {
    // Outer None = not read yet, so the first state is always logged.
    let mut current: Option<Option<String>> = None;
    let mut hotkey: Option<Hotkey> = None;
    loop {
        let wanted = fixed
            .clone()
            .or_else(|| Config::load().hotkey)
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty());
        if current.as_ref() != Some(&wanted) {
            hotkey = match wanted.as_deref().map(parse_hotkey) {
                Some(Ok(h)) => {
                    eprintln!("hotkey: {}", wanted.as_deref().unwrap_or_default());
                    Some(h)
                }
                Some(Err(e)) => {
                    eprintln!("ignoring hotkey: {e:#}");
                    None
                }
                None => {
                    eprintln!("no hotkey configured; waiting for one to be set in Settings");
                    None
                }
            };
            current = Some(wanted);
            if tx.send(Msg::Hotkey(hotkey)).is_err() {
                return;
            }
        }
        let Some(key) = hotkey.map(|h| h.key) else {
            std::thread::sleep(RESCAN);
            continue;
        };
        if let Ok(dir) = std::fs::read_dir("/dev/input") {
            for path in dir.flatten().map(|e| e.path()) {
                let is_event = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event"));
                if !is_event || open.lock().unwrap().contains(&path) || !has_key(&path, key) {
                    continue;
                }
                match File::open(&path) {
                    Ok(file) => {
                        eprintln!("listening on {} ({})", path.display(), device_name(&path));
                        open.lock().unwrap().insert(path.clone());
                        let tx = tx.clone();
                        std::thread::spawn(move || read_device(path, file, tx));
                    }
                    Err(e) => eprintln!("can't open {}: {e}", path.display()),
                }
            }
        }
        std::thread::sleep(RESCAN);
    }
}

fn device_name(event_node: &Path) -> String {
    event_node
        .file_name()
        .map(|n| Path::new("/sys/class/input").join(n).join("device/name"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn toggle(monitors: &mut Option<Monitors>) -> Result<String> {
    let mut cfg = Config::load();
    let attempt = |m: &mut Monitors| -> Result<String> {
        let key = m.resolve(cfg.monitor.as_deref())?;
        let cur = m.current_input(&key)?;
        let target = cfg
            .toggle_target(cur)
            .ok_or_else(|| anyhow!("set both 'this' and 'other' inputs first (config set …)"))?;
        m.set_input(&key, target)?;
        Ok(format!("{key} → {}", cfg.input_label(target)))
    };
    // Keep the monitor handles between presses; re-enumerate once if they've gone stale.
    if let Some(m) = monitors.as_mut() {
        if let Ok(s) = attempt(m) {
            return Ok(s);
        }
    }
    let m = monitors.insert(Monitors::open(cfg.monitor.as_deref(), cfg.i2c_bus));
    let result = attempt(m);
    let bus = m.resolve(cfg.monitor.as_deref()).ok().and_then(|k| m.bus(&k));
    if bus.is_some() && bus != cfg.i2c_bus {
        cfg.i2c_bus = bus;
        let _ = cfg.save();
    }
    result
}

/// Listens for `hotkey` (or the configured one, following changes) and toggles between
/// computers. Never returns unless `hotkey` is invalid.
pub fn run(hotkey: Option<&str>) -> Result<()> {
    if let Some(h) = hotkey {
        parse_hotkey(h)?;
    }
    let (tx, rx) = mpsc::channel();
    let open = Arc::new(Mutex::new(HashSet::new()));
    {
        let open = open.clone();
        let fixed = hotkey.map(str::to_string);
        std::thread::spawn(move || watch(fixed, tx, open));
    }

    let mut hotkey: Option<Hotkey> = None;
    let mut monitors: Option<Monitors> = None;
    // Keys held per device, so an unplugged keyboard's keys don't stay "held".
    let mut held: HashMap<PathBuf, HashSet<u16>> = HashMap::new();
    for msg in rx {
        match msg {
            Msg::Gone(dev) => {
                eprintln!("lost {}", dev.display());
                held.remove(&dev);
                open.lock().unwrap().remove(&dev);
            }
            Msg::Key { dev, code, pressed: false } => {
                if let Some(keys) = held.get_mut(&dev) {
                    keys.remove(&code);
                }
            }
            Msg::Hotkey(h) => hotkey = h,
            Msg::Key { dev, code, pressed: true } => {
                held.entry(dev).or_default().insert(code);
                let Some(hotkey) = hotkey.filter(|h| h.key == code) else {
                    continue;
                };
                let modifiers = MODIFIER_KEYS
                    .iter()
                    .filter(|(k, _)| held.values().any(|keys| keys.contains(k)))
                    .fold(0, |acc, (_, m)| acc | m);
                if modifiers != hotkey.modifiers {
                    continue;
                }
                match toggle(&mut monitors) {
                    Ok(s) => eprintln!("{s}"),
                    Err(e) => eprintln!("switch failed: {e:#}"),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_hotkeys() {
        assert_eq!(parse_hotkey("Alt+Shift+KeyZ").unwrap(), Hotkey { modifiers: ALT | SHIFT, key: 44 });
        assert_eq!(parse_hotkey("Ctrl+Alt+K").unwrap(), Hotkey { modifiers: CTRL | ALT, key: 37 });
        assert_eq!(parse_hotkey("Super+F12").unwrap(), Hotkey { modifiers: SUPER, key: 88 });
        assert_eq!(parse_hotkey("CommandOrControl+Digit1").unwrap(), Hotkey { modifiers: CTRL, key: 2 });
        assert_eq!(parse_hotkey("Shift+Space").unwrap().key, 57);
        assert_eq!(parse_hotkey("F13").unwrap().key, 183);
        assert!(parse_hotkey("Ctrl+Alt").is_err());
        assert!(parse_hotkey("Ctrl+A+B").is_err());
    }
}

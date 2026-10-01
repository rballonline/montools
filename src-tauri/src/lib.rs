pub mod config;
pub mod inputs;
pub mod kvm;
pub mod monitors;
mod worker;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use config::Config;
use kvm::KvmState;
use monitors::MonitorStatus;
use worker::DdcWorker;

const TRAY_ID: &str = "main";
/// How long a USB mapping change waits for confirmation before reverting itself.
const KVM_CONFIRM_SECS: u64 = 5;

/// A USB mapping change that reverts unless confirmed (like a display-resolution change).
struct PendingKvm {
    token: u64,
    monitor: String,
    previous: Vec<(u8, u8)>,
}

struct AppState {
    ddc: DdcWorker,
    config: Mutex<Config>,
    pending_kvm: Mutex<Option<PendingKvm>>,
    next_token: AtomicU64,
}

fn monitor_key(state: &AppState) -> Option<String> {
    state.config.lock().unwrap().monitor.clone()
}

fn do_switch(app: &AppHandle, input: u8) -> Result<(), String> {
    let state = app.state::<AppState>();
    let key = monitor_key(&state);
    state
        .ddc
        .run(move |m| {
            let k = m.resolve(key.as_deref())?;
            m.set_input(&k, input)
        })
        .map_err(|e| format!("{e:#}"))?;
    refresh_tray(app);
    Ok(())
}

fn do_toggle(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let key = cfg.monitor.clone();
    let current = state
        .ddc
        .run(move |m| {
            let k = m.resolve(key.as_deref())?;
            m.current_input(&k)
        })
        .map_err(|e| format!("{e:#}"))?;
    let target = cfg
        .toggle_target(current)
        .ok_or("Set 'This PC' and 'Other PC' inputs in Settings first")?;
    do_switch(app, target)
}

/// Rebuilds the tray menu and tooltip from the monitor's live state, off the UI thread.
fn refresh_tray(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let cfg = state.config.lock().unwrap().clone();
        let key = cfg.monitor.clone();
        let status = state.ddc.run(move |m| {
            let k = m.resolve(key.as_deref())?;
            m.status(&k)
        });
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = build_tray_menu(&app, &cfg, status.as_ref().ok()).map(|menu| tray.set_menu(Some(menu)));
            let tip = match &status {
                Ok(s) => match s.current_input {
                    Some(i) => format!("montools · {} on {}", s.model, cfg.input_label(i)),
                    None => format!("montools · {}", s.model),
                },
                Err(e) => format!("montools · {e}"),
            };
            let _ = tray.set_tooltip(Some(tip));
        }
    });
}

fn build_tray_menu(app: &AppHandle, cfg: &Config, status: Option<&MonitorStatus>) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    let header = match status {
        Some(s) => match s.current_input {
            Some(i) => format!("{}: {}", s.model, cfg.input_label(i)),
            None => s.model.clone(),
        },
        None => "No monitor found".into(),
    };
    menu.append(&MenuItem::with_id(app, "status", header, false, None::<&str>)?)?;

    let configured = cfg.this_input.is_some() && cfg.other_input.is_some();
    let target = status.and_then(|s| s.current_input).and_then(|i| cfg.toggle_target(i)).or(cfg.this_input);
    let toggle_label = match target.and_then(|t| cfg.computer_name(t)) {
        Some(name) if configured => format!("Switch to {name}"),
        _ => "Switch computer (set up in Settings)".to_string(),
    };
    menu.append(&MenuItem::with_id(app, "toggle", toggle_label, configured, None::<&str>)?)?;

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref().to_string();
    let app = app.clone();
    match id.as_str() {
        "settings" => show_settings(&app),
        "quit" => app.exit(0),
        "toggle" => {
            std::thread::spawn(move || report(&app, do_toggle(&app)));
        }
        _ => {}
    }
}


fn report(app: &AppHandle, result: Result<(), String>) {
    if let Err(e) = result {
        eprintln!("montools: {e}");
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(format!("montools · {e}")));
        }
    }
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn apply_hotkey(app: &AppHandle, hotkey: Option<&str>) -> Result<(), String> {
    let gs = app.global_shortcut();
    gs.unregister_all().map_err(|e| e.to_string())?;
    match hotkey.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => gs.register(h).map_err(|e| format!("Couldn't register hotkey '{h}': {e}")),
        None => Ok(()),
    }
}

// ---- commands for the settings window ----

#[tauri::command]
async fn get_monitors(state: State<'_, AppState>, refresh: bool) -> Result<Vec<MonitorStatus>, String> {
    if refresh {
        state.ddc.refresh().map_err(|e| format!("{e:#}"))?;
    }
    state.ddc.run(|m| Ok(m.all_status())).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    let mut cfg = state.config.lock().unwrap().clone();
    if cfg.monitor.is_none() {
        cfg.monitor = state.ddc.run(|m| m.resolve(None)).ok();
    }
    Ok(cfg)
}

#[tauri::command]
async fn save_config(app: AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
    config.save().map_err(|e| format!("{e:#}"))?;
    let hotkey_result = apply_hotkey(&app, config.hotkey.as_deref());
    *state.config.lock().unwrap() = config;
    refresh_tray(&app);
    hotkey_result
}

#[tauri::command]
async fn switch_input(app: AppHandle, input: u8) -> Result<(), String> {
    do_switch(&app, input)
}

#[tauri::command]
async fn toggle_computer(app: AppHandle) -> Result<(), String> {
    do_toggle(&app)
}

#[derive(serde::Deserialize)]
struct PortChange {
    input: u8,
    port: u8,
}

#[derive(serde::Serialize)]
struct AppliedKvm {
    token: u64,
    seconds: u64,
    kvm: KvmState,
}

fn write_kvm(state: &AppState, monitor: String, changes: Vec<(u8, u8)>) -> Result<KvmState, String> {
    state
        .ddc
        .run(move |m| m.set_kvm_ports(&monitor, &changes))
        .map_err(|e| format!("{e:#}"))
}

/// Restores the pending change's previous mapping, if `token` is still the pending one.
fn revert_pending(app: &AppHandle, token: u64) -> Result<Option<KvmState>, String> {
    let state = app.state::<AppState>();
    let pending = {
        let mut slot = state.pending_kvm.lock().unwrap();
        match slot.as_ref() {
            Some(p) if p.token == token => slot.take(),
            _ => None,
        }
    };
    let Some(p) = pending else { return Ok(None) };
    let kvm = write_kvm(&state, p.monitor, p.previous)?;
    let _ = app.emit("kvm-reverted", &kvm);
    refresh_tray(app);
    Ok(Some(kvm))
}

/// Writes the new mapping and starts the confirm countdown; reverts automatically on timeout.
#[tauri::command]
async fn apply_kvm_ports(app: AppHandle, state: State<'_, AppState>, monitor: String, changes: Vec<PortChange>) -> Result<AppliedKvm, String> {
    // A still-pending earlier change is reverted first so "previous" is always a confirmed state.
    let stale = state.pending_kvm.lock().unwrap().as_ref().map(|p| p.token);
    if let Some(t) = stale {
        revert_pending(&app, t)?;
    }

    let before = {
        let monitor = monitor.clone();
        state
            .ddc
            .run(move |m| Ok(m.status(&monitor)?.kvm))
            .map_err(|e| format!("{e:#}"))?
            .ok_or("this monitor has no KVM mapping")?
    };
    let changes: Vec<(u8, u8)> = changes.into_iter().map(|c| (c.input, c.port)).collect();
    let previous: Vec<(u8, u8)> = before
        .slots
        .iter()
        .filter(|s| changes.iter().any(|(i, _)| *i == s.input))
        .map(|s| (s.input, s.port))
        .collect();

    let kvm = write_kvm(&state, monitor.clone(), changes)?;
    let token = state.next_token.fetch_add(1, Ordering::Relaxed) + 1;
    *state.pending_kvm.lock().unwrap() = Some(PendingKvm { token, monitor, previous });
    refresh_tray(&app);

    let timer_app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(KVM_CONFIRM_SECS));
        if let Err(e) = revert_pending(&timer_app, token) {
            report(&timer_app, Err(format!("Couldn't revert USB mapping: {e}")));
        }
    });

    Ok(AppliedKvm { token, seconds: KVM_CONFIRM_SECS, kvm })
}

#[tauri::command]
async fn confirm_kvm(state: State<'_, AppState>, token: u64) -> Result<bool, String> {
    let mut slot = state.pending_kvm.lock().unwrap();
    match slot.as_ref() {
        Some(p) if p.token == token => {
            *slot = None;
            Ok(true)
        }
        // Already reverted by the timer.
        _ => Ok(false),
    }
}

#[tauri::command]
async fn revert_kvm(app: AppHandle, token: u64) -> Result<Option<KvmState>, String> {
    revert_pending(&app, token)
}

#[derive(serde::Serialize)]
struct PlatformInfo {
    os: &'static str,
    wayland: bool,
}

#[tauri::command]
fn platform_info() -> PlatformInfo {
    let wayland = cfg!(target_os = "linux")
        && (std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland")));
    PlatformInfo { os: std::env::consts::OS, wayland }
}

#[tauri::command]
async fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_settings(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let app = app.clone();
                        std::thread::spawn(move || report(&app, do_toggle(&app)));
                    }
                })
                .build(),
        )
        .manage(AppState {
            ddc: DdcWorker::spawn(),
            config: Mutex::new(Config::load()),
            pending_kvm: Mutex::new(None),
            next_token: AtomicU64::new(0),
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let cfg = app.state::<AppState>().config.lock().unwrap().clone();

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .tooltip("montools")
                .menu(&build_tray_menu(&handle, &cfg, None)?)
                .show_menu_on_left_click(true)
                .on_menu_event(on_menu_event)
                .build(app)?;

            if let Err(e) = apply_hotkey(&handle, cfg.hotkey.as_deref()) {
                eprintln!("montools: {e}");
            }
            refresh_tray(&handle);

            // First run (nothing configured yet): open settings so the user can pick inputs.
            if cfg.this_input.is_none() || cfg.other_input.is_none() {
                show_settings(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window just hides it; the app lives in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_monitors,
            get_config,
            save_config,
            switch_input,
            toggle_computer,
            apply_kvm_ports,
            confirm_kvm,
            revert_kvm,
            platform_info,
            get_autostart,
            set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

//! Tauri shell for AgentLight.
//!
//! All state parsing, aggregation, and file writes live in `agentlight-core`.
//! This layer only wires that logic to a window, a tray icon, a file watcher,
//! and IPC commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use agentlight_core::{Config, Snapshot, Status};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

// Required in scope for `RecommendedWatcher::watch`.
use notify::Watcher;

/// Shared app state managed by Tauri.
struct AppState {
    config: Mutex<Config>,
    config_path: PathBuf,
    /// Bumped whenever the watcher should be rebuilt. Older watcher threads see
    /// a stale value and exit.
    generation: AtomicU64,
    /// Last seen status per session, for edge-triggered notifications.
    previous: Mutex<HashMap<String, Status>>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    let config = current_config(&state);
    agentlight_core::build_snapshot(&config.state_file(), &config)
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    current_config(&state)
}

#[tauri::command]
fn set_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: Config,
) -> Result<Config, String> {
    let config = config.sanitized();
    agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_always_on_top(config.always_on_top);
    }
    apply_autostart(&app, config.start_at_login);

    *state.config.lock().unwrap() = config.clone();
    restart_watcher(&app);
    let _ = app.emit("config-changed", &config);
    Ok(config)
}

#[tauri::command]
fn clear_session(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<bool, String> {
    let path = current_config(&state).state_file();
    let removed = agentlight_core::clear_session(&path, &session_id).map_err(|e| e.to_string())?;
    if removed {
        emit_snapshot(&app);
    }
    Ok(removed)
}

#[tauri::command]
fn clear_done(app: AppHandle, state: State<'_, AppState>) -> Result<usize, String> {
    let path = current_config(&state).state_file();
    let removed = agentlight_core::clear_done(&path).map_err(|e| e.to_string())?;
    if removed > 0 {
        emit_snapshot(&app);
    }
    Ok(removed)
}

#[tauri::command]
fn pick_state_file(app: AppHandle, state: State<'_, AppState>) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let Some(chosen) = app.dialog().file().blocking_pick_file() else {
        return Ok(None);
    };
    let path = chosen.into_path().map_err(|e| e.to_string())?;
    let path_string = path.to_string_lossy().to_string();

    let mut config = current_config(&state);
    config.state_path = Some(path_string.clone());
    agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = config.clone();

    restart_watcher(&app);
    let _ = app.emit("config-changed", &config);
    Ok(Some(path_string))
}

#[tauri::command]
fn set_always_on_top(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_always_on_top(enabled)
            .map_err(|e| e.to_string())?;
    }
    let mut config = current_config(&state);
    if config.always_on_top != enabled {
        config.always_on_top = enabled;
        agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;
        *state.config.lock().unwrap() = config.clone();
        let _ = app.emit("config-changed", &config);
    }
    Ok(())
}

#[tauri::command]
fn resize_window(app: AppHandle, width: f64, height: f64) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_size(tauri::LogicalSize::new(width, height));
    }
}

#[tauri::command]
fn window_minimize(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.minimize();
    }
}

#[tauri::command]
fn window_hide(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn current_config(state: &State<'_, AppState>) -> Config {
    state.config.lock().unwrap().clone()
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn toggle_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let visible = window.is_visible().unwrap_or(false);
        if visible {
            let _ = window.hide();
        } else {
            show_window(app);
        }
    }
}

fn apply_autostart(app: &AppHandle, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;

    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(error) = result {
        eprintln!("agentlight: could not update autostart: {error}");
    }
}

fn file_signature(path: &Path) -> Option<(u64, u128)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((metadata.len(), modified))
}

/// Rebuild the snapshot and push it to the frontend, with edge-triggered
/// notifications on the side.
fn emit_snapshot(app: &AppHandle) {
    let config = current_config(&app.state::<AppState>());
    let snapshot = agentlight_core::build_snapshot(&config.state_file(), &config);
    maybe_notify(app, &config, &snapshot);
    let _ = app.emit("state-changed", &snapshot);
}

fn maybe_notify(app: &AppHandle, config: &Config, snapshot: &Snapshot) {
    if !config.notifications {
        return;
    }
    use tauri_plugin_notification::NotificationExt;

    let state = app.state::<AppState>();
    let mut previous = state.previous.lock().unwrap();

    for session in &snapshot.sessions {
        if session.status == Status::NeedsHelp
            && previous.get(&session.session_id) != Some(&Status::NeedsHelp)
        {
            let _ = app
                .notification()
                .builder()
                .title("AgentLight")
                .body(format!("\"{}\" needs help", session.name))
                .show();
        }
    }

    previous.clear();
    for session in &snapshot.sessions {
        previous.insert(session.session_id.clone(), session.status);
    }
}

/// Bump the generation and start a fresh watcher for the current config.
fn restart_watcher(app: &AppHandle) {
    app.state::<AppState>()
        .generation
        .fetch_add(1, Ordering::SeqCst);
    spawn_watcher(app.clone());
}

/// Watch the state file. `notify` gives instant updates where the filesystem
/// supports it; a `recv_timeout` poll is the backstop for network/bind-mount
/// filesystems (for example a Windows host reading a container volume) that
/// emit no events at all.
fn spawn_watcher(app: AppHandle) {
    let (config, generation) = {
        let state = app.state::<AppState>();
        (
            state.config.lock().unwrap().clone(),
            state.generation.load(Ordering::SeqCst),
        )
    };
    let path = config.state_file();
    let poll = config.poll_ms.max(250);

    std::thread::spawn(move || {
        let directory = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |_| {
            let _ = tx.send(());
        })
        .ok();
        if let Some(watcher) = watcher.as_mut() {
            let _ = watcher.watch(&directory, notify::RecursiveMode::NonRecursive);
        }

        let mut last = file_signature(&path);
        emit_snapshot(&app);

        loop {
            if app.state::<AppState>().generation.load(Ordering::SeqCst) != generation {
                break;
            }
            let _ = rx.recv_timeout(Duration::from_millis(poll));
            let signature = file_signature(&path);
            if signature != last {
                last = signature;
                emit_snapshot(&app);
            }
        }
    });
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show / Hide", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit AgentLight", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &settings, &separator, &quit])?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => toggle_window(app),
            "settings" => {
                show_window(app);
                let _ = app.emit("open-settings", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() {
    let config_path = agentlight_core::config_path();
    let config = agentlight_core::load_config(&config_path);

    tauri::Builder::default()
        // Must be registered first so a second launch forwards to this one.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            config: Mutex::new(config.clone()),
            config_path,
            generation: AtomicU64::new(0),
            previous: Mutex::new(HashMap::new()),
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_config,
            set_config,
            clear_session,
            clear_done,
            pick_state_file,
            set_always_on_top,
            resize_window,
            window_minimize,
            window_hide,
            quit_app,
        ])
        .setup(move |app| {
            let handle = app.handle();

            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.set_always_on_top(config.always_on_top);
            }
            apply_autostart(handle, config.start_at_login);

            setup_tray(handle)?;
            spawn_watcher(handle.clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // The widget lives in the tray: closing the window hides it.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running AgentLight");
}

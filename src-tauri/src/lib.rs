//! Tauri shell for AgentLight.
//!
//! All state parsing, aggregation, watching, and notification policy live in
//! `agentlight-core`. This layer only wires the engine to a window, a tray
//! icon, and IPC commands.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agentlight_core::{
    ClawlightFileSource, Config, Engine, SessionKey, Snapshot, SourceCommand, SourceId, Update,
    CLAWLIGHT_SOURCE_ID,
};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, RunEvent, State,
    WebviewWindow, WindowEvent,
};
use tauri_plugin_autostart::MacosLauncher;

/// The embedded server's state, as shown in the settings view.
#[derive(serde::Serialize)]
struct ServerStatus {
    enabled: bool,
    url: Option<String>,
    admin_token_set: bool,
    pairing_code: Option<String>,
    pairing_expires_at: Option<String>,
    devices: Vec<agentlight_server::DeviceInfo>,
}

/// Shared app state managed by Tauri.
struct AppState {
    /// Owns source lifecycle, merge, retention, and notification edges.
    engine: Arc<Engine>,
    config: Mutex<Config>,
    config_path: PathBuf,
    /// Whether the window is currently shown. Tracked explicitly because
    /// `Window::is_visible` reads a cache that can lag after `hide()`.
    visible: AtomicBool,
    /// Which view owns the window right now (`mini`, `detail`, or `settings`).
    window_mode: Mutex<String>,
    /// Top-left of the collapsed window, restored when expanding did not move
    /// it. `None` until the first expand.
    mini_anchor: Mutex<Option<PhysicalPosition<i32>>>,
    /// Last position we placed the window at programmatically. Lets a real
    /// user drag be told apart from our own clamping move.
    applied_position: Mutex<Option<PhysicalPosition<i32>>>,
    /// Opt-in embedded HTTP hub over the same engine. Stopped on exit.
    server: Mutex<Option<agentlight_server::ServerHandle>>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    state.engine.snapshot_now()
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
    // The settings view is write-only for the admin token: a non-empty legacy
    // `server_token` is the user typing a new secret, so hash it here and drop
    // the plaintext before anything is persisted.
    let mut config = config;
    if let Some(token) = config.server_token.take() {
        let token = token.trim();
        if !token.is_empty() {
            config.server_token_hash = Some(agentlight_server::hash_token(token));
        }
    }
    let config = config.sanitized();
    let previous = current_config(&state);

    // Reject a bad bind before persisting or stopping the running server, so
    // the old handle keeps serving and the typo never lands on disk.
    if config.server_enabled {
        parse_server_bind(&config.server_bind)?;
    }

    agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_always_on_top(config.always_on_top);
    }
    apply_autostart(&app, config.start_at_login);

    *state.config.lock().unwrap() = config.clone();
    state.engine.set_config(config.clone());
    restart_source(&state.engine, &config);
    restart_server(&app, &state, &previous, &config)?;
    let _ = app.emit("config-changed", &config);
    Ok(config)
}

#[tauri::command]
fn clear_session(state: State<'_, AppState>, session_id: String) -> Result<bool, String> {
    let key = SessionKey::new(SourceId::new(CLAWLIGHT_SOURCE_ID), session_id);
    let existed = state.engine.has_session(&key);
    state
        .engine
        .dispatch(SourceCommand::RemoveSession(key))
        .map_err(|e| e.to_string())?;
    if existed {
        state.engine.refresh();
    }
    Ok(existed)
}

#[tauri::command]
fn clear_done(state: State<'_, AppState>) -> Result<usize, String> {
    let removed = state.engine.clear_done().map_err(|e| e.to_string())?;
    if removed > 0 {
        state.engine.refresh();
    }
    Ok(removed)
}

#[tauri::command]
fn get_server_status(state: State<'_, AppState>) -> ServerStatus {
    let admin_token_set = current_config(&state).server_token_hash.is_some();
    match state.server.lock().unwrap().as_ref() {
        Some(handle) => {
            let pair = handle.pair_info();
            ServerStatus {
                enabled: true,
                url: Some(format!("http://{}/", handle.addr())),
                admin_token_set,
                pairing_code: Some(pair.code),
                pairing_expires_at: Some(pair.expires_at),
                devices: handle.devices(),
            }
        }
        None => ServerStatus {
            enabled: false,
            url: None,
            admin_token_set,
            pairing_code: None,
            pairing_expires_at: None,
            devices: Vec::new(),
        },
    }
}

#[tauri::command]
fn regenerate_pairing(state: State<'_, AppState>) -> Result<agentlight_server::PairInfo, String> {
    let server = state.server.lock().unwrap();
    let handle = server.as_ref().ok_or("server is not running")?;
    Ok(handle.regenerate_pairing())
}

#[tauri::command]
fn revoke_device(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let server = state.server.lock().unwrap();
    let handle = server.as_ref().ok_or("server is not running")?;
    Ok(handle.revoke_device(&id))
}

// `async` here means Tauri runs the body on a worker thread. The blocking
// native file dialog deadlocks if called on the main thread, where a plain
// synchronous command would run.
#[tauri::command(async)]
fn pick_state_file(app: AppHandle, state: State<'_, AppState>) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let Some(chosen) = app.dialog().file().blocking_pick_file() else {
        return Ok(None);
    };
    let path = chosen.into_path().map_err(|e| e.to_string())?;
    let path_string = path.to_string_lossy().to_string();

    let previous = current_config(&state);
    let mut config = previous.clone();
    config.state_path = Some(path_string.clone());
    agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = config.clone();

    state.engine.set_config(config.clone());
    restart_source(&state.engine, &config);
    // The hub shares the engine, so a source change needs no rebind; only a
    // server-field change restarts it.
    restart_server(&app, &state, &previous, &config)?;
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
        state.engine.set_config(config.clone());
        let _ = app.emit("config-changed", &config);
    }
    Ok(())
}

/// Resize the window for a view and keep it inside the monitor work area.
///
/// `mode` is the target view. Going back to `mini` restores the position the
/// collapsed window had before expanding, unless the user dragged the window
/// in the meantime. Expanding clamps so the larger window stays on screen.
#[tauri::command]
fn resize_window(app: AppHandle, width: f64, height: f64, mode: String) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical = LogicalSize::new(width, height);
    let target: PhysicalSize<u32> = logical.to_physical(scale);
    let state = app.state::<AppState>();

    if mode == "mini" {
        let current = window.outer_position().ok();
        let moved = match (current, *state.applied_position.lock().unwrap()) {
            (Some(now), Some(applied)) => now != applied,
            _ => true,
        };
        let _ = window.set_size(logical);
        let desired = if moved {
            current
        } else {
            *state.mini_anchor.lock().unwrap()
        };
        if let Some(desired) = desired.or(current) {
            let placed = clamp_to_work_area(&window, desired, target);
            let _ = window.set_position(placed);
            *state.applied_position.lock().unwrap() = Some(placed);
        }
        *state.window_mode.lock().unwrap() = "mini".to_string();
    } else {
        if *state.window_mode.lock().unwrap() == "mini" {
            if let Ok(position) = window.outer_position() {
                *state.mini_anchor.lock().unwrap() = Some(position);
            }
        }
        let _ = window.set_size(logical);
        if let Ok(current) = window.outer_position() {
            let placed = clamp_to_work_area(&window, current, target);
            if placed != current {
                let _ = window.set_position(placed);
                *state.applied_position.lock().unwrap() = Some(placed);
            }
        }
        *state.window_mode.lock().unwrap() = mode;
    }
}

/// Pull `desired` inside the current monitor work area, leaving the window
/// fully visible. Falls back to the primary monitor when needed.
fn clamp_to_work_area(
    window: &WebviewWindow,
    desired: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return desired;
    };
    let area = monitor.work_area();
    let left = area.position.x;
    let top = area.position.y;
    let right = area.position.x + area.size.width as i32;
    let bottom = area.position.y + area.size.height as i32;
    let max_x = (right - size.width as i32).max(left);
    let max_y = (bottom - size.height as i32).max(top);
    PhysicalPosition::new(desired.x.clamp(left, max_x), desired.y.clamp(top, max_y))
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
    app.state::<AppState>()
        .visible
        .store(false, Ordering::SeqCst);
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

/// Rebuild the source for the current config. Dropping the old source stops its
/// watcher; the new one emits an initial change on startup.
fn restart_source(engine: &Engine, config: &Config) {
    engine.replace_source(Arc::new(ClawlightFileSource::from_config(config)));
}

fn parse_server_bind(value: &str) -> Result<SocketAddr, String> {
    value
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid server bind \"{value}\": {error}"))
}

/// Start the embedded hub over the shared engine and remember the handle.
fn start_server(app: &AppHandle, state: &AppState, config: &Config) -> Result<(), String> {
    let bind = parse_server_bind(&config.server_bind)?;
    let mut server_config = agentlight_server::ServerConfig::new(
        bind,
        config.server_token_hash.clone(),
        config.clone(),
    );
    // Keep paired devices beside the config so they survive server restarts.
    server_config.devices_path = state
        .config_path
        .parent()
        .map(|dir| dir.join("devices.json"));
    let handle = agentlight_server::start((*state.engine).clone(), server_config)
        .map_err(|error| format!("could not start server: {error}"))?;
    eprintln!(
        "agentlight: embedded server listening on http://{}/",
        handle.addr()
    );
    *state.server.lock().unwrap() = Some(handle);
    let _ = app.emit("server-changed", ());
    Ok(())
}

/// Stop the embedded hub if one is running.
fn stop_server(state: &AppState) {
    if let Some(mut handle) = state.server.lock().unwrap().take() {
        handle.shutdown();
    }
}

/// Restart the hub only when a server field changed. The engine is shared, so
/// a `state_path` change alone must not churn the socket.
fn restart_server(
    app: &AppHandle,
    state: &AppState,
    previous: &Config,
    config: &Config,
) -> Result<(), String> {
    let changed = previous.server_enabled != config.server_enabled
        || previous.server_bind != config.server_bind
        || previous.server_token_hash != config.server_token_hash;
    // Also start when the config wants the server but no handle is live (for
    // example a failed startup). Otherwise Start would be a no-op.
    let running = state.server.lock().unwrap().is_some();
    if !changed && running == config.server_enabled {
        return Ok(());
    }
    stop_server(state);
    if config.server_enabled {
        start_server(app, state, config)?;
    }
    Ok(())
}

/// Forward an engine update: the wire snapshot to the frontend, and any
/// notification edges to a desktop toast.
fn emit_update(app: &AppHandle, update: &Update) {
    let _ = app.emit("state-changed", &update.snapshot);
    if update.notifications.is_empty() {
        return;
    }
    use tauri_plugin_notification::NotificationExt;

    for notification in &update.notifications {
        let _ = app
            .notification()
            .builder()
            .title(notification.title.as_str())
            .body(notification.body.as_str())
            .show();
    }
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    app.state::<AppState>()
        .visible
        .store(true, Ordering::SeqCst);
}

fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    // Use our own flag, not `is_visible`: the latter reflects a cached state
    // that can still read "visible" right after `hide()`, so a second tray
    // click would hide an already-hidden window and look unresponsive.
    let visible = app.state::<AppState>().visible.load(Ordering::SeqCst);
    let minimized = window.is_minimized().unwrap_or(false);
    if visible && !minimized {
        let _ = window.hide();
        app.state::<AppState>()
            .visible
            .store(false, Ordering::SeqCst);
    } else {
        show_window(app);
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

/// Whether the config file on disk still carries the legacy plaintext admin
/// token. Used once at startup to persist the migrated, hashed config.
fn legacy_plaintext_token_present(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("server_token").cloned())
        .is_some_and(|token| token.is_string())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() {
    let config_path = agentlight_core::config_path();
    let config = agentlight_core::load_config(&config_path);
    if legacy_plaintext_token_present(&config_path) {
        // `load_config` already hashed the plaintext into `server_token_hash`;
        // write the sanitized config back so the file no longer holds the
        // secret. Best-effort: a read-only config dir keeps the old file.
        let _ = agentlight_core::save_config(&config_path, &config);
    }
    let engine = Arc::new(Engine::new(config.clone()));

    let app = tauri::Builder::default()
        // Must be registered first so a second launch forwards to this one.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        // Persist only the position: the frontend owns the window size per
        // view, and restoring an old size would clip the light/detail layout.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(tauri_plugin_window_state::StateFlags::POSITION)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            engine: engine.clone(),
            config: Mutex::new(config.clone()),
            config_path,
            visible: AtomicBool::new(true),
            window_mode: Mutex::new("mini".to_string()),
            mini_anchor: Mutex::new(None),
            applied_position: Mutex::new(None),
            server: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_config,
            set_config,
            clear_session,
            clear_done,
            get_server_status,
            regenerate_pairing,
            revoke_device,
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

            // Register the update sink before starting the source so the
            // initial file change is delivered on startup.
            let sink_handle = handle.clone();
            engine.subscribe(Arc::new(move |update: Update| {
                emit_update(&sink_handle, &update);
            }));
            engine.add_source(Arc::new(ClawlightFileSource::from_config(&config)));

            if config.server_enabled {
                let state = handle.state::<AppState>();
                if let Err(error) = start_server(&handle, &state, &config) {
                    eprintln!("agentlight: embedded server not started: {error}");
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // The widget lives in the tray: closing the window hides it.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                window
                    .state::<AppState>()
                    .visible
                    .store(false, Ordering::SeqCst);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building AgentLight");

    app.run(|app_handle, event| {
        if let RunEvent::Exit = event {
            stop_server(&app_handle.state::<AppState>());
        }
    });
}

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
    ClawlightFileSource, Config, Engine, SessionKey, Snapshot, SourceCommand, SourceId, SourceKind,
    StateSource, Update, CLAWLIGHT_SOURCE_ID,
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

    // Reject a bad bind before doing anything else.
    if config.server_enabled {
        parse_server_bind(&config.server_bind)?;
    }

    // Apply the server transition before persisting. A failed start keeps the
    // previous handle and leaves the persisted `server_enabled` untouched.
    apply_server_change(&app, &state, &previous, &config)?;

    agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_always_on_top(config.always_on_top);
    }
    apply_autostart(&app, config.start_at_login);

    *state.config.lock().unwrap() = config.clone();
    state.engine.set_config(config.clone());
    if source_fields_changed(&previous, &config) {
        restart_source(&state.engine, &config);
    }
    let _ = app.emit("config-changed", &config);
    Ok(config)
}

#[tauri::command]
fn clear_session(state: State<'_, AppState>, session_id: String) -> Result<bool, String> {
    // Use whatever source the engine is currently reading so Remove works in
    // both file and hub mode.
    let source = state
        .engine
        .source_id()
        .unwrap_or_else(|| SourceId::new(CLAWLIGHT_SOURCE_ID));
    let key = SessionKey::new(source, session_id);
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
    // Choosing a file is an implicit switch back to file mode.
    config.source_kind = SourceKind::File;
    agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = config.clone();

    state.engine.set_config(config.clone());
    restart_source(&state.engine, &config);
    // The hub shares the engine, so a source change needs no rebind; only a
    // server-field change restarts it.
    apply_server_change(&app, &state, &previous, &config)?;
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

/// Build the configured source adapter: a local clawlight file or a remote hub.
fn build_source(config: &Config) -> Arc<dyn StateSource> {
    match config.source_kind {
        SourceKind::File => Arc::new(ClawlightFileSource::from_config(config)),
        SourceKind::Hub => Arc::new(agentlight_hub_client::HubSource::new(
            agentlight_hub_client::HubConfig {
                id: agentlight_hub_client::DEFAULT_HUB_ID.to_string(),
                base_url: config.hub_url.clone(),
                token: config.hub_token.clone(),
                poll_ms: config.poll_ms,
            },
        )),
    }
}

/// Rebuild the source for the current config. `set_source` drops the previous
/// adapter (stopping its watcher/poll thread) even when the new one has a
/// different id, as when switching between the file and hub sources.
fn restart_source(engine: &Engine, config: &Config) {
    engine.set_source(build_source(config));
}

/// Whether a config change alters where or how fast state is read, so the
/// source adapter must be rebuilt. Display-only fields (yellow mode, retention,
/// notifications) do not need a new source.
fn source_fields_changed(previous: &Config, config: &Config) -> bool {
    previous.source_kind != config.source_kind
        || previous.state_path != config.state_path
        || previous.hub_url != config.hub_url
        || previous.hub_token != config.hub_token
        || previous.poll_ms != config.poll_ms
}

fn parse_server_bind(value: &str) -> Result<SocketAddr, String> {
    value
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid server bind \"{value}\": {error}"))
}

/// Start the embedded hub over the shared engine and return its handle. Does
/// not touch the current handle or emit an event; callers decide what to do on
/// success or failure so a failed start cannot drop a working server.
fn start_server_handle(
    state: &AppState,
    config: &Config,
) -> Result<agentlight_server::ServerHandle, String> {
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
    Ok(handle)
}

/// Stop the embedded hub if one is running.
fn stop_server(state: &AppState) {
    if let Some(mut handle) = state.server.lock().unwrap().take() {
        handle.shutdown();
    }
}

fn emit_server_changed(app: &AppHandle) {
    let _ = app.emit("server-changed", ());
}

/// Apply a server start/stop/restart described by `config`, keeping the
/// persisted flag and the live handle consistent.
///
/// A failed start is never fatal to a running server: when the bind address is
/// unchanged the old handle is restored on a failed restart, and when the
/// address changes the new server is bound before the old one is stopped. The
/// caller persists the config only if this returns `Ok`, so the on-disk
/// `server_enabled` never changes on a failed start.
fn apply_server_change(
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

    if !config.server_enabled {
        stop_server(state);
        emit_server_changed(app);
        return Ok(());
    }

    // Restarting on the same address must free it before rebinding, so keep the
    // previous config to restore the server if the new one cannot bind.
    let same_bind_live = running && previous.server_bind == config.server_bind;
    if same_bind_live {
        stop_server(state);
        match start_server_handle(state, config) {
            Ok(server) => {
                *state.server.lock().unwrap() = Some(server);
                emit_server_changed(app);
                Ok(())
            }
            Err(error) => {
                match start_server_handle(state, previous) {
                    Ok(server) => *state.server.lock().unwrap() = Some(server),
                    Err(restore_error) => eprintln!(
                        "agentlight: could not restore server after a failed restart: {restore_error}"
                    ),
                }
                emit_server_changed(app);
                Err(error)
            }
        }
    } else {
        // A different address, or nothing running: bind first so a failure
        // leaves any existing server untouched.
        match start_server_handle(state, config) {
            Ok(server) => {
                stop_server(state);
                *state.server.lock().unwrap() = Some(server);
                emit_server_changed(app);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
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
            engine.add_source(build_source(&config));

            if config.server_enabled {
                let state = handle.state::<AppState>();
                match start_server_handle(&state, &config) {
                    // The persisted flag is the user's preference and is left
                    // untouched when a startup bind fails; next launch retries.
                    Ok(server) => *state.server.lock().unwrap() = Some(server),
                    Err(error) => eprintln!("agentlight: embedded server not started: {error}"),
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

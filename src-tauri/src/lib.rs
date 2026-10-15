//! Tauri shell for AgentLight.
//!
//! All state parsing, aggregation, watching, and notification policy live in
//! `agentlight-core`. This layer only wires the engine to a window, a tray
//! icon, and IPC commands.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentlight_core::{
    ClawlightFileSource, CollapseStyle, Config, Engine, SessionKey, Snapshot, SourceCommand,
    SourceId, SourceKind, StateSource, Update, CLAWLIGHT_SOURCE_ID,
};
use tauri::menu::{ContextMenu, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, RunEvent, State,
    WebviewWindow, WindowEvent,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_clipboard_manager::ClipboardExt;

mod diagnostics;

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
    /// When the window last moved. A drag also blurs the window, so the
    /// collapse-on-outside-click check uses this to tell a drag from a real
    /// click somewhere else.
    last_moved: Mutex<Option<Instant>>,
    /// When the webview reported a drag start (mousedown on a drag region).
    /// Focus loss while dragging must not collapse the view; the flag is only
    /// trusted briefly, so a missed mouseup cannot suppress a real click away.
    dragging_since: Mutex<Option<Instant>>,
    /// Last `/healthz` probe of the configured hub, for Settings and
    /// diagnostics. Filled at startup, when the hub source changes, and by the
    /// Settings Refresh button; never probed implicitly on open or save.
    hub_status: Mutex<Option<(HubStatus, Instant)>>,
    /// Top-left of the collapsed window, restored when expanding did not move
    /// it. `None` until the first collapse.
    mini_anchor: Mutex<Option<PhysicalPosition<i32>>>,
    /// Work area of the monitor the collapsed window calls home. Kept so an
    /// expand/collapse pair stays on that monitor even when the grown window
    /// would otherwise overlap a neighbouring one.
    mini_home: Mutex<Option<Rect>>,
    /// Last position we placed the window at programmatically. Lets a real
    /// user drag be told apart from our own clamping move.
    applied_position: Mutex<Option<PhysicalPosition<i32>>>,
    /// Opt-in embedded HTTP hub over the same engine. Stopped on exit.
    server: Mutex<Option<agentlight_server::ServerHandle>>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// How long a webview-reported drag start suppresses the outside-click
/// collapse. Focus loss while starting a drag fires right after mousedown, so a
/// sub-second window is enough; a missed mouseup must not suppress real
/// click-away for longer than this.
const DRAG_GRACE: Duration = Duration::from_millis(750);
/// Fallback for a missed drag report: a window that moved in the last few
/// hundred milliseconds is being dragged, not clicked away from.
const MOVE_GRACE: Duration = Duration::from_millis(250);

// `async` runs the body on a worker thread instead of the main thread. The
// snapshot may read a file (or a hub-backed cache) and a mutation may do a
// blocking HTTP round trip to the hub, so neither may block the event loop and
// freeze the widget.
#[tauri::command(async)]
fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    state.engine.snapshot_now()
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    current_config(&state)
}

/// The app version and build id, for the Settings "Version" line so the running
/// build is identifiable (the package version alone does not change per build).
#[tauri::command]
fn app_version() -> String {
    version_string()
}

fn version_string() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("AGENTLIGHT_BUILD_ID") {
        Some(build) if !build.is_empty() && build != "dev" => format!("{version} · {build}"),
        _ => version.to_string(),
    }
}

/// The last `/healthz` result for a hub: reachability plus the hub's own
/// version. Cached in [`AppState`] so opening Settings or copying diagnostics
/// never waits on the network; only startup, a hub-source change, and the
/// Settings Refresh button probe.
#[derive(Clone, serde::Serialize)]
struct HubStatus {
    url: String,
    reachable: bool,
    version: Option<String>,
    schema_version: Option<u32>,
    error: Option<String>,
    /// When this result was checked, local time.
    checked_at: String,
}

/// One `/healthz` request. `url` is the address to probe (the caller decides
/// whether that is the saved config or the Settings field). The duration is
/// logged: one slow request is the usual reason a hub command feels slow.
fn probe_hub(url: &str, token: Option<String>) -> HubStatus {
    let started = Instant::now();
    let client = agentlight_hub_client::HubClient::new(agentlight_hub_client::HubConfig {
        id: agentlight_hub_client::DEFAULT_HUB_ID.to_string(),
        base_url: url.to_string(),
        token,
        poll_ms: agentlight_hub_client::DEFAULT_POLL_MS,
    });
    let checked_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let status = match client.health() {
        Ok(health) => HubStatus {
            url: url.to_string(),
            reachable: true,
            version: health.version,
            schema_version: Some(health.schema_version),
            error: None,
            checked_at,
        },
        Err(error) => HubStatus {
            url: url.to_string(),
            reachable: false,
            version: None,
            schema_version: None,
            error: Some(error.to_string()),
            checked_at,
        },
    };
    diagnostics::line(
        "info",
        &format!(
            "hub probe {url}: {} ({} ms, checked {})",
            hub_status_line(&status),
            started.elapsed().as_millis(),
            status.checked_at
        ),
    );
    status
}

/// Probe the configured hub and remember the result. No-op for a file source.
fn refresh_hub_status(state: &AppState) {
    let config = state.config.lock().unwrap().clone();
    if config.source_kind != SourceKind::Hub {
        return;
    }
    let status = probe_hub(&config.hub_url, config.hub_token.clone());
    *state.hub_status.lock().unwrap() = Some((status, Instant::now()));
}

/// Probe off the caller's thread: startup and hub-source changes must not block
/// the window or a Save.
fn refresh_hub_status_in_background(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        refresh_hub_status(&state);
    });
}

/// A one-line rendering of a hub status, shared by the log and diagnostics.
fn hub_status_line(status: &HubStatus) -> String {
    if status.reachable {
        format!(
            "reachable · version {} · schema {}",
            status.version.as_deref().unwrap_or("unknown"),
            status
                .schema_version
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".to_string())
        )
    } else {
        format!(
            "unreachable: {}",
            status.error.as_deref().unwrap_or("no answer")
        )
    }
}

/// Probe a hub address on demand (the Settings Refresh button) and cache it. A
/// blank `url` falls back to the configured hub address. `async` because it is
/// a network call; `/healthz` needs no token, so this works before pairing.
#[tauri::command(async)]
fn get_hub_status(state: State<'_, AppState>, url: String, token: Option<String>) -> HubStatus {
    let config = current_config(&state);
    let url = url.trim();
    let base_url = if url.is_empty() {
        config.hub_url.clone()
    } else {
        url.to_string()
    };
    let token = token.or_else(|| config.hub_token.clone());
    let status = probe_hub(&base_url, token);
    *state.hub_status.lock().unwrap() = Some((status.clone(), Instant::now()));
    status
}

/// The cached probe result, without touching the network. Settings renders this
/// on open; diagnostics use it too.
#[tauri::command]
fn get_hub_status_cached(state: State<'_, AppState>) -> Option<HubStatus> {
    state
        .hub_status
        .lock()
        .unwrap()
        .as_ref()
        .map(|(status, _)| status.clone())
}

/// Put a redacted diagnostics report (versions, source state, hub health, log
/// tail) on the clipboard. Tokens are reported as set/not set, never copied.
#[tauri::command(async)]
fn copy_diagnostics(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let report = build_diagnostics(&state).render();
    app.clipboard()
        .write_text(report)
        .map_err(|e| e.to_string())
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

    // A no-op save writes nothing and applies nothing: the settings form can be
    // saved repeatedly (Ctrl+S) without a disk write or an autostart/topmost
    // toggle. An enabled-but-not-running server is the exception, so a failed
    // startup can be retried by saving.
    let server_running = state.server.lock().unwrap().is_some();
    if config == previous && config.server_enabled == server_running {
        return Ok(config);
    }

    // Reject a bad bind before doing anything else.
    if config.server_enabled {
        parse_server_bind(&config.server_bind)?;
    }

    // Apply the server transition before persisting. A failed start keeps the
    // previous handle and leaves the persisted `server_enabled` untouched.
    apply_server_change(&app, &state, &previous, &config)?;

    agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;

    if config.always_on_top != previous.always_on_top {
        if let Some(window) = app.get_webview_window("main") {
            // The pin / "Always on top" setting is the single authority on topmost.
            let _ = window.set_always_on_top(config.always_on_top);
        }
    }
    if config.start_at_login != previous.start_at_login {
        apply_autostart(&app, config.start_at_login);
    }

    *state.config.lock().unwrap() = config.clone();
    state.engine.set_config(config.clone());
    if source_fields_changed(&previous, &config) {
        restart_source(&state.engine, &config);
        // A hub address/token change is a new connection: probe it in the
        // background so Settings and diagnostics have a fresh cached result.
        refresh_hub_status_in_background(&app);
    }
    diagnostics::line(
        "info",
        &format!("config saved to {}", state.config_path.display()),
    );
    let _ = app.emit("config-changed", &config);
    Ok(config)
}

/// Remove one session. `async` keeps the blocking hub round trip off the main
/// thread, so the window keeps painting while the command is in flight.
#[tauri::command(async)]
fn clear_session(state: State<'_, AppState>, session_id: String) -> Result<bool, String> {
    let started = Instant::now();
    // Use whatever source the engine is currently reading so Remove works in
    // both file and hub mode.
    let source = state
        .engine
        .source_id()
        .unwrap_or_else(|| SourceId::new(CLAWLIGHT_SOURCE_ID));
    let key = SessionKey::new(source, session_id.clone());
    let existed = state.engine.has_session(&key);
    state
        .engine
        .dispatch(SourceCommand::RemoveSession(key))
        .map_err(|e| e.to_string())?;
    if existed {
        state.engine.refresh();
    }
    // The duration covers the hub round trip(s); the log is the place to see
    // where a slow Remove spent its time.
    diagnostics::line(
        "info",
        &format!(
            "remove {session_id} (existed={existed}): {} ms",
            started.elapsed().as_millis()
        ),
    );
    Ok(existed)
}

#[tauri::command(async)]
fn clear_done(state: State<'_, AppState>) -> Result<usize, String> {
    let started = Instant::now();
    let removed = state.engine.clear_done().map_err(|e| e.to_string())?;
    if removed > 0 {
        state.engine.refresh();
    }
    diagnostics::line(
        "info",
        &format!(
            "clear done ({removed}): {} ms",
            started.elapsed().as_millis()
        ),
    );
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

// Same reason as `pick_state_file`: the blocking native dialog must not run on
// the main thread. The chosen path is returned to the caller, which owns
// persisting it.
#[tauri::command]
async fn pick_sound_file(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;

    app.dialog()
        .file()
        .add_filter("WAV audio", &["wav"])
        .blocking_pick_file()
        .and_then(|chosen| chosen.into_path().ok())
        .map(|path| path.to_string_lossy().to_string())
}

/// Open an `http`/`https` URL in the OS default browser without waiting for it
/// to exit. Any other scheme is rejected.
///
/// Windows uses `ShellExecuteW` directly: it hands the URL to the shell without
/// spawning a process, so no console window flashes. macOS and Linux use the
/// usual launchers.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let scheme = url.trim().to_ascii_lowercase();
    if !(scheme.starts_with("http://") || scheme.starts_with("https://")) {
        return Err("only http:// and https:// URLs can be opened".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        use std::ffi::{c_void, OsStr};
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                hwnd: *mut c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> *mut c_void;
        }

        const SW_SHOWNORMAL: i32 = 1;
        let wide =
            |value: &OsStr| -> Vec<u16> { value.encode_wide().chain(std::iter::once(0)).collect() };
        let operation = wide(OsStr::new("open"));
        let file = wide(OsStr::new(&url));

        // Safety: both pointers are valid NUL-terminated UTF-16 strings for the
        // duration of the call, and `ShellExecuteW` copies what it needs. The
        // return value is a shell HINSTANCE; anything <= 32 is an error code.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        return if result as isize > 32 {
            Ok(())
        } else {
            Err(format!(
                "could not open the URL (ShellExecute code {})",
                result as isize
            ))
        };
    }

    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = url;
        Err("opening a browser is not supported on this platform".to_string())
    }
}

#[tauri::command]
fn set_always_on_top(app: AppHandle, enabled: bool) -> Result<(), String> {
    apply_always_on_top(&app, enabled)
}

/// Apply and persist the pin / "Always on top" flag. Shared by the command, the
/// Settings checkbox (via the command), and the collapsed view's context menu.
fn apply_always_on_top(app: &AppHandle, enabled: bool) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_always_on_top(enabled)
            .map_err(|e| e.to_string())?;
    }
    let state = app.state::<AppState>();
    let mut config = current_config(&state);
    if config.always_on_top != enabled {
        config.always_on_top = enabled;
        agentlight_core::save_config(&state.config_path, &config).map_err(|e| e.to_string())?;
        *state.config.lock().unwrap() = config.clone();
        state.engine.set_config(config.clone());
        diagnostics::line("info", &format!("always on top -> {enabled}"));
        let _ = app.emit("config-changed", &config);
    }
    Ok(())
}

/// The webview reports a drag start (mousedown on a drag region) and end
/// (mouseup) so the outside-click collapse can tell a drag from a real click
/// away without deferring real clicks.
#[tauri::command]
fn set_dragging(state: State<'_, AppState>, dragging: bool) {
    *state.dragging_since.lock().unwrap() = dragging.then(Instant::now);
}

/// Resize the window for a view and keep it inside the right monitor work area.
///
/// `mode` is the target view. Going back to `mini` restores the position the
/// collapsed window had before expanding, unless the user dragged the window
/// in the meantime. Expanding clamps so the larger window stays on the monitor
/// the collapsed window called home, rather than following the grown window
/// onto a neighbouring monitor.
#[tauri::command]
fn resize_window(app: AppHandle, width: f64, height: f64, mode: String) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let state = app.state::<AppState>();

    if mode == "mini" {
        let logical = LogicalSize::new(width, height);
        let target: PhysicalSize<u32> = logical.to_physical(scale);

        let current = window.outer_position().ok();
        let moved = match (current, *state.applied_position.lock().unwrap()) {
            (Some(now), Some(applied)) => now != applied,
            _ => true,
        };
        let desired = if moved {
            current
        } else {
            *state.mini_anchor.lock().unwrap()
        };
        let Some(desired) = desired.or(current) else {
            *state.window_mode.lock().unwrap() = "mini".to_string();
            return;
        };
        // Pick the home monitor from the collapsed rectangle, not the expanded
        // one, so a window parked near a seam stays put.
        let collapsed = Rect::new(desired.x, desired.y, target.width, target.height);
        let home = work_area_for(&window, collapsed).unwrap_or(collapsed);
        let (x, y) = clamp_rect((desired.x, desired.y), (target.width, target.height), home);
        let placed = PhysicalPosition::new(x, y);
        // The collapsed window is fixed-size again: every edge of a frameless
        // resizable window resizes, and the OS has no corner-only mode, so the
        // aspect-snapping experiment fought the user's drags. The layout sizes
        // come from the frontend.
        let _ = window.set_resizable(false);
        let _ = window.set_size(logical);
        let _ = window.set_position(placed);
        *state.mini_anchor.lock().unwrap() = Some(placed);
        *state.mini_home.lock().unwrap() = Some(home);
        *state.applied_position.lock().unwrap() = Some(placed);
        *state.window_mode.lock().unwrap() = "mini".to_string();
        return;
    }

    let logical = LogicalSize::new(width, height);
    let target: PhysicalSize<u32> = logical.to_physical(scale);
    let was_mini = *state.window_mode.lock().unwrap() == "mini";
    let current = window.outer_position().ok();
    // Expanding from `mini` stays on the collapsed window's monitor; every
    // other view change stays on whichever monitor the window already uses.
    let home = if was_mini {
        let collapsed = current.and_then(|position| {
            window
                .outer_size()
                .ok()
                .map(|size| Rect::new(position.x, position.y, size.width, size.height))
        });
        match collapsed {
            Some(rect) => {
                *state.mini_anchor.lock().unwrap() = Some(PhysicalPosition::new(rect.x, rect.y));
                let home = work_area_for(&window, rect).unwrap_or(rect);
                *state.mini_home.lock().unwrap() = Some(home);
                Some(home)
            }
            None => *state.mini_home.lock().unwrap(),
        }
    } else {
        current_work_area(&window).or_else(|| *state.mini_home.lock().unwrap())
    };

    // Every view is fixed-size; only the view commands change it.
    let _ = window.set_resizable(false);
    let _ = window.set_size(logical);
    if let (Some(home), Some(current)) = (home, current) {
        let (x, y) = clamp_rect((current.x, current.y), (target.width, target.height), home);
        let placed = PhysicalPosition::new(x, y);
        if placed != current {
            let _ = window.set_position(placed);
            *state.applied_position.lock().unwrap() = Some(placed);
        }
    }
    *state.window_mode.lock().unwrap() = mode;
}

/// Work area (position + size) of the monitor a window rectangle belongs to:
/// the one with the largest overlap. Ties go to the monitor under the window's
/// top-left corner, then to `fallback`.
fn select_home_area(window_rect: Rect, monitors: &[Rect], fallback: Rect) -> Rect {
    let Some(max_overlap) = monitors
        .iter()
        .map(|monitor| overlap_area(window_rect, *monitor))
        .max()
    else {
        return fallback;
    };
    let candidates: Vec<Rect> = monitors
        .iter()
        .copied()
        .filter(|monitor| overlap_area(window_rect, *monitor) == max_overlap)
        .collect();
    if let [only] = candidates.as_slice() {
        return *only;
    }
    let mut containing = candidates
        .iter()
        .copied()
        .filter(|monitor| monitor.contains(window_rect.x, window_rect.y));
    let first = containing.next();
    match (first, containing.next()) {
        (Some(only), None) => only,
        _ => fallback,
    }
}

/// Intersection area of two rectangles, `0` when they do not overlap.
fn overlap_area(a: Rect, b: Rect) -> i64 {
    let width = (a.right().min(b.right()) as i64) - (a.x.max(b.x) as i64);
    let height = (a.bottom().min(b.bottom()) as i64) - (a.y.max(b.y) as i64);
    width.max(0) * height.max(0)
}

/// Pull `desired` inside `area`, keeping the top-left edge when it already fits
/// so a growing window extends right and down before it shifts.
fn clamp_rect(desired: (i32, i32), size: (u32, u32), area: Rect) -> (i32, i32) {
    let max_x = (area.right() - size.0 as i32).max(area.x);
    let max_y = (area.bottom() - size.1 as i32).max(area.y);
    (
        desired.0.clamp(area.x, max_x),
        desired.1.clamp(area.y, max_y),
    )
}

/// Work area of the monitor that owns `window_rect`, falling back to the
/// primary monitor.
fn work_area_for(window: &WebviewWindow, window_rect: Rect) -> Option<Rect> {
    let monitors: Vec<Rect> = window
        .available_monitors()
        .ok()
        .unwrap_or_default()
        .iter()
        .map(work_area_rect)
        .collect();
    let primary = window
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| work_area_rect(&monitor));
    let fallback = primary.or_else(|| monitors.first().copied())?;
    Some(select_home_area(window_rect, &monitors, fallback))
}

/// Work area of the monitor the window currently sits on, falling back to the
/// primary monitor.
fn current_work_area(window: &WebviewWindow) -> Option<Rect> {
    window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
        .map(|monitor| work_area_rect(&monitor))
}

fn work_area_rect(monitor: &tauri::Monitor) -> Rect {
    let area = monitor.work_area();
    Rect::new(
        area.position.x,
        area.position.y,
        area.size.width,
        area.size.height,
    )
}

/// Integer rectangle used by the window-placement helpers. Kept GUI-free so the
/// overlap/tie/clamp rules are easy to reason about and unit test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Rect {
    fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn right(&self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
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

/// A one-line source description for logs and diagnostics.
fn source_summary(config: &Config) -> String {
    match config.source_kind {
        SourceKind::Hub => format!("hub {}", config.hub_url),
        SourceKind::File | SourceKind::Push => {
            format!("file {}", config.state_file().display())
        }
    }
}

/// The log file beside the config, so a portable exe keeps it with its state.
fn log_file_path(state: &AppState) -> PathBuf {
    state
        .config_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("agentlight.log"))
        .unwrap_or_else(|| PathBuf::from("agentlight.log"))
}

/// Build the redacted "Copy diagnostics" report. Never includes a token or a
/// token hash; only whether one is set.
fn build_diagnostics(state: &AppState) -> diagnostics::Diagnostics {
    let config = state.config.lock().unwrap().clone();
    let snapshot = state.engine.snapshot_now();
    // Cached only: Copy diagnostics must be instant and must never hold the
    // clipboard back on a network probe.
    let hub = if config.source_kind == SourceKind::Hub {
        match state.hub_status.lock().unwrap().as_ref() {
            Some((status, _)) => format!(
                "{} (checked {})",
                hub_status_line(status),
                status.checked_at
            ),
            None => "not probed yet".to_string(),
        }
    } else {
        "not a hub source".to_string()
    };
    let server = match state.server.lock().unwrap().as_ref() {
        Some(handle) => format!(
            "running at http://{}/ (devices: {})",
            handle.addr(),
            handle.devices().len()
        ),
        None => "stopped".to_string(),
    };
    let source = match config.source_kind {
        SourceKind::Hub => format!(
            "hub {} (token set: {})",
            config.hub_url,
            if config.hub_token.is_some() {
                "yes"
            } else {
                "no"
            }
        ),
        SourceKind::File | SourceKind::Push => format!(
            "file {} (poll {} ms)",
            config.state_file().display(),
            config.poll_ms
        ),
    };
    let settings = format!(
        "always_on_top={} notifications={} alarms={} show_done={} show_subagents={} session_link={}",
        config.always_on_top,
        config.notifications,
        config.alarms_enabled,
        config.show_done,
        config.show_subagents,
        config.session_link.key()
    );
    let log_path = log_file_path(state);
    diagnostics::Diagnostics {
        app: version_string(),
        os: format!(
            "{} {} · webview {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            tauri::webview_version().unwrap_or_else(|_| "unknown".to_string())
        ),
        config_path: state.config_path.display().to_string(),
        source,
        settings,
        snapshot: format!(
            "ok={} aggregate={} counts={{needs_help:{} active:{} inactive:{} done:{} total:{}}} error={}",
            snapshot.ok,
            snapshot.aggregate,
            snapshot.counts.needs_help,
            snapshot.counts.active,
            snapshot.counts.inactive,
            snapshot.counts.done,
            snapshot.counts.total,
            snapshot.error.as_deref().unwrap_or("none")
        ),
        server: format!(
            "{server} (admin token set: {})",
            if config.server_token_hash.is_some() {
                "yes"
            } else {
                "no"
            }
        ),
        hub,
        log_tail: diagnostics::tail(&log_path),
        log_path: log_path.display().to_string(),
    }
}

/// Register the app's `AppUserModelID` so Windows can resolve its toast
/// notifications. An installed build gets one from the NSIS shortcut;
/// an unpackaged build (the portable exe) must register it itself or Windows
/// shows the raw AUMID as the toast's app name. `HKCU\Software\Classes\
/// AppUserModelId\<id>` with `DisplayName` and `IconUri` is the documented
/// unpackaged-app registration (Windows 10 1709+). Best-effort: a failure only
/// affects notifications. No-op elsewhere.
#[cfg(target_os = "windows")]
fn register_app_user_model_id(identifier: &str) {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegCreateKeyExW(
            hkey: *mut c_void,
            sub_key: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            desired: u32,
            security: *mut c_void,
            result: *mut *mut c_void,
            disposition: *mut u32,
        ) -> i32;
        fn RegSetValueExW(
            hkey: *mut c_void,
            name: *const u16,
            reserved: u32,
            kind: u32,
            data: *const u8,
            bytes: u32,
        ) -> i32;
        fn RegCloseKey(hkey: *mut c_void) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SetCurrentProcessExplicitAppUserModelID(app_id: *const u16) -> i32;
    }

    // Predefined registry handles are negative 32-bit values sign-extended to
    // pointer width (`(HKEY)(ULONG_PTR)((LONG)0x80000001)`); zero-extending
    // 0x80000001 produces an invalid handle and every call fails.
    const HKEY_CURRENT_USER: *mut c_void = 0x8000_0001u32 as i32 as isize as *mut c_void;
    // `KEY_WRITE`: set values and create the key if needed.
    const KEY_WRITE: u32 = 0x20006;
    const REG_SZ: u32 = 1;

    let wide =
        |value: &OsStr| -> Vec<u16> { value.encode_wide().chain(std::iter::once(0)).collect() };
    let key_path = format!("Software\\Classes\\AppUserModelId\\{identifier}");
    let identifier = wide(OsStr::new(identifier));
    let key_path = wide(OsStr::new(&key_path));
    let display_value = wide(OsStr::new("DisplayName"));
    let icon_value = wide(OsStr::new("IconUri"));
    let display_name = wide(OsStr::new("AgentLight"));
    // The exe path gives Windows an icon for the toast; the name is what shows
    // in the header.
    let icon = std::env::current_exe()
        .ok()
        .map(|path| wide(OsStr::new(&path)));

    // Safety: every pointer is valid for its call, the created key is closed
    // before returning, and errors are ignored (notifications are best-effort).
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(identifier.as_ptr());
        let mut key: *mut c_void = ptr::null_mut();
        let mut disposition = 0u32;
        let created = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            ptr::null(),
            0,
            KEY_WRITE,
            ptr::null_mut(),
            &mut key,
            &mut disposition,
        );
        if created != 0 || key.is_null() {
            return;
        }
        let _ = RegSetValueExW(
            key,
            display_value.as_ptr(),
            0,
            REG_SZ,
            display_name.as_ptr().cast(),
            (display_name.len() * 2) as u32,
        );
        if let Some(icon) = &icon {
            let _ = RegSetValueExW(
                key,
                icon_value.as_ptr(),
                0,
                REG_SZ,
                icon.as_ptr().cast(),
                (icon.len() * 2) as u32,
            );
        }
        let _ = RegCloseKey(key);
    }
}

#[cfg(not(target_os = "windows"))]
fn register_app_user_model_id(_identifier: &str) {}

/// A blocking hub client for the configured hub (and diagnostics probes).
fn hub_client(config: &Config) -> agentlight_hub_client::HubClient {
    agentlight_hub_client::HubClient::new(agentlight_hub_client::HubConfig {
        id: agentlight_hub_client::DEFAULT_HUB_ID.to_string(),
        base_url: config.hub_url.clone(),
        token: config.hub_token.clone(),
        poll_ms: config.poll_ms,
    })
}

/// Build the configured source adapter: a local clawlight file or a remote hub.
fn build_source(config: &Config) -> Arc<dyn StateSource> {
    match config.source_kind {
        // Push is a server-only source; the desktop has no ingest surface, so
        // an unexpected push selection falls back to the local file.
        SourceKind::File | SourceKind::Push => Arc::new(ClawlightFileSource::from_config(config)),
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
    diagnostics::line("info", &format!("source: {}", source_summary(config)));
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
    diagnostics::line(
        "info",
        &format!("embedded server listening on http://{}/", handle.addr()),
    );
    Ok(handle)
}

/// Stop the embedded hub if one is running.
fn stop_server(state: &AppState) {
    if let Some(mut handle) = state.server.lock().unwrap().take() {
        diagnostics::line(
            "info",
            &format!("embedded server stopping (http://{}/)", handle.addr()),
        );
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
                    Err(restore_error) => diagnostics::line(
                        "error",
                        &format!(
                            "could not restore server after a failed restart: {restore_error}"
                        ),
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

/// Forward an engine update: the wire snapshot to the frontend, any
/// notification edges to a desktop toast, and any alarm edges to sound.
fn emit_update(app: &AppHandle, update: &Update) {
    let _ = app.emit("state-changed", &update.snapshot);

    if !update.notifications.is_empty() {
        use tauri_plugin_notification::NotificationExt;

        for notification in &update.notifications {
            diagnostics::line(
                "notify",
                &format!("{} — {}", notification.title, notification.body),
            );
            let _ = app
                .notification()
                .builder()
                .title(notification.title.as_str())
                .body(notification.body.as_str())
                .show();
        }
    }

    // Alarms are independent of the notification toggle: the engine already
    // applies `alarm_trigger`, this layer only honors the enable flag.
    if !update.alarms.is_empty() {
        let config = current_config(&app.state::<AppState>());
        if config.alarms_enabled {
            for alarm in &update.alarms {
                diagnostics::line("alarm", &alarm.body);
            }
            play_alarm(config.alarm_sound, config.alarm_volume);
        }
    }
}

/// The bundled default alarm: a short two-note chime, so the default does not
/// depend on a system sound scheme or spawn a console beep.
#[cfg(target_os = "windows")]
static DEFAULT_ALARM: &[u8] = include_bytes!("../assets/alarm.wav");

/// Play the attention alarm on a detached thread so the engine's update sink
/// never blocks on playback.
///
/// Windows-only playback through the Win32 `PlaySoundW` API. The clip (the
/// bundled default or a custom file) is loaded into memory and its samples are
/// scaled by `volume`, so the setting affects only this alarm. No child process
/// is spawned, so no console window flashes. Everywhere else this is a no-op.
fn play_alarm(sound: Option<String>, volume: u8) {
    std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        {
            use std::ffi::{c_void, OsStr};
            use std::os::windows::ffi::OsStrExt;

            #[link(name = "winmm")]
            extern "system" {
                fn PlaySoundW(psz_sound: *const u16, hmod: *mut c_void, fdw_sound: u32) -> i32;
            }

            const SND_NODEFAULT: u32 = 0x0002;
            const SND_MEMORY: u32 = 0x0004;
            const SND_FILENAME: u32 = 0x0002_0000;
            const SND_SYSTEM: u32 = 0x0020_0000;

            // A zero volume means silence: play nothing.
            if volume == 0 {
                return;
            }

            let custom = sound
                .as_deref()
                .map(str::trim)
                .filter(|path| !path.is_empty());

            // Read the clip into memory and scale its samples. An unreadable
            // custom file falls back to filename playback, unscaled.
            let clip: Vec<u8> = match custom {
                Some(path) => match std::fs::read(path) {
                    Ok(bytes) => agentlight_core::wav::scale_wav_pcm16(&bytes, volume),
                    Err(_) => Vec::new(),
                },
                None => agentlight_core::wav::scale_wav_pcm16(DEFAULT_ALARM, volume),
            };

            if clip.is_empty() {
                if let Some(path) = custom {
                    let wide: Vec<u16> = OsStr::new(path)
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect();
                    // Safety: `wide` outlives the synchronous call and the flags
                    // are the documented `PlaySound` constants.
                    unsafe {
                        PlaySoundW(
                            wide.as_ptr(),
                            std::ptr::null_mut(),
                            SND_FILENAME | SND_NODEFAULT | SND_SYSTEM,
                        );
                    }
                }
                return;
            }

            // Safety: `SND_MEMORY` playback is synchronous, so `clip` outlives
            // the call. `SND_SYSTEM` routes the alarm to the system-notification
            // audio session.
            unsafe {
                PlaySoundW(
                    clip.as_ptr().cast(),
                    std::ptr::null_mut(),
                    SND_MEMORY | SND_NODEFAULT | SND_SYSTEM,
                );
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = (sound, volume);
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!("agentlight: alarm sound is only implemented on Windows");
            });
        }
    });
}

/// Play the alarm once, at the given sound and volume, for the Settings "Test"
/// button. Uses the same path as a real alarm so the user hears exactly it.
#[tauri::command]
fn test_alarm(sound: Option<String>, volume: u8) {
    play_alarm(sound, volume);
}

/// Persist a collapsed-view layout chosen from the native context menu.
fn set_collapse_style(app: &AppHandle, style: CollapseStyle) {
    let state = app.state::<AppState>();
    let mut config = current_config(&state);
    if config.collapse_style == style {
        return;
    }
    config.collapse_style = style;
    let _ = agentlight_core::save_config(&state.config_path, &config);
    *state.config.lock().unwrap() = config.clone();
    state.engine.set_config(config.clone());
    let _ = app.emit("config-changed", &config);
}

/// Right-click menu for the collapsed window. Built as a native OS menu so it
/// can extend past the small window instead of being clipped by it.
#[tauri::command]
fn show_mini_menu(app: AppHandle) -> Result<(), String> {
    let config = current_config(&app.state::<AppState>());
    let mark = |style: CollapseStyle| {
        if style == config.collapse_style {
            "\u{2713} "
        } else {
            "   "
        }
    };
    let item = |id: &str, label: String| {
        MenuItem::with_id(&app, id, label, true, None::<&str>).map_err(|e| e.to_string())
    };
    let single = item(
        "mini-single",
        format!("{}Single light", mark(CollapseStyle::Single)),
    )?;
    let horizontal = item(
        "mini-triple",
        format!("{}Horizontal lights", mark(CollapseStyle::Triple)),
    )?;
    let vertical = item(
        "mini-triple-vertical",
        format!("{}Vertical lights", mark(CollapseStyle::TripleVertical)),
    )?;
    let separator = PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?;
    let top = item(
        "mini-top",
        format!(
            "{}Always on top",
            if config.always_on_top {
                "\u{2713} "
            } else {
                "   "
            }
        ),
    )?;
    let separator_two = PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?;
    let hide = item("mini-hide", "Hide".to_string())?;
    let menu = Menu::with_items(
        &app,
        &[
            &single,
            &horizontal,
            &vertical,
            &separator,
            &top,
            &separator_two,
            &hide,
        ],
    )
    .map_err(|e| e.to_string())?;
    let webview = app
        .get_webview_window("main")
        .ok_or("no window")?
        .as_ref()
        .window();
    menu.popup(webview).map_err(|e| e.to_string())
}

/// Whether a focus loss should tuck the expanded view back to the light.
///
/// Immediate on purpose: an outside click must feel instant. The cases that are
/// *not* an outside click are excluded up front instead of being deferred — a
/// click on our own title bar (the OS move loop deactivates the window with the
/// cursor still over it), a webview-reported drag start, and a recent move.
/// Pure so the rules are unit-testable without a window.
fn should_collapse(
    always_on_top: bool,
    mode: &str,
    dragging: bool,
    moved_recently: bool,
    own_window: bool,
) -> bool {
    always_on_top && mode == "detail" && !dragging && !moved_recently && !own_window
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

/// Whether `window` is the foreground (active) window.
///
/// The tray click can blur the window before the click is handled, so
/// `is_focused()` is not reliable there; on Windows the foreground window is
/// asked directly. Elsewhere the focused flag is the best available signal.
fn window_is_foreground(window: &WebviewWindow) -> bool {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetForegroundWindow() -> isize;
        }

        if let Ok(hwnd) = window.hwnd() {
            // Safety: `GetForegroundWindow` takes no arguments and the returned
            // handle is only compared, never dereferenced.
            return unsafe { GetForegroundWindow() } == hwnd.0 as isize;
        }
    }
    window.is_focused().unwrap_or(false)
}

/// Whether the tray menu's "Show / Hide" item should hide. The item is chosen
/// while the window is visible but not the foreground window (the open menu
/// owns the foreground), so unlike the tray *icon* it cannot use focus as the
/// signal; plain visibility is the right one and makes the item deterministic.
fn menu_toggle_hides(visible: bool, minimized: bool) -> bool {
    visible && !minimized
}

/// Whether the cursor was over our own window when it lost focus.
///
/// A click on our title bar starts the OS move loop, which can deactivate the
/// window; an outside click deactivates it with the cursor over the *other*
/// window. Comparing the root window under the cursor at the moment of the
/// focus loss is what tells the two apart, and it works for a plain click as
/// well as a drag — no timing, no webview event needed.
#[cfg(target_os = "windows")]
fn focus_loss_from_own_window(window: &tauri::Window) -> bool {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(point: *mut Point) -> i32;
        fn WindowFromPoint(point: Point) -> *mut c_void;
        fn GetAncestor(hwnd: *mut c_void, flags: u32) -> *mut c_void;
    }

    const GA_ROOT: u32 = 2;
    let Ok(hwnd) = window.hwnd() else {
        return false;
    };
    let ours = hwnd.0 as *mut c_void;
    let mut point = Point { x: 0, y: 0 };
    // Safety: both calls take an out pointer / value by copy, and the returned
    // window handles are only compared, never dereferenced.
    unsafe {
        if GetCursorPos(&mut point) == 0 {
            return false;
        }
        let under_cursor = WindowFromPoint(point);
        if under_cursor.is_null() {
            return false;
        }
        GetAncestor(under_cursor, GA_ROOT) == ours
    }
}

#[cfg(not(target_os = "windows"))]
fn focus_loss_from_own_window(_window: &tauri::Window) -> bool {
    false
}

/// One tray click brings the window back: hidden or minimized windows are
/// shown and focused, and a visible-but-buried window is raised. Only a window
/// that is already the active window is hidden, so the click can never be a
/// no-op (the old toggle could hide a window the user just clicked away from,
/// which made a single click look like it did nothing).
fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    // Use our own flag, not `is_visible`: the latter reflects a cached state
    // that can still read "visible" right after `hide()`, so a second tray
    // click would hide an already-hidden window and look unresponsive.
    let visible = app.state::<AppState>().visible.load(Ordering::SeqCst);
    let minimized = window.is_minimized().unwrap_or(false);
    if menu_toggle_hides(visible, minimized) {
        let _ = window.hide();
        app.state::<AppState>()
            .visible
            .store(false, Ordering::SeqCst);
    } else {
        show_window(app);
    }
}

/// A left click on the tray *icon*: bring the window back when it is hidden,
/// minimized, or buried; hide it only when it is already the active window.
fn toggle_window_from_icon(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let visible = app.state::<AppState>().visible.load(Ordering::SeqCst);
    let minimized = window.is_minimized().unwrap_or(false);
    if visible && !minimized && window_is_foreground(&window) {
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
    // Disabling something that was never enabled fails on Windows (os error 2);
    // skip the no-op write entirely so the log is not noisy on every start.
    if manager.is_enabled().unwrap_or(false) == enabled {
        return;
    }
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(error) = result {
        diagnostics::line("warn", &format!("could not update autostart: {error}"));
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
                toggle_window_from_icon(tray.app_handle());
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

    // Diagnostics first, so a panic and the startup lines land in the log.
    let log_path = config_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("agentlight.log"))
        .unwrap_or_else(|| PathBuf::from("agentlight.log"));
    diagnostics::init(log_path);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        diagnostics::line("panic", &info.to_string());
        default_hook(info);
    }));
    diagnostics::line("info", &format!("AgentLight {} starting", version_string()));
    diagnostics::line("info", &format!("config {}", config_path.display()));
    diagnostics::line("info", &format!("source {}", source_summary(&config)));

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
        .plugin(tauri_plugin_clipboard_manager::init())
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
            last_moved: Mutex::new(None),
            dragging_since: Mutex::new(None),
            hub_status: Mutex::new(None),
            mini_anchor: Mutex::new(None),
            mini_home: Mutex::new(None),
            applied_position: Mutex::new(None),
            server: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_config,
            app_version,
            set_config,
            clear_session,
            clear_done,
            get_server_status,
            get_hub_status,
            get_hub_status_cached,
            copy_diagnostics,
            regenerate_pairing,
            revoke_device,
            pick_state_file,
            pick_sound_file,
            open_url,
            set_always_on_top,
            set_dragging,
            test_alarm,
            show_mini_menu,
            resize_window,
            window_minimize,
            window_hide,
            quit_app,
        ])
        .setup(move |app| {
            let handle = app.handle();

            // Windows resolves toasts through the app identity; register it so
            // the portable build can notify too (an installed build gets it
            // from the NSIS shortcut).
            register_app_user_model_id(&handle.config().identifier);

            if let Some(window) = handle.get_webview_window("main") {
                // The pin / "Always on top" setting is the single authority on
                // whether the window is topmost.
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
            diagnostics::line(
                "info",
                &format!("source starting: {}", source_summary(&config)),
            );
            engine.add_source(build_source(&config));
            if config.source_kind == SourceKind::Hub {
                refresh_hub_status_in_background(handle);
            }

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
        .on_menu_event(|app, event| match event.id.as_ref() {
            "mini-single" => set_collapse_style(app, CollapseStyle::Single),
            "mini-triple" => set_collapse_style(app, CollapseStyle::Triple),
            "mini-triple-vertical" => set_collapse_style(app, CollapseStyle::TripleVertical),
            "mini-top" => {
                let enabled = !current_config(&app.state::<AppState>()).always_on_top;
                if let Err(error) = apply_always_on_top(app, enabled) {
                    diagnostics::line("warn", &format!("could not toggle always on top: {error}"));
                }
            }
            "mini-hide" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
                app.state::<AppState>()
                    .visible
                    .store(false, Ordering::SeqCst);
            }
            _ => {}
        })
        .on_window_event(|window, event| match event {
            // The widget lives in the tray: closing the window hides it.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
                window
                    .state::<AppState>()
                    .visible
                    .store(false, Ordering::SeqCst);
            }
            // A drag also blurs the window; remember moves so the collapse
            // below can tell a drag from a real click outside.
            WindowEvent::Moved(_) => {
                *window.state::<AppState>().last_moved.lock().unwrap() = Some(Instant::now());
            }
            // With "Always on top" on, clicking away from the expanded view
            // tucks it back to the collapsed light — immediately, so the click
            // feels instant. Settings is left alone so an in-progress edit is
            // not lost to an outside click. A focus loss is only an outside
            // click when the cursor is not over our own window (a title-bar
            // click/drag starts the OS move loop, which also deactivates the
            // window); the webview drag report and a recent move are backups.
            WindowEvent::Focused(false) => {
                let state = window.state::<AppState>();
                let always_on_top = state.config.lock().unwrap().always_on_top;
                let mode = state.window_mode.lock().unwrap().clone();
                let dragging = state
                    .dragging_since
                    .lock()
                    .unwrap()
                    .is_some_and(|at| at.elapsed() < DRAG_GRACE);
                let moved_recently = state
                    .last_moved
                    .lock()
                    .unwrap()
                    .is_some_and(|at| at.elapsed() < MOVE_GRACE);
                let own_window = focus_loss_from_own_window(&window);
                if should_collapse(always_on_top, &mode, dragging, moved_recently, own_window) {
                    let _ = window.emit("collapse", ());
                } else if always_on_top && mode == "detail" {
                    // A title-bar click or drag also blurs the window; log why
                    // it did not collapse so a misfire is diagnosable.
                    diagnostics::line(
                        "debug",
                        &format!(
                            "focus lost: collapse skipped (own_window={own_window} dragging={dragging} moved={moved_recently})"
                        ),
                    );
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building AgentLight");

    app.run(|app_handle, event| {
        if let RunEvent::Exit = event {
            stop_server(&app_handle.state::<AppState>());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect::new(x, y, width, height)
    }

    fn test_state(config: Config, config_path: PathBuf) -> AppState {
        AppState {
            engine: Arc::new(Engine::new(config.clone())),
            config: Mutex::new(config),
            config_path,
            visible: AtomicBool::new(true),
            window_mode: Mutex::new("mini".to_string()),
            last_moved: Mutex::new(None),
            dragging_since: Mutex::new(None),
            hub_status: Mutex::new(None),
            mini_anchor: Mutex::new(None),
            mini_home: Mutex::new(None),
            applied_position: Mutex::new(None),
            server: Mutex::new(None),
        }
    }

    fn cached_hub(reachable: bool, version: Option<&str>, error: Option<&str>) -> HubStatus {
        HubStatus {
            url: "http://127.0.0.1:1".to_string(),
            reachable,
            version: version.map(str::to_string),
            schema_version: reachable.then_some(1),
            error: error.map(str::to_string),
            checked_at: "2026-10-15 12:00:00".to_string(),
        }
    }

    #[test]
    fn diagnostics_report_is_complete_and_never_leaks_tokens() {
        let config = Config {
            source_kind: SourceKind::Hub,
            // Port 1 refuses immediately, so a probe cannot hang.
            hub_url: "http://127.0.0.1:1".to_string(),
            hub_token: Some("client-secret".to_string()),
            server_token_hash: Some("admin-hash".to_string()),
            ..Config::default()
        };
        let state = test_state(config, PathBuf::from("/tmp/AgentLight/config.json"));
        *state.hub_status.lock().unwrap() = Some((
            cached_hub(false, None, Some("connection refused")),
            Instant::now(),
        ));

        let report = build_diagnostics(&state).render();
        assert!(report.contains("AgentLight diagnostics"), "{report}");
        assert!(report.contains(&format!("app:       {}", version_string())));
        assert!(report.contains("os:        "), "{report}");
        assert!(report.contains("config:    /tmp/AgentLight/config.json"));
        assert!(
            report.contains("source:    hub http://127.0.0.1:1 (token set: yes)"),
            "{report}"
        );
        assert!(report.contains("settings:  always_on_top=true"), "{report}");
        assert!(report.contains("snapshot:  ok=false"), "{report}");
        assert!(
            report.contains("server:    stopped (admin token set: yes)"),
            "{report}"
        );
        assert!(
            report.contains(
                "hub:       unreachable: connection refused (checked 2026-10-15 12:00:00)"
            ),
            "{report}"
        );
        assert!(
            report.contains("log:") && report.contains("agentlight.log"),
            "{report}"
        );
        assert!(!report.contains("client-secret"), "the hub token leaked");
        assert!(!report.contains("admin-hash"), "the admin hash leaked");

        // Before any probe the report says so, and a file source has no hub.
        let state = test_state(
            Config {
                source_kind: SourceKind::Hub,
                ..Config::default()
            },
            PathBuf::from("/tmp/AgentLight/config.json"),
        );
        assert!(
            build_diagnostics(&state)
                .render()
                .contains("hub:       not probed yet"),
            "{report}"
        );

        let file_state = test_state(
            Config::default(),
            PathBuf::from("/tmp/AgentLight/config.json"),
        );
        let report = build_diagnostics(&file_state).render();
        assert!(report.contains("hub:       not a hub source"), "{report}");
        assert!(report.contains("source:    file"), "{report}");
    }

    #[test]
    fn hub_status_line_renders_both_outcomes() {
        assert_eq!(
            hub_status_line(&cached_hub(true, Some("0.5.0"), None)),
            "reachable · version 0.5.0 · schema 1"
        );
        assert_eq!(
            hub_status_line(&cached_hub(true, None, None)),
            "reachable · version unknown · schema 1"
        );
        assert_eq!(
            hub_status_line(&cached_hub(false, None, Some("refused"))),
            "unreachable: refused"
        );
        assert_eq!(
            hub_status_line(&cached_hub(false, None, None)),
            "unreachable: no answer"
        );
    }

    #[test]
    fn tray_menu_toggle_follows_visibility_not_focus() {
        assert!(menu_toggle_hides(true, false));
        assert!(!menu_toggle_hides(true, true));
        assert!(!menu_toggle_hides(false, false));
        assert!(!menu_toggle_hides(false, true));
    }

    fn side_by_side() -> (Rect, Rect) {
        (rect(0, 0, 1920, 1080), rect(1920, 0, 1920, 1080))
    }

    #[test]
    fn overlap_is_zero_when_disjoint() {
        assert_eq!(
            overlap_area(rect(0, 0, 100, 100), rect(200, 0, 100, 100)),
            0
        );
        assert_eq!(
            overlap_area(rect(0, 0, 100, 100), rect(50, 50, 100, 100)),
            2500
        );
    }

    #[test]
    fn home_prefers_the_monitor_with_most_overlap() {
        let (left, right) = side_by_side();
        // Fully on the left monitor, hard against the seam.
        let window = rect(1800, 100, 120, 88);
        assert_eq!(select_home_area(window, &[left, right], right), left);
    }

    #[test]
    fn tie_follows_the_top_left_corner_over_the_fallback() {
        let (left, right) = side_by_side();
        // Straddles the seam with 20px on each monitor; top-left is on `left`.
        let window = rect(1900, 100, 40, 40);
        assert_eq!(select_home_area(window, &[left, right], right), left);
    }

    #[test]
    fn outside_every_monitor_falls_back_to_primary() {
        let (left, right) = side_by_side();
        let window = rect(9000, 9000, 88, 88);
        assert_eq!(select_home_area(window, &[left, right], right), right);
    }

    #[test]
    fn exact_boundary_is_deterministic() {
        let (left, right) = side_by_side();
        let window = rect(1920, 100, 88, 88);
        let first = select_home_area(window, &[left, right], left);
        assert_eq!(first, select_home_area(window, &[left, right], left));
        assert_eq!(first, right);
    }

    #[test]
    fn expand_keeps_the_left_edge_until_it_overflows() {
        let home = rect(0, 0, 1920, 1080);
        // Already inside: left edge is preserved.
        assert_eq!(clamp_rect((100, 200), (420, 548), home), (100, 200));
        // Overflows the right edge: shift left just enough to fit.
        assert_eq!(clamp_rect((1600, 200), (420, 548), home), (1500, 200));
        // Overflows the bottom edge: shift up just enough to fit.
        assert_eq!(clamp_rect((100, 900), (420, 548), home), (100, 532));
    }

    #[test]
    fn collapse_restores_the_anchor_clamped_to_home() {
        let home = rect(0, 0, 1920, 1080);
        assert_eq!(clamp_rect((-50, -50), (88, 88), home), (0, 0));
    }

    #[test]
    fn collapse_needs_detail_topmost_and_no_drag() {
        // A plain click outside the expanded view: collapse immediately.
        assert!(should_collapse(true, "detail", false, false, false));
        // The cursor was over our own window: a title-bar click or drag, never
        // an outside click.
        assert!(!should_collapse(true, "detail", false, false, true));
        // A webview-reported drag start: no collapse while it is fresh.
        assert!(!should_collapse(true, "detail", true, false, false));
        // The move fallback: a window that just moved is being dragged.
        assert!(!should_collapse(true, "detail", false, true, false));
        assert!(!should_collapse(true, "detail", true, true, true));
        // Only the detail view collapses; Settings keeps its draft, and the
        // collapsed light has nothing to collapse.
        assert!(!should_collapse(true, "settings", false, false, false));
        assert!(!should_collapse(true, "mini", false, false, false));
        // Without the pin there is no click-away behavior.
        assert!(!should_collapse(false, "detail", false, false, false));
    }
}

//! Tauri shell for AgentLight.
//!
//! All state parsing, aggregation, watching, and notification policy live in
//! `agentlight-core`. This layer only wires the engine to a window, a tray
//! icon, and IPC commands.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentlight_core::{
    ClawlightFileSource, CollapseStyle, Config, Engine, SessionKey, Snapshot, SourceCommand,
    SourceId, SourceKind, StateSource, Update, CLAWLIGHT_SOURCE_ID,
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
    /// it. `None` until the first collapse.
    mini_anchor: Mutex<Option<PhysicalPosition<i32>>>,
    /// Work area of the monitor the collapsed window calls home. Kept so an
    /// expand/collapse pair stays on that monitor even when the grown window
    /// would otherwise overlap a neighbouring one.
    mini_home: Mutex<Option<Rect>>,
    /// Last position we placed the window at programmatically. Lets a real
    /// user drag be told apart from our own clamping move.
    applied_position: Mutex<Option<PhysicalPosition<i32>>>,
    /// Logical size of the last programmatic expand/collapse resize. A
    /// `Resized` event matching it is ours and must not be persisted as a user
    /// resize.
    programmatic_resize: Mutex<Option<(f64, f64)>>,
    /// Ignore `Resized` events until this instant; covers the brief window
    /// while a programmatic resize (including the resizable-style toggle) is
    /// still settling.
    resize_guard_until: Mutex<Option<Instant>>,
    /// Newest collapsed size seen from a user resize, awaiting persistence.
    pending_mini_size: Mutex<Option<(f64, f64)>>,
    /// Bumped on every user resize so the debounce saver can coalesce a drag.
    resize_generation: AtomicU64,
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
    let mut config = config.sanitized();
    let previous = current_config(&state);

    // A different collapsed layout has a different design size, so the size the
    // user dragged for the old layout would otherwise be re-applied and look
    // "stuck". Clear it so the new style's own size takes over.
    if previous.collapse_style != config.collapse_style {
        config.mini_width = None;
        config.mini_height = None;
    }

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
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let scheme = url.trim().to_ascii_lowercase();
    if !(scheme.starts_with("http://") || scheme.starts_with("https://")) {
        return Err("only http:// and https:// URLs can be opened".to_string());
    }

    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();

    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(&url).spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(&url).spawn();

    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    let spawned: std::io::Result<std::process::Child> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening a browser is not supported on this platform",
    ));

    spawned.map(|_| ()).map_err(|error| error.to_string())
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
    *state.resize_guard_until.lock().unwrap() = Some(Instant::now() + RESIZE_GUARD);

    if mode == "mini" {
        // A persisted collapsed size wins over the built-in style size the
        // frontend passes, so the user's last drag survives restarts.
        let config = current_config(&state);
        let (width, height) = match (config.mini_width, config.mini_height) {
            (Some(width), Some(height)) => (width, height),
            _ => (width, height),
        };
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
        *state.programmatic_resize.lock().unwrap() = Some((width, height));
        // Only the collapsed window is user-resizable, and it scales on a
        // single diagonal: the minimum leaves the lights and labels fully
        // visible, unlike the OS's much larger default tracking minimum.
        let (min_w, min_h) = mini_min(&config);
        let _ = window.set_min_size(Some(LogicalSize::new(min_w, min_h)));
        let _ = window.set_resizable(true);
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

    *state.programmatic_resize.lock().unwrap() = Some((width, height));
    // Expanded views keep their fixed size; only the collapsed window resizes.
    let _ = window.set_min_size(None::<LogicalSize<f64>>);
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

/// How long a collapsed resize must be quiet before it is written to disk.
const RESIZE_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// How long to ignore `Resized` events after a programmatic resize.
const RESIZE_GUARD: Duration = Duration::from_millis(300);
/// How often the opt-in topmost re-assert fires.
const TOPMOST_REASSERT_INTERVAL: Duration = Duration::from_secs(3);

/// Record a user resize of the collapsed window as the pending size to persist.
/// Ignored for expanded views and for our own programmatic resizes.
fn note_user_resize(scale: f64, state: &AppState, size: PhysicalSize<u32>) {
    if *state.window_mode.lock().unwrap() != "mini" {
        return;
    }
    if let Some(until) = *state.resize_guard_until.lock().unwrap() {
        if Instant::now() < until {
            return;
        }
    }
    let logical = size.to_logical::<f64>(scale);
    let programmatic = *state.programmatic_resize.lock().unwrap();
    if let Some((width, height)) = programmatic {
        if size_matches(width, logical.width) && size_matches(height, logical.height) {
            return;
        }
        // A different size means the user has taken over; stop ignoring ours.
        *state.programmatic_resize.lock().unwrap() = None;
    }
    *state.pending_mini_size.lock().unwrap() = Some((logical.width, logical.height));
    state.resize_generation.fetch_add(1, Ordering::SeqCst);
}

/// Whether two logical sizes are the same within a sub-pixel tolerance.
fn size_matches(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0
}

/// The design size of the collapsed window for a layout. Its aspect ratio is
/// what the user drags on, so the collapsed window scales on one diagonal.
fn mini_base(config: &Config) -> (f64, f64) {
    match config.collapse_style {
        CollapseStyle::Single => (88.0, 88.0),
        CollapseStyle::Triple => (172.0, 68.0),
        CollapseStyle::TripleVertical => (68.0, 172.0),
    }
}

/// The smallest collapsed size that still shows the lights, and the labels when
/// they are on. With labels the design height is the floor so text is never
/// clipped; without them the window may shrink somewhat.
fn mini_min(config: &Config) -> (f64, f64) {
    let (width, height) = mini_base(config);
    if config.mini_show_labels {
        (width, height)
    } else {
        (width * 0.7, height * 0.7)
    }
}

/// Snap a collapsed size back onto the layout's aspect ratio, never below the
/// minimum. Scale follows whichever axis the user moved furthest, so dragging a
/// corner grows or shrinks both dimensions together.
fn snap_to_aspect(size: (f64, f64), config: &Config) -> (f64, f64) {
    let (base_w, base_h) = mini_base(config);
    let (min_w, min_h) = mini_min(config);
    let floor = (min_w / base_w).max(min_h / base_h);
    let scale = (size.0 / base_w).max(size.1 / base_h).max(floor);
    (base_w * scale, base_h * scale)
}

/// Re-assert a `WebviewWindow`'s topmost flag using the native Win32 call. The
/// Tauri `set_always_on_top` can be ignored over an exclusive/borderless
/// full-screen window; `SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE)` is the
/// documented way to stay above it. No-op on other platforms.
#[cfg(target_os = "windows")]
fn reassert_topmost(window: &WebviewWindow) {
    use std::ffi::c_void;

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowPos(
            hwnd: *mut c_void,
            insert_after: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
    }

    const HWND_TOPMOST: *mut c_void = -1isize as *mut c_void;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOACTIVATE: u32 = 0x0010;

    if let Ok(hwnd) = window.hwnd() {
        // Safety: `hwnd` is this window's live handle and the constants are the
        // documented `SetWindowPos` flags.
        unsafe {
            SetWindowPos(
                hwnd.0,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn reassert_topmost(_window: &WebviewWindow) {}

/// Background saver for the collapsed window size. Coalesces a drag into one
/// write: it wakes on a fixed interval and only persists the newest pending
/// size once the generation has stopped changing. Detached; ends with the
/// process.
fn spawn_resize_persister(app: AppHandle) {
    std::thread::spawn(move || {
        let mut saved = 0u64;
        loop {
            std::thread::sleep(RESIZE_SAVE_DEBOUNCE);
            let state = app.state::<AppState>();
            if *state.window_mode.lock().unwrap() != "mini" {
                continue;
            }
            let generation = state.resize_generation.load(Ordering::SeqCst);
            if generation == saved {
                continue;
            }
            let Some((width, height)) = *state.pending_mini_size.lock().unwrap() else {
                continue;
            };
            let mut config = current_config(&state);
            if config.mini_width == Some(width) && config.mini_height == Some(height) {
                saved = generation;
                continue;
            }
            config.mini_width = Some(width);
            config.mini_height = Some(height);
            match agentlight_core::save_config(&state.config_path, &config) {
                Ok(()) => {
                    *state.config.lock().unwrap() = config.clone();
                    state.engine.set_config(config);
                    saved = generation;
                }
                Err(error) => eprintln!("agentlight: could not save collapsed size: {error}"),
            }
        }
    });
}

/// Keep re-asserting always-on-top so the widget stays above borderless or
/// exclusive full-screen apps. A no-op unless both `topmost_reassert` and
/// `always_on_top` are set. Detached; ends with the process.
fn spawn_topmost_reassert(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(TOPMOST_REASSERT_INTERVAL);
        let config = current_config(&app.state::<AppState>());
        if !(config.topmost_reassert && config.always_on_top) {
            continue;
        }
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_always_on_top(true);
            reassert_topmost(&window);
        }
    });
}

/// After a collapsed resize, snap the window back onto the layout's aspect
/// ratio so it can only be dragged on one diagonal. Records the corrected size
/// and marks it programmatic so its own `Resized` event is not re-snapped.
fn snap_mini_aspect(window: &tauri::Window, state: &AppState, scale: f64) {
    if *state.window_mode.lock().unwrap() != "mini" {
        return;
    }
    if let Some(until) = *state.resize_guard_until.lock().unwrap() {
        if Instant::now() < until {
            return;
        }
    }
    let config = state.config.lock().unwrap().clone();
    let Ok(size) = window.inner_size() else {
        return;
    };
    let logical = size.to_logical::<f64>(scale);
    // Our own programmatic sizes (startup, view switches) already match the
    // requested view; only snap sizes the user actually dragged.
    if let Some((w, h)) = *state.programmatic_resize.lock().unwrap() {
        if size_matches(w, logical.width) && size_matches(h, logical.height) {
            return;
        }
    }
    let (width, height) = snap_to_aspect((logical.width, logical.height), &config);
    if size_matches(width, logical.width) && size_matches(height, logical.height) {
        return;
    }
    *state.programmatic_resize.lock().unwrap() = Some((width, height));
    *state.resize_guard_until.lock().unwrap() = Some(Instant::now() + RESIZE_GUARD);
    let _ = window.set_size(LogicalSize::new(width, height));
    *state.pending_mini_size.lock().unwrap() = Some((width, height));
    state.resize_generation.fetch_add(1, Ordering::SeqCst);
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

/// Forward an engine update: the wire snapshot to the frontend, any
/// notification edges to a desktop toast, and any alarm edges to sound.
fn emit_update(app: &AppHandle, update: &Update) {
    let _ = app.emit("state-changed", &update.snapshot);

    if !update.notifications.is_empty() {
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

    // Alarms are independent of the notification toggle: the engine already
    // applies `alarm_trigger`, this layer only honors the enable flag.
    if !update.alarms.is_empty() {
        let config = current_config(&app.state::<AppState>());
        if config.alarms_enabled {
            play_alarm(config.alarm_sound);
        }
    }
}

/// Play the attention alarm on a detached thread so the engine's update sink
/// never blocks on process spawn or sound playback.
///
/// Windows-only playback, with no extra crates: a custom file goes through
/// PowerShell's `Media.SoundPlayer`, the default through the console beep.
/// Everywhere else this is a no-op.
fn play_alarm(sound: Option<String>) {
    std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        {
            use std::process::Command;

            let script = match sound {
                // Single-quote the path for PowerShell and escape any embedded
                // single quote by doubling it.
                Some(path) => {
                    let escaped = path.replace('\'', "''");
                    format!("(New-Object Media.SoundPlayer -ArgumentList '{escaped}').PlaySync()")
                }
                None => "[console]::beep(880,150)".to_string(),
            };
            let _ = Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .spawn();
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = sound;
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!("agentlight: alarm sound is only implemented on Windows");
            });
        }
    });
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
            mini_home: Mutex::new(None),
            applied_position: Mutex::new(None),
            programmatic_resize: Mutex::new(None),
            resize_guard_until: Mutex::new(None),
            pending_mini_size: Mutex::new(None),
            resize_generation: AtomicU64::new(0),
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
            pick_sound_file,
            open_url,
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
                // Treat the startup size as ours so its `Resized` event is not
                // mistaken for the user dragging the collapsed window.
                if let Ok(size) = window.inner_size() {
                    let scale = window.scale_factor().unwrap_or(1.0);
                    let logical = size.to_logical::<f64>(scale);
                    let state = handle.state::<AppState>();
                    *state.programmatic_resize.lock().unwrap() =
                        Some((logical.width, logical.height));
                }
            }
            apply_autostart(handle, config.start_at_login);

            setup_tray(handle)?;

            spawn_resize_persister(handle.clone());
            spawn_topmost_reassert(handle.clone());

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
            WindowEvent::Resized(size) => {
                let state = window.state::<AppState>();
                let scale = window.scale_factor().unwrap_or(1.0);
                note_user_resize(scale, &state, *size);
                snap_mini_aspect(window, &state, scale);
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
}

//! Host-side reader for clawlight's `state.json`.
//!
//! This crate is deliberately free of Tauri and platform GUI dependencies so
//! its parsing, aggregation, and write logic can be unit-tested on any host.
//! The Tauri app in `src-tauri` is a thin shell over it.
//!
//! Contract: [`docs/state-format.md`](../../docs/state-format.md).

pub mod config;
pub mod session;
pub mod snapshot;
pub mod state;

pub use config::{config_path, load as load_config, save as save_config, Config, YellowMode};
pub use session::{harness_badge, load_sessions, DisplaySession, DONE_RETENTION};
pub use snapshot::{build_snapshot, build_snapshot_at, Counts, Snapshot};
pub use state::{
    acquire_state_lock, aggregate, clear_done, clear_session, default_state_path, load_state,
    reap_stale, resolve_state_path, write_state_atomic, Aggregate, Error, HookState, Load, Result,
    SessionStatus, Status, STALE_AFTER_HOURS,
};

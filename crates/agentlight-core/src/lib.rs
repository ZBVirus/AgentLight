//! Host-side reader for clawlight's `state.json`, plus the source/engine seam
//! that lets other backends feed the same policy.
//!
//! This crate is deliberately free of Tauri and platform GUI dependencies so
//! its parsing, aggregation, and write logic can be unit-tested on any host.
//! The Tauri app in `src-tauri` is a thin shell over it.
//!
//! Contract: [`docs/state-format.md`](../../docs/state-format.md).

pub mod config;
pub mod engine;
pub mod session;
pub mod snapshot;
pub mod source;
pub mod state;

pub use config::{
    config_path, hash_token, load as load_config, save as save_config, Config, SourceKind,
    YellowMode,
};
pub use engine::{Engine, Notification, Update, UpdateSink, Urgency};
pub use session::{display_session, harness_badge, load_sessions, DisplaySession, DONE_RETENTION};
pub use snapshot::{build_snapshot, build_snapshot_at, Counts, Snapshot};
pub use source::clawlight::{ClawlightFileSource, CLAWLIGHT_SOURCE_ID};
pub use source::{
    Capabilities, FixtureSource, Session, SessionKey, SourceCommand, SourceEvent, SourceHealth,
    SourceId, SourceSink, SourceSnapshot, StateSource,
};
pub use state::{
    acquire_state_lock, aggregate, aggregate_statuses, clear_done, clear_session,
    default_state_path, load_state, reap_stale, resolve_state_path, write_state_atomic, Aggregate,
    Error, HookState, Load, Result, SessionStatus, Status, STALE_AFTER_HOURS,
};

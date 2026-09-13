//! Push-based [`StateSource`](agentlight_core::StateSource): agents report
//! normalized session events over the hub instead of writing a shared file.
//!
//! This crate is synchronous and runtime-free, like `agentlight-core`. It holds
//! the latest event per `session_id` in memory and exposes it through the
//! normalized source model; the server's `POST /api/v1/ingest` route feeds it.
//! Retention, aggregation, and notification policy stay in the engine.

#![forbid(unsafe_code)]

mod event;
mod source;

pub use event::SessionEvent;
pub use source::{EventPushSource, EVENTS_SOURCE_ID};

//! Remote `StateSource`: read an AgentLight hub over HTTP instead of a local
//! `state.json`.
//!
//! This crate is a blocking, runtime-free client for the wire contract in
//! `docs/protocol.md` plus a [`StateSource`](agentlight_core::StateSource)
//! adapter over it. It is deliberately not async and pulls no TLS stack: the
//! hub is plaintext HTTP/1.1, the LAN is the trust boundary, and off-LAN access
//! is expected to go through a VPN (WireGuard/Tailscale). TLS is deferred until
//! there is a reason to expose the hub beyond a trusted network.
//!
//! # Layout
//!
//! - [`HubConfig`] — one hub's connection settings.
//! - [`HubClient`] — blocking `snapshot` / `remove_session` / `clear_done`.
//! - [`HubSource`] — the [`StateSource`](agentlight_core::StateSource) adapter,
//!   including a poll thread that emits [`SourceEvent::Changed`] on change.
//!
//! [`SourceEvent::Changed`]: agentlight_core::SourceEvent::Changed

#![forbid(unsafe_code)]

mod client;
mod source;

pub use client::{
    HubClient, HubConfig, HubError, DEFAULT_BASE_URL, DEFAULT_HUB_ID, DEFAULT_POLL_MS,
};
pub use source::HubSource;

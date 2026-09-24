//! The wire shape a producer pushes.

use agentlight_core::Status;
use serde::{Deserialize, Serialize};

/// One session state report pushed by a producer.
///
/// Only `session_id` and `status` are required; everything else refines the
/// display row. Unknown fields are ignored on read so producers can add fields
/// without a protocol bump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub status: Status,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub project_path: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
    /// RFC 3339 timestamp echoed verbatim and parsed for ordering.
    #[serde(default)]
    pub last_updated: Option<String>,
    /// Optional deep link back to the session in its harness UI.
    #[serde(default)]
    pub url: Option<String>,
    /// Optional producer identity. A `snapshot` batch prunes only sessions
    /// recorded for the same producer, so independent producers do not evict
    /// each other. Absent means legacy global pruning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
}

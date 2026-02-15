use serde::{Deserialize, Serialize};

/// Filters for querying events (used by CLI and tests).
#[derive(Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct EventFilters {
    pub session_id: Option<String>,
    pub vm_id: Option<String>,
    pub category: Option<String>,
    pub event_type: Option<String>,
    pub limit: Option<i64>,
}

/// Summary of a session returned by list_sessions (used by CLI and tests).
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct SessionInfo {
    pub id: String,
    pub started_at: String,
    pub stopped_at: Option<String>,
    pub server_version: String,
    pub event_count: i64,
}

/// A single event row returned by queries (used by CLI and tests).
#[derive(Debug, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Event {
    pub id: i64,
    pub session_id: String,
    pub timestamp: String,
    pub category: String,
    pub event_type: String,
    pub vm_id: Option<String>,
    pub correlation_id: Option<String>,
    pub duration_ms: Option<i64>,
    pub success: Option<bool>,
    pub data: serde_json::Value,
}

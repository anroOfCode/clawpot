use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::info;

use super::types::{Event, EventFilters, SessionInfo};

/// What to persist to SQLite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistMode {
    /// Every event (default).
    All,
    /// Only typed events (vm.*, network.*, test.*), not general "log" messages.
    Structured,
    /// Stdout only, no DB writes.
    None,
}

impl PersistMode {
    pub fn from_env() -> Self {
        match std::env::var("CLAWPOT_EVENTS_PERSIST")
            .unwrap_or_default()
            .as_str()
        {
            "structured" => Self::Structured,
            "none" => Self::None,
            _ => Self::All,
        }
    }
}

/// Internal record sent through the channel to the background writer.
struct EventRecord {
    timestamp: String,
    category: String,
    event_type: String,
    vm_id: Option<String>,
    correlation_id: Option<String>,
    duration_ms: Option<i64>,
    success: Option<bool>,
    data: String, // JSON string
}

enum WriterMsg {
    Event(EventRecord),
    Close {
        resp: tokio::sync::oneshot::Sender<()>,
    },
}

/// Unified event logging backed by SQLite + tracing stdout.
///
/// Every `emit()` call writes to both SQLite (via an async channel to a background
/// writer task) and to stdout via `tracing::info!()`.
#[derive(Clone)]
pub struct EventStore {
    tx: mpsc::UnboundedSender<WriterMsg>,
    #[cfg_attr(not(test), allow(dead_code))]
    session_id: Arc<String>,
    persist_mode: PersistMode,
    next_id: Arc<AtomicI64>,
}

impl EventStore {
    /// Open the database, create tables, insert a session row, and spawn the
    /// background writer task. Returns an `EventStore` handle.
    pub fn new(
        path: &Path,
        session_id: &str,
        server_version: &str,
        config: &str,
        persist_mode: PersistMode,
    ) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create DB directory: {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open events DB at {}", path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .context("Failed to set SQLite pragmas")?;

        Self::create_tables(&conn)?;

        // Insert session row
        conn.execute(
            "INSERT INTO sessions (id, started_at, server_version, config) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                session_id,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                server_version,
                config,
            ],
        )
        .context("Failed to insert session")?;

        let (tx, rx) = mpsc::unbounded_channel();
        let sid = session_id.to_string();

        // Spawn background writer
        tokio::spawn(background_writer(conn, sid.clone(), rx));

        info!(
            "Event store opened at {} (session {})",
            path.display(),
            &sid
        );

        Ok(Self {
            tx,
            session_id: Arc::new(sid),
            persist_mode,
            next_id: Arc::new(AtomicI64::new(1)),
        })
    }

    fn create_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id             TEXT PRIMARY KEY,
                started_at     TEXT NOT NULL,
                stopped_at     TEXT,
                server_version TEXT NOT NULL,
                config         TEXT
            );

            CREATE TABLE IF NOT EXISTS events (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id      TEXT NOT NULL REFERENCES sessions(id),
                timestamp       TEXT NOT NULL,
                category        TEXT NOT NULL,
                event_type      TEXT NOT NULL,
                vm_id           TEXT,
                correlation_id  TEXT,
                duration_ms     INTEGER,
                success         INTEGER,
                data            TEXT NOT NULL DEFAULT '{}'
            );

            CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
            CREATE INDEX IF NOT EXISTS idx_events_ts ON events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_events_vm ON events(vm_id);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
            CREATE INDEX IF NOT EXISTS idx_events_corr ON events(correlation_id);",
        )
        .context("Failed to create events tables")?;
        Ok(())
    }

    /// Core event emission. Writes to SQLite (async) and emits `tracing::info!()`.
    /// Infallible: never panics or returns errors.
    pub fn emit<D: Serialize>(
        &self,
        event_type: &str,
        category: &str,
        vm_id: Option<&str>,
        correlation_id: Option<&str>,
        data: &D,
    ) -> i64 {
        self.emit_inner(
            event_type,
            category,
            vm_id,
            correlation_id,
            None,
            None,
            data,
        )
    }

    /// Event with duration and outcome (for completed operations).
    pub fn emit_with_duration<D: Serialize>(
        &self,
        event_type: &str,
        category: &str,
        vm_id: Option<&str>,
        correlation_id: Option<&str>,
        duration_ms: i64,
        success: Option<bool>,
        data: &D,
    ) -> i64 {
        self.emit_inner(
            event_type,
            category,
            vm_id,
            correlation_id,
            Some(duration_ms),
            success,
            data,
        )
    }

    /// Simple log message — event_type="log", data={"message": "..."}.
    pub fn log(&self, category: &str, vm_id: Option<&str>, message: &str) -> i64 {
        self.emit_inner(
            "log",
            category,
            vm_id,
            None,
            None,
            None,
            &serde_json::json!({"message": message}),
        )
    }

    fn emit_inner<D: Serialize>(
        &self,
        event_type: &str,
        category: &str,
        vm_id: Option<&str>,
        correlation_id: Option<&str>,
        duration_ms: Option<i64>,
        success: Option<bool>,
        data: &D,
    ) -> i64 {
        let local_id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let data_json = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());

        // Emit to tracing (stdout/OTLP)
        if let Some(vid) = vm_id {
            info!("[{}] vm={} {}", event_type, vid, &data_json);
        } else {
            info!("[{}] {}", event_type, &data_json);
        }

        // Check persist mode
        let should_persist = match self.persist_mode {
            PersistMode::All => true,
            PersistMode::Structured => event_type != "log",
            PersistMode::None => false,
        };

        if should_persist {
            let record = EventRecord {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                category: category.to_string(),
                event_type: event_type.to_string(),
                vm_id: vm_id.map(String::from),
                correlation_id: correlation_id.map(String::from),
                duration_ms,
                success,
                data: data_json,
            };

            if self.tx.send(WriterMsg::Event(record)).is_err() {
                eprintln!("EventStore: failed to send event to writer (channel closed)");
            }
        }

        local_id
    }

    /// Close the session (set `stopped_at`), flush pending writes.
    pub async fn close_session(&self) {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(WriterMsg::Close { resp: resp_tx });
        // Wait for flush with a timeout
        let _ = tokio::time::timeout(Duration::from_secs(5), resp_rx).await;
    }

    /// Returns the session ID.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    // -----------------------------------------------------------------------
    // Query methods (used by CLI and tests)
    // -----------------------------------------------------------------------

    /// Open a read-only connection for queries.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open_readonly(path: &Path) -> Result<Connection> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("Failed to open events DB at {}", path.display()))?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")
            .context("Failed to set busy timeout")?;
        Ok(conn)
    }

    /// List all sessions with event counts.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn list_sessions(conn: &Connection) -> Result<Vec<SessionInfo>> {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.started_at, s.stopped_at, s.server_version,
                    (SELECT COUNT(*) FROM events e WHERE e.session_id = s.id) as event_count
             FROM sessions s
             ORDER BY s.started_at DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SessionInfo {
                id: row.get(0)?,
                started_at: row.get(1)?,
                stopped_at: row.get(2)?,
                server_version: row.get(3)?,
                event_count: row.get(4)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Query events with filters.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn query_events(conn: &Connection, filters: &EventFilters) -> Result<Vec<Event>> {
        let mut sql = String::from(
            "SELECT id, session_id, timestamp, category, event_type, vm_id,
                    correlation_id, duration_ms, success, data
             FROM events WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref sid) = filters.session_id {
            let _ = write!(sql, " AND session_id = ?{}", params.len() + 1);
            params.push(Box::new(sid.clone()));
        }
        if let Some(ref vid) = filters.vm_id {
            let _ = write!(sql, " AND vm_id = ?{}", params.len() + 1);
            params.push(Box::new(vid.clone()));
        }
        if let Some(ref cat) = filters.category {
            let _ = write!(sql, " AND category = ?{}", params.len() + 1);
            params.push(Box::new(cat.clone()));
        }
        if let Some(ref et) = filters.event_type {
            let _ = write!(sql, " AND event_type = ?{}", params.len() + 1);
            params.push(Box::new(et.clone()));
        }

        sql.push_str(" ORDER BY timestamp ASC, id ASC");

        if let Some(limit) = filters.limit {
            let _ = write!(sql, " LIMIT {limit}");
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| &**p).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let success_int: Option<i32> = row.get(8)?;
            let data_str: String = row.get(9)?;
            Ok(Event {
                id: row.get(0)?,
                session_id: row.get(1)?,
                timestamp: row.get(2)?,
                category: row.get(3)?,
                event_type: row.get(4)?,
                vm_id: row.get(5)?,
                correlation_id: row.get(6)?,
                duration_ms: row.get(7)?,
                success: success_int.map(|v| v != 0),
                data: serde_json::from_str(&data_str)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }
}

/// Background task that batches event writes into SQLite transactions.
async fn background_writer(
    conn: Connection,
    session_id: String,
    mut rx: mpsc::UnboundedReceiver<WriterMsg>,
) {
    let mut batch: Vec<EventRecord> = Vec::with_capacity(64);

    loop {
        // Wait for at least one message
        match rx.recv().await {
            Some(WriterMsg::Event(record)) => {
                batch.push(record);
            }
            Some(WriterMsg::Close { resp }) => {
                // Drain any remaining events in the channel before flushing
                while let Ok(msg) = rx.try_recv() {
                    if let WriterMsg::Event(record) = msg {
                        batch.push(record);
                    }
                }
                // Flush all events, close session, checkpoint WAL, then respond
                flush_batch(&conn, &session_id, &mut batch);
                close_session_row(&conn, &session_id);
                checkpoint_wal(&conn);
                let _ = resp.send(());
                return;
            }
            None => {
                // Channel closed without explicit close — flush and exit
                flush_batch(&conn, &session_id, &mut batch);
                close_session_row(&conn, &session_id);
                checkpoint_wal(&conn);
                return;
            }
        }

        // Drain any additional messages that are already waiting (non-blocking)
        loop {
            match rx.try_recv() {
                Ok(WriterMsg::Event(record)) => batch.push(record),
                Ok(WriterMsg::Close { resp }) => {
                    flush_batch(&conn, &session_id, &mut batch);
                    close_session_row(&conn, &session_id);
                    checkpoint_wal(&conn);
                    let _ = resp.send(());
                    return;
                }
                Err(_) => break,
            }
        }

        // Flush the batch if we have events
        if !batch.is_empty() {
            flush_batch(&conn, &session_id, &mut batch);
        }
    }
}

fn flush_batch(conn: &Connection, session_id: &str, batch: &mut Vec<EventRecord>) {
    if batch.is_empty() {
        return;
    }

    if let Err(e) = flush_batch_inner(conn, session_id, batch) {
        eprintln!(
            "EventStore: failed to flush batch ({} events): {e}",
            batch.len()
        );
    }
    batch.clear();
}

fn flush_batch_inner(conn: &Connection, session_id: &str, batch: &[EventRecord]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO events (session_id, timestamp, category, event_type, vm_id,
                                 correlation_id, duration_ms, success, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;

        for record in batch {
            let success_int = record.success.map(i32::from);
            stmt.execute(rusqlite::params![
                session_id,
                record.timestamp,
                record.category,
                record.event_type,
                record.vm_id,
                record.correlation_id,
                record.duration_ms,
                success_int,
                record.data,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Checkpoint WAL into the main database file so that read-only connections
/// (e.g. the CLI) can see all committed data without needing WAL recovery.
fn checkpoint_wal(conn: &Connection) {
    if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        eprintln!("EventStore: WAL checkpoint failed: {e}");
    }
}

fn close_session_row(conn: &Connection, session_id: &str) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    if let Err(e) = conn.execute(
        "UPDATE sessions SET stopped_at = ?1 WHERE id = ?2",
        rusqlite::params![now, session_id],
    ) {
        eprintln!("EventStore: failed to close session: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_db_path() -> PathBuf {
        let f = NamedTempFile::new().unwrap();
        f.into_temp_path().to_path_buf()
    }

    #[tokio::test]
    async fn test_event_store_basic() {
        let path = temp_db_path();
        let store =
            EventStore::new(&path, "test-session-1", "0.1.0", "{}", PersistMode::All).unwrap();

        // Emit some events
        store.emit(
            "vm.create.started",
            "vm",
            Some("vm-123"),
            None,
            &json!({"vcpu_count": 1, "mem_size_mib": 256}),
        );
        store.emit(
            "vm.create.ip_allocated",
            "vm",
            Some("vm-123"),
            None,
            &json!({"ip_address": "192.168.100.2"}),
        );
        store.log("server", None, "Hello from test");

        // Close and flush
        store.close_session().await;

        // Query back
        let conn = EventStore::open_readonly(&path).unwrap();
        let sessions = EventStore::list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "test-session-1");
        assert_eq!(sessions[0].event_count, 3);
        assert!(sessions[0].stopped_at.is_some());

        let events = EventStore::query_events(&conn, &EventFilters::default()).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "vm.create.started");
        assert_eq!(events[0].category, "vm");
        assert_eq!(events[0].vm_id.as_deref(), Some("vm-123"));
        assert_eq!(events[1].event_type, "vm.create.ip_allocated");
        assert_eq!(events[2].event_type, "log");
    }

    #[tokio::test]
    async fn test_event_store_filters() {
        let path = temp_db_path();
        let store =
            EventStore::new(&path, "test-session-2", "0.1.0", "{}", PersistMode::All).unwrap();

        store.emit("vm.create.started", "vm", Some("vm-1"), None, &json!({}));
        store.emit(
            "network.http.request",
            "network",
            Some("vm-1"),
            Some("corr-1"),
            &json!({"method": "GET"}),
        );
        store.emit("vm.create.started", "vm", Some("vm-2"), None, &json!({}));

        store.close_session().await;

        let conn = EventStore::open_readonly(&path).unwrap();

        // Filter by vm_id
        let events = EventStore::query_events(
            &conn,
            &EventFilters {
                vm_id: Some("vm-1".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(events.len(), 2);

        // Filter by category
        let events = EventStore::query_events(
            &conn,
            &EventFilters {
                category: Some("network".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].correlation_id.as_deref(), Some("corr-1"));

        // Filter by limit
        let events = EventStore::query_events(
            &conn,
            &EventFilters {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn test_persist_mode_structured() {
        let path = temp_db_path();
        let store = EventStore::new(
            &path,
            "test-session-3",
            "0.1.0",
            "{}",
            PersistMode::Structured,
        )
        .unwrap();

        store.emit("vm.create.started", "vm", Some("vm-1"), None, &json!({}));
        store.log("server", None, "this should not be persisted");

        store.close_session().await;

        let conn = EventStore::open_readonly(&path).unwrap();
        let events = EventStore::query_events(&conn, &EventFilters::default()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "vm.create.started");
    }

    #[tokio::test]
    async fn test_emit_with_duration() {
        let path = temp_db_path();
        let store =
            EventStore::new(&path, "test-session-4", "0.1.0", "{}", PersistMode::All).unwrap();

        store.emit_with_duration(
            "vm.create.completed",
            "vm",
            Some("vm-1"),
            None,
            1500,
            Some(true),
            &json!({"ip": "192.168.100.2"}),
        );

        store.close_session().await;

        let conn = EventStore::open_readonly(&path).unwrap();
        let events = EventStore::query_events(&conn, &EventFilters::default()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].duration_ms, Some(1500));
        assert_eq!(events[0].success, Some(true));
    }
}

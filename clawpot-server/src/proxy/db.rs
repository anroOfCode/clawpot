use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::info;

/// SQLite-backed request/response log for network traffic.
#[derive(Clone)]
pub struct RequestDb {
    conn: Arc<Mutex<Connection>>,
}

impl RequestDb {
    pub fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create DB directory: {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open SQLite DB at {}", path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .context("Failed to set SQLite pragmas")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS network_requests (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                vm_id           TEXT NOT NULL,
                timestamp       TEXT NOT NULL,
                request_type    TEXT NOT NULL,

                method          TEXT,
                url             TEXT,
                headers         TEXT,
                req_body_size   INTEGER,
                req_body        BLOB,
                req_body_path   TEXT,

                query_name      TEXT,
                query_type      TEXT,

                authorized      INTEGER,
                auth_reason     TEXT,
                auth_latency_ms INTEGER,

                status_code     INTEGER,
                resp_body_size  INTEGER,
                resp_body       BLOB,
                resp_body_path  TEXT,
                resp_headers    TEXT,
                resp_dns_answers TEXT,
                duration_ms     INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_requests_vm_id ON network_requests(vm_id);
            CREATE INDEX IF NOT EXISTS idx_requests_timestamp ON network_requests(timestamp);",
        )
        .context("Failed to create network_requests table")?;

        info!("Request database opened at {}", path.display());

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Log an incoming request. Returns the request_id for subsequent updates.
    pub fn log_request(
        &self,
        vm_id: &str,
        request_type: &str,
        method: Option<&str>,
        url: Option<&str>,
        headers: Option<&str>,
        query_name: Option<&str>,
        query_type: Option<&str>,
        req_body_size: Option<i64>,
        req_body: Option<&[u8]>,
        req_body_path: Option<&str>,
    ) -> Result<i64> {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO network_requests
                (vm_id, timestamp, request_type, method, url, headers,
                 query_name, query_type, req_body_size, req_body, req_body_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                vm_id,
                timestamp,
                request_type,
                method,
                url,
                headers,
                query_name,
                query_type,
                req_body_size,
                req_body,
                req_body_path,
            ],
        )
        .context("Failed to insert request")?;
        Ok(conn.last_insert_rowid())
    }

    /// Log the authorization decision for a request.
    pub fn log_authorization(
        &self,
        request_id: i64,
        authorized: bool,
        reason: &str,
        latency_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE network_requests
             SET authorized = ?1, auth_reason = ?2, auth_latency_ms = ?3
             WHERE id = ?4",
            rusqlite::params![i32::from(authorized), reason, latency_ms, request_id],
        )
        .context("Failed to update authorization")?;
        Ok(())
    }

    /// Log the response for a request.
    pub fn log_response(
        &self,
        request_id: i64,
        status_code: Option<i32>,
        resp_body_size: Option<i64>,
        resp_body: Option<&[u8]>,
        resp_body_path: Option<&str>,
        resp_headers: Option<&str>,
        resp_dns_answers: Option<&str>,
        duration_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE network_requests
             SET status_code = ?1, resp_body_size = ?2, resp_body = ?3,
                 resp_body_path = ?4, resp_headers = ?5, resp_dns_answers = ?6,
                 duration_ms = ?7
             WHERE id = ?8",
            rusqlite::params![
                status_code,
                resp_body_size,
                resp_body,
                resp_body_path,
                resp_headers,
                resp_dns_answers,
                duration_ms,
                request_id,
            ],
        )
        .context("Failed to update response")?;
        Ok(())
    }
}

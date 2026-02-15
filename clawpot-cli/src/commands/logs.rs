use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;

/// Summary of a session.
#[derive(Debug, Serialize, Deserialize)]
struct SessionInfo {
    id: String,
    started_at: String,
    stopped_at: Option<String>,
    server_version: String,
    event_count: i64,
}

/// A single event row.
#[derive(Debug, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
struct Event {
    id: i64,
    session_id: String,
    timestamp: String,
    category: String,
    event_type: String,
    vm_id: Option<String>,
    correlation_id: Option<String>,
    duration_ms: Option<i64>,
    success: Option<bool>,
    data: serde_json::Value,
}

fn open_db(path: &str) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open events DB at {path}"))?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")
        .context("Failed to set busy timeout")?;
    Ok(conn)
}

fn list_sessions(conn: &Connection) -> Result<Vec<SessionInfo>> {
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

fn query_events(
    conn: &Connection,
    session_id: Option<&str>,
    vm_id: Option<&str>,
    category: Option<&str>,
    event_type: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<Event>> {
    let mut sql = String::from(
        "SELECT id, session_id, timestamp, category, event_type, vm_id,
                correlation_id, duration_ms, success, data
         FROM events WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(sid) = session_id {
        let _ = write!(sql, " AND session_id = ?{}", params.len() + 1);
        params.push(Box::new(sid.to_string()));
    }
    if let Some(vid) = vm_id {
        let _ = write!(sql, " AND vm_id = ?{}", params.len() + 1);
        params.push(Box::new(vid.to_string()));
    }
    if let Some(cat) = category {
        let _ = write!(sql, " AND category = ?{}", params.len() + 1);
        params.push(Box::new(cat.to_string()));
    }
    if let Some(et) = event_type {
        let _ = write!(sql, " AND event_type = ?{}", params.len() + 1);
        params.push(Box::new(et.to_string()));
    }

    sql.push_str(" ORDER BY timestamp ASC, id ASC");

    if let Some(limit) = limit {
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

/// Default DB path based on CLAWPOT_ROOT.
fn default_db_path() -> String {
    let root = std::env::var("CLAWPOT_ROOT").unwrap_or_else(|_| "/workspaces/clawpot".to_string());
    format!("{root}/data/events.db")
}

pub fn execute_sessions(db_path: Option<&str>) -> Result<()> {
    let path = db_path.map_or_else(default_db_path, String::from);

    if !Path::new(&path).exists() {
        println!("No events database found at {path}");
        return Ok(());
    }

    let conn = open_db(&path)?;
    let sessions = list_sessions(&conn)?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    println!(
        "{:<38} {:<26} {:<26} {:<10} {:>6}",
        "SESSION ID", "STARTED", "STOPPED", "VERSION", "EVENTS"
    );
    println!("{}", "-".repeat(110));
    for s in &sessions {
        println!(
            "{:<38} {:<26} {:<26} {:<10} {:>6}",
            s.id,
            &s.started_at,
            s.stopped_at.as_deref().unwrap_or("(running)"),
            &s.server_version,
            s.event_count,
        );
    }

    Ok(())
}

pub fn execute_show(
    db_path: Option<&str>,
    session_id: Option<&str>,
    vm_id: Option<&str>,
    category: Option<&str>,
    event_type: Option<&str>,
    limit: Option<i64>,
) -> Result<()> {
    let path = db_path.map_or_else(default_db_path, String::from);

    if !Path::new(&path).exists() {
        println!("No events database found at {path}");
        return Ok(());
    }

    let conn = open_db(&path)?;
    let events = query_events(&conn, session_id, vm_id, category, event_type, limit)?;

    if events.is_empty() {
        println!("No events found.");
        return Ok(());
    }

    println!(
        "{:<26} {:<12} {:<32} {:<38} {:>8} {:<7}",
        "TIMESTAMP", "CATEGORY", "EVENT TYPE", "VM ID", "DUR(ms)", "OK"
    );
    println!("{}", "-".repeat(130));
    for e in &events {
        println!(
            "{:<26} {:<12} {:<32} {:<38} {:>8} {:<7}",
            &e.timestamp,
            &e.category,
            &e.event_type,
            e.vm_id.as_deref().unwrap_or("-"),
            e.duration_ms
                .map_or_else(|| "-".to_string(), |d| d.to_string()),
            match e.success {
                Some(true) => "yes",
                Some(false) => "no",
                None => "-",
            },
        );
    }

    println!("\nTotal: {} event(s)", events.len());
    Ok(())
}

pub fn execute_export(db_path: Option<&str>, session_id: Option<&str>, format: &str) -> Result<()> {
    let path = db_path.map_or_else(default_db_path, String::from);

    if !Path::new(&path).exists() {
        anyhow::bail!("No events database found at {path}");
    }

    let conn = open_db(&path)?;
    let events = query_events(&conn, session_id, None, None, None, None)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&events)?);
        }
        _ => {
            // JSONL — one JSON object per line
            for event in &events {
                println!("{}", serde_json::to_string(event)?);
            }
        }
    }

    Ok(())
}

pub fn execute_timeline(
    db_path: Option<&str>,
    session_id: Option<&str>,
    vm_id: Option<&str>,
) -> Result<()> {
    let path = db_path.map_or_else(default_db_path, String::from);

    if !Path::new(&path).exists() {
        println!("No events database found at {path}");
        return Ok(());
    }

    let conn = open_db(&path)?;
    let events = query_events(&conn, session_id, vm_id, None, None, None)?;

    if events.is_empty() {
        println!("No events found.");
        return Ok(());
    }

    for e in &events {
        // Format: timestamp [category] event_type (vm_id) data_summary
        let vm_part = e
            .vm_id
            .as_deref()
            .map_or_else(String::new, |v| format!(" vm={v}"));
        let duration_part = e
            .duration_ms
            .map_or_else(String::new, |d| format!(" ({d}ms)"));
        let success_part = match e.success {
            Some(true) => " OK".to_string(),
            Some(false) => " FAIL".to_string(),
            None => String::new(),
        };

        // Extract a brief summary from data
        let data_summary = format_data_summary(&e.event_type, &e.data);

        println!(
            "{} [{}] {}{}{}{} {}",
            &e.timestamp,
            &e.category,
            &e.event_type,
            vm_part,
            duration_part,
            success_part,
            data_summary,
        );
    }

    Ok(())
}

fn format_data_summary(event_type: &str, data: &serde_json::Value) -> String {
    match event_type {
        "log" => data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "network.http.request" => {
            let method = data.get("method").and_then(|v| v.as_str()).unwrap_or("?");
            let url = data.get("url").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{method} {url}")
        }
        "network.http.response" => {
            let status = data
                .get("status_code")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            format!("status={status}")
        }
        "network.dns.request" => {
            let name = data
                .get("query_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let qtype = data
                .get("query_type")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("{name} {qtype}")
        }
        "vm.create.ip_allocated" => data
            .get("ip_address")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "vm.exec" => {
            let cmd = data.get("command").and_then(|v| v.as_str()).unwrap_or("?");
            let exit = data
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1);
            format!("{cmd} exit={exit}")
        }
        _ => {
            // Compact JSON for other types
            let s = serde_json::to_string(data).unwrap_or_default();
            if s.len() > 80 {
                format!("{}...", &s[..77])
            } else {
                s
            }
        }
    }
}

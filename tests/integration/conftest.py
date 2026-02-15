"""
Pytest plugin that emits test lifecycle events to the Clawpot events database.

Writes test.session.started, test.case.started, test.case.completed, and
test.session.completed events directly to SQLite so they appear alongside
server events in the unified timeline.
"""

import json
import os
import sqlite3
import time
from datetime import datetime, timezone


def _events_db_path():
    """Resolve the events DB path from CLAWPOT_EVENTS_DB or CLAWPOT_ROOT."""
    path = os.environ.get("CLAWPOT_EVENTS_DB")
    if path:
        return path
    root = os.environ.get(
        "CLAWPOT_ROOT",
        os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..")),
    )
    return os.path.join(root, "data", "events.db")


def _now_rfc3339():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def _get_session_id(conn):
    """Find the most recent active session."""
    row = conn.execute("SELECT id FROM sessions ORDER BY started_at DESC LIMIT 1").fetchone()
    return row[0] if row else None


def _emit_event(
    conn,
    session_id,
    event_type,
    category,
    vm_id=None,
    correlation_id=None,
    duration_ms=None,
    success=None,
    data=None,
):
    """Insert an event row."""
    conn.execute(
        """INSERT INTO events (session_id, timestamp, category, event_type,
                               vm_id, correlation_id, duration_ms, success, data)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)""",
        (
            session_id,
            _now_rfc3339(),
            category,
            event_type,
            vm_id,
            correlation_id,
            duration_ms,
            1 if success is True else (0 if success is False else None),
            json.dumps(data or {}),
        ),
    )
    conn.commit()


class EventLogPlugin:
    """Pytest plugin that logs test events to the events DB.

    Connection is lazy: we try to connect on the first test setup, after
    session-scoped fixtures (like the server fixture) have already started.
    """

    def __init__(self):
        self.conn = None
        self.session_id = None
        self._test_starts = {}
        self._connect_attempted = False

    def _ensure_connected(self):
        """Try to connect to the events DB (once)."""
        if self.conn is not None:
            return True
        if self._connect_attempted:
            return False
        self._connect_attempted = True
        db_path = _events_db_path()
        if not os.path.exists(db_path):
            return False
        try:
            self.conn = sqlite3.connect(db_path)
            self.conn.execute("PRAGMA busy_timeout=5000")
            self.session_id = _get_session_id(self.conn)
            if self.session_id is None:
                self.conn.close()
                self.conn = None
                return False
            return True
        except Exception:
            self.conn = None
            return False

    def pytest_runtest_setup(self, item):
        if not self._ensure_connected():
            return
        self._test_starts[item.nodeid] = time.monotonic()
        _emit_event(
            self.conn,
            self.session_id,
            "test.case.started",
            "test",
            data={"test_name": item.nodeid},
        )

    def pytest_runtest_makereport(self, item, call):
        if not self.conn or call.when != "call":
            return
        start = self._test_starts.pop(item.nodeid, None)
        duration_ms = int((time.monotonic() - start) * 1000) if start else None
        outcome = "passed" if call.excinfo is None else "failed"
        _emit_event(
            self.conn,
            self.session_id,
            "test.case.completed",
            "test",
            duration_ms=duration_ms,
            success=call.excinfo is None,
            data={
                "test_name": item.nodeid,
                "outcome": outcome,
                "duration_ms": duration_ms,
            },
        )

    def pytest_sessionfinish(self, session, exitstatus):
        if not self.conn:
            return
        passed = session.testscollected - session.testsfailed
        _emit_event(
            self.conn,
            self.session_id,
            "test.session.completed",
            "test",
            data={
                "passed": passed,
                "failed": session.testsfailed,
                "errors": exitstatus,
            },
        )
        self.conn.close()


def pytest_configure(config):
    """Register the event log plugin."""
    config.pluginmanager.register(EventLogPlugin(), "clawpot_event_log")

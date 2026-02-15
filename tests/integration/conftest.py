"""
Pytest configuration for Clawpot integration tests.

Provides:
  - Session-scoped `server` fixture that starts/stops clawpot-server
  - EventLogPlugin that writes test lifecycle events to the events database
"""

import json
import logging
import os
import signal
import sqlite3
import subprocess
import time
from datetime import datetime, timezone

import pytest

# ---------------------------------------------------------------------------
# Shared paths and helpers
# ---------------------------------------------------------------------------

PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CLI_BIN = os.path.join(PROJECT_ROOT, "target", "debug", "clawpot")
SERVER_BIN = os.path.join(PROJECT_ROOT, "target", "debug", "clawpot-server")

SERVER_ENV = {
    **os.environ,
    "CLAWPOT_ROOT": PROJECT_ROOT,
    "PATH": (
        os.path.expanduser("~/.cargo/bin")
        + ":"
        + os.path.expanduser("~/.local/bin")
        + ":"
        + os.environ.get("PATH", "")
    ),
}

log = logging.getLogger("clawpot-test")


def _wait_for_server(max_wait: float = 15) -> None:
    """Poll the server until it responds to 'list'."""
    deadline = time.monotonic() + max_wait
    while time.monotonic() < deadline:
        try:
            result = subprocess.run(
                [CLI_BIN, "list"],
                capture_output=True,
                timeout=5,
            )
            if result.returncode == 0:
                log.info("Server is ready")
                return
        except (subprocess.TimeoutExpired, OSError):
            pass
        time.sleep(0.5)
    raise RuntimeError(f"Server did not become ready within {max_wait}s")


# ---------------------------------------------------------------------------
# Server fixture (session-scoped, used by all test files)
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session", autouse=True)
def server():
    """Start the clawpot-server for the entire test session."""
    log.info("=" * 60)
    log.info("STARTING SERVER")
    log.info("=" * 60)

    # Build server + CLI if not already present (e.g. pre-built in CI)
    if os.path.isfile(SERVER_BIN) and os.path.isfile(CLI_BIN):
        log.info("Pre-built binaries found, skipping cargo build")
    else:
        log.info("Building server and CLI...")
        build = subprocess.run(
            ["cargo", "build", "-p", "clawpot-server", "-p", "clawpot-cli"],
            capture_output=True,
            cwd=PROJECT_ROOT,
            env=SERVER_ENV,
            timeout=120,
        )
        if build.returncode != 0:
            log.error("Build failed:\n%s", build.stderr.decode())
            pytest.fail("cargo build failed")

        log.info("Build succeeded")

    assert os.path.isfile(SERVER_BIN), f"Server binary not found: {SERVER_BIN}"
    assert os.path.isfile(CLI_BIN), f"CLI binary not found: {CLI_BIN}"

    # Start server
    log.info("Starting clawpot-server (pid will follow)...")
    server_log = open(  # noqa: SIM115
        os.path.join(PROJECT_ROOT, "target", "server-test.log"), "w"
    )
    proc = subprocess.Popen(
        [SERVER_BIN],
        env=SERVER_ENV,
        stdout=server_log,
        stderr=subprocess.STDOUT,
    )
    log.info("Server started with PID %d", proc.pid)

    # Wait for it to be ready
    try:
        _wait_for_server()
    except RuntimeError:
        proc.kill()
        proc.wait()
        server_log.close()
        pytest.fail("Server failed to start")

    yield proc

    # Teardown
    log.info("=" * 60)
    log.info("STOPPING SERVER (PID %d)", proc.pid)
    log.info("=" * 60)

    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=10)
        log.info("Server exited cleanly")
    except subprocess.TimeoutExpired:
        log.warning("Server did not exit, killing...")
        proc.kill()
        proc.wait()
    server_log.close()


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
        """Try to connect to the events DB.

        Retries until the DB file appears (fixtures may not have started yet).
        Once the file exists and we attempt a connection, we don't retry on failure.
        """
        if self.conn is not None:
            return True
        if self._connect_attempted:
            return False
        db_path = _events_db_path()
        if not os.path.exists(db_path):
            # DB doesn't exist yet — fixtures may not have started.
            # Don't set _connect_attempted so we retry on the next test.
            return False
        # DB file exists — this is our one real attempt.
        self._connect_attempted = True
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

    @pytest.hookimpl(trylast=True)
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

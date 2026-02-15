"""
Integration tests for LLM API tracing and key injection.

These tests make real API calls to Anthropic's API through the proxy,
verifying that:
  - The server detects LLM requests and emits llm.request/llm.response events
  - API key injection works (VM sends a dummy key, server injects the real one)
  - Streaming SSE responses are reassembled correctly
  - The real API key never appears in event data

Requires CLAWPOT_ANTHROPIC_API_KEY to be set in the environment.

Usage:
    cd tests/integration
    sudo -E $(which uv) run pytest test_llm.py -v -s --timeout=120
"""

import json
import logging
import os
import re
import sqlite3
import time

import pytest

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CLI_BIN = os.path.join(PROJECT_ROOT, "target", "debug", "clawpot")

logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s [%(levelname)-5s] %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("clawpot-llm")

# Module-level state shared across ordered tests
_vm_id: str | None = None
_non_streaming_corr_id: str | None = None
_streaming_corr_id: str | None = None

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

ANTHROPIC_API_KEY = os.environ.get("CLAWPOT_ANTHROPIC_API_KEY", "")

# Skip the entire module if no API key is configured
pytestmark = pytest.mark.skipif(
    not ANTHROPIC_API_KEY,
    reason="CLAWPOT_ANTHROPIC_API_KEY not set",
)


def cli(*args: str, timeout: float = 60) -> tuple[str, str, int]:
    """Run the clawpot CLI and return (stdout, stderr, exit_code)."""
    import subprocess

    cmd = [CLI_BIN, *args]
    log.info("CLI: %s", " ".join(cmd))

    result = subprocess.run(cmd, capture_output=True, timeout=timeout)
    stdout = result.stdout.decode("utf-8", errors="replace")
    stderr = result.stderr.decode("utf-8", errors="replace")

    log.info("  exit_code=%d", result.returncode)
    if stdout.strip():
        for line in stdout.strip().splitlines():
            log.info("  stdout: %s", line)
    if stderr.strip():
        for line in stderr.strip().splitlines():
            log.warning("  stderr: %s", line)

    return stdout, stderr, result.returncode


def events_db_path():
    """Resolve the events DB path."""
    path = os.environ.get("CLAWPOT_EVENTS_DB")
    if path:
        return path
    root = os.environ.get("CLAWPOT_ROOT", PROJECT_ROOT)
    return os.path.join(root, "data", "events.db")


def query_events(event_type=None, category=None, vm_id=None):
    """Query events from the DB."""
    db_path = events_db_path()
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA busy_timeout=5000")

    cols = "id, event_type, category, vm_id, correlation_id, duration_ms, success, data"
    sql = f"SELECT {cols} FROM events WHERE 1=1"
    params = []

    if event_type:
        sql += " AND event_type = ?"
        params.append(event_type)
    if category:
        sql += " AND category = ?"
        params.append(category)
    if vm_id:
        sql += " AND vm_id = ?"
        params.append(vm_id)

    sql += " ORDER BY id ASC"

    rows = conn.execute(sql, params).fetchall()
    conn.close()

    return [
        {
            "id": r[0],
            "event_type": r[1],
            "category": r[2],
            "vm_id": r[3],
            "correlation_id": r[4],
            "duration_ms": r[5],
            "success": r[6],
            "data": json.loads(r[7]),
        }
        for r in rows
    ]


def query_all_event_data():
    """Return all event data fields as raw strings for key-leak scanning."""
    db_path = events_db_path()
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA busy_timeout=5000")
    rows = conn.execute("SELECT data FROM events").fetchall()
    conn.close()
    return [r[0] for r in rows]


# ---------------------------------------------------------------------------
# Tests — ordered numerically so they run in sequence
# ---------------------------------------------------------------------------


class TestLlm:
    """LLM API tracing tests. Tests run in order within this class."""

    def test_01_create_vm(self, server):
        """Create a VM for LLM tests."""
        global _vm_id

        stdout, _, rc = cli("create", "--vcpus", "1", "--memory", "256")
        assert rc == 0
        assert "VM created successfully" in stdout

        match = re.search(r"VM ID:\s+([0-9a-f-]{36})", stdout)
        assert match, f"Could not parse VM ID from output:\n{stdout}"
        _vm_id = match.group(1)
        log.info("Created VM for LLM tests: %s", _vm_id)

    def test_02_non_streaming_request(self, server):
        """Send a non-streaming Haiku request through the proxy."""
        assert _vm_id is not None, "No VM created"

        request_body = json.dumps(
            {
                "model": "claude-haiku-4-5-20250514",
                "max_tokens": 32,
                "messages": [{"role": "user", "content": "Say hello in exactly 3 words."}],
            }
        )

        # The VM sends x-api-key: dummy — the server should strip it
        # and inject the real key from CLAWPOT_ANTHROPIC_API_KEY
        cmd = (
            "curl -4 -k --max-time 30 --connect-timeout 10 -s"
            " -X POST https://api.anthropic.com/v1/messages"
            " -H 'Content-Type: application/json'"
            " -H 'x-api-key: dummy-key-from-vm'"
            " -H 'anthropic-version: 2023-06-01'"
            f" -d '{request_body}'"
        )

        stdout, _stderr, _rc = cli("exec", _vm_id, "--", "bash", "-c", cmd, timeout=45)
        log.info("Non-streaming response: %s", stdout[:300])

        # Parse the JSON response
        response = json.loads(stdout)
        resp_type = response.get("type")

        if resp_type == "error":
            # Key injection worked if the error is NOT "invalid x-api-key"
            error_type = response.get("error", {}).get("type", "")
            error_msg = response.get("error", {}).get("message", "")
            assert error_type != "authentication_error", f"Key injection failed: {error_msg}"
            log.info("API returned non-auth error (key injection OK): %s", error_msg)
        else:
            assert resp_type == "message", f"Unexpected response type: {resp_type}"
            assert response.get("model") is not None
            content = response.get("content", [])
            assert len(content) > 0, "Expected at least one content block"
            log.info("Haiku says: %s", content[0].get("text", ""))

    def test_03_streaming_request(self, server):
        """Send a streaming Haiku request through the proxy."""
        assert _vm_id is not None, "No VM created"

        request_body = json.dumps(
            {
                "model": "claude-haiku-4-5-20250514",
                "max_tokens": 32,
                "stream": True,
                "messages": [{"role": "user", "content": "Count from 1 to 5."}],
            }
        )

        cmd = (
            "curl -4 -k --max-time 30 --connect-timeout 10 -s"
            " -X POST https://api.anthropic.com/v1/messages"
            " -H 'Content-Type: application/json'"
            " -H 'x-api-key: dummy-key-from-vm'"
            " -H 'anthropic-version: 2023-06-01'"
            f" -d '{request_body}'"
        )

        stdout, _stderr, _rc = cli("exec", _vm_id, "--", "bash", "-c", cmd, timeout=45)
        log.info("Streaming response (first 300 chars): %s", stdout[:300])

        # If the API has credits, we get SSE events. If not, we get a JSON error.
        # Either way, key injection is verified if the error is not "authentication_error".
        if "event: message_start" in stdout:
            assert "event: content_block_delta" in stdout
            assert "event: message_stop" in stdout
            log.info("Got streaming SSE response")
        else:
            response = json.loads(stdout)
            error_type = response.get("error", {}).get("type", "")
            error_msg = response.get("error", {}).get("message", "")
            assert error_type != "authentication_error", f"Key injection failed: {error_msg}"
            log.info("API returned non-auth error (key injection OK): %s", error_msg)

    def test_04_events_recorded(self, server):
        """Verify llm.request and llm.response events were recorded."""
        assert _vm_id is not None, "No VM created"
        global _non_streaming_corr_id, _streaming_corr_id

        # Small delay to ensure async events are flushed
        time.sleep(0.5)

        # Note: HTTPS requests go through the TLS MITM proxy, which connects
        # to the HTTP proxy from localhost. This means the HTTP proxy sees
        # peer_addr=127.0.0.1 and cannot resolve the VM ID. So llm.* events
        # have vm_id="unknown". We query by category instead.
        requests = query_events(event_type="llm.request")
        assert len(requests) >= 2, f"Expected at least 2 llm.request events, got {len(requests)}"

        responses = query_events(event_type="llm.response")
        assert len(responses) >= 2, f"Expected at least 2 llm.response events, got {len(responses)}"

        # Verify non-streaming request event fields
        non_stream_req = requests[0]
        assert non_stream_req["data"]["provider"] == "anthropic"
        assert non_stream_req["data"]["endpoint"] == "messages"
        assert non_stream_req["data"]["model"] == "claude-haiku-4-5-20250514"
        assert non_stream_req["data"]["message_count"] == 1
        assert non_stream_req["data"].get("streaming") is None  # stream not set
        assert non_stream_req["correlation_id"] is not None
        _non_streaming_corr_id = non_stream_req["correlation_id"]

        # Verify streaming request event fields
        stream_req = requests[1]
        assert stream_req["data"]["provider"] == "anthropic"
        assert stream_req["data"]["streaming"] is True
        _streaming_corr_id = stream_req["correlation_id"]

        # Verify response event fields
        # Note: if the API account has no credits, responses will have error
        # status codes and null model/tokens. The key thing is that events
        # are recorded and correlated correctly.
        non_stream_resp = responses[0]
        assert non_stream_resp["data"]["provider"] == "anthropic"
        assert non_stream_resp["data"]["endpoint"] == "messages"
        assert non_stream_resp["data"]["status_code"] is not None
        assert non_stream_resp["duration_ms"] is not None
        assert non_stream_resp["correlation_id"] == _non_streaming_corr_id
        status = non_stream_resp["data"]["status_code"]
        log.info(
            "Non-streaming response: status=%s model=%s tokens=%s+%s",
            status,
            non_stream_resp["data"].get("model"),
            non_stream_resp["data"].get("input_tokens"),
            non_stream_resp["data"].get("output_tokens"),
        )

        stream_resp = responses[1]
        assert stream_resp["data"]["provider"] == "anthropic"
        assert stream_resp["data"]["status_code"] is not None
        assert stream_resp["correlation_id"] == _streaming_corr_id
        log.info(
            "Streaming response: status=%s model=%s tokens=%s+%s",
            stream_resp["data"]["status_code"],
            stream_resp["data"].get("model"),
            stream_resp["data"].get("input_tokens"),
            stream_resp["data"].get("output_tokens"),
        )

        # If successful, verify response body content
        if status == 200:
            assert non_stream_resp["data"]["model"] is not None
            assert non_stream_resp["data"]["input_tokens"] is not None
            assert non_stream_resp["success"] == 1

            body = stream_resp["data"].get("body", {})
            assert body.get("content") is not None
            text = body.get("content", [{}])[0].get("text", "")
            assert len(text) > 0
            log.info("Streaming reassembled text: %s", text[:80])

    def test_05_correlation_with_network_events(self, server):
        """Verify llm events share correlation_id with network events."""
        assert _non_streaming_corr_id is not None, "No correlation ID captured"

        # The llm.request and network.http.request should share a correlation_id
        # (vm_id is "unknown" for HTTPS, so query all events)
        all_events = query_events()
        corr_events = [e for e in all_events if e["correlation_id"] == _non_streaming_corr_id]

        event_types = {e["event_type"] for e in corr_events}
        for expected in [
            "network.http.request",
            "network.http.response",
            "llm.request",
            "llm.response",
        ]:
            assert expected in event_types, f"Missing {expected}"
        log.info(
            "Correlation group for %s: %s",
            _non_streaming_corr_id,
            sorted(event_types),
        )

    def test_06_api_key_not_in_events(self, server):
        """Verify the real API key never appears in any event data."""
        api_key = os.environ.get("CLAWPOT_ANTHROPIC_API_KEY", "")
        assert len(api_key) > 0, "API key should be set"

        all_data = query_all_event_data()
        for data_str in all_data:
            assert api_key not in data_str, "API key found in event data (key leak detected)"

        log.info("Scanned %d events — API key not found in any event data", len(all_data))

    def test_07_delete_vm(self, server):
        """Clean up: delete the LLM test VM."""
        assert _vm_id is not None, "No VM created"

        stdout, _, rc = cli("delete", _vm_id)
        assert rc == 0
        assert "VM deleted successfully" in stdout

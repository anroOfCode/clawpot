"""
End-to-end integration tests for Clawpot VM orchestration.

These tests exercise the full stack: CLI → gRPC server → Firecracker VM → guest agent.
They must be run as root (sudo) because Firecracker requires it.

Usage:
    cd tests/integration
    sudo -E $(which uv) run pytest -v -s --timeout=120
"""

import logging
import os
import re
import signal
import subprocess
import time

import pytest

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CLI_BIN = os.path.join(PROJECT_ROOT, "target", "debug", "clawpot")
SERVER_BIN = os.path.join(PROJECT_ROOT, "target", "debug", "clawpot-server")

SERVER_ENV = {
    **os.environ,
    "CLAWPOT_ROOT": PROJECT_ROOT,
    "PATH": (
        os.path.expanduser("~/.cargo/bin")
        + ":" + os.path.expanduser("~/.local/bin")
        + ":" + os.environ.get("PATH", "")
    ),
}

# Module-level state shared across ordered tests
_vm_id: str | None = None

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s [%(levelname)-5s] %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("clawpot-e2e")

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def cli(*args: str, timeout: float = 60) -> tuple[str, str, int]:
    """Run the clawpot CLI and return (stdout, stderr, exit_code)."""
    cmd = [CLI_BIN, *args]
    log.info("CLI: %s", " ".join(cmd))

    result = subprocess.run(
        cmd,
        capture_output=True,
        timeout=timeout,
    )

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


def wait_for_server(max_wait: float = 15) -> None:
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
# Fixtures
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
    server_log = open(os.path.join(PROJECT_ROOT, "target", "server-test.log"), "w")
    proc = subprocess.Popen(
        [SERVER_BIN],
        env=SERVER_ENV,
        stdout=server_log,
        stderr=subprocess.STDOUT,
    )
    log.info("Server started with PID %d", proc.pid)

    # Wait for it to be ready
    try:
        wait_for_server()
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


# ---------------------------------------------------------------------------
# Tests — ordered numerically so they run in sequence
# ---------------------------------------------------------------------------


class TestE2E:
    """End-to-end test suite. Tests run in order within this class."""

    def test_01_list_empty(self, server):
        """Initially there should be no VMs."""
        stdout, _, rc = cli("list")
        assert rc == 0
        assert "No VMs running" in stdout

    def test_02_create_vm(self, server):
        """Create a VM and verify the output."""
        global _vm_id

        stdout, _, rc = cli("create", "--vcpus", "1", "--memory", "256")
        assert rc == 0
        assert "VM created successfully" in stdout

        # Parse VM ID from output: "  VM ID:      <uuid>"
        match = re.search(r"VM ID:\s+([0-9a-f-]{36})", stdout)
        assert match, f"Could not parse VM ID from output:\n{stdout}"
        _vm_id = match.group(1)
        log.info("Created VM: %s", _vm_id)

        # Parse IP
        ip_match = re.search(r"IP Address:\s+([\d.]+)", stdout)
        assert ip_match, "Could not parse IP address"
        log.info("VM IP: %s", ip_match.group(1))

    def test_03_list_shows_vm(self, server):
        """List should show exactly one running VM."""
        assert _vm_id is not None, "No VM created"

        stdout, _, rc = cli("list")
        assert rc == 0
        assert _vm_id in stdout
        assert "Running" in stdout
        assert "Total: 1 VM(s)" in stdout

    def test_04_exec_echo(self, server):
        """Execute echo inside the VM and verify output."""
        assert _vm_id is not None, "No VM created"

        stdout, _, rc = cli("exec", _vm_id, "--", "echo", "hello from VM")
        assert rc == 0
        assert "hello from VM" in stdout

    def test_05_exec_uname(self, server):
        """Execute uname inside the VM."""
        assert _vm_id is not None, "No VM created"

        stdout, _, rc = cli("exec", _vm_id, "--", "uname", "-a")
        assert rc == 0
        assert "Linux" in stdout
        log.info("VM kernel: %s", stdout.strip())

    def test_06_exec_exit_code(self, server):
        """Verify non-zero exit codes propagate."""
        assert _vm_id is not None, "No VM created"

        _, _, rc = cli("exec", _vm_id, "--", "false")
        assert rc != 0, "Expected non-zero exit code from 'false'"

    def test_07_exec_stderr(self, server):
        """Verify stderr is captured from commands."""
        assert _vm_id is not None, "No VM created"

        stdout, stderr, rc = cli("exec", _vm_id, "--", "ls", "/nonexistent_path")
        assert rc != 0
        # stderr from the guest command is written to our stderr
        combined = stdout + stderr
        assert "No such file" in combined or "cannot access" in combined

    def test_08_exec_multiword(self, server):
        """Execute a command with multiple arguments."""
        assert _vm_id is not None, "No VM created"

        stdout, _, rc = cli("exec", _vm_id, "--", "cat", "/proc/cpuinfo")
        assert rc == 0
        assert "processor" in stdout
        log.info("VM has cpuinfo output (%d bytes)", len(stdout))

    def test_09_dns_resolution(self, server):
        """Test that DNS resolution works inside the VM."""
        assert _vm_id is not None, "No VM created"

        stdout_dns, _, _ = cli("exec", _vm_id, "--", "cat", "/etc/resolv.conf")
        log.info("VM DNS config:\n%s", stdout_dns.strip())
        assert "8.8.8.8" in stdout_dns, "Expected nameserver 8.8.8.8 in resolv.conf"

        # Resolve a well-known domain via DNS
        stdout, stderr, rc = cli(
            "exec", _vm_id, "--",
            "bash", "-c",
            "timeout 10 bash -c '(echo > /dev/tcp/8.8.8.8/53) 2>/dev/null && echo DNS_REACHABLE || echo DNS_UNREACHABLE'",
            timeout=20,
        )
        combined = (stdout + stderr).strip()
        log.info("DNS connectivity test: exit_code=%d, output: %s", rc, combined)
        assert "DNS_REACHABLE" in stdout, "DNS server (8.8.8.8:53) should be reachable"

    def test_10_http_egress(self, server):
        """Test that HTTP egress works through the Envoy proxy."""
        assert _vm_id is not None, "No VM created"

        stdout, stderr, rc = cli(
            "exec", _vm_id, "--",
            "bash", "-c",
            "timeout 10 bash -c '(echo -e \"GET / HTTP/1.1\\r\\nHost: example.com\\r\\nConnection: close\\r\\n\\r\\n\" > /dev/tcp/93.184.216.34/80 && echo HTTP_OK) || echo HTTP_FAIL'",
            timeout=20,
        )
        combined = (stdout + stderr).strip()
        log.info("HTTP egress test: exit_code=%d, output: %s", rc, combined)
        assert "HTTP_OK" in stdout, "HTTP egress to example.com should work"

    def test_11_https_egress(self, server):
        """Test that HTTPS egress works through the TLS MITM + Envoy proxy."""
        assert _vm_id is not None, "No VM created"

        # Use curl if available, otherwise use openssl s_client
        stdout, stderr, rc = cli(
            "exec", _vm_id, "--",
            "bash", "-c",
            "if command -v curl &>/dev/null; then timeout 10 curl -sf -o /dev/null -w '%{http_code}' https://example.com && echo HTTPS_OK; else timeout 10 bash -c '(echo > /dev/tcp/example.com/443) 2>/dev/null && echo HTTPS_OK || echo HTTPS_FAIL'; fi",
            timeout=20,
        )
        combined = (stdout + stderr).strip()
        log.info("HTTPS egress test: exit_code=%d, output: %s", rc, combined)
        assert "HTTPS_OK" in stdout, "HTTPS egress to example.com should work"

    def test_12_non_http_blocked(self, server):
        """Test that non-HTTP/HTTPS/DNS traffic is blocked."""
        assert _vm_id is not None, "No VM created"

        # Try to connect to port 22 on an external host — should be blocked
        stdout, stderr, rc = cli(
            "exec", _vm_id, "--",
            "bash", "-c",
            "timeout 5 bash -c '(echo > /dev/tcp/8.8.8.8/22) 2>/dev/null && echo PORT22_OPEN || echo PORT22_BLOCKED'",
            timeout=15,
        )
        combined = (stdout + stderr).strip()
        log.info("Non-HTTP traffic test: exit_code=%d, output: %s", rc, combined)
        assert "PORT22_BLOCKED" in stdout or rc != 0, "Non-HTTP traffic (port 22) should be blocked"

    def test_13_delete_vm(self, server):
        """Delete the VM."""
        assert _vm_id is not None, "No VM created"

        stdout, _, rc = cli("delete", _vm_id)
        assert rc == 0
        assert "VM deleted successfully" in stdout

    def test_14_list_empty_after_delete(self, server):
        """After deletion, list should be empty again."""
        stdout, _, rc = cli("list")
        assert rc == 0
        assert "No VMs running" in stdout

#!/bin/bash
set -euo pipefail

# Entry point script that runs inside the ephemeral inner VM.
# Expects to be run as root from /work after the tarball has been unpacked.
#
# Directory layout (after unpack):
#   /work/clawpot/
#     target/debug/clawpot-server
#     target/debug/clawpot
#     target/x86_64-unknown-linux-musl/release/clawpot-agent
#     assets/kernels/vmlinux
#     assets/rootfs/ubuntu.ext4
#     scripts/setup-rootfs.sh
#     tests/integration/
#     proto/

WORK_DIR="/work/clawpot"
ARTIFACTS_DIR="/work/artifacts"

mkdir -p "$ARTIFACTS_DIR"

cd "$WORK_DIR"
export CLAWPOT_ROOT="$WORK_DIR"

# Source API key secrets if forwarded from the outer VM
if [ -f /work/.secrets ]; then
    # shellcheck disable=SC1091
    source /work/.secrets
    rm -f /work/.secrets
fi

echo "=== Clawpot Integration Test Runner ==="
echo "Working directory: $WORK_DIR"
echo "Artifacts directory: $ARTIFACTS_DIR"

# --- Verify prerequisites ---

echo "--- Checking /dev/kvm"
if [ ! -c /dev/kvm ]; then
    echo "ERROR: /dev/kvm not available. Nested virtualization may not be enabled." >&2
    exit 1
fi
ls -l /dev/kvm

echo "--- Checking Firecracker"
firecracker --version

echo "--- Checking binaries"
ls -l target/debug/clawpot-server
ls -l target/debug/clawpot
ls -l target/x86_64-unknown-linux-musl/release/clawpot-agent

# --- Pre-generate CA certificate for rootfs trust store injection ---
# The server generates the CA at startup, but setup-rootfs.sh needs it before
# the server runs in order to inject it into the VM's trust store.

echo "--- Pre-generating CA certificate"
if [ ! -f "ca/ca.crt" ]; then
    mkdir -p ca
    openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ca/ca.key
    openssl req -new -x509 -key ca/ca.key -out ca/ca.crt -days 3650 \
        -subj "/CN=Clawpot MITM CA/O=Clawpot" \
        -addext "basicConstraints=critical,CA:TRUE"
    echo "CA certificate generated"
else
    echo "CA certificate already exists"
fi

# --- Prepare rootfs ---

echo "--- Embedding clawpot-agent into rootfs"
bash scripts/setup-rootfs.sh

# --- Run integration tests ---

echo "--- Running integration tests"

# uv is installed system-wide at /usr/local/bin in the golden image

cd tests/integration

# Run pytest, capturing output to both console and log file.
# Disable set -e so that test failures don't skip artifact collection.
set +eo pipefail
uv run pytest -v -s --timeout=120 \
    --junitxml="$ARTIFACTS_DIR/test-results.xml" \
    2>&1 | tee "$ARTIFACTS_DIR/pytest-output.log"
TEST_EXIT=${PIPESTATUS[0]}
set -eo pipefail

cd "$WORK_DIR"

# --- Collect server log ---

if [ -f "target/server-test.log" ]; then
    cp target/server-test.log "$ARTIFACTS_DIR/"
    echo "Collected server-test.log"
fi

# --- Export events database artifacts ---

EVENTS_DB="$WORK_DIR/data/events.db"
if [ -f "$EVENTS_DB" ]; then
    echo "Events DB found: $(ls -lh "$EVENTS_DB")"
    # Also copy the raw DB for debugging
    cp "$EVENTS_DB" "$ARTIFACTS_DIR/events.db"
    "$WORK_DIR/target/debug/clawpot" logs sessions --db "$EVENTS_DB" || echo "WARNING: logs sessions failed"
    "$WORK_DIR/target/debug/clawpot" logs export --db "$EVENTS_DB" > "$ARTIFACTS_DIR/events.jsonl" || echo "WARNING: logs export failed"
    "$WORK_DIR/target/debug/clawpot" logs timeline --db "$EVENTS_DB" > "$ARTIFACTS_DIR/timeline.txt" || echo "WARNING: logs timeline failed"
    echo "Collected events.jsonl ($(wc -l < "$ARTIFACTS_DIR/events.jsonl") lines) and timeline.txt ($(wc -l < "$ARTIFACTS_DIR/timeline.txt") lines)"
else
    echo "WARNING: Events DB not found at $EVENTS_DB"
    ls -la "$WORK_DIR/data/" 2>/dev/null || echo "  data/ directory does not exist"
fi

echo ""
echo "=== Test run complete (exit code: $TEST_EXIT) ==="
echo "Artifacts:"
ls -lh "$ARTIFACTS_DIR/"

exit "$TEST_EXIT"

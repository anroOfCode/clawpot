#!/bin/bash
set -euo pipefail

# Buildkite integration test step:
# 1. Launch ephemeral inner VM
# 2. SCP build tarball into it
# 3. Run install_and_test.sh via SSH
# 4. Collect artifacts
# 5. Destroy the VM (always, even on failure)

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$PROJECT_ROOT"

CI_DIR="$PROJECT_ROOT/ci/inner-vm"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
CONN_ENV=""

cleanup() {
    echo "--- :broom: Cleanup"
    if [ -n "$CONN_ENV" ] && [ -f "$CONN_ENV" ]; then
        "$CI_DIR/destroy.sh" "$CONN_ENV"
    fi
}
trap cleanup EXIT

# --- Launch inner VM ---
echo "--- :rocket: Launch ephemeral inner VM"
CONN_ENV=$("$CI_DIR/launch.sh" "${BUILDKITE_JOB_ID:-$(date +%s)}")

# shellcheck source=/dev/null
source "$CONN_ENV"

SSH_CMD="ssh $SSH_OPTS -i $INNER_VM_SSH_KEY -p $INNER_VM_SSH_PORT ${INNER_VM_SSH_USER}@${INNER_VM_SSH_HOST}"
SCP_CMD="scp $SSH_OPTS -i $INNER_VM_SSH_KEY -P $INNER_VM_SSH_PORT"

echo "Inner VM ready (PID: $INNER_VM_PID, SSH port: $INNER_VM_SSH_PORT)"

# --- Transfer build tarball ---
echo "--- :arrow_up: Upload build tarball"

TARBALL="$PROJECT_ROOT/build.tar.gz"
if [ ! -f "$TARBALL" ]; then
    echo "ERROR: build.tar.gz not found. Build step must run first." >&2
    exit 1
fi

$SCP_CMD "$TARBALL" "${INNER_VM_SSH_USER}@${INNER_VM_SSH_HOST}:/work/build.tar.gz"
echo "Tarball uploaded"

# --- Run tests inside inner VM ---
echo "--- :microscope: Run integration tests"

# Unpack and run install_and_test.sh as root inside the inner VM
$SSH_CMD "cd /work && tar xzf build.tar.gz && sudo bash /work/clawpot/install_and_test.sh"
TEST_EXIT=$?

# --- Collect artifacts ---
echo "--- :floppy_disk: Collect artifacts"

mkdir -p "$PROJECT_ROOT/artifacts"
$SCP_CMD "${INNER_VM_SSH_USER}@${INNER_VM_SSH_HOST}:/work/artifacts/*" \
         "$PROJECT_ROOT/artifacts/" 2>/dev/null || echo "Warning: some artifacts may not have been produced"

# List collected artifacts
if ls "$PROJECT_ROOT/artifacts/"* &>/dev/null; then
    echo "Collected artifacts:"
    ls -lh "$PROJECT_ROOT/artifacts/"
else
    echo "No artifacts collected"
fi

# Inner VM is destroyed by the EXIT trap

exit "$TEST_EXIT"

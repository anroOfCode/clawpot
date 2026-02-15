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

# --- Ensure golden image is up to date ---
echo "--- :framed_picture: Check inner VM golden image"

GOLDEN_IMG="/var/lib/clawpot-ci/inner-vm.qcow2"
GOLDEN_HASH_FILE="$GOLDEN_IMG.sha256"
BUILD_IMAGE_SCRIPT="$CI_DIR/build-image.sh"

CURRENT_HASH=$(sha256sum "$BUILD_IMAGE_SCRIPT" | awk '{print $1}')
STORED_HASH=""
if [ -f "$GOLDEN_HASH_FILE" ]; then
    STORED_HASH=$(cat "$GOLDEN_HASH_FILE")
fi

if [ ! -f "$GOLDEN_IMG" ] || [ "$CURRENT_HASH" != "$STORED_HASH" ]; then
    if [ ! -f "$GOLDEN_IMG" ]; then
        echo "Golden image not found, building..."
    else
        echo "build-image.sh has changed, rebuilding golden image..."
    fi
    sudo bash "$BUILD_IMAGE_SCRIPT"
else
    echo "Golden image is up to date"
fi

# --- Launch inner VM ---
echo "--- :rocket: Launch ephemeral inner VM"
CONN_ENV=$("$CI_DIR/launch.sh" "${BUILDKITE_JOB_ID:-$(date +%s)}")

# shellcheck source=/dev/null
source "$CONN_ENV"

SSH_CMD="ssh $SSH_OPTS -i $INNER_VM_SSH_KEY -p $INNER_VM_SSH_PORT ${INNER_VM_SSH_USER}@${INNER_VM_SSH_HOST}"
SCP_CMD="scp $SSH_OPTS -i $INNER_VM_SSH_KEY -P $INNER_VM_SSH_PORT"

echo "Inner VM ready (PID: $INNER_VM_PID, SSH port: $INNER_VM_SSH_PORT)"

# --- Download build tarball from build step ---
echo "--- :arrow_down: Download build tarball"

TARBALL="$PROJECT_ROOT/build.tar.gz"
buildkite-agent artifact download build.tar.gz .
if [ ! -f "$TARBALL" ]; then
    echo "ERROR: build.tar.gz not found. Build step must have uploaded it." >&2
    exit 1
fi

# --- Transfer build tarball to inner VM ---
echo "--- :arrow_up: Upload build tarball to inner VM"

$SCP_CMD "$TARBALL" "${INNER_VM_SSH_USER}@${INNER_VM_SSH_HOST}:/work/build.tar.gz"
echo "Tarball uploaded"

# --- Forward API key secrets to inner VM ---
_secrets_file=$(mktemp)
for _var in CLAWPOT_ANTHROPIC_API_KEY CLAWPOT_OPENAI_API_KEY; do
    if [ -n "${!_var:-}" ]; then
        printf 'export %s=%q\n' "$_var" "${!_var}" >> "$_secrets_file"
    fi
done
if [ -s "$_secrets_file" ]; then
    $SCP_CMD "$_secrets_file" "${INNER_VM_SSH_USER}@${INNER_VM_SSH_HOST}:/work/.secrets"
    $SSH_CMD "chmod 600 /work/.secrets"
    echo "Forwarded API key secrets to inner VM"
else
    echo "No API key secrets to forward"
fi
rm -f "$_secrets_file"

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

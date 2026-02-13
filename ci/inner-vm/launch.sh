#!/bin/bash
set -euo pipefail

# Launches an ephemeral inner VM from the golden image.
# Creates a COW overlay so the golden image is never mutated.
# Outputs connection details to stdout for the caller to parse.
#
# Usage: ci/inner-vm/launch.sh [job-id]
# Output: Writes SSH connection info to /tmp/inner-vm-<job-id>/connection.env

GREEN='\033[0;32m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $*" >&2; }

CI_DIR="/var/lib/clawpot-ci"
GOLDEN_IMG="$CI_DIR/inner-vm.qcow2"
SSH_KEY="$CI_DIR/ssh/id_ed25519"
JOB_ID="${1:-$(date +%s)}"

VM_DIR="/tmp/inner-vm-${JOB_ID}"
OVERLAY_IMG="$VM_DIR/overlay.qcow2"

if [ ! -f "$GOLDEN_IMG" ]; then
    echo "ERROR: Golden image not found at $GOLDEN_IMG" >&2
    echo "Run ci/inner-vm/build-image.sh first" >&2
    exit 1
fi

if [ ! -f "$SSH_KEY" ]; then
    echo "ERROR: SSH key not found at $SSH_KEY" >&2
    exit 1
fi

# Clean up any previous run with this job ID
rm -rf "$VM_DIR"
mkdir -p "$VM_DIR"

# Create COW overlay — writes go here, golden image stays clean
info "Creating COW overlay for job $JOB_ID..."
qemu-img create -b "$GOLDEN_IMG" -F qcow2 -f qcow2 "$OVERLAY_IMG" >&2

# Pick a random high port for SSH forwarding
SSH_PORT=$(shuf -i 10000-60000 -n 1)

# Launch QEMU
info "Launching inner VM (SSH port $SSH_PORT)..."
qemu-system-x86_64 \
    -m 4096 \
    -smp 2 \
    -cpu host \
    -enable-kvm \
    -drive "file=$OVERLAY_IMG,format=qcow2,if=virtio" \
    -netdev "user,id=net0,hostfwd=tcp::${SSH_PORT}-:22" \
    -device virtio-net-pci,netdev=net0 \
    -display none \
    -serial "file:$VM_DIR/console.log" \
    -pidfile "$VM_DIR/qemu.pid" \
    -daemonize

QEMU_PID=$(cat "$VM_DIR/qemu.pid")
info "QEMU started with PID $QEMU_PID"

# Wait for SSH to become available
info "Waiting for SSH..."
MAX_WAIT=120
ELAPSED=0

while [ $ELAPSED -lt $MAX_WAIT ]; do
    if ssh -i "$SSH_KEY" \
           -o StrictHostKeyChecking=no \
           -o UserKnownHostsFile=/dev/null \
           -o ConnectTimeout=3 \
           -o BatchMode=yes \
           -p "$SSH_PORT" \
           ci@localhost true 2>/dev/null; then
        info "SSH is ready"
        break
    fi
    sleep 3
    ELAPSED=$((ELAPSED + 3))
done

if [ $ELAPSED -ge $MAX_WAIT ]; then
    echo "ERROR: SSH did not become available within ${MAX_WAIT}s" >&2
    echo "Console log:" >&2
    tail -20 "$VM_DIR/console.log" >&2
    kill "$QEMU_PID" 2>/dev/null || true
    exit 1
fi

# Write connection details
cat > "$VM_DIR/connection.env" << EOF
INNER_VM_SSH_PORT=$SSH_PORT
INNER_VM_SSH_KEY=$SSH_KEY
INNER_VM_SSH_USER=ci
INNER_VM_SSH_HOST=localhost
INNER_VM_PID=$QEMU_PID
INNER_VM_DIR=$VM_DIR
INNER_VM_JOB_ID=$JOB_ID
EOF

info "Inner VM is ready"
info "Connection details written to $VM_DIR/connection.env"

# Print the env file path to stdout for the caller
echo "$VM_DIR/connection.env"

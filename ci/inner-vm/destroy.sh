#!/bin/bash
set -euo pipefail

# Tears down an ephemeral inner VM.
#
# Usage: ci/inner-vm/destroy.sh <connection-env-file>
#    or: ci/inner-vm/destroy.sh <job-id>

GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }

ARG="${1:?Usage: destroy.sh <connection.env file or job-id>}"

# Accept either a connection.env path or a job ID
if [ -f "$ARG" ]; then
    ENV_FILE="$ARG"
else
    ENV_FILE="/tmp/inner-vm-${ARG}/connection.env"
fi

if [ ! -f "$ENV_FILE" ]; then
    warn "Connection file not found: $ENV_FILE"
    warn "VM may have already been destroyed"
    exit 0
fi

# shellcheck source=/dev/null
source "$ENV_FILE"

info "Destroying inner VM (job: ${INNER_VM_JOB_ID:-unknown}, PID: ${INNER_VM_PID:-unknown})..."

# Kill the QEMU process
if [ -n "${INNER_VM_PID:-}" ] && kill -0 "$INNER_VM_PID" 2>/dev/null; then
    kill "$INNER_VM_PID" 2>/dev/null || true
    sleep 2
    # Force kill if still running
    if kill -0 "$INNER_VM_PID" 2>/dev/null; then
        warn "QEMU did not exit gracefully, force killing..."
        kill -9 "$INNER_VM_PID" 2>/dev/null || true
    fi
    info "QEMU process stopped"
else
    info "QEMU process already stopped"
fi

# Remove the VM directory (overlay image, logs, connection info)
if [ -n "${INNER_VM_DIR:-}" ] && [ -d "$INNER_VM_DIR" ]; then
    rm -rf "$INNER_VM_DIR"
    info "Cleaned up $INNER_VM_DIR"
fi

info "Inner VM destroyed"

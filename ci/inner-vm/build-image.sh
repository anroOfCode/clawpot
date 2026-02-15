#!/bin/bash
set -euo pipefail

# Builds the golden qcow2 image for ephemeral inner VMs.
# Run on the outer VM (clawpot-ci) as root.
# Re-run whenever the inner VM's dependencies change.
#
# The golden image is never modified at runtime — each job creates
# a COW overlay via launch.sh.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

if [ "$(id -u)" -ne 0 ]; then
    error "This script must be run as root"
    exit 1
fi

CI_DIR="/var/lib/clawpot-ci"
BASE_IMG="$CI_DIR/ubuntu-24.04-cloudimg.img"
GOLDEN_IMG="$CI_DIR/inner-vm.qcow2"
SSH_PUBKEY="$CI_DIR/ssh/id_ed25519.pub"

FIRECRACKER_VERSION="v1.9.1"
ARCH="$(uname -m)"

if [ ! -f "$BASE_IMG" ]; then
    error "Base cloud image not found at $BASE_IMG"
    error "This should have been downloaded during outer VM provisioning"
    exit 1
fi

if [ ! -f "$SSH_PUBKEY" ]; then
    error "SSH public key not found at $SSH_PUBKEY"
    error "This should have been generated during outer VM provisioning"
    exit 1
fi

# Create the golden image from the cloud base
info "Creating golden image from cloud base..."
cp "$BASE_IMG" "$GOLDEN_IMG"
qemu-img resize "$GOLDEN_IMG" 20G

# Build cloud-init config for the inner VM
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

CI_SSH_PUBKEY=$(cat "$SSH_PUBKEY")

cat > "$TMPDIR/user-data" << EOF
#cloud-config
hostname: clawpot-test

users:
  - name: ci
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    groups: kvm
    ssh_authorized_keys:
      - $CI_SSH_PUBKEY

package_update: true

packages:
  - e2fsprogs
  - iptables
  - iproute2
  - curl
  - file
  - openssh-server
  - python3
  - python3-pip

runcmd:
  # Install uv (system-wide so it works under sudo too)
  - curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh

  # Install Firecracker
  - |
    curl -fsSL "https://github.com/firecracker-microvm/firecracker/releases/download/${FIRECRACKER_VERSION}/firecracker-${FIRECRACKER_VERSION}-${ARCH}.tgz" -o /tmp/firecracker.tgz
    tar -xzf /tmp/firecracker.tgz -C /tmp
    mv /tmp/release-${FIRECRACKER_VERSION}-${ARCH}/firecracker-${FIRECRACKER_VERSION}-${ARCH} /usr/local/bin/firecracker
    chmod +x /usr/local/bin/firecracker
    rm -rf /tmp/firecracker.tgz /tmp/release-${FIRECRACKER_VERSION}-${ARCH}

  # Ensure SSH is enabled
  - systemctl enable --now ssh

  # Create work directory
  - mkdir -p /work/artifacts
  - chown -R ci:ci /work

  # Shut down after provisioning so the host knows we're done
  - poweroff
EOF

cat > "$TMPDIR/meta-data" << EOF
instance-id: clawpot-inner-golden
local-hostname: clawpot-test
EOF

cloud-localds "$TMPDIR/cloud-init.iso" "$TMPDIR/user-data" "$TMPDIR/meta-data"

# Boot the image with cloud-init to install everything
info "Booting golden image to install dependencies (this takes a few minutes)..."
info "Starting QEMU in the background..."

qemu-system-x86_64 \
    -m 2048 \
    -smp 2 \
    -cpu host \
    -enable-kvm \
    -drive "file=$GOLDEN_IMG,format=qcow2,if=virtio" \
    -drive "file=$TMPDIR/cloud-init.iso,format=raw,if=virtio" \
    -netdev user,id=net0 \
    -device virtio-net-pci,netdev=net0 \
    -display none \
    -serial "file:$TMPDIR/console.log" \
    -pidfile "$TMPDIR/qemu.pid" \
    -daemonize

QEMU_PID=$(cat "$TMPDIR/qemu.pid")
info "QEMU started with PID $QEMU_PID"

# Wait for cloud-init to finish (poll via QMP or just wait)
info "Waiting for cloud-init to complete inside the golden image..."
info "This typically takes 3-5 minutes."

MAX_WAIT=600  # 10 minutes
ELAPSED=0
INTERVAL=15

while [ $ELAPSED -lt $MAX_WAIT ]; do
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        # QEMU exited — might be cloud-init poweroff or crash
        break
    fi
    sleep "$INTERVAL"
    ELAPSED=$((ELAPSED + INTERVAL))
    info "  ... waiting ($ELAPSED/${MAX_WAIT}s)"
done

# Give it a moment, then shut down
if kill -0 "$QEMU_PID" 2>/dev/null; then
    info "Sending ACPI shutdown to golden image VM..."
    kill "$QEMU_PID" 2>/dev/null || true
    sleep 5
    # Force kill if still running
    if kill -0 "$QEMU_PID" 2>/dev/null; then
        kill -9 "$QEMU_PID" 2>/dev/null || true
    fi
fi

# Remove the cloud-init disk from the image so it doesn't re-run on next boot
# (The cloud-init data is baked in now)
info "Golden image built successfully"

# Verify the image
GOLDEN_SIZE=$(qemu-img info --output=json "$GOLDEN_IMG" | jq -r '.["actual-size"]')
info "Golden image actual size: $(numfmt --to=iec-i --suffix=B "$GOLDEN_SIZE" 2>/dev/null || echo "${GOLDEN_SIZE} bytes")"

# Store a hash of this script so we can detect when a rebuild is needed
SCRIPT_PATH="$(readlink -f "${BASH_SOURCE[0]}")"
sha256sum "$SCRIPT_PATH" | awk '{print $1}' > "$GOLDEN_IMG.sha256"
info "Stored build hash at $GOLDEN_IMG.sha256"

echo ""
info "============================================"
info "  Golden inner VM image ready"
info "============================================"
info "Image: $GOLDEN_IMG"
info "To rebuild, re-run this script."

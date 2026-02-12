#!/bin/bash
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
ROOTFS_PATH="$PROJECT_ROOT/assets/rootfs/ubuntu.ext4"
AGENT_BINARY="$PROJECT_ROOT/target/x86_64-unknown-linux-musl/release/clawpot-agent"
MOUNT_POINT="/tmp/clawpot-rootfs-mount"

# Verify prerequisites
if [ ! -f "$ROOTFS_PATH" ]; then
    error "Rootfs not found at $ROOTFS_PATH"
    error "Run ./scripts/install-vm-assets.sh first"
    exit 1
fi

if [ ! -f "$AGENT_BINARY" ]; then
    error "Agent binary not found at $AGENT_BINARY"
    error "Build it first:"
    error "  rustup target add x86_64-unknown-linux-musl"
    error "  cargo build --release --target x86_64-unknown-linux-musl -p clawpot-agent"
    exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
    error "This script must be run as root (for mount)"
    exit 1
fi

info "Setting up rootfs with clawpot-agent..."

# Resize rootfs to ensure space (+32MB)
info "Resizing rootfs image..."
truncate -s +$((32 * 1024 * 1024)) "$ROOTFS_PATH"
e2fsck -f -y "$ROOTFS_PATH" || true
resize2fs "$ROOTFS_PATH"

# Mount
info "Mounting rootfs..."
mkdir -p "$MOUNT_POINT"
mount -o loop "$ROOTFS_PATH" "$MOUNT_POINT"

# Cleanup on exit
cleanup() {
    info "Unmounting rootfs..."
    sync
    umount "$MOUNT_POINT" 2>/dev/null || true
    rmdir "$MOUNT_POINT" 2>/dev/null || true
}
trap cleanup EXIT

# Copy agent binary
info "Copying agent binary..."
cp "$AGENT_BINARY" "$MOUNT_POINT/usr/local/bin/clawpot-agent"
chmod 755 "$MOUNT_POINT/usr/local/bin/clawpot-agent"

# Create systemd service
info "Creating systemd service..."
cat > "$MOUNT_POINT/etc/systemd/system/clawpot-agent.service" << 'UNIT'
[Unit]
Description=Clawpot Guest Agent
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/clawpot-agent
Restart=always
RestartSec=2
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
UNIT

# Enable the service via symlink
info "Enabling service..."
mkdir -p "$MOUNT_POINT/etc/systemd/system/multi-user.target.wants"
ln -sf /etc/systemd/system/clawpot-agent.service \
    "$MOUNT_POINT/etc/systemd/system/multi-user.target.wants/clawpot-agent.service"

info "Rootfs updated successfully with clawpot-agent!"
info "Agent binary size: $(du -h "$AGENT_BINARY" | cut -f1)"

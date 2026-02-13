#!/bin/bash
set -euo pipefail

# One-time bare-metal host setup for Clawpot CI.
# Enables nested virtualization and installs libvirt/QEMU.
# Run as root. Reboot recommended after first run.

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

# --- Detect CPU vendor and enable nested virtualization ---

if grep -q "vendor_id.*GenuineIntel" /proc/cpuinfo; then
    KVM_MODULE="kvm_intel"
elif grep -q "vendor_id.*AuthenticAMD" /proc/cpuinfo; then
    KVM_MODULE="kvm_amd"
else
    error "Unsupported CPU vendor. Intel or AMD required."
    exit 1
fi

info "Detected KVM module: $KVM_MODULE"

# Check current nested virt status
NESTED_PATH="/sys/module/$KVM_MODULE/parameters/nested"
if [ -f "$NESTED_PATH" ]; then
    CURRENT=$(cat "$NESTED_PATH")
    if [ "$CURRENT" = "Y" ] || [ "$CURRENT" = "1" ]; then
        info "Nested virtualization is already enabled"
    else
        warn "Nested virtualization is currently disabled ($CURRENT)"
    fi
fi

# Write persistent modprobe config
MODPROBE_CONF="/etc/modprobe.d/kvm-nested.conf"
info "Writing $MODPROBE_CONF..."
cat > "$MODPROBE_CONF" << EOF
options $KVM_MODULE nested=1
EOF
info "Nested virtualization configured persistently"

# Try to reload the module if no VMs are running
if ! lsmod | grep -q "$KVM_MODULE"; then
    warn "$KVM_MODULE is not loaded, will be loaded on next boot"
elif [ -f "$NESTED_PATH" ] && { [ "$(cat "$NESTED_PATH")" = "N" ] || [ "$(cat "$NESTED_PATH")" = "0" ]; }; then
    warn "Module is loaded but nested virt is off."
    warn "A reboot is required to enable nested virtualization."
    warn "Alternatively, if no VMs are running: modprobe -r $KVM_MODULE && modprobe $KVM_MODULE"
fi

# --- Install libvirt, QEMU, and utilities ---

info "Installing packages..."
apt-get update -qq
apt-get install -y \
    qemu-kvm \
    libvirt-daemon-system \
    libvirt-clients \
    virtinst \
    cloud-image-utils \
    qemu-utils

systemctl enable --now libvirtd
info "libvirtd is running"

# --- Download Ubuntu 24.04 cloud image ---

UBUNTU_IMG_DIR="/var/lib/libvirt/images"
UBUNTU_IMG="$UBUNTU_IMG_DIR/ubuntu-24.04-server-cloudimg-amd64.img"

if [ -f "$UBUNTU_IMG" ]; then
    info "Ubuntu 24.04 cloud image already exists at $UBUNTU_IMG"
else
    info "Downloading Ubuntu 24.04 cloud image..."
    curl -fsSL --progress-bar \
        "https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img" \
        -o "$UBUNTU_IMG"
    info "Downloaded to $UBUNTU_IMG"
fi

# --- Generate SSH keypair for CI VM access ---

CI_SSH_DIR="/home/${SUDO_USER:-$USER}/.ssh"
CI_SSH_KEY="$CI_SSH_DIR/clawpot-ci"

if [ -f "$CI_SSH_KEY" ]; then
    info "CI SSH key already exists at $CI_SSH_KEY"
else
    info "Generating CI SSH keypair (passwordless, for automated VM access)..."
    sudo -u "${SUDO_USER:-$USER}" ssh-keygen -t ed25519 -f "$CI_SSH_KEY" -N "" -C "clawpot-ci-access"
    info "Generated $CI_SSH_KEY"
fi

CI_SSH_PUBKEY=$(cat "${CI_SSH_KEY}.pub")
info "Public key: $CI_SSH_PUBKEY"
info "Add this key to ci/outer-vm/cloud-init.yaml under ssh_authorized_keys"

echo ""
info "============================================"
info "  Host setup complete"
info "============================================"
echo ""
info "Next steps:"
info "  1. Reboot to ensure nested virtualization is active"
info "  2. Verify: cat $NESTED_PATH  (should show Y or 1)"
info "  3. Run: BUILDKITE_AGENT_TOKEN=xxx ci/outer-vm/provision.sh"

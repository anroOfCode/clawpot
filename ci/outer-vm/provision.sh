#!/bin/bash
set -euo pipefail

# Creates the Clawpot CI outer VM using libvirt.
# Run on the bare-metal host as root.
#
# Required: BUILDKITE_AGENT_TOKEN env var
# Prerequisite: ci/host-setup.sh has been run

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

if [ "$(id -u)" -ne 0 ]; then
    error "This script must be run as root"
    exit 1
fi

BUILDKITE_AGENT_TOKEN="${BUILDKITE_AGENT_TOKEN:?Set BUILDKITE_AGENT_TOKEN env var}"

VM_NAME="clawpot-ci"
VCPUS=4
RAM_MB=12288          # 12 GB — headroom for inner VMs
DISK_GB=60
UBUNTU_IMG="/var/lib/libvirt/images/ubuntu-24.04-server-cloudimg-amd64.img"
VM_DISK="/var/lib/libvirt/images/${VM_NAME}.qcow2"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Verify prerequisites
if [ ! -f "$UBUNTU_IMG" ]; then
    error "Ubuntu cloud image not found at $UBUNTU_IMG"
    error "Run ci/host-setup.sh first"
    exit 1
fi

# Destroy existing VM if present
if virsh dominfo "$VM_NAME" &>/dev/null; then
    info "Destroying existing VM '$VM_NAME'..."
    virsh destroy "$VM_NAME" 2>/dev/null || true
    virsh undefine "$VM_NAME" --remove-all-storage 2>/dev/null || true
fi

# Create disk from cloud image
info "Creating VM disk ($DISK_GB GB)..."
cp "$UBUNTU_IMG" "$VM_DISK"
qemu-img resize "$VM_DISK" "${DISK_GB}G"

# Generate cloud-init ISO with real token substituted
# Place the ISO in libvirt's images dir so QEMU can access it
CLOUD_INIT_ISO="/var/lib/libvirt/images/${VM_NAME}-cloud-init.iso"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

sed "s/REPLACE_WITH_AGENT_TOKEN/${BUILDKITE_AGENT_TOKEN}/" \
    "$SCRIPT_DIR/cloud-init.yaml" > "$TMPDIR/user-data"

cat > "$TMPDIR/meta-data" << EOF
instance-id: ${VM_NAME}
local-hostname: ${VM_NAME}
EOF

info "Generating cloud-init seed ISO..."
cloud-localds "$CLOUD_INIT_ISO" "$TMPDIR/user-data" "$TMPDIR/meta-data"

# Create VM
# --cpu host-passthrough is critical: exposes host CPU features (VMX/SVM)
# to the guest, enabling nested virtualization for Firecracker inside inner VMs.
info "Creating VM '$VM_NAME'..."
virt-install \
    --name "$VM_NAME" \
    --vcpus "$VCPUS" \
    --memory "$RAM_MB" \
    --disk "path=$VM_DISK,format=qcow2,bus=virtio" \
    --disk "path=$CLOUD_INIT_ISO,device=cdrom" \
    --os-variant ubuntu24.04 \
    --network network=default,model=virtio \
    --cpu host-passthrough \
    --graphics none \
    --console pty,target_type=serial \
    --noautoconsole \
    --import

echo ""
info "============================================"
info "  VM '$VM_NAME' created and booting"
info "============================================"
echo ""
info "cloud-init will install all dependencies (~5-10 minutes on first boot)."
echo ""
info "Monitor progress:"
info "  virsh console $VM_NAME"
echo ""
info "Get VM IP:"
info "  virsh domifaddr $VM_NAME"
echo ""
info "SSH in (after cloud-init completes):"
info "  ssh buildkite@\$(virsh domifaddr $VM_NAME | grep -oP '[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+')"

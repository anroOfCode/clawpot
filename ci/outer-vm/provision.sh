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

info "VM '$VM_NAME' created, waiting for cloud-init to complete..."

# --- Locate the CI SSH key (used for both VM access and GitHub deploy key) ---

CI_SSH_KEY="/home/${SUDO_USER:-$USER}/.ssh/clawpot-ci"
if [ ! -f "$CI_SSH_KEY" ]; then
    error "CI SSH key not found at $CI_SSH_KEY"
    error "Run ci/host-setup.sh first"
    exit 1
fi

# --- Wait for VM to get an IP and SSH to become available ---

info "Waiting for SSH..."
VM_IP=""
for i in $(seq 1 60); do
    VM_IP=$(virsh domifaddr "$VM_NAME" 2>/dev/null | grep -oP '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' || true)
    if [ -n "$VM_IP" ]; then
        if ssh -i "$CI_SSH_KEY" -o StrictHostKeyChecking=no -o ConnectTimeout=3 -o BatchMode=yes \
            "ci@$VM_IP" "echo ok" &>/dev/null; then
            break
        fi
    fi
    sleep 10
done

if [ -z "$VM_IP" ]; then
    error "Timed out waiting for VM IP"
    exit 1
fi

info "VM is reachable at $VM_IP"

# --- Wait for cloud-init to finish ---

info "Waiting for cloud-init..."
SSH_CMD="ssh -i $CI_SSH_KEY -o StrictHostKeyChecking=no ci@$VM_IP"
for i in $(seq 1 60); do
    STATUS=$($SSH_CMD "cloud-init status 2>/dev/null" 2>/dev/null || echo "unknown")
    if echo "$STATUS" | grep -qE "done|error"; then
        break
    fi
    sleep 15
done

if echo "$STATUS" | grep -q "error"; then
    error "cloud-init finished with errors"
    $SSH_CMD "cloud-init status --long" 2>&1 || true
    exit 1
fi

info "cloud-init complete"

# --- Copy CI SSH key to buildkite-agent for GitHub access ---

info "Configuring GitHub SSH access for buildkite-agent..."
scp -i "$CI_SSH_KEY" -o StrictHostKeyChecking=no "$CI_SSH_KEY" "ci@$VM_IP:/tmp/ci-deploy-key" >/dev/null 2>&1

$SSH_CMD bash -s << 'REMOTE'
sudo mkdir -p /var/lib/buildkite-agent/.ssh
sudo mv /tmp/ci-deploy-key /var/lib/buildkite-agent/.ssh/id_ed25519
sudo chown -R buildkite-agent:buildkite-agent /var/lib/buildkite-agent/.ssh
sudo chmod 700 /var/lib/buildkite-agent/.ssh
sudo chmod 600 /var/lib/buildkite-agent/.ssh/id_ed25519
sudo su - buildkite-agent -c 'ssh-keyscan github.com >> ~/.ssh/known_hosts 2>/dev/null'
REMOTE

info "GitHub SSH access configured"

# --- Done ---

echo ""
info "============================================"
info "  VM '$VM_NAME' is ready"
info "============================================"
echo ""
info "  IP:  $VM_IP"
info "  SSH: ssh -i $CI_SSH_KEY ci@$VM_IP"
info ""
info "Buildkite agent is running. Trigger a build to verify."

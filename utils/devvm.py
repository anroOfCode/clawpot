#!/usr/bin/env python3
"""Dev VM manager — launch, sync, and run commands in a QEMU dev VM.

Provides a clean environment with /dev/kvm, Firecracker, Rust, and all
test dependencies. Uses the same cloud-init approach as CI inner VMs
but runs directly on the host.

Usage:
    sudo python utils/devvm.py launch [--rebuild]
    python utils/devvm.py status
    python utils/devvm.py sync
    python utils/devvm.py ssh
    python utils/devvm.py run <command>
    python utils/devvm.py destroy
"""

import argparse
import os
import random
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

# Persistent state (survives VM destroy, holds golden image + keys)
DEV_DIR = "/var/lib/clawpot-dev"
GOLDEN_IMG = f"{DEV_DIR}/devvm-golden.qcow2"
BASE_IMG = f"{DEV_DIR}/ubuntu-24.04-cloudimg.img"
SSH_KEY = f"{DEV_DIR}/ssh/id_ed25519"
SSH_PUBKEY = f"{DEV_DIR}/ssh/id_ed25519.pub"

# Runtime state (per-VM instance, destroyed with VM)
VM_DIR = "/tmp/devvm"
OVERLAY_IMG = f"{VM_DIR}/overlay.qcow2"
CONN_ENV = f"{VM_DIR}/connection.env"
PID_FILE = f"{VM_DIR}/qemu.pid"
CONSOLE_LOG = f"{VM_DIR}/console.log"

BASE_IMG_URL = "https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"
FIRECRACKER_VERSION = "v1.9.1"

SSH_USER = "ci"
SSH_OPTS = [
    "-o", "StrictHostKeyChecking=no",
    "-o", "UserKnownHostsFile=/dev/null",
    "-o", "LogLevel=ERROR",
]

GREEN = "\033[0;32m"
RED = "\033[0;31m"
YELLOW = "\033[0;33m"
NC = "\033[0m"


def info(msg):
    print(f"{GREEN}[INFO]{NC} {msg}", file=sys.stderr)


def error(msg):
    print(f"{RED}[ERROR]{NC} {msg}", file=sys.stderr)


def warn(msg):
    print(f"{YELLOW}[WARN]{NC} {msg}", file=sys.stderr)


def run(cmd, **kwargs):
    """Run a command, raising on failure."""
    return subprocess.run(cmd, check=True, **kwargs)


def project_root():
    """Find the project root (directory containing Cargo.toml)."""
    d = os.path.dirname(os.path.abspath(__file__))
    while d != "/":
        if os.path.exists(os.path.join(d, "Cargo.toml")):
            return d
        d = os.path.dirname(d)
    error("Could not find project root (no Cargo.toml found)")
    sys.exit(1)


def load_connection():
    """Load connection.env and return dict, or None if not running."""
    if not os.path.exists(CONN_ENV):
        return None
    conn = {}
    with open(CONN_ENV) as f:
        for line in f:
            line = line.strip()
            if "=" in line and not line.startswith("#"):
                k, v = line.split("=", 1)
                conn[k] = v
    # Verify the process is actually alive
    pid = conn.get("DEVVM_PID")
    if pid:
        try:
            os.kill(int(pid), 0)
        except ProcessLookupError:
            # PID doesn't exist — VM is gone
            return None
        except PermissionError:
            # PID exists but owned by root — VM is running
            pass
        except (OSError, ValueError):
            return None
    return conn


def ssh_cmd(conn, extra_opts=None):
    """Build base SSH command from connection info."""
    cmd = [
        "ssh",
        "-i", SSH_KEY,
        "-p", conn["DEVVM_SSH_PORT"],
        *SSH_OPTS,
    ]
    if extra_opts:
        cmd.extend(extra_opts)
    cmd.append(f"{SSH_USER}@localhost")
    return cmd


# --- Bootstrap helpers ---


def ensure_dev_dir():
    os.makedirs(DEV_DIR, exist_ok=True)
    os.makedirs(f"{DEV_DIR}/ssh", exist_ok=True)


def ensure_ssh_key():
    if os.path.exists(SSH_KEY):
        return
    info("Generating SSH keypair for dev VM access...")
    run([
        "ssh-keygen", "-t", "ed25519",
        "-f", SSH_KEY, "-N", "", "-C", "clawpot-dev",
    ])
    # Make the key usable by the real (non-root) user when run via sudo.
    # SSH requires private keys to be owned by the user running ssh.
    real_user = os.environ.get("SUDO_USER")
    if real_user:
        import pwd
        pw = pwd.getpwnam(real_user)
        for f in [SSH_KEY, SSH_PUBKEY]:
            os.chown(f, pw.pw_uid, pw.pw_gid)
        os.chown(f"{DEV_DIR}/ssh", pw.pw_uid, pw.pw_gid)
    info(f"Generated {SSH_KEY}")


def ensure_base_image():
    if os.path.exists(BASE_IMG):
        return
    info("Downloading Ubuntu 24.04 cloud image (this may take a minute)...")
    run(["curl", "-fsSL", "--progress-bar", BASE_IMG_URL, "-o", BASE_IMG])
    info(f"Downloaded to {BASE_IMG}")


def build_golden_image():
    """Build the golden qcow2 image with cloud-init provisioning."""
    info("Building golden dev VM image (this takes 3-5 minutes)...")

    # Create golden image from cloud base
    shutil.copy2(BASE_IMG, GOLDEN_IMG)
    run(["qemu-img", "resize", GOLDEN_IMG, "20G"])

    ssh_pubkey = open(SSH_PUBKEY).read().strip()
    arch = os.uname().machine

    with tempfile.TemporaryDirectory() as tmpdir:
        # Write cloud-init user-data
        user_data = f"""\
#cloud-config
hostname: clawpot-dev

users:
  - name: ci
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    groups: kvm
    ssh_authorized_keys:
      - {ssh_pubkey}

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
  - build-essential
  - pkg-config
  - libssl-dev
  - protobuf-compiler

runcmd:
  # Install uv system-wide
  - curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh

  # Install Rust toolchain for the ci user
  - su - ci -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"

  # Install musl target
  - apt-get install -y musl-tools
  - su - ci -c ". ~/.cargo/env && rustup target add x86_64-unknown-linux-musl"

  # Install Firecracker
  - |
    curl -fsSL "https://github.com/firecracker-microvm/firecracker/releases/download/{FIRECRACKER_VERSION}/firecracker-{FIRECRACKER_VERSION}-{arch}.tgz" -o /tmp/firecracker.tgz
    tar -xzf /tmp/firecracker.tgz -C /tmp
    mv /tmp/release-{FIRECRACKER_VERSION}-{arch}/firecracker-{FIRECRACKER_VERSION}-{arch} /usr/local/bin/firecracker
    chmod +x /usr/local/bin/firecracker
    rm -rf /tmp/firecracker.tgz /tmp/release-{FIRECRACKER_VERSION}-{arch}

  # Ensure SSH is enabled
  - systemctl enable --now ssh

  # Create work directory
  - mkdir -p /work/artifacts
  - chown -R ci:ci /work

  # Power off when done
  - poweroff
"""
        meta_data = """\
instance-id: clawpot-dev-golden
local-hostname: clawpot-dev
"""
        with open(f"{tmpdir}/user-data", "w") as f:
            f.write(user_data)
        with open(f"{tmpdir}/meta-data", "w") as f:
            f.write(meta_data)

        run(["cloud-localds", f"{tmpdir}/cloud-init.iso",
             f"{tmpdir}/user-data", f"{tmpdir}/meta-data"])

        # Boot with cloud-init
        info("Booting golden image for provisioning...")
        run([
            "qemu-system-x86_64",
            "-m", "2048", "-smp", "2",
            "-cpu", "host", "-enable-kvm",
            "-drive", f"file={GOLDEN_IMG},format=qcow2,if=virtio",
            "-drive", f"file={tmpdir}/cloud-init.iso,format=raw,if=virtio",
            "-netdev", "user,id=net0",
            "-device", "virtio-net-pci,netdev=net0",
            "-display", "none",
            "-serial", f"file:{tmpdir}/console.log",
            "-pidfile", f"{tmpdir}/qemu.pid",
            "-daemonize",
        ])

        qemu_pid = int(open(f"{tmpdir}/qemu.pid").read().strip())
        info(f"Provisioning VM started (PID {qemu_pid})")

        # Wait for cloud-init to finish (VM powers off when done)
        max_wait = 600
        elapsed = 0
        interval = 15
        while elapsed < max_wait:
            try:
                os.kill(qemu_pid, 0)
            except OSError:
                break
            time.sleep(interval)
            elapsed += interval
            info(f"  ... waiting ({elapsed}/{max_wait}s)")

        if elapsed >= max_wait:
            try:
                os.kill(qemu_pid, signal.SIGKILL)
            except OSError:
                pass
            error("Golden image provisioning timed out")
            sys.exit(1)

    info("Golden dev VM image built successfully")


# --- Subcommands ---


def cmd_launch(args):
    if os.geteuid() != 0:
        error("launch requires root (for KVM access). Run with sudo.")
        sys.exit(1)

    conn = load_connection()
    if conn is not None:
        error("Dev VM is already running. Use 'destroy' first or 'status' to check.")
        sys.exit(1)

    # Bootstrap prerequisites
    ensure_dev_dir()
    ensure_ssh_key()
    ensure_base_image()

    if args.rebuild or not os.path.exists(GOLDEN_IMG):
        build_golden_image()

    # Create runtime directory (world-readable so non-root commands work)
    if os.path.exists(VM_DIR):
        shutil.rmtree(VM_DIR)
    os.makedirs(VM_DIR, mode=0o755)

    # Create COW overlay
    info("Creating COW overlay...")
    run(["qemu-img", "create", "-b", GOLDEN_IMG, "-F", "qcow2",
         "-f", "qcow2", OVERLAY_IMG])

    # Pick random SSH port
    ssh_port = random.randint(10000, 60000)

    # Launch QEMU
    info(f"Launching dev VM (SSH port {ssh_port})...")
    run([
        "qemu-system-x86_64",
        "-m", "4096", "-smp", "2",
        "-cpu", "host", "-enable-kvm",
        "-drive", f"file={OVERLAY_IMG},format=qcow2,if=virtio",
        "-netdev", f"user,id=net0,hostfwd=tcp::{ssh_port}-:22",
        "-device", "virtio-net-pci,netdev=net0",
        "-display", "none",
        "-serial", f"file:{CONSOLE_LOG}",
        "-pidfile", PID_FILE,
        "-daemonize",
    ])

    qemu_pid = open(PID_FILE).read().strip()
    info(f"QEMU started (PID {qemu_pid})")

    # Wait for SSH
    info("Waiting for SSH...")
    max_wait = 120
    elapsed = 0
    while elapsed < max_wait:
        result = subprocess.run(
            ["ssh", "-i", SSH_KEY, "-p", str(ssh_port), *SSH_OPTS,
             "-o", "ConnectTimeout=3", "-o", "BatchMode=yes",
             f"{SSH_USER}@localhost", "true"],
            capture_output=True,
        )
        if result.returncode == 0:
            info("SSH is ready")
            break
        time.sleep(3)
        elapsed += 3

    if elapsed >= max_wait:
        error(f"SSH did not become available within {max_wait}s")
        error(f"Check console log: {CONSOLE_LOG}")
        sys.exit(1)

    # Write connection env
    with open(CONN_ENV, "w") as f:
        f.write(f"DEVVM_SSH_PORT={ssh_port}\n")
        f.write(f"DEVVM_SSH_KEY={SSH_KEY}\n")
        f.write(f"DEVVM_SSH_USER={SSH_USER}\n")
        f.write(f"DEVVM_SSH_HOST=localhost\n")
        f.write(f"DEVVM_PID={qemu_pid}\n")
        f.write(f"DEVVM_DIR={VM_DIR}\n")

    # Make runtime files readable by non-root users
    for f in [CONN_ENV, CONSOLE_LOG, PID_FILE]:
        if os.path.exists(f):
            os.chmod(f, 0o644)

    info("Dev VM is ready!")
    info(f"  SSH: ssh -i {SSH_KEY} -p {ssh_port} {SSH_USER}@localhost")
    info(f"  Or:  python utils/devvm.py ssh")


def cmd_destroy(args):
    conn = load_connection()
    if conn is None:
        warn("No running dev VM found")
        # Clean up stale runtime dir if it exists
        if os.path.exists(VM_DIR):
            shutil.rmtree(VM_DIR)
            info(f"Cleaned up stale {VM_DIR}")
        return

    pid = int(conn["DEVVM_PID"])
    info(f"Destroying dev VM (PID {pid})...")

    try:
        os.kill(pid, signal.SIGTERM)
        time.sleep(2)
        try:
            os.kill(pid, 0)
            warn("QEMU did not exit gracefully, force killing...")
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
        info("QEMU process stopped")
    except OSError:
        info("QEMU process already stopped")

    if os.path.exists(VM_DIR):
        shutil.rmtree(VM_DIR)
        info(f"Cleaned up {VM_DIR}")

    info("Dev VM destroyed")


def cmd_status(args):
    conn = load_connection()
    if conn is None:
        print("No dev VM running")
        return

    print(f"Dev VM is running")
    print(f"  PID:      {conn['DEVVM_PID']}")
    print(f"  SSH port: {conn['DEVVM_SSH_PORT']}")
    print(f"  SSH cmd:  ssh -i {SSH_KEY} -p {conn['DEVVM_SSH_PORT']} {SSH_USER}@localhost")


def cmd_sync(args):
    conn = load_connection()
    if conn is None:
        error("No running dev VM. Run 'launch' first.")
        sys.exit(1)

    root = project_root()
    info(f"Syncing {root} -> {SSH_USER}@vm:/work/clawpot/")

    run([
        "rsync", "-az", "--delete",
        "--exclude", "target/",
        "--exclude", ".git/",
        "--exclude", "__pycache__/",
        "--exclude", "assets/kernels/",
        "--exclude", "assets/rootfs/",
        "-e", f"ssh -i {SSH_KEY} -p {conn['DEVVM_SSH_PORT']} {' '.join(SSH_OPTS)}",
        f"{root}/",
        f"{SSH_USER}@localhost:/work/clawpot/",
    ])

    info("Sync complete")


def cmd_ssh(args):
    conn = load_connection()
    if conn is None:
        error("No running dev VM. Run 'launch' first.")
        sys.exit(1)

    cmd = ssh_cmd(conn, ["-t"])
    os.execvp(cmd[0], cmd)


def cmd_run(args):
    conn = load_connection()
    if conn is None:
        error("No running dev VM. Run 'launch' first.")
        sys.exit(1)

    cmd = ssh_cmd(conn)
    # Wrap in a login shell so ~/.profile and ~/.cargo/env are sourced
    cmd.append(f"bash -l -c {shlex.quote(args.command)}")
    result = subprocess.run(cmd)
    sys.exit(result.returncode)


def main():
    parser = argparse.ArgumentParser(
        description="Dev VM manager for Clawpot",
        prog="devvm",
    )
    sub = parser.add_subparsers(dest="subcmd", required=True)

    p_launch = sub.add_parser("launch", help="Launch a dev VM")
    p_launch.add_argument("--rebuild", action="store_true",
                          help="Rebuild the golden image even if it exists")

    sub.add_parser("destroy", help="Destroy the dev VM")
    sub.add_parser("status", help="Show dev VM status")
    sub.add_parser("sync", help="Sync working tree to the dev VM")
    sub.add_parser("ssh", help="SSH into the dev VM")

    p_run = sub.add_parser("run", help="Run a command in the dev VM")
    p_run.add_argument("command", help="Command to run (quoted)")

    args = parser.parse_args()

    commands = {
        "launch": cmd_launch,
        "destroy": cmd_destroy,
        "status": cmd_status,
        "sync": cmd_sync,
        "ssh": cmd_ssh,
        "run": cmd_run,
    }
    commands[args.subcmd](args)


if __name__ == "__main__":
    main()

# Clawpot

A Firecracker-based microVM driver for running lightweight, isolated Linux VMs with minimal overhead.

## Overview

Clawpot provides a simple Rust-based CLI tool for managing Firecracker VMs. It handles VM lifecycle management including starting, stopping, and monitoring virtual machines running minimal Ubuntu images.

## Features

- Fast VM boot times (under 1 second)
- Simple CLI interface for VM management
- Minimal resource overhead
- Automated VM asset downloading
- Clean lifecycle management with proper cleanup

## Prerequisites

- Linux host with KVM support
- `/dev/kvm` accessible (for hardware virtualization)
- Firecracker v1.9.1 or later installed
- Rust toolchain (for building)

### Checking KVM Support

```bash
# Check if KVM device exists
ls -l /dev/kvm

# Check KVM kernel module
lsmod | grep kvm
```

If the KVM module is not loaded:
```bash
# For Intel processors
sudo modprobe kvm_intel

# For AMD processors
sudo modprobe kvm_amd
```

## Installation

### 1. Install Firecracker

If using the devcontainer, Firecracker is already installed. Otherwise:

```bash
FIRECRACKER_VERSION="v1.9.1"
ARCH="$(uname -m)"

curl -fsSL "https://github.com/firecracker-microvm/firecracker/releases/download/${FIRECRACKER_VERSION}/firecracker-${FIRECRACKER_VERSION}-${ARCH}.tgz" -o /tmp/firecracker.tgz

tar -xzf /tmp/firecracker.tgz -C /tmp
sudo mv /tmp/release-${FIRECRACKER_VERSION}-${ARCH}/firecracker-${FIRECRACKER_VERSION}-${ARCH} /usr/local/bin/firecracker
sudo chmod +x /usr/local/bin/firecracker

firecracker --version
```

### 2. Download VM Assets

Download the kernel and root filesystem images:

```bash
./scripts/install-vm-assets.sh
```

This will download:
- Linux kernel (vmlinux) - ~80MB
- Ubuntu minimal rootfs - ~300MB

Assets are stored in `assets/kernels/` and `assets/rootfs/`.

### 3. Build clawpot-driver

```bash
cd clawpot-driver
cargo build --release
```

The binary will be available at `target/release/clawpot-driver`.

## Usage

### Starting a VM

```bash
sudo ./clawpot-driver/target/release/clawpot-driver start \
  --kernel assets/kernels/vmlinux \
  --rootfs assets/rootfs/ubuntu.ext4
```

With custom resources:

```bash
sudo ./clawpot-driver/target/release/clawpot-driver start \
  --kernel assets/kernels/vmlinux \
  --rootfs assets/rootfs/ubuntu.ext4 \
  --vcpus 2 \
  --memory 512
```

The VM will start and run until you press Ctrl+C.

### Checking VM Status

In another terminal:

```bash
sudo ./clawpot-driver/target/release/clawpot-driver status
```

### Stopping a VM

The VM will automatically stop when you press Ctrl+C in the terminal running the `start` command.

Alternatively, you can stop it manually:

```bash
sudo ./clawpot-driver/target/release/clawpot-driver stop
```

## CLI Reference

### `clawpot-driver start`

Start a Firecracker VM.

**Options:**
- `--kernel <PATH>` - Path to kernel image (required)
- `--rootfs <PATH>` - Path to rootfs image (required)
- `--vcpus <N>` - Number of virtual CPUs (default: 1)
- `--memory <MB>` - Memory in MiB (default: 256)
- `--socket <PATH>` - Socket path for Firecracker API (default: /tmp/firecracker.sock)

### `clawpot-driver stop`

Stop a running VM.

**Options:**
- `--socket <PATH>` - Socket path for Firecracker API (default: /tmp/firecracker.sock)

### `clawpot-driver status`

Get the status of a running VM.

**Options:**
- `--socket <PATH>` - Socket path for Firecracker API (default: /tmp/firecracker.sock)

## Architecture

### Components

1. **Installation Script** (`scripts/install-vm-assets.sh`)
   - Downloads pre-built kernel and rootfs images
   - Validates downloads
   - Idempotent (safe to run multiple times)

2. **Firecracker Module** (`clawpot-driver/src/firecracker/`)
   - `client.rs` - HTTP client for Firecracker Unix socket API
   - `config.rs` - VM configuration builder
   - `models.rs` - Data models for API requests/responses

3. **VM Module** (`clawpot-driver/src/vm/`)
   - `manager.rs` - High-level VM lifecycle manager
   - `lifecycle.rs` - State machine for VM states

4. **CLI** (`clawpot-driver/src/main.rs`)
   - Command-line interface
   - Argument parsing and command routing

### VM Lifecycle

```
NotStarted → Starting → Running → Stopping → Stopped
                ↓           ↓
              Error       Error
```

The VM manager handles:
1. Starting Firecracker process with Unix socket
2. Waiting for socket to be ready
3. Configuring VM via API (boot source, drives, CPU/memory)
4. Starting the instance
5. Monitoring and cleanup

## Troubleshooting

### "No such device" error

Ensure KVM is available:
```bash
ls -l /dev/kvm
lsmod | grep kvm
```

### "Permission denied on /dev/kvm"

Run with `sudo` or add your user to the `kvm` group:
```bash
sudo usermod -aG kvm $USER
# Log out and back in for changes to take effect
```

### "Socket already exists"

Clean up stale socket files:
```bash
rm /tmp/firecracker.sock
```

### VM doesn't boot

- Verify kernel and rootfs paths are correct
- Check files exist and are not corrupted
- Review Firecracker logs for errors

## Development

### Project Structure

```
clawpot/
├── assets/                   # VM assets (kernel, rootfs)
│   ├── kernels/
│   └── rootfs/
├── scripts/
│   └── install-vm-assets.sh  # Asset installation script
├── clawpot-driver/           # Rust application
│   ├── src/
│   │   ├── firecracker/      # Firecracker API client
│   │   ├── vm/               # VM lifecycle management
│   │   └── main.rs           # CLI entry point
│   └── Cargo.toml
└── README.md
```

### Building from Source

```bash
# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build debug version
cd clawpot-driver
cargo build

# Build release version
cargo build --release

# Run tests
cargo test
```

### Running Tests

```bash
cargo test --workspace
```

## Future Enhancements

Potential improvements for future versions:

- Networking support (TAP devices, NAT)
- Multiple VM instance management
- Jailer integration for sandboxing
- Custom kernel/rootfs builder scripts
- Metrics and monitoring
- Interactive console access

## License

This project is provided as-is for educational and development purposes.

## Resources

- [Firecracker Documentation](https://github.com/firecracker-microvm/firecracker/tree/main/docs)
- [Firecracker Getting Started](https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md)
- [Firecracker Rootfs Setup](https://github.com/firecracker-microvm/firecracker/blob/main/docs/rootfs-and-kernel-setup.md)

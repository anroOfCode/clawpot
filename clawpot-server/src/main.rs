mod agent;
mod grpc;
mod network;
mod vm;

use anyhow::{Context, Result};
use clawpot_common::proto::clawpot_service_server::ClawpotServiceServer;
use grpc::ClawpotServiceImpl;
use network::{ip_allocator::IpAllocator, NetworkManager};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::Mutex;
use tonic::transport::Server;
use tracing::{error, info, warn};
use vm::VmRegistry;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    info!("Starting clawpot-server...");

    // Check if running as root
    if !nix::unistd::geteuid().is_root() {
        error!("Server must be run as root (use sudo)");
        anyhow::bail!("Server must be run as root (sudo required for TAP devices, bridge, and iptables)");
    }

    info!("Running as root ✓");

    // Initialize networking
    let network_manager = Arc::new(NetworkManager::new());

    info!("Ensuring network bridge exists...");
    network_manager
        .ensure_bridge()
        .context("Failed to ensure bridge exists")?;

    info!("Network bridge ready ✓");

    // Initialize IP allocator
    let ip_allocator = Arc::new(Mutex::new(IpAllocator::new()));
    info!("IP allocator initialized (192.168.100.2-254) ✓");

    // Create VM registry
    let vm_registry = Arc::new(VmRegistry::new());
    info!("VM registry initialized ✓");

    // Server configuration - resolve paths relative to the binary or use env override
    let project_root = std::env::var("CLAWPOT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/workspaces/clawpot"));
    let kernel_path = project_root.join("assets/kernels/vmlinux");
    let rootfs_path = project_root.join("assets/rootfs/ubuntu.ext4");

    // Verify assets exist
    if !kernel_path.exists() {
        error!("Kernel not found at {:?}", kernel_path);
        anyhow::bail!(
            "Kernel image not found. Run './scripts/install-vm-assets.sh' to download assets."
        );
    }

    if !rootfs_path.exists() {
        error!("Rootfs not found at {:?}", rootfs_path);
        anyhow::bail!(
            "Rootfs image not found. Run './scripts/install-vm-assets.sh' to download assets."
        );
    }

    info!("VM assets verified ✓");

    // Create gRPC service
    let service = ClawpotServiceImpl::new(
        vm_registry.clone(),
        ip_allocator.clone(),
        network_manager.clone(),
        kernel_path,
        rootfs_path,
    );

    // Bind address
    let addr = "0.0.0.0:50051".parse()?;
    info!("Starting gRPC server on {}", addr);

    // Start gRPC server with graceful shutdown
    Server::builder()
        .add_service(ClawpotServiceServer::new(service))
        .serve_with_shutdown(addr, shutdown_signal(vm_registry, network_manager, ip_allocator))
        .await
        .context("gRPC server failed")?;

    info!("Server shut down successfully");

    Ok(())
}

/// Graceful shutdown handler
async fn shutdown_signal(
    registry: Arc<VmRegistry>,
    network_manager: Arc<NetworkManager>,
    ip_allocator: Arc<Mutex<IpAllocator>>,
) {
    // Wait for Ctrl+C
    match signal::ctrl_c().await {
        Ok(()) => {
            info!("Received Ctrl+C, initiating graceful shutdown...");
        }
        Err(err) => {
            error!("Failed to listen for Ctrl+C: {}", err);
            return;
        }
    }

    // Cleanup all VMs
    info!("Cleaning up all VMs...");

    let vms_list = registry.list().await;
    info!("Found {} VMs to clean up", vms_list.len());

    for (vm_id, ip_address, tap_name, _, _, _) in vms_list {
        info!("Cleaning up VM {}", vm_id);

        // Remove from registry and stop VM
        match registry.remove(&vm_id).await {
            Ok(mut entry) => {
                // Stop VM
                if let Err(e) = entry.manager.stop().await {
                    warn!("Failed to stop VM {}: {}", vm_id, e);
                }

                // Delete TAP device
                if let Err(e) = network_manager.delete_tap(&tap_name, ip_address) {
                    warn!("Failed to delete TAP device {}: {}", tap_name, e);
                }

                // Release IP
                if let Err(e) = ip_allocator.lock().await.release(ip_address) {
                    warn!("Failed to release IP {}: {}", ip_address, e);
                }

                info!("VM {} cleaned up successfully", vm_id);
            }
            Err(e) => {
                warn!("Failed to remove VM {} from registry: {}", vm_id, e);
            }
        }
    }

    info!("All VMs cleaned up. Server shutting down.");
}

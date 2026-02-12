use crate::network::{ip_allocator::IpAllocator, NetworkManager};
use crate::vm::{VmEntry, VmId, VmRegistry};
use anyhow::Context;
use clawpot_common::firecracker::VmConfig;
use clawpot_common::proto::{
    clawpot_service_server::ClawpotService, CreateVmRequest, CreateVmResponse, DeleteVmRequest,
    DeleteVmResponse, ListVmsRequest, ListVmsResponse, VmInfo, VmState as ProtoVmState,
};
use clawpot_common::vm::VmManager;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{error, info};
use uuid::Uuid;

/// gRPC service implementation for Clawpot
pub struct ClawpotServiceImpl {
    vm_registry: Arc<VmRegistry>,
    ip_allocator: Arc<Mutex<IpAllocator>>,
    network_manager: Arc<NetworkManager>,
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
}

impl ClawpotServiceImpl {
    pub fn new(
        vm_registry: Arc<VmRegistry>,
        ip_allocator: Arc<Mutex<IpAllocator>>,
        network_manager: Arc<NetworkManager>,
        kernel_path: PathBuf,
        rootfs_path: PathBuf,
    ) -> Self {
        Self {
            vm_registry,
            ip_allocator,
            network_manager,
            kernel_path,
            rootfs_path,
        }
    }
}

#[tonic::async_trait]
impl ClawpotService for ClawpotServiceImpl {
    async fn create_vm(
        &self,
        request: Request<CreateVmRequest>,
    ) -> Result<Response<CreateVmResponse>, Status> {
        let req = request.into_inner();
        info!(
            "CreateVM request: vcpus={:?}, memory={:?}",
            req.vcpu_count, req.mem_size_mib
        );

        // Generate VM ID
        let vm_id = Uuid::new_v4();
        info!("Generated VM ID: {}", vm_id);

        // Allocate IP address
        let ip_address = self
            .ip_allocator
            .lock()
            .await
            .allocate()
            .map_err(|e| {
                error!("Failed to allocate IP: {}", e);
                Status::resource_exhausted(format!("No available IP addresses: {}", e))
            })?;

        info!("Allocated IP address: {}", ip_address);

        // Create TAP device name (max 15 chars for Linux interface names)
        // Use first 11 chars of UUID: "tap-" (4 chars) + UUID prefix (11 chars) = 15 chars
        let uuid_short = &vm_id.simple().to_string()[..11];
        let tap_name = format!("tap-{}", uuid_short);

        // Create and configure TAP device
        if let Err(e) = self.network_manager.create_tap(&tap_name, ip_address) {
            // Cleanup: release IP
            let _ = self.ip_allocator.lock().await.release(ip_address);
            error!("Failed to create TAP device: {}", e);
            return Err(Status::internal(format!(
                "Failed to create TAP device: {}",
                e
            )));
        }

        info!("Created TAP device: {}", tap_name);

        // Build VM configuration
        let vcpu_count = req.vcpu_count.unwrap_or(1) as u8;
        let mem_size_mib = req.mem_size_mib.unwrap_or(256);

        let config = VmConfig::new(self.kernel_path.clone(), self.rootfs_path.clone())
            .with_vcpus(vcpu_count)
            .with_memory(mem_size_mib)
            .with_network(tap_name.clone(), ip_address.to_string());

        // Create socket path
        let socket_path = PathBuf::from(format!("/tmp/fc-{}.sock", vm_id.simple()));

        // Create and start VM manager
        let mut manager = VmManager::new(socket_path.clone());

        if let Err(e) = manager.start(config).await {
            // Cleanup: delete TAP, release IP
            let _ = self.network_manager.delete_tap(&tap_name, ip_address);
            let _ = self.ip_allocator.lock().await.release(ip_address);
            error!("Failed to start VM: {}", e);
            return Err(Status::internal(format!("Failed to start VM: {}", e)));
        }

        info!("VM {} started successfully", vm_id);

        // Create VM entry
        let entry = VmEntry {
            id: vm_id,
            manager,
            ip_address,
            tap_name,
            created_at: SystemTime::now(),
            vcpu_count,
            mem_size_mib,
        };

        // Insert into registry
        if let Err(e) = self.vm_registry.insert(vm_id, entry).await {
            error!("Failed to insert VM into registry: {}", e);
            // Note: VM is already started, this is a critical error
            return Err(Status::internal(format!(
                "Failed to register VM: {}",
                e
            )));
        }

        // Return response
        Ok(Response::new(CreateVmResponse {
            vm_id: vm_id.to_string(),
            ip_address: ip_address.to_string(),
            socket_path: socket_path.to_string_lossy().to_string(),
        }))
    }

    async fn delete_vm(
        &self,
        request: Request<DeleteVmRequest>,
    ) -> Result<Response<DeleteVmResponse>, Status> {
        let req = request.into_inner();
        info!("DeleteVM request: vm_id={}", req.vm_id);

        // Parse VM ID
        let vm_id = Uuid::parse_str(&req.vm_id).map_err(|e| {
            error!("Invalid VM ID format: {}", e);
            Status::invalid_argument(format!("Invalid VM ID: {}", e))
        })?;

        // Remove from registry
        let mut entry = self.vm_registry.remove(&vm_id).await.map_err(|e| {
            error!("VM not found: {}", e);
            Status::not_found(format!("VM not found: {}", e))
        })?;

        info!("Removed VM {} from registry", vm_id);

        // Stop VM
        if let Err(e) = entry.manager.stop().await {
            error!("Failed to stop VM {}: {}", vm_id, e);
            // Continue with cleanup even if stop fails
        }

        // Delete TAP device
        if let Err(e) = self
            .network_manager
            .delete_tap(&entry.tap_name, entry.ip_address)
        {
            error!("Failed to delete TAP device: {}", e);
            // Continue with cleanup
        }

        // Release IP address
        if let Err(e) = self.ip_allocator.lock().await.release(entry.ip_address) {
            error!("Failed to release IP address: {}", e);
            // Continue
        }

        info!("VM {} deleted successfully", vm_id);

        Ok(Response::new(DeleteVmResponse { success: true }))
    }

    async fn list_v_ms(
        &self,
        _request: Request<ListVmsRequest>,
    ) -> Result<Response<ListVmsResponse>, Status> {
        info!("ListVMs request");

        let vms_list = self.vm_registry.list().await;

        let vms: Vec<VmInfo> = vms_list
            .into_iter()
            .map(|(id, ip_address, _tap_name, vcpu_count, mem_size_mib, created_at)| {
                let created_timestamp = created_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;

                VmInfo {
                    vm_id: id.to_string(),
                    state: ProtoVmState::Running as i32, // Simplified for now
                    ip_address: ip_address.to_string(),
                    vcpu_count: vcpu_count as u32,
                    mem_size_mib,
                    created_at: created_timestamp,
                    socket_path: format!("/tmp/fc-{}.sock", id.simple()),
                }
            })
            .collect();

        info!("Returning {} VMs", vms.len());

        Ok(Response::new(ListVmsResponse { vms }))
    }
}

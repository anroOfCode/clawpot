use crate::agent;
use crate::network::{ip_allocator::IpAllocator, NetworkManager};
use crate::vm::{VmEntry, VmRegistry};
use clawpot_common::firecracker::VmConfig;
use clawpot_common::proto::{
    clawpot_service_server::ClawpotService, CreateVmRequest, CreateVmResponse, DeleteVmRequest,
    DeleteVmResponse, ExecVmRequest, ExecVmResponse, ExecVmStreamInput, ExecVmStreamOutput,
    ListVmsRequest, ListVmsResponse, VmInfo, VmState as ProtoVmState,
};
use clawpot_common::vm::VmManager;
use clawpot_common::AGENT_VSOCK_PORT;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{error, info, warn, Span};
use uuid::Uuid;

const GUEST_CID: u32 = 3;

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
    #[tracing::instrument(
        name = "grpc.CreateVM",
        skip_all,
        fields(
            vm_id = tracing::field::Empty,
            vcpu_count = tracing::field::Empty,
            mem_size_mib = tracing::field::Empty,
            ip_address = tracing::field::Empty,
        )
    )]
    async fn create_vm(
        &self,
        request: Request<CreateVmRequest>,
    ) -> Result<Response<CreateVmResponse>, Status> {
        let req = request.into_inner();
        let span = Span::current();
        let vcpu_count_val = req.vcpu_count.unwrap_or(1);
        let mem_size_mib_val = req.mem_size_mib.unwrap_or(256);
        span.record("vcpu_count", vcpu_count_val);
        span.record("mem_size_mib", mem_size_mib_val);
        info!(
            "CreateVM request: vcpus={:?}, memory={:?}",
            req.vcpu_count, req.mem_size_mib
        );

        // Generate VM ID
        let vm_id = Uuid::new_v4();
        span.record("vm_id", vm_id.to_string().as_str());
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

        span.record("ip_address", ip_address.to_string().as_str());
        info!("Allocated IP address: {}", ip_address);

        // Create TAP device name (max 15 chars for Linux interface names)
        let uuid_short = &vm_id.simple().to_string()[..11];
        let tap_name = format!("tap-{}", uuid_short);

        // Create and configure TAP device
        if let Err(e) = self.network_manager.create_tap(&tap_name, ip_address).await {
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

        // Vsock UDS path for this VM
        let vsock_uds_path = format!("/tmp/fc-{}-vsock.sock", vm_id.simple());

        let config = VmConfig::new(self.kernel_path.clone(), self.rootfs_path.clone())
            .with_vcpus(vcpu_count)
            .with_memory(mem_size_mib)
            .with_network(tap_name.clone(), ip_address.to_string())
            .with_vsock(GUEST_CID, vsock_uds_path.clone());

        // Create socket path (Firecracker API socket)
        let socket_path = PathBuf::from(format!("/tmp/fc-{}.sock", vm_id.simple()));

        // Create and start VM manager
        let mut manager = VmManager::new(socket_path.clone());

        if let Err(e) = manager.start(config).await {
            let _ = self.network_manager.delete_tap(&tap_name, ip_address).await;
            let _ = self.ip_allocator.lock().await.release(ip_address);
            error!("Failed to start VM: {}", e);
            return Err(Status::internal(format!("Failed to start VM: {}", e)));
        }

        info!("VM {} started successfully", vm_id);

        // Wait for guest agent to become ready (non-fatal)
        info!("Waiting for guest agent on VM {}...", vm_id);
        match agent::client::AgentClient::wait_ready(
            &vsock_uds_path,
            Duration::from_secs(30),
        )
        .await
        {
            Ok(_) => info!("Guest agent ready on VM {}", vm_id),
            Err(e) => warn!(
                "Guest agent not ready on VM {} (will retry on exec): {}",
                vm_id, e
            ),
        }

        // Create VM entry
        let entry = VmEntry {
            id: vm_id,
            manager,
            ip_address,
            tap_name,
            created_at: SystemTime::now(),
            vcpu_count,
            mem_size_mib,
            vsock_uds_path,
        };

        // Insert into registry
        if let Err(e) = self.vm_registry.insert(vm_id, entry).await {
            error!("Failed to insert VM into registry: {}", e);
            return Err(Status::internal(format!(
                "Failed to register VM: {}",
                e
            )));
        }

        Ok(Response::new(CreateVmResponse {
            vm_id: vm_id.to_string(),
            ip_address: ip_address.to_string(),
            socket_path: socket_path.to_string_lossy().to_string(),
        }))
    }

    #[tracing::instrument(name = "grpc.DeleteVM", skip_all, fields(vm_id = tracing::field::Empty))]
    async fn delete_vm(
        &self,
        request: Request<DeleteVmRequest>,
    ) -> Result<Response<DeleteVmResponse>, Status> {
        let req = request.into_inner();
        Span::current().record("vm_id", req.vm_id.as_str());
        info!("DeleteVM request: vm_id={}", req.vm_id);

        let vm_id = Uuid::parse_str(&req.vm_id).map_err(|e| {
            error!("Invalid VM ID format: {}", e);
            Status::invalid_argument(format!("Invalid VM ID: {}", e))
        })?;

        let mut entry = self.vm_registry.remove(&vm_id).await.map_err(|e| {
            error!("VM not found: {}", e);
            Status::not_found(format!("VM not found: {}", e))
        })?;

        info!("Removed VM {} from registry", vm_id);

        if let Err(e) = entry.manager.stop().await {
            error!("Failed to stop VM {}: {}", vm_id, e);
        }

        if let Err(e) = self
            .network_manager
            .delete_tap(&entry.tap_name, entry.ip_address)
            .await
        {
            error!("Failed to delete TAP device: {}", e);
        }

        if let Err(e) = self.ip_allocator.lock().await.release(entry.ip_address) {
            error!("Failed to release IP address: {}", e);
        }

        // Clean up vsock UDS
        let _ = std::fs::remove_file(&entry.vsock_uds_path);

        info!("VM {} deleted successfully", vm_id);

        Ok(Response::new(DeleteVmResponse { success: true }))
    }

    #[tracing::instrument(name = "grpc.ListVMs", skip_all, fields(vm_count = tracing::field::Empty))]
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
                    state: ProtoVmState::Running as i32,
                    ip_address: ip_address.to_string(),
                    vcpu_count: vcpu_count as u32,
                    mem_size_mib,
                    created_at: created_timestamp,
                    socket_path: format!("/tmp/fc-{}.sock", id.simple()),
                }
            })
            .collect();

        Span::current().record("vm_count", vms.len());
        info!("Returning {} VMs", vms.len());

        Ok(Response::new(ListVmsResponse { vms }))
    }

    #[tracing::instrument(
        name = "grpc.ExecVM",
        skip_all,
        fields(vm_id = tracing::field::Empty, command = tracing::field::Empty, exit_code = tracing::field::Empty)
    )]
    async fn exec_vm(
        &self,
        request: Request<ExecVmRequest>,
    ) -> Result<Response<ExecVmResponse>, Status> {
        let req = request.into_inner();
        let span = Span::current();
        span.record("vm_id", req.vm_id.as_str());
        span.record("command", req.command.as_str());
        info!("ExecVM request: vm_id={}, cmd={}", req.vm_id, req.command);

        let vm_id = Uuid::parse_str(&req.vm_id).map_err(|e| {
            Status::invalid_argument(format!("Invalid VM ID: {}", e))
        })?;

        let vsock_path = self.vm_registry.get_vsock_path(&vm_id).await.map_err(|e| {
            Status::not_found(format!("VM not found: {}", e))
        })?;

        let mut agent_client =
            agent::client::AgentClient::connect(vsock_path)
                .await
                .map_err(|e| {
                    Status::unavailable(format!("Failed to connect to agent: {}", e))
                })?;

        let agent_req = clawpot_common::agent_proto::ExecRequest {
            command: req.command,
            args: req.args,
            env: req.env,
            working_dir: req.working_dir,
        };

        let agent_resp = agent_client.exec(agent_req).await.map_err(|e| {
            Status::internal(format!("Agent exec failed: {}", e))
        })?;

        span.record("exit_code", agent_resp.exit_code);

        Ok(Response::new(ExecVmResponse {
            exit_code: agent_resp.exit_code,
            stdout: agent_resp.stdout,
            stderr: agent_resp.stderr,
        }))
    }

    type ExecVMStreamStream =
        tokio_stream::wrappers::ReceiverStream<Result<ExecVmStreamOutput, Status>>;

    #[tracing::instrument(name = "grpc.ExecVMStream", skip_all)]
    async fn exec_vm_stream(
        &self,
        _request: Request<tonic::Streaming<ExecVmStreamInput>>,
    ) -> Result<Response<Self::ExecVMStreamStream>, Status> {
        // Streaming exec is more complex — implement in a follow-up
        Err(Status::unimplemented(
            "ExecVMStream not yet implemented. Use ExecVM for now.",
        ))
    }
}

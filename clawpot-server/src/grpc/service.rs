use crate::agent;
use crate::clawpot_event;
use crate::events::EventStore;
use crate::network::{ip_allocator::IpAllocator, NetworkManager};
use crate::vm::{VmEntry, VmRegistry};
use clawpot_common::firecracker::VmConfig;
use clawpot_common::proto::{
    clawpot_service_server::ClawpotService, CreateVmRequest, CreateVmResponse, DeleteVmRequest,
    DeleteVmResponse, ExecVmRequest, ExecVmResponse, ExecVmStreamInput, ExecVmStreamOutput,
    ListVmsRequest, ListVmsResponse, VmInfo, VmState as ProtoVmState,
};
use clawpot_common::vm::VmManager;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{error, Span};
use uuid::Uuid;

const GUEST_CID: u32 = 3;

/// gRPC service implementation for Clawpot
pub struct ClawpotServiceImpl {
    vm_registry: Arc<VmRegistry>,
    ip_allocator: Arc<Mutex<IpAllocator>>,
    network_manager: Arc<NetworkManager>,
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
    event_store: EventStore,
}

impl ClawpotServiceImpl {
    pub fn new(
        vm_registry: Arc<VmRegistry>,
        ip_allocator: Arc<Mutex<IpAllocator>>,
        network_manager: Arc<NetworkManager>,
        kernel_path: PathBuf,
        rootfs_path: PathBuf,
        event_store: EventStore,
    ) -> Self {
        Self {
            vm_registry,
            ip_allocator,
            network_manager,
            kernel_path,
            rootfs_path,
            event_store,
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
        let start = Instant::now();
        let req = request.into_inner();
        let span = Span::current();
        let vcpu_count_val = req.vcpu_count.unwrap_or(1);
        let mem_size_mib_val = req.mem_size_mib.unwrap_or(256);
        span.record("vcpu_count", vcpu_count_val);
        span.record("mem_size_mib", mem_size_mib_val);

        // Generate VM ID
        let vm_id = Uuid::new_v4();
        let vm_id_str = vm_id.to_string();
        span.record("vm_id", vm_id_str.as_str());

        clawpot_event!(self.event_store, "vm.create.started", "vm", vm_id = vm_id_str, {
            "vcpu_count": vcpu_count_val,
            "mem_size_mib": mem_size_mib_val
        });

        // Allocate IP address
        let ip_address = self.ip_allocator.lock().await.allocate().map_err(|e| {
            clawpot_event!(self.event_store, "vm.create.failed", "vm", vm_id = vm_id_str, {
                "error": e.to_string(),
                "step": "ip_allocation"
            });
            Status::resource_exhausted(format!("No available IP addresses: {e}"))
        })?;

        span.record("ip_address", ip_address.to_string().as_str());
        clawpot_event!(self.event_store, "vm.create.ip_allocated", "vm", vm_id = vm_id_str, {
            "ip_address": ip_address.to_string()
        });

        // Create TAP device name (max 15 chars for Linux interface names)
        let uuid_short = &vm_id.simple().to_string()[..11];
        let tap_name = format!("tap-{uuid_short}");

        // Create and configure TAP device
        if let Err(e) = self.network_manager.create_tap(&tap_name, ip_address).await {
            let _ = self.ip_allocator.lock().await.release(ip_address);
            clawpot_event!(self.event_store, "vm.create.failed", "vm", vm_id = vm_id_str, {
                "error": e.to_string(),
                "step": "tap_creation"
            });
            return Err(Status::internal(format!(
                "Failed to create TAP device: {e}"
            )));
        }

        clawpot_event!(self.event_store, "vm.create.tap_created", "vm", vm_id = vm_id_str, {
            "tap_name": tap_name
        });

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
            clawpot_event!(self.event_store, "vm.create.failed", "vm", vm_id = vm_id_str, {
                "error": e.to_string(),
                "step": "firecracker_start"
            });
            return Err(Status::internal(format!("Failed to start VM: {e}")));
        }

        clawpot_event!(self.event_store, "vm.create.firecracker_started", "vm", vm_id = vm_id_str, {
            "socket_path": socket_path.to_string_lossy().to_string(),
            "vsock_uds_path": vsock_uds_path
        });

        // Wait for guest agent to become ready (non-fatal)
        let agent_start = Instant::now();
        match agent::client::AgentClient::wait_ready(&vsock_uds_path, Duration::from_secs(30)).await
        {
            Ok(_) => {
                clawpot_event!(self.event_store, "vm.create.agent_ready", "vm", vm_id = vm_id_str, {
                    "wait_ms": agent_start.elapsed().as_millis() as i64
                });
            }
            Err(e) => {
                clawpot_event!(self.event_store, "vm.create.agent_timeout", "vm", vm_id = vm_id_str, {
                    "error": e.to_string()
                });
            }
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
            clawpot_event!(self.event_store, "vm.create.failed", "vm", vm_id = vm_id_str, {
                "error": e.to_string(),
                "step": "registry_insert"
            });
            return Err(Status::internal(format!("Failed to register VM: {e}")));
        }

        let duration_ms = start.elapsed().as_millis() as i64;
        self.event_store.emit_with_duration(
            "vm.create.completed",
            "vm",
            Some(&vm_id_str),
            None,
            duration_ms,
            Some(true),
            &serde_json::json!({
                "ip_address": ip_address.to_string(),
                "socket_path": socket_path.to_string_lossy().to_string(),
            }),
        );

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
        let start = Instant::now();
        let req = request.into_inner();
        Span::current().record("vm_id", req.vm_id.as_str());

        let vm_id = Uuid::parse_str(&req.vm_id)
            .map_err(|e| Status::invalid_argument(format!("Invalid VM ID: {e}")))?;
        let vm_id_str = vm_id.to_string();

        clawpot_event!(
            self.event_store,
            "vm.delete.started",
            "vm",
            vm_id = vm_id_str,
            {}
        );

        let mut entry = self
            .vm_registry
            .remove(&vm_id)
            .await
            .map_err(|e| Status::not_found(format!("VM not found: {e}")))?;

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

        let duration_ms = start.elapsed().as_millis() as i64;
        self.event_store.emit_with_duration(
            "vm.delete.completed",
            "vm",
            Some(&vm_id_str),
            None,
            duration_ms,
            Some(true),
            &serde_json::json!({}),
        );

        Ok(Response::new(DeleteVmResponse { success: true }))
    }

    #[tracing::instrument(name = "grpc.ListVMs", skip_all, fields(vm_count = tracing::field::Empty))]
    async fn list_v_ms(
        &self,
        _request: Request<ListVmsRequest>,
    ) -> Result<Response<ListVmsResponse>, Status> {
        let vms_list = self.vm_registry.list().await;

        let vms: Vec<VmInfo> = vms_list
            .into_iter()
            .map(
                |(id, ip_address, _tap_name, vcpu_count, mem_size_mib, created_at)| {
                    let created_timestamp = created_at
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;

                    VmInfo {
                        vm_id: id.to_string(),
                        state: ProtoVmState::Running as i32,
                        ip_address: ip_address.to_string(),
                        vcpu_count: u32::from(vcpu_count),
                        mem_size_mib,
                        created_at: created_timestamp,
                        socket_path: format!("/tmp/fc-{}.sock", id.simple()),
                    }
                },
            )
            .collect();

        Span::current().record("vm_count", vms.len());

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
        let start = Instant::now();
        let req = request.into_inner();
        let span = Span::current();
        span.record("vm_id", req.vm_id.as_str());
        span.record("command", req.command.as_str());

        let vm_id = Uuid::parse_str(&req.vm_id)
            .map_err(|e| Status::invalid_argument(format!("Invalid VM ID: {e}")))?;

        let vsock_path = self
            .vm_registry
            .get_vsock_path(&vm_id)
            .await
            .map_err(|e| Status::not_found(format!("VM not found: {e}")))?;

        let mut agent_client = agent::client::AgentClient::connect(vsock_path)
            .await
            .map_err(|e| Status::unavailable(format!("Failed to connect to agent: {e}")))?;

        let agent_req = clawpot_common::agent_proto::ExecRequest {
            command: req.command.clone(),
            args: req.args.clone(),
            env: req.env,
            working_dir: req.working_dir,
        };

        let agent_resp = agent_client
            .exec(agent_req)
            .await
            .map_err(|e| Status::internal(format!("Agent exec failed: {e}")))?;

        span.record("exit_code", agent_resp.exit_code);

        let duration_ms = start.elapsed().as_millis() as i64;
        let vm_id_str = vm_id.to_string();
        self.event_store.emit_with_duration(
            "vm.exec",
            "vm",
            Some(&vm_id_str),
            None,
            duration_ms,
            Some(agent_resp.exit_code == 0),
            &serde_json::json!({
                "command": req.command,
                "args": req.args,
                "exit_code": agent_resp.exit_code,
                "stdout_len": agent_resp.stdout.len(),
                "stderr_len": agent_resp.stderr.len(),
            }),
        );

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

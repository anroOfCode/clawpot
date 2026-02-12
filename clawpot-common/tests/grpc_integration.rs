//! Integration tests for the gRPC service protocol.
//!
//! These tests spin up a real tonic server with a mock service implementation
//! and connect a real gRPC client to verify the full request/response cycle.

use clawpot_common::proto::{
    clawpot_service_client::ClawpotServiceClient,
    clawpot_service_server::{ClawpotService, ClawpotServiceServer},
    CreateVmRequest, CreateVmResponse, DeleteVmRequest, DeleteVmResponse, ListVmsRequest,
    ListVmsResponse, VmInfo, VmState as ProtoVmState,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tonic::{transport::Server, Request, Response, Status};

/// A mock implementation of the ClawpotService that stores VMs in memory.
/// No root, no Firecracker, no networking required.
struct MockClawpotService {
    vms: Arc<Mutex<HashMap<String, VmInfo>>>,
    next_ip: Arc<Mutex<u8>>,
}

impl MockClawpotService {
    fn new() -> Self {
        Self {
            vms: Arc::new(Mutex::new(HashMap::new())),
            next_ip: Arc::new(Mutex::new(2)),
        }
    }
}

#[tonic::async_trait]
impl ClawpotService for MockClawpotService {
    async fn create_vm(
        &self,
        request: Request<CreateVmRequest>,
    ) -> Result<Response<CreateVmResponse>, Status> {
        let req = request.into_inner();
        let vm_id = uuid::Uuid::new_v4().to_string();
        let mut ip_counter = self.next_ip.lock().await;
        let ip_address = format!("192.168.100.{}", *ip_counter);
        *ip_counter += 1;
        let socket_path = format!("/tmp/fc-{}.sock", vm_id);

        let vcpu_count = req.vcpu_count.unwrap_or(1);
        let mem_size_mib = req.mem_size_mib.unwrap_or(256);

        let info = VmInfo {
            vm_id: vm_id.clone(),
            state: ProtoVmState::Running as i32,
            ip_address: ip_address.clone(),
            vcpu_count,
            mem_size_mib,
            created_at: 1700000000,
            socket_path: socket_path.clone(),
        };

        self.vms.lock().await.insert(vm_id.clone(), info);

        Ok(Response::new(CreateVmResponse {
            vm_id,
            ip_address,
            socket_path,
        }))
    }

    async fn delete_vm(
        &self,
        request: Request<DeleteVmRequest>,
    ) -> Result<Response<DeleteVmResponse>, Status> {
        let req = request.into_inner();
        let mut vms = self.vms.lock().await;

        if vms.remove(&req.vm_id).is_some() {
            Ok(Response::new(DeleteVmResponse { success: true }))
        } else {
            Err(Status::not_found(format!("VM {} not found", req.vm_id)))
        }
    }

    async fn list_v_ms(
        &self,
        _request: Request<ListVmsRequest>,
    ) -> Result<Response<ListVmsResponse>, Status> {
        let vms = self.vms.lock().await;
        let vm_list: Vec<VmInfo> = vms.values().cloned().collect();
        Ok(Response::new(ListVmsResponse { vms: vm_list }))
    }
}

/// Start a mock gRPC server on a random port and return the address.
async fn start_mock_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let addr_str = format!("http://127.0.0.1:{}", addr.port());

    let service = MockClawpotService::new();

    tokio::spawn(async move {
        Server::builder()
            .add_service(ClawpotServiceServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    // Give the server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    addr_str
}

#[tokio::test]
async fn test_list_vms_empty() {
    let addr = start_mock_server().await;
    let mut client = ClawpotServiceClient::connect(addr).await.unwrap();

    let response = client
        .list_v_ms(ListVmsRequest {})
        .await
        .unwrap()
        .into_inner();

    assert!(response.vms.is_empty(), "Expected empty VM list");
}

#[tokio::test]
async fn test_create_vm_default_params() {
    let addr = start_mock_server().await;
    let mut client = ClawpotServiceClient::connect(addr).await.unwrap();

    let response = client
        .create_vm(CreateVmRequest {
            vcpu_count: None,
            mem_size_mib: None,
        })
        .await
        .unwrap()
        .into_inner();

    assert!(!response.vm_id.is_empty(), "VM ID should not be empty");
    assert!(
        response.ip_address.starts_with("192.168.100."),
        "IP should be in the 192.168.100.x range, got: {}",
        response.ip_address
    );
    assert!(
        response.socket_path.contains(&response.vm_id),
        "Socket path should contain the VM ID"
    );
}

#[tokio::test]
async fn test_create_vm_custom_params() {
    let addr = start_mock_server().await;
    let mut client = ClawpotServiceClient::connect(addr).await.unwrap();

    let response = client
        .create_vm(CreateVmRequest {
            vcpu_count: Some(2),
            mem_size_mib: Some(512),
        })
        .await
        .unwrap()
        .into_inner();

    assert!(!response.vm_id.is_empty());

    // Verify the VM shows up in list with correct params
    let list = client
        .list_v_ms(ListVmsRequest {})
        .await
        .unwrap()
        .into_inner();

    assert_eq!(list.vms.len(), 1);
    let vm = &list.vms[0];
    assert_eq!(vm.vcpu_count, 2);
    assert_eq!(vm.mem_size_mib, 512);
    assert_eq!(vm.state, ProtoVmState::Running as i32);
}

#[tokio::test]
async fn test_create_and_delete_vm() {
    let addr = start_mock_server().await;
    let mut client = ClawpotServiceClient::connect(addr).await.unwrap();

    // Create a VM
    let create_resp = client
        .create_vm(CreateVmRequest {
            vcpu_count: Some(1),
            mem_size_mib: Some(256),
        })
        .await
        .unwrap()
        .into_inner();

    let vm_id = create_resp.vm_id.clone();

    // Verify it exists
    let list = client
        .list_v_ms(ListVmsRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.vms.len(), 1);

    // Delete it
    let delete_resp = client
        .delete_vm(DeleteVmRequest {
            vm_id: vm_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(delete_resp.success);

    // Verify it's gone
    let list = client
        .list_v_ms(ListVmsRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(list.vms.is_empty());
}

#[tokio::test]
async fn test_delete_nonexistent_vm() {
    let addr = start_mock_server().await;
    let mut client = ClawpotServiceClient::connect(addr).await.unwrap();

    let result = client
        .delete_vm(DeleteVmRequest {
            vm_id: "nonexistent-vm-id".to_string(),
        })
        .await;

    assert!(result.is_err(), "Deleting nonexistent VM should fail");
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn test_create_multiple_vms_unique_ips() {
    let addr = start_mock_server().await;
    let mut client = ClawpotServiceClient::connect(addr).await.unwrap();

    let resp1 = client
        .create_vm(CreateVmRequest {
            vcpu_count: None,
            mem_size_mib: None,
        })
        .await
        .unwrap()
        .into_inner();

    let resp2 = client
        .create_vm(CreateVmRequest {
            vcpu_count: None,
            mem_size_mib: None,
        })
        .await
        .unwrap()
        .into_inner();

    assert_ne!(resp1.vm_id, resp2.vm_id, "VM IDs should be unique");
    assert_ne!(
        resp1.ip_address, resp2.ip_address,
        "IP addresses should be unique"
    );

    // Both should appear in list
    let list = client
        .list_v_ms(ListVmsRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.vms.len(), 2);
}

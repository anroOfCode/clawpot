use anyhow::Result;
use clawpot_common::proto::{clawpot_service_client::ClawpotServiceClient, CreateVmRequest};
use tonic::transport::Channel;

pub async fn execute(
    client: &mut ClawpotServiceClient<Channel>,
    vcpus: Option<u32>,
    memory: Option<u32>,
) -> Result<()> {
    let request = CreateVmRequest {
        vcpu_count: vcpus,
        mem_size_mib: memory,
    };

    println!("Creating VM...");

    let response = client.create_vm(request).await?;
    let vm_info = response.into_inner();

    println!("\n✓ VM created successfully!");
    println!("  VM ID:      {}", vm_info.vm_id);
    println!("  IP Address: {}", vm_info.ip_address);
    println!("  Socket:     {}", vm_info.socket_path);

    Ok(())
}

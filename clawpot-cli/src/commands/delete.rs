use anyhow::Result;
use clawpot_common::proto::{clawpot_service_client::ClawpotServiceClient, DeleteVmRequest};
use tonic::transport::Channel;

pub async fn execute(
    client: &mut ClawpotServiceClient<Channel>,
    vm_id: String,
) -> Result<()> {
    let request = DeleteVmRequest { vm_id: vm_id.clone() };

    println!("Deleting VM {}...", vm_id);

    let response = client.delete_vm(request).await?;
    let result = response.into_inner();

    if result.success {
        println!("\n✓ VM deleted successfully!");
    } else {
        println!("\n✗ Failed to delete VM");
    }

    Ok(())
}

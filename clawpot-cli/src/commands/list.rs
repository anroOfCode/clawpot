use anyhow::Result;
use clawpot_common::proto::{clawpot_service_client::ClawpotServiceClient, ListVmsRequest, VmState};
use tabled::{Table, Tabled};
use tonic::transport::Channel;

#[derive(Tabled)]
struct VmRow {
    #[tabled(rename = "VM ID")]
    vm_id: String,
    #[tabled(rename = "State")]
    state: String,
    #[tabled(rename = "IP Address")]
    ip_address: String,
    #[tabled(rename = "vCPUs")]
    vcpus: u32,
    #[tabled(rename = "Memory (MiB)")]
    memory: u32,
}

pub async fn execute(client: &mut ClawpotServiceClient<Channel>) -> Result<()> {
    let request = ListVmsRequest {};

    let response = client.list_v_ms(request).await?;
    let vms = response.into_inner().vms;

    if vms.is_empty() {
        println!("No VMs running");
        return Ok(());
    }

    let rows: Vec<VmRow> = vms
        .into_iter()
        .map(|vm| {
            let state_str = match VmState::try_from(vm.state) {
                Ok(VmState::Unspecified) => "Unspecified",
                Ok(VmState::Starting) => "Starting",
                Ok(VmState::Running) => "Running",
                Ok(VmState::Stopping) => "Stopping",
                Ok(VmState::Stopped) => "Stopped",
                Ok(VmState::Error) => "Error",
                Err(_) => "Unknown",
            };

            VmRow {
                vm_id: vm.vm_id,
                state: state_str.to_string(),
                ip_address: vm.ip_address,
                vcpus: vm.vcpu_count,
                memory: vm.mem_size_mib,
            }
        })
        .collect();

    let count = rows.len();
    let table = Table::new(rows).to_string();
    println!("{}", table);
    println!("\nTotal: {} VM(s)", count);

    Ok(())
}

use anyhow::{bail, Result};
use clawpot_common::proto::{clawpot_service_client::ClawpotServiceClient, ExecVmRequest};
use std::collections::HashMap;
use std::io::Write;
use tonic::transport::Channel;

pub async fn execute(
    client: &mut ClawpotServiceClient<Channel>,
    vm_id: String,
    command: Vec<String>,
) -> Result<()> {
    let (cmd, args) = match command.split_first() {
        Some((first, rest)) => (first.clone(), rest.to_vec()),
        None => bail!("No command specified"),
    };

    let request = ExecVmRequest {
        vm_id,
        command: cmd,
        args,
        env: HashMap::new(),
        working_dir: String::new(),
    };

    let response = client.exec_vm(request).await?.into_inner();

    std::io::stdout().write_all(&response.stdout)?;
    std::io::stderr().write_all(&response.stderr)?;

    std::process::exit(response.exit_code);
}

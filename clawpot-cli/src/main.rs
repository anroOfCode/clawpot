mod commands;

use anyhow::Result;
use clap::{Parser, Subcommand};
use clawpot_common::proto::clawpot_service_client::ClawpotServiceClient;
use tonic::transport::Channel;

#[derive(Parser)]
#[command(name = "clawpot")]
#[command(version = "0.1.0")]
#[command(about = "Clawpot multi-VM orchestration CLI", long_about = None)]
struct Cli {
    /// Server address
    #[arg(long, default_value = "http://127.0.0.1:50051", global = true)]
    server: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new VM
    Create {
        /// Number of vCPUs (default: 1)
        #[arg(long)]
        vcpus: Option<u32>,

        /// Memory in MiB (default: 256)
        #[arg(long)]
        memory: Option<u32>,
    },

    /// Delete a VM
    Delete {
        /// VM ID to delete
        vm_id: String,
    },

    /// List all VMs
    List,

    /// Execute a command in a VM
    Exec {
        /// VM ID
        vm_id: String,

        /// Command and arguments to execute
        #[arg(last = true)]
        command: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Connect to gRPC server
    let channel = Channel::from_shared(cli.server.clone())?
        .connect()
        .await?;

    let mut client = ClawpotServiceClient::new(channel);

    // Execute command
    match cli.command {
        Commands::Create { vcpus, memory } => {
            commands::create::execute(&mut client, vcpus, memory).await?;
        }
        Commands::Delete { vm_id } => {
            commands::delete::execute(&mut client, vm_id).await?;
        }
        Commands::List => {
            commands::list::execute(&mut client).await?;
        }
        Commands::Exec { vm_id, command } => {
            commands::exec::execute(&mut client, vm_id, command).await?;
        }
    }

    Ok(())
}

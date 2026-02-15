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

    /// Query event logs from the events database
    Logs {
        /// Path to the events database
        #[arg(long)]
        db: Option<String>,

        #[command(subcommand)]
        action: LogsAction,
    },
}

#[derive(Subcommand)]
enum LogsAction {
    /// List all server sessions
    Sessions,

    /// Show events (filtered)
    Show {
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,

        /// Filter by VM ID
        #[arg(long)]
        vm: Option<String>,

        /// Filter by category (server, vm, network, test)
        #[arg(long)]
        category: Option<String>,

        /// Filter by event type
        #[arg(long, name = "type")]
        event_type: Option<String>,

        /// Limit number of results
        #[arg(long)]
        limit: Option<i64>,
    },

    /// Export events as JSONL or JSON
    Export {
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,

        /// Output format: jsonl (default) or json
        #[arg(long, default_value = "jsonl")]
        format: String,
    },

    /// Show a human-readable chronological timeline
    Timeline {
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,

        /// Filter by VM ID
        #[arg(long)]
        vm: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle logs command without gRPC connection
    if let Commands::Logs { db, action } = &cli.command {
        return match action {
            LogsAction::Sessions => commands::logs::execute_sessions(db.as_deref()),
            LogsAction::Show {
                session,
                vm,
                category,
                event_type,
                limit,
            } => commands::logs::execute_show(
                db.as_deref(),
                session.as_deref(),
                vm.as_deref(),
                category.as_deref(),
                event_type.as_deref(),
                *limit,
            ),
            LogsAction::Export { session, format } => {
                commands::logs::execute_export(db.as_deref(), session.as_deref(), format)
            }
            LogsAction::Timeline { session, vm } => {
                commands::logs::execute_timeline(db.as_deref(), session.as_deref(), vm.as_deref())
            }
        };
    }

    // Connect to gRPC server
    let channel = Channel::from_shared(cli.server.clone())?.connect().await?;

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
        Commands::Logs { .. } => unreachable!(),
    }

    Ok(())
}

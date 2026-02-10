mod firecracker;
mod vm;

use clap::{Parser, Subcommand};
use firecracker::VmConfig;
use std::path::PathBuf;
use vm::VmManager;

#[derive(Parser)]
#[command(name = "clawpot-driver")]
#[command(version = "0.1.0")]
#[command(about = "Firecracker VM driver for clawpot", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a Firecracker VM
    Start {
        /// Path to kernel image
        #[arg(long)]
        kernel: PathBuf,

        /// Path to rootfs image
        #[arg(long)]
        rootfs: PathBuf,

        /// Number of vCPUs
        #[arg(long, default_value = "1")]
        vcpus: u8,

        /// Memory in MiB
        #[arg(long, default_value = "256")]
        memory: u32,

        /// Socket path for Firecracker API
        #[arg(long, default_value = "/tmp/firecracker.sock")]
        socket: PathBuf,
    },

    /// Stop a running VM
    Stop {
        /// Socket path for Firecracker API
        #[arg(long, default_value = "/tmp/firecracker.sock")]
        socket: PathBuf,
    },

    /// Get VM status
    Status {
        /// Socket path for Firecracker API
        #[arg(long, default_value = "/tmp/firecracker.sock")]
        socket: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing subscriber for logging
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start {
            kernel,
            rootfs,
            vcpus,
            memory,
            socket,
        } => {
            // Build VM configuration
            let config = VmConfig::new(kernel, rootfs)
                .with_vcpus(vcpus)
                .with_memory(memory);

            // Create VM manager
            let mut manager = VmManager::new(socket);

            // Start the VM
            manager.start(config).await?;

            println!("\n✓ VM started successfully!");
            println!("\nThe VM is now running. Press Ctrl+C to stop it.");
            println!("You can access the VM console via the serial output.\n");

            // Wait for Ctrl+C signal
            tokio::signal::ctrl_c().await?;

            println!("\nReceived Ctrl+C, shutting down VM...");

            // Stop the VM
            manager.stop().await?;

            println!("✓ VM stopped successfully!");
        }

        Commands::Stop { socket } => {
            let mut manager = VmManager::new(socket);
            manager.stop().await?;
            println!("✓ VM stopped successfully!");
        }

        Commands::Status { socket } => {
            let manager = VmManager::new(socket);
            match manager.status().await {
                Ok(status) => {
                    println!("VM Status:");
                    println!("{}", status);
                }
                Err(e) => {
                    eprintln!("Failed to get VM status: {}", e);
                    eprintln!("Is the VM running?");
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}

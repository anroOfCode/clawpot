mod exec;
mod service;
mod stream;

pub mod proto {
    tonic::include_proto!("clawpot.agent.v1");
}

use anyhow::Result;
use proto::agent_service_server::AgentServiceServer;
use tonic::transport::Server;
use tracing::info;

const VSOCK_PORT: u32 = 10051;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    info!("clawpot-agent v{} starting...", env!("CARGO_PKG_VERSION"));

    let service = service::AgentServiceImpl::new();

    // Try vsock first, fall back to TCP for development/testing
    let vsock_addr = tokio_vsock::VsockAddr::new(libc::VMADDR_CID_ANY, VSOCK_PORT);
    match tokio_vsock::VsockListener::bind(vsock_addr) {
        Ok(listener) => {
            info!("Listening on vsock port {}", VSOCK_PORT);

            Server::builder()
                .add_service(AgentServiceServer::new(service))
                .serve_with_incoming(listener.incoming())
                .await?;
        }
        Err(e) => {
            // Fallback to TCP for development outside a VM
            let addr = "0.0.0.0:10051".parse()?;
            info!(
                "vsock not available ({}), falling back to TCP on {}",
                e, addr
            );

            Server::builder()
                .add_service(AgentServiceServer::new(service))
                .serve(addr)
                .await?;
        }
    }

    Ok(())
}

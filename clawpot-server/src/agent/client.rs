use anyhow::{anyhow, Context, Result};
use clawpot_common::agent_proto::{
    agent_service_client::AgentServiceClient, ExecRequest, ExecResponse, HealthRequest,
};
use clawpot_common::AGENT_VSOCK_PORT;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;
use tracing::{debug, warn};

/// Client for communicating with the guest agent over vsock
pub struct AgentClient {
    inner: AgentServiceClient<Channel>,
}

impl AgentClient {
    /// Connect to the agent via Firecracker's vsock UDS.
    ///
    /// Protocol:
    /// 1. Connect to the host-side Unix socket (created by Firecracker)
    /// 2. Send "CONNECT <port>\n"
    /// 3. Read "OK <port>\n" response
    /// 4. The stream is now connected to the guest's vsock listener
    pub async fn connect(vsock_uds_path: String) -> Result<Self> {
        let path = vsock_uds_path.clone();

        let channel = Endpoint::try_from("http://[::]:50051")? // dummy URI, not actually used
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = path.clone();
                async move {
                    // Connect to the Firecracker vsock UDS
                    let mut stream = UnixStream::connect(&path).await?;

                    // Perform the CONNECT handshake
                    let connect_cmd = format!("CONNECT {}\n", AGENT_VSOCK_PORT);
                    stream.write_all(connect_cmd.as_bytes()).await?;

                    // Read response line
                    let mut reader = BufReader::new(&mut stream);
                    let mut response = String::new();
                    reader.read_line(&mut response).await?;

                    if !response.starts_with("OK") {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionRefused,
                            format!("vsock CONNECT failed: {}", response.trim()),
                        ));
                    }

                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .context("Failed to connect to agent via vsock")?;

        Ok(Self {
            inner: AgentServiceClient::new(channel),
        })
    }

    /// Wait for the agent to become ready, retrying with backoff.
    pub async fn wait_ready(vsock_uds_path: &str, timeout: Duration) -> Result<Self> {
        let start = std::time::Instant::now();
        let interval = Duration::from_millis(500);

        loop {
            match Self::connect(vsock_uds_path.to_string()).await {
                Ok(mut client) => {
                    // Try health check
                    match client.inner.health(HealthRequest {}).await {
                        Ok(resp) => {
                            let health = resp.into_inner();
                            debug!(
                                "Agent ready: version={}, uptime={}s",
                                health.version, health.uptime_secs
                            );
                            return Ok(client);
                        }
                        Err(e) => {
                            debug!("Agent health check failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    debug!("Agent not yet reachable: {}", e);
                }
            }

            if start.elapsed() >= timeout {
                return Err(anyhow!(
                    "Agent did not become ready within {:?}",
                    timeout
                ));
            }

            tokio::time::sleep(interval).await;
        }
    }

    /// Execute a command and return the result
    pub async fn exec(&mut self, req: ExecRequest) -> Result<ExecResponse> {
        let response = self
            .inner
            .exec(req)
            .await
            .map_err(|e| anyhow!("Agent exec failed: {}", e))?;
        Ok(response.into_inner())
    }
}

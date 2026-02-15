use crate::firecracker::models::{
    BootSource, Drive, EntropyDevice, ErrorResponse, InstanceActionInfo, InstanceInfo,
    MachineConfig, NetworkInterface, VsockDevice,
};
use anyhow::{anyhow, Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyperlocal::{UnixConnector, Uri};
use std::path::PathBuf;

/// HTTP client for communicating with Firecracker API over Unix socket
pub struct FirecrackerClient {
    socket_path: PathBuf,
    client: Client<UnixConnector, Full<Bytes>>,
}

impl FirecrackerClient {
    /// Create a new Firecracker client
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        let connector = UnixConnector;
        let client = Client::builder(TokioExecutor::new()).build(connector);

        Self {
            socket_path: socket_path.into(),
            client,
        }
    }

    /// Build a URI for the Unix socket
    fn build_uri(&self, path: &str) -> Result<Uri> {
        let socket_str = self
            .socket_path
            .to_str()
            .ok_or_else(|| anyhow!("Invalid socket path"))?;

        Ok(Uri::new(socket_str, path))
    }

    /// Make a PUT request to the Firecracker API
    #[tracing::instrument(name = "firecracker.put", skip(self, body), fields(path = %path))]
    async fn put<T: serde::Serialize>(&self, path: &str, body: &T) -> Result<()> {
        let uri = self.build_uri(path)?;
        let json = serde_json::to_string(body).context("Failed to serialize request body")?;

        let request = Request::builder()
            .method(Method::PUT)
            .uri(uri)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(json)))
            .context("Failed to build request")?;

        let response = self
            .client
            .request(request)
            .await
            .context("Failed to send request")?;

        let status = response.status();
        let body_bytes = response
            .into_body()
            .collect()
            .await
            .context("Failed to read response body")?
            .to_bytes();

        if !status.is_success() {
            let error_text = String::from_utf8_lossy(&body_bytes);

            // Try to parse as Firecracker error response
            if let Ok(error_response) = serde_json::from_slice::<ErrorResponse>(&body_bytes) {
                return Err(anyhow!(
                    "Firecracker API error ({}): {}",
                    status,
                    error_response.fault_message
                ));
            }

            return Err(anyhow!("Request failed with status {status}: {error_text}"));
        }

        Ok(())
    }

    /// Make a GET request to the Firecracker API
    #[tracing::instrument(name = "firecracker.get", skip(self), fields(path = %path))]
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let uri = self.build_uri(path)?;

        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Full::new(Bytes::new()))
            .context("Failed to build request")?;

        let response = self
            .client
            .request(request)
            .await
            .context("Failed to send request")?;

        let status = response.status();
        let body_bytes = response
            .into_body()
            .collect()
            .await
            .context("Failed to read response body")?
            .to_bytes();

        if !status.is_success() {
            let error_text = String::from_utf8_lossy(&body_bytes);

            // Try to parse as Firecracker error response
            if let Ok(error_response) = serde_json::from_slice::<ErrorResponse>(&body_bytes) {
                return Err(anyhow!(
                    "Firecracker API error ({}): {}",
                    status,
                    error_response.fault_message
                ));
            }

            return Err(anyhow!("Request failed with status {status}: {error_text}"));
        }

        serde_json::from_slice(&body_bytes).context("Failed to deserialize response")
    }

    /// Set the boot source configuration
    pub async fn set_boot_source(&self, config: BootSource) -> Result<()> {
        self.put("/boot-source", &config)
            .await
            .context("Failed to set boot source")
    }

    /// Set a drive configuration
    pub async fn set_drive(&self, drive: Drive) -> Result<()> {
        let path = format!("/drives/{}", drive.drive_id);
        self.put(&path, &drive).await.context("Failed to set drive")
    }

    /// Set the machine configuration (CPU and memory)
    pub async fn set_machine_config(&self, config: MachineConfig) -> Result<()> {
        self.put("/machine-config", &config)
            .await
            .context("Failed to set machine config")
    }

    /// Start the VM instance
    pub async fn start_instance(&self) -> Result<()> {
        let action = InstanceActionInfo::start();
        self.put("/actions", &action)
            .await
            .context("Failed to start instance")
    }

    /// Send Ctrl+Alt+Del to the instance
    pub async fn send_ctrl_alt_del(&self) -> Result<()> {
        let action = InstanceActionInfo::send_ctrl_alt_del();
        self.put("/actions", &action)
            .await
            .context("Failed to send Ctrl+Alt+Del")
    }

    /// Get instance information
    pub async fn get_instance_info(&self) -> Result<InstanceInfo> {
        self.get("/").await.context("Failed to get instance info")
    }

    /// Set a network interface configuration
    pub async fn set_network_interface(&self, iface: NetworkInterface) -> Result<()> {
        let path = format!("/network-interfaces/{}", iface.iface_id);
        self.put(&path, &iface)
            .await
            .context("Failed to set network interface")
    }

    /// Set the vsock device configuration
    pub async fn set_vsock(&self, vsock: VsockDevice) -> Result<()> {
        self.put("/vsock", &vsock)
            .await
            .context("Failed to set vsock device")
    }

    /// Enable the entropy device (virtio-rng)
    pub async fn set_entropy(&self, entropy: EntropyDevice) -> Result<()> {
        self.put("/entropy", &entropy)
            .await
            .context("Failed to set entropy device")
    }
}

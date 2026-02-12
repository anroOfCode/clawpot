use crate::firecracker::{BootSource, Drive, FirecrackerClient, MachineConfig, VmConfig};
use crate::vm::lifecycle::{VmLifecycle, VmState};
use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;
use tracing::{debug, info, warn};

/// High-level VM manager that orchestrates Firecracker process and configuration
pub struct VmManager {
    socket_path: PathBuf,
    firecracker_process: Option<Child>,
    client: FirecrackerClient,
    lifecycle: VmLifecycle,
}

impl VmManager {
    /// Create a new VM manager with the specified socket path
    pub fn new(socket_path: PathBuf) -> Self {
        let client = FirecrackerClient::new(&socket_path);
        Self {
            socket_path,
            firecracker_process: None,
            client,
            lifecycle: VmLifecycle::new(),
        }
    }

    /// Get the current lifecycle state
    pub fn state(&self) -> VmState {
        self.lifecycle.current_state()
    }

    /// Start the VM with the given configuration
    #[tracing::instrument(
        name = "vm.start",
        skip(self, config),
        fields(
            socket_path = %self.socket_path.display(),
            vcpu_count = config.vcpu_count,
            mem_size_mib = config.mem_size_mib,
        )
    )]
    pub async fn start(&mut self, config: VmConfig) -> Result<()> {
        info!("Starting Firecracker VM...");

        // Validate configuration
        config.validate().context("Invalid VM configuration")?;

        // Transition to starting state
        self.lifecycle
            .transition_to(VmState::Starting)
            .context("Failed to transition to Starting state")?;

        // Clean up any existing socket
        if self.socket_path.exists() {
            warn!(
                "Socket file already exists at {}, removing it",
                self.socket_path.display()
            );
            std::fs::remove_file(&self.socket_path)
                .context("Failed to remove existing socket file")?;
        }

        // Start Firecracker process
        self.start_firecracker_process()
            .context("Failed to start Firecracker process")?;

        // Wait for socket to be ready
        self.wait_for_socket()
            .await
            .context("Socket did not become ready")?;

        // Configure VM via API
        self.configure_vm(config)
            .await
            .context("Failed to configure VM")?;

        // Start the instance
        info!("Starting VM instance...");
        self.client
            .start_instance()
            .await
            .context("Failed to start instance")?;

        // Transition to running state
        self.lifecycle
            .transition_to(VmState::Running)
            .context("Failed to transition to Running state")?;

        info!("VM started successfully!");

        Ok(())
    }

    /// Start the Firecracker process
    fn start_firecracker_process(&mut self) -> Result<()> {
        info!(
            "Spawning Firecracker process with socket: {}",
            self.socket_path.display()
        );

        let child = Command::new("firecracker")
            .arg("--api-sock")
            .arg(&self.socket_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn firecracker process")?;

        self.firecracker_process = Some(child);
        debug!("Firecracker process spawned");

        Ok(())
    }

    /// Wait for the Unix socket to become ready
    #[tracing::instrument(name = "vm.wait_for_socket", skip(self))]
    async fn wait_for_socket(&self) -> Result<()> {
        info!("Waiting for socket to be ready...");

        let max_attempts = 50;
        let delay = Duration::from_millis(100);

        for attempt in 1..=max_attempts {
            if self.socket_path.exists() {
                info!("Socket is ready after {} attempts", attempt);
                // Give it a bit more time to be fully ready
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Ok(());
            }

            tokio::time::sleep(delay).await;
        }

        Err(anyhow!(
            "Socket did not become ready after {} attempts ({} ms)",
            max_attempts,
            max_attempts * delay.as_millis() as u32
        ))
    }

    /// Configure the VM via Firecracker API
    #[tracing::instrument(name = "vm.configure", skip_all)]
    async fn configure_vm(&self, config: VmConfig) -> Result<()> {
        info!("Configuring VM...");

        // Set boot source
        debug!("Setting boot source: {:?}", config.kernel_path);
        let boot_source = BootSource {
            kernel_image_path: config
                .kernel_path
                .to_str()
                .ok_or_else(|| anyhow!("Invalid kernel path"))?
                .to_string(),
            boot_args: config.boot_args,
        };
        self.client
            .set_boot_source(boot_source)
            .await
            .context("Failed to set boot source")?;

        // Set root drive
        debug!("Setting root drive: {:?}", config.rootfs_path);
        let drive = Drive {
            drive_id: "rootfs".to_string(),
            path_on_host: config
                .rootfs_path
                .to_str()
                .ok_or_else(|| anyhow!("Invalid rootfs path"))?
                .to_string(),
            is_root_device: true,
            is_read_only: false,
        };
        self.client
            .set_drive(drive)
            .await
            .context("Failed to set root drive")?;

        // Set machine config
        debug!(
            "Setting machine config: {} vCPUs, {} MiB memory",
            config.vcpu_count, config.mem_size_mib
        );
        let machine_config = MachineConfig {
            vcpu_count: config.vcpu_count,
            mem_size_mib: config.mem_size_mib,
        };
        self.client
            .set_machine_config(machine_config)
            .await
            .context("Failed to set machine config")?;

        // Set network interface if configured
        if let (Some(tap_device), Some(_ip)) = (&config.tap_device, &config.ip_address) {
            debug!("Setting network interface: {}", tap_device);
            let network_interface = crate::firecracker::NetworkInterface {
                iface_id: "eth0".to_string(),
                host_dev_name: tap_device.clone(),
                guest_mac: None,
            };
            self.client
                .set_network_interface(network_interface)
                .await
                .context("Failed to set network interface")?;
        }

        // Set vsock device if configured
        if let (Some(guest_cid), Some(uds_path)) =
            (config.guest_cid, &config.vsock_uds_path)
        {
            debug!("Setting vsock device: CID={}, UDS={}", guest_cid, uds_path);
            let vsock = crate::firecracker::VsockDevice {
                guest_cid,
                uds_path: uds_path.clone(),
            };
            self.client
                .set_vsock(vsock)
                .await
                .context("Failed to set vsock device")?;
        }

        info!("VM configured successfully");

        Ok(())
    }

    /// Stop the VM
    #[tracing::instrument(name = "vm.stop", skip(self), fields(socket_path = %self.socket_path.display()))]
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping VM...");

        // Transition to stopping state
        if let Err(e) = self.lifecycle.transition_to(VmState::Stopping) {
            warn!("Failed to transition to Stopping state: {}", e);
        }

        // Try to send Ctrl+Alt+Del for graceful shutdown
        if let Err(e) = self.client.send_ctrl_alt_del().await {
            warn!("Failed to send Ctrl+Alt+Del: {}", e);
        }

        // Give the VM a moment to shut down gracefully
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Kill the Firecracker process
        if let Some(mut child) = self.firecracker_process.take() {
            debug!("Killing Firecracker process");
            if let Err(e) = child.kill() {
                warn!("Failed to kill Firecracker process: {}", e);
            }

            if let Err(e) = child.wait() {
                warn!("Failed to wait for Firecracker process: {}", e);
            }
        }

        // Clean up socket file
        if self.socket_path.exists() {
            debug!("Removing socket file");
            if let Err(e) = std::fs::remove_file(&self.socket_path) {
                warn!("Failed to remove socket file: {}", e);
            }
        }

        // Transition to stopped state
        self.lifecycle
            .transition_to(VmState::Stopped)
            .context("Failed to transition to Stopped state")?;

        info!("VM stopped successfully");

        Ok(())
    }

    /// Get VM status
    pub async fn status(&self) -> Result<String> {
        let info = self
            .client
            .get_instance_info()
            .await
            .context("Failed to get instance info")?;

        Ok(format!(
            "State: {}\nFirecracker State: {}\nVMM Version: {}",
            self.lifecycle.current_state(),
            info.state,
            info.vmm_version
        ))
    }
}

impl Drop for VmManager {
    fn drop(&mut self) {
        // Best effort cleanup on drop
        if let Some(mut child) = self.firecracker_process.take() {
            let _ = child.kill();
        }

        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

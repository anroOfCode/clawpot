use std::path::PathBuf;

/// VM configuration builder for Firecracker
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// Path to the kernel image
    pub kernel_path: PathBuf,
    /// Path to the root filesystem image
    pub rootfs_path: PathBuf,
    /// Number of virtual CPUs (default: 1)
    pub vcpu_count: u8,
    /// Memory size in MiB (default: 256)
    pub mem_size_mib: u32,
    /// Boot arguments for the kernel
    pub boot_args: String,
    /// TAP device name for networking
    pub tap_device: Option<String>,
    /// IP address for the VM
    pub ip_address: Option<String>,
    /// Guest CID for vsock
    pub guest_cid: Option<u32>,
    /// Path to the host-side vsock Unix Domain Socket
    pub vsock_uds_path: Option<String>,
}

impl VmConfig {
    /// Create a new VM configuration with default settings
    ///
    /// Default configuration:
    /// - 1 vCPU
    /// - 256 MiB memory
    /// - Standard boot args for serial console
    pub fn new(kernel_path: PathBuf, rootfs_path: PathBuf) -> Self {
        Self {
            kernel_path,
            rootfs_path,
            vcpu_count: 1,
            mem_size_mib: 256,
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
            tap_device: None,
            ip_address: None,
            guest_cid: None,
            vsock_uds_path: None,
        }
    }

    /// Set the number of virtual CPUs
    #[must_use]
    pub fn with_vcpus(mut self, count: u8) -> Self {
        self.vcpu_count = count;
        self
    }

    /// Set the memory size in MiB
    #[must_use]
    pub fn with_memory(mut self, mib: u32) -> Self {
        self.mem_size_mib = mib;
        self
    }

    /// Set custom boot arguments
    #[must_use]
    pub fn with_boot_args(mut self, args: String) -> Self {
        self.boot_args = args;
        self
    }

    /// Configure networking with TAP device and IP address
    /// Automatically updates boot args to include IP configuration
    #[must_use]
    pub fn with_network(mut self, tap_device: String, ip_address: String) -> Self {
        self.tap_device = Some(tap_device);

        // Update boot args to include IP configuration
        // Format: ip=<client-ip>::<gw-ip>:<netmask>::<device>:<autoconf>
        let ip_config = format!("ip={ip_address}::192.168.100.1:255.255.255.0::eth0:off");
        self.boot_args = format!("console=ttyS0 reboot=k panic=1 pci=off {ip_config}");
        self.ip_address = Some(ip_address);

        self
    }

    /// Configure vsock device for guest-host communication
    #[must_use]
    pub fn with_vsock(mut self, guest_cid: u32, uds_path: String) -> Self {
        self.guest_cid = Some(guest_cid);
        self.vsock_uds_path = Some(uds_path);
        self
    }

    /// Validate the configuration
    pub fn validate(&self) -> anyhow::Result<()> {
        // Check kernel path exists
        if !self.kernel_path.exists() {
            return Err(anyhow::anyhow!(
                "Kernel image not found: {}",
                self.kernel_path.display()
            ));
        }

        // Check rootfs path exists
        if !self.rootfs_path.exists() {
            return Err(anyhow::anyhow!(
                "Rootfs image not found: {}",
                self.rootfs_path.display()
            ));
        }

        // Validate vCPU count
        if self.vcpu_count == 0 {
            return Err(anyhow::anyhow!("vCPU count must be at least 1"));
        }

        // Validate memory
        if self.mem_size_mib < 128 {
            return Err(anyhow::anyhow!("Memory size must be at least 128 MiB"));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = VmConfig::new(PathBuf::from("/tmp/kernel"), PathBuf::from("/tmp/rootfs"));

        assert_eq!(config.vcpu_count, 1);
        assert_eq!(config.mem_size_mib, 256);
        assert_eq!(config.boot_args, "console=ttyS0 reboot=k panic=1 pci=off");
    }

    #[test]
    fn test_builder_pattern() {
        let config = VmConfig::new(PathBuf::from("/tmp/kernel"), PathBuf::from("/tmp/rootfs"))
            .with_vcpus(4)
            .with_memory(1024);

        assert_eq!(config.vcpu_count, 4);
        assert_eq!(config.mem_size_mib, 1024);
    }
}

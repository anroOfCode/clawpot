use serde::{Deserialize, Serialize};

/// Boot source configuration for Firecracker VM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootSource {
    /// Path to the kernel image file
    pub kernel_image_path: String,
    /// Boot arguments passed to the kernel
    pub boot_args: String,
}

/// Drive configuration for Firecracker VM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drive {
    /// Unique identifier for the drive
    pub drive_id: String,
    /// Path to the filesystem image on the host
    pub path_on_host: String,
    /// Whether this is the root device
    pub is_root_device: bool,
    /// Whether the drive is read-only
    pub is_read_only: bool,
}

/// Machine configuration (CPU and memory)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineConfig {
    /// Number of virtual CPUs
    pub vcpu_count: u8,
    /// Memory size in MiB
    pub mem_size_mib: u32,
}

/// Instance action request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceActionInfo {
    /// Type of action to perform
    pub action_type: String,
}

impl InstanceActionInfo {
    /// Create a start instance action
    pub fn start() -> Self {
        Self {
            action_type: "InstanceStart".to_string(),
        }
    }

    /// Create a send Ctrl+Alt+Del action
    pub fn send_ctrl_alt_del() -> Self {
        Self {
            action_type: "SendCtrlAltDel".to_string(),
        }
    }
}

/// Instance information response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// Instance ID
    #[serde(default)]
    pub id: String,
    /// Current instance state
    pub state: String,
    /// VM ID
    #[serde(default)]
    pub vmm_version: String,
    /// Application name
    #[serde(default)]
    pub app_name: String,
}

/// Error response from Firecracker API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Error message
    pub fault_message: String,
}

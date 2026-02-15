pub mod agent_proto;
pub mod firecracker;
pub mod network_auth_proto;
pub mod proto;
pub mod types;
pub mod vm;

// Re-export commonly used types
pub use types::VmId;

/// Default vsock port the guest agent listens on
pub const AGENT_VSOCK_PORT: u32 = 10051;

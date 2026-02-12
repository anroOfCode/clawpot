pub mod bridge;
pub mod ip_allocator;
pub mod iptables;
pub mod tap;

use anyhow::Result;
use std::net::IpAddr;
use tracing::info;

/// Network manager that orchestrates TAP devices, bridge, and iptables
pub struct NetworkManager {
    bridge_name: String,
}

impl NetworkManager {
    /// Create a new network manager
    pub fn new() -> Self {
        Self {
            bridge_name: "br0".to_string(),
        }
    }

    /// Ensure the bridge exists at server startup
    /// Creates bridge with gateway IP 192.168.100.1/24 if it doesn't exist
    pub fn ensure_bridge(&self) -> Result<()> {
        let gateway_ip: IpAddr = "192.168.100.1".parse().unwrap();
        bridge::ensure_bridge(&self.bridge_name, gateway_ip)?;
        info!("Network bridge {} is ready", self.bridge_name);
        Ok(())
    }

    /// Create and configure a TAP device for a VM
    /// This includes:
    /// 1. Creating the TAP device
    /// 2. Attaching it to the bridge
    /// 3. Adding iptables rule to enforce source IP
    #[tracing::instrument(name = "network.create_tap", skip(self), fields(tap_name = %tap_name, ip = %ip))]
    pub fn create_tap(&self, tap_name: &str, ip: IpAddr) -> Result<()> {
        // Create TAP device and bring it up
        tap::create_tap(tap_name)?;

        // Attach to bridge
        bridge::attach_tap_to_bridge(&self.bridge_name, tap_name)?;

        // Add iptables rule to enforce source IP
        iptables::add_source_ip_rule(tap_name, ip)?;

        info!(
            "TAP device {} configured with IP {} and attached to {}",
            tap_name, ip, self.bridge_name
        );

        Ok(())
    }

    /// Delete a TAP device and clean up associated rules
    /// This includes:
    /// 1. Removing iptables rules
    /// 2. Deleting the TAP device
    #[tracing::instrument(name = "network.delete_tap", skip(self), fields(tap_name = %tap_name, ip = %ip))]
    pub fn delete_tap(&self, tap_name: &str, ip: IpAddr) -> Result<()> {
        // Remove iptables rule (best effort)
        let _ = iptables::remove_source_ip_rule(tap_name, ip);

        // Delete TAP device
        tap::delete_tap(tap_name)?;

        info!("TAP device {} deleted and cleaned up", tap_name);

        Ok(())
    }

    /// Get the bridge name
    pub fn bridge_name(&self) -> &str {
        &self.bridge_name
    }
}

impl Default for NetworkManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_manager_creation() {
        let manager = NetworkManager::new();
        assert_eq!(manager.bridge_name(), "br0");
    }
}

use anyhow::{Context, Result};
use std::net::IpAddr;
use std::process::Command;
use tracing::info;

/// Ensure a bridge device exists, create if missing
/// Assigns the gateway IP and brings it up
pub fn ensure_bridge(name: &str, gateway_ip: IpAddr) -> Result<()> {
    // Check if bridge already exists
    let output = Command::new("ip")
        .args(["link", "show", name])
        .output()
        .context("Failed to check if bridge exists")?;

    if !output.status.success() {
        // Bridge doesn't exist, create it
        info!("Bridge {} does not exist, creating...", name);
        create_bridge(name, gateway_ip)?;
    } else {
        info!("Bridge {} already exists", name);
    }

    Ok(())
}

/// Create a new bridge device
fn create_bridge(name: &str, gateway_ip: IpAddr) -> Result<()> {
    // Create bridge
    let output = Command::new("ip")
        .args(["link", "add", name, "type", "bridge"])
        .output()
        .context("Failed to create bridge")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to create bridge {}: {}",
            name,
            stderr
        ));
    }

    info!("Created bridge: {}", name);

    // Assign IP to bridge
    let ip_with_mask = format!("{}/24", gateway_ip);
    let output = Command::new("ip")
        .args(["addr", "add", &ip_with_mask, "dev", name])
        .output()
        .context("Failed to assign IP to bridge")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to assign IP {} to bridge {}: {}",
            ip_with_mask,
            name,
            stderr
        ));
    }

    info!("Assigned IP {} to bridge {}", ip_with_mask, name);

    // Bring bridge up
    let output = Command::new("ip")
        .args(["link", "set", name, "up"])
        .output()
        .context("Failed to bring up bridge")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to bring up bridge {}: {}",
            name,
            stderr
        ));
    }

    info!("Brought up bridge: {}", name);

    // Enable IP forwarding
    enable_ip_forwarding()?;

    Ok(())
}

/// Enable IP forwarding
fn enable_ip_forwarding() -> Result<()> {
    // Try to enable IP forwarding
    match std::fs::write("/proc/sys/net/ipv4/ip_forward", "1") {
        Ok(()) => {
            info!("Enabled IP forwarding");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // Check if it's already enabled
            let current = std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
                .context("Failed to read IP forwarding status")?;

            if current.trim() == "1" {
                info!("IP forwarding already enabled");
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "IP forwarding is disabled and cannot be enabled: {}",
                    e
                ))
            }
        }
        Err(e) => Err(anyhow::anyhow!("Failed to enable IP forwarding: {}", e)),
    }
}

/// Attach a TAP device to a bridge
pub fn attach_tap_to_bridge(bridge: &str, tap: &str) -> Result<()> {
    let output = Command::new("ip")
        .args(["link", "set", tap, "master", bridge])
        .output()
        .context("Failed to attach TAP to bridge")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to attach TAP {} to bridge {}: {}",
            tap,
            bridge,
            stderr
        ));
    }

    info!("Attached TAP device {} to bridge {}", tap, bridge);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    #[ignore] // Requires root privileges
    fn test_ensure_bridge() {
        let gateway = IpAddr::V4(Ipv4Addr::new(192, 168, 100, 1));
        ensure_bridge("test-br0", gateway).expect("Failed to ensure bridge");

        // Cleanup
        Command::new("ip")
            .args(["link", "del", "test-br0"])
            .output()
            .ok();
    }
}

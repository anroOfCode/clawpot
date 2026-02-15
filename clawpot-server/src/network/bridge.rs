use anyhow::{Context, Result};
use futures_util::stream::TryStreamExt;
use rtnetlink::{Handle, LinkBridge, LinkUnspec};
use std::net::IpAddr;
use tracing::info;

/// Ensure a bridge device exists, create if missing
/// Assigns the gateway IP and brings it up
pub async fn ensure_bridge(handle: &Handle, name: &str, gateway_ip: IpAddr) -> Result<()> {
    // Check if bridge already exists
    let mut links = handle.link().get().match_name(name.to_string()).execute();

    if links.try_next().await.is_ok() {
        info!("Bridge {} already exists", name);
    } else {
        // Bridge doesn't exist, create it
        info!("Bridge {} does not exist, creating...", name);
        create_bridge(handle, name, gateway_ip).await?;
    }

    // Always ensure iptables rules and IP forwarding are set up,
    // even if the bridge already existed (rules may have been flushed).
    enable_ip_forwarding()?;
    super::iptables::ensure_proxy_redirect_rules(name)?;
    super::iptables::ensure_egress_filter_rules(name)?;

    Ok(())
}

/// Create a new bridge device
async fn create_bridge(handle: &Handle, name: &str, gateway_ip: IpAddr) -> Result<()> {
    // Create bridge
    handle
        .link()
        .add(LinkBridge::new(name).build())
        .execute()
        .await
        .context(format!("Failed to create bridge {name}"))?;

    info!("Created bridge: {}", name);

    // Get bridge index for subsequent operations
    let index = get_link_index(handle, name)
        .await
        .context(format!("Failed to get index for bridge {name}"))?;

    // Assign IP to bridge
    handle
        .address()
        .add(index, gateway_ip, 24)
        .execute()
        .await
        .context(format!(
            "Failed to assign IP {gateway_ip}/24 to bridge {name}"
        ))?;

    info!("Assigned IP {}/24 to bridge {}", gateway_ip, name);

    // Bring bridge up
    handle
        .link()
        .set(LinkUnspec::new_with_index(index).up().build())
        .execute()
        .await
        .context(format!("Failed to bring up bridge {name}"))?;

    info!("Brought up bridge: {}", name);

    // Enable IP forwarding
    enable_ip_forwarding()?;

    // Set up proxy redirect and egress filter rules
    super::iptables::add_proxy_redirect_rules(name)?;
    super::iptables::add_egress_filter_rules(name)?;

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
                    "IP forwarding is disabled and cannot be enabled: {e}"
                ))
            }
        }
        Err(e) => Err(anyhow::anyhow!("Failed to enable IP forwarding: {e}")),
    }
}

/// Attach a TAP device to a bridge
pub async fn attach_tap_to_bridge(handle: &Handle, bridge: &str, tap: &str) -> Result<()> {
    let bridge_index = get_link_index(handle, bridge)
        .await
        .context(format!("Failed to get index for bridge {bridge}"))?;

    let tap_index = get_link_index(handle, tap)
        .await
        .context(format!("Failed to get index for TAP {tap}"))?;

    handle
        .link()
        .set(
            LinkUnspec::new_with_index(tap_index)
                .controller(bridge_index)
                .build(),
        )
        .execute()
        .await
        .context(format!("Failed to attach TAP {tap} to bridge {bridge}"))?;

    info!("Attached TAP device {} to bridge {}", tap, bridge);
    Ok(())
}

/// Get the interface index for a named link
pub async fn get_link_index(handle: &Handle, name: &str) -> Result<u32> {
    let mut links = handle.link().get().match_name(name.to_string()).execute();

    let link = links
        .try_next()
        .await
        .context(format!("Failed to query link {name}"))?
        .ok_or_else(|| anyhow::anyhow!("Link {name} not found"))?;

    Ok(link.header.index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    #[ignore] // Requires root privileges
    async fn test_ensure_bridge() {
        let (connection, handle, _) = rtnetlink::new_connection().unwrap();
        tokio::spawn(connection);

        let gateway = IpAddr::V4(Ipv4Addr::new(192, 168, 100, 1));
        ensure_bridge(&handle, "test-br0", gateway)
            .await
            .expect("Failed to ensure bridge");

        // Cleanup
        if let Ok(index) = get_link_index(&handle, "test-br0").await {
            let _ = handle.link().del(index).execute().await;
        }
    }
}

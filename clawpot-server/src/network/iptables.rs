use anyhow::Result;
use std::net::IpAddr;
use tracing::{info, warn};

/// Helper to convert iptables Box<dyn Error> results into anyhow errors
fn ipt_new() -> Result<iptables::IPTables> {
    iptables::new(false).map_err(|e| anyhow::anyhow!("Failed to initialize iptables: {}", e))
}

/// Helper to run an iptables operation with proper error conversion
fn ipt_append(ipt: &iptables::IPTables, table: &str, chain: &str, rule: &str, description: &str) -> Result<()> {
    ipt.append(table, chain, rule)
        .map_err(|e| anyhow::anyhow!("iptables rule '{}' failed: {}", description, e))?;
    info!("iptables: {}", description);
    Ok(())
}

/// Add an iptables rule to enforce source IP for a TAP device
/// Drops all packets from the TAP device if the source IP doesn't match the assigned IP
pub fn add_source_ip_rule(tap: &str, ip: IpAddr) -> Result<()> {
    let ipt = ipt_new()?;
    let ip_str = ip.to_string();

    // iptables -A FORWARD -i <tap> ! -s <ip> -j DROP
    let rule = format!("-i {} ! -s {} -j DROP", tap, ip_str);
    ipt_append(&ipt, "filter", "FORWARD", &rule, &format!("enforce source IP {} on {}", ip_str, tap))?;

    info!(
        "Added iptables rule: {} must use source IP {}",
        tap, ip_str
    );

    Ok(())
}

/// Remove an iptables rule for a TAP device
/// Best-effort removal - doesn't fail if rule doesn't exist
pub fn remove_source_ip_rule(tap: &str, ip: IpAddr) -> Result<()> {
    let ipt = match iptables::new(false) {
        Ok(ipt) => ipt,
        Err(e) => {
            warn!("Failed to initialize iptables for rule removal: {}", e);
            return Ok(());
        }
    };
    let ip_str = ip.to_string();

    let rule = format!("-i {} ! -s {} -j DROP", tap, ip_str);
    match ipt.delete("filter", "FORWARD", &rule) {
        Ok(_) => {
            info!(
                "Removed iptables rule for {} with IP {}",
                tap, ip_str
            );
        }
        Err(e) => {
            warn!(
                "Failed to remove iptables rule for {} with IP {} (may not exist): {}",
                tap, ip_str, e
            );
            // Don't return error - rule might not exist
        }
    }

    Ok(())
}

/// Add iptables rules to redirect HTTP/HTTPS traffic from the bridge to the proxy.
/// Called once at bridge setup time, not per-VM.
pub fn add_proxy_redirect_rules(bridge: &str) -> Result<()> {
    let ipt = ipt_new()?;

    // Redirect HTTP (port 80) to Envoy transparent proxy
    let rule = format!(
        "-i {} -p tcp --dport 80 -j REDIRECT --to-port 10080",
        bridge
    );
    ipt_append(&ipt, "nat", "PREROUTING", &rule, "REDIRECT port 80 -> 10080")?;

    // Redirect HTTPS (port 443) to TLS MITM proxy
    let rule = format!(
        "-i {} -p tcp --dport 443 -j REDIRECT --to-port 10443",
        bridge
    );
    ipt_append(&ipt, "nat", "PREROUTING", &rule, "REDIRECT port 443 -> 10443")?;

    info!("Proxy redirect rules added for bridge {}", bridge);
    Ok(())
}

/// Add iptables rules to allow DNS through and block all other egress.
/// Called once at bridge setup time.
pub fn add_egress_filter_rules(bridge: &str) -> Result<()> {
    let ipt = ipt_new()?;

    // Accept return traffic for established connections. This is required
    // because the default FORWARD policy may be DROP on some distros,
    // which would silently drop response packets for DNS and proxied traffic.
    ipt_append(
        &ipt, "filter", "FORWARD",
        "-m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT",
        "ACCEPT ESTABLISHED/RELATED return traffic",
    )?;

    // Allow DNS (UDP) forwarding
    let rule = format!("-i {} -p udp --dport 53 -j ACCEPT", bridge);
    ipt_append(&ipt, "filter", "FORWARD", &rule, "ACCEPT DNS UDP forward")?;

    // Allow DNS (TCP) forwarding
    let rule = format!("-i {} -p tcp --dport 53 -j ACCEPT", bridge);
    ipt_append(&ipt, "filter", "FORWARD", &rule, "ACCEPT DNS TCP forward")?;

    // MASQUERADE DNS traffic so it can reach external resolvers
    ipt_append(
        &ipt, "nat", "POSTROUTING",
        "-s 192.168.100.0/24 -p udp --dport 53 -j MASQUERADE",
        "MASQUERADE DNS UDP",
    )?;

    ipt_append(
        &ipt, "nat", "POSTROUTING",
        "-s 192.168.100.0/24 -p tcp --dport 53 -j MASQUERADE",
        "MASQUERADE DNS TCP",
    )?;

    // Drop all other forwarded traffic from the bridge (must be last)
    let rule = format!("-i {} -j DROP", bridge);
    ipt_append(&ipt, "filter", "FORWARD", &rule, "DROP all other forwarded traffic")?;

    info!("Egress filter rules added for bridge {}", bridge);
    Ok(())
}

/// Remove proxy redirect and egress filter rules (best-effort, for cleanup).
pub fn remove_proxy_rules(bridge: &str) {
    let ipt = match iptables::new(false) {
        Ok(ipt) => ipt,
        Err(e) => {
            warn!("Failed to initialize iptables for cleanup: {}", e);
            return;
        }
    };

    let rules: &[(&str, &str, String)] = &[
        (
            "nat",
            "PREROUTING",
            format!(
                "-i {} -p tcp --dport 80 -j REDIRECT --to-port 10080",
                bridge
            ),
        ),
        (
            "nat",
            "PREROUTING",
            format!(
                "-i {} -p tcp --dport 443 -j REDIRECT --to-port 10443",
                bridge
            ),
        ),
        (
            "filter",
            "FORWARD",
            "-m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT".to_string(),
        ),
        (
            "filter",
            "FORWARD",
            format!("-i {} -p udp --dport 53 -j ACCEPT", bridge),
        ),
        (
            "filter",
            "FORWARD",
            format!("-i {} -p tcp --dport 53 -j ACCEPT", bridge),
        ),
        (
            "nat",
            "POSTROUTING",
            "-s 192.168.100.0/24 -p udp --dport 53 -j MASQUERADE".to_string(),
        ),
        (
            "nat",
            "POSTROUTING",
            "-s 192.168.100.0/24 -p tcp --dport 53 -j MASQUERADE".to_string(),
        ),
        (
            "filter",
            "FORWARD",
            format!("-i {} -j DROP", bridge),
        ),
    ];

    for (table, chain, rule) in rules {
        if let Err(e) = ipt.delete(table, chain, rule) {
            warn!("Failed to remove iptables rule (may not exist): {}", e);
        }
    }

    info!(
        "Proxy iptables rules removed (best-effort) for bridge {}",
        bridge
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    #[ignore] // Requires root privileges and iptables
    fn test_add_and_remove_rule() {
        let tap = "test-tap0";
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 100, 2));

        // Add rule
        add_source_ip_rule(tap, ip).expect("Failed to add iptables rule");

        // Remove rule
        remove_source_ip_rule(tap, ip).expect("Failed to remove iptables rule");
    }
}

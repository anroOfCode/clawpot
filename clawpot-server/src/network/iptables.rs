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

/// Add iptables rules to redirect DNS to the proxy and block all other egress.
/// Called once at bridge setup time.
pub fn add_egress_filter_rules(bridge: &str) -> Result<()> {
    let ipt = ipt_new()?;

    // Redirect DNS (UDP) to DNS proxy
    let rule = format!(
        "-i {} -p udp --dport 53 -j REDIRECT --to-port 10053",
        bridge
    );
    ipt_append(&ipt, "nat", "PREROUTING", &rule, "REDIRECT DNS UDP → 10053")?;

    // Redirect DNS (TCP) to DNS proxy
    let rule = format!(
        "-i {} -p tcp --dport 53 -j REDIRECT --to-port 10053",
        bridge
    );
    ipt_append(&ipt, "nat", "PREROUTING", &rule, "REDIRECT DNS TCP → 10053")?;

    // Drop all other forwarded traffic from the bridge (must be last)
    let rule = format!("-i {} -j DROP", bridge);
    ipt_append(&ipt, "filter", "FORWARD", &rule, "DROP all other forwarded traffic")?;

    info!("Egress filter rules added for bridge {}", bridge);
    Ok(())
}

/// Idempotently ensure proxy redirect rules exist.
/// Uses iptables crate's `exists` check before appending to avoid duplicates.
pub fn ensure_proxy_redirect_rules(bridge: &str) -> Result<()> {
    let ipt = ipt_new()?;

    let rules: &[(&str, &str, String, &str)] = &[
        (
            "nat", "PREROUTING",
            format!("-i {} -p tcp --dport 80 -j REDIRECT --to-port 10080", bridge),
            "REDIRECT port 80 → 10080",
        ),
        (
            "nat", "PREROUTING",
            format!("-i {} -p tcp --dport 443 -j REDIRECT --to-port 10443", bridge),
            "REDIRECT port 443 → 10443",
        ),
    ];

    for (table, chain, rule, desc) in rules {
        ensure_iptables_rule(&ipt, table, chain, rule, desc)?;
    }
    Ok(())
}

/// Idempotently ensure egress filter rules exist.
pub fn ensure_egress_filter_rules(bridge: &str) -> Result<()> {
    let ipt = ipt_new()?;

    let rules: &[(&str, &str, String, &str)] = &[
        (
            "nat", "PREROUTING",
            format!("-i {} -p udp --dport 53 -j REDIRECT --to-port 10053", bridge),
            "REDIRECT DNS UDP → 10053",
        ),
        (
            "nat", "PREROUTING",
            format!("-i {} -p tcp --dport 53 -j REDIRECT --to-port 10053", bridge),
            "REDIRECT DNS TCP → 10053",
        ),
        (
            "filter", "FORWARD",
            format!("-i {} -j DROP", bridge),
            "DROP all other forwarded traffic",
        ),
    ];

    for (table, chain, rule, desc) in rules {
        ensure_iptables_rule(&ipt, table, chain, rule, desc)?;
    }
    Ok(())
}

/// Check if an iptables rule exists, and add it if not.
fn ensure_iptables_rule(ipt: &iptables::IPTables, table: &str, chain: &str, rule: &str, description: &str) -> Result<()> {
    let exists = ipt.exists(table, chain, rule)
        .map_err(|e| anyhow::anyhow!("iptables exists check for '{}' failed: {}", description, e))?;

    if exists {
        info!("iptables: {} (already exists)", description);
        return Ok(());
    }

    ipt_append(ipt, table, chain, rule, description)
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
            "nat",
            "PREROUTING",
            format!(
                "-i {} -p udp --dport 53 -j REDIRECT --to-port 10053",
                bridge
            ),
        ),
        (
            "nat",
            "PREROUTING",
            format!(
                "-i {} -p tcp --dport 53 -j REDIRECT --to-port 10053",
                bridge
            ),
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

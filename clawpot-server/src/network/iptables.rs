use anyhow::{Context, Result};
use std::net::IpAddr;
use std::process::Command;
use tracing::{info, warn};

/// Add an iptables rule to enforce source IP for a TAP device
/// Drops all packets from the TAP device if the source IP doesn't match the assigned IP
pub fn add_source_ip_rule(tap: &str, ip: IpAddr) -> Result<()> {
    let ip_str = ip.to_string();

    // Add rule to DROP packets from TAP if source IP doesn't match
    // iptables -A FORWARD -i <tap> ! -s <ip> -j DROP
    let output = Command::new("iptables")
        .args([
            "-A",
            "FORWARD",
            "-i",
            tap,
            "!",
            "-s",
            &ip_str,
            "-j",
            "DROP",
        ])
        .output()
        .context("Failed to execute iptables command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to add iptables rule for {} with IP {}: {}",
            tap,
            ip_str,
            stderr
        ));
    }

    info!(
        "Added iptables rule: {} must use source IP {}",
        tap, ip_str
    );

    Ok(())
}

/// Remove an iptables rule for a TAP device
/// Best-effort removal - doesn't fail if rule doesn't exist
pub fn remove_source_ip_rule(tap: &str, ip: IpAddr) -> Result<()> {
    let ip_str = ip.to_string();

    // Remove rule: iptables -D FORWARD -i <tap> ! -s <ip> -j DROP
    let output = Command::new("iptables")
        .args([
            "-D",
            "FORWARD",
            "-i",
            tap,
            "!",
            "-s",
            &ip_str,
            "-j",
            "DROP",
        ])
        .output()
        .context("Failed to execute iptables command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "Failed to remove iptables rule for {} with IP {} (may not exist): {}",
            tap, ip_str, stderr
        );
        // Don't return error - rule might not exist
        return Ok(());
    }

    info!(
        "Removed iptables rule for {} with IP {}",
        tap, ip_str
    );

    Ok(())
}

/// Add iptables rules to redirect HTTP/HTTPS traffic from the bridge to the proxy.
/// Called once at bridge setup time, not per-VM.
pub fn add_proxy_redirect_rules(bridge: &str) -> Result<()> {
    // Redirect HTTP (port 80) to Envoy transparent proxy
    run_iptables(&[
        "-t", "nat", "-A", "PREROUTING",
        "-i", bridge, "-p", "tcp", "--dport", "80",
        "-j", "REDIRECT", "--to-port", "10080",
    ], "REDIRECT port 80 → 10080")?;

    // Redirect HTTPS (port 443) to TLS MITM proxy
    run_iptables(&[
        "-t", "nat", "-A", "PREROUTING",
        "-i", bridge, "-p", "tcp", "--dport", "443",
        "-j", "REDIRECT", "--to-port", "10443",
    ], "REDIRECT port 443 → 10443")?;

    info!("Proxy redirect rules added for bridge {}", bridge);
    Ok(())
}

/// Add iptables rules to redirect DNS to the proxy and block all other egress.
/// Called once at bridge setup time.
pub fn add_egress_filter_rules(bridge: &str) -> Result<()> {
    // Redirect DNS (UDP) to DNS proxy
    run_iptables(&[
        "-t", "nat", "-A", "PREROUTING",
        "-i", bridge, "-p", "udp", "--dport", "53",
        "-j", "REDIRECT", "--to-port", "10053",
    ], "REDIRECT DNS UDP → 10053")?;

    // Redirect DNS (TCP) to DNS proxy
    run_iptables(&[
        "-t", "nat", "-A", "PREROUTING",
        "-i", bridge, "-p", "tcp", "--dport", "53",
        "-j", "REDIRECT", "--to-port", "10053",
    ], "REDIRECT DNS TCP → 10053")?;

    // Drop all other forwarded traffic from the bridge (must be last)
    run_iptables(&[
        "-A", "FORWARD", "-i", bridge, "-j", "DROP",
    ], "DROP all other forwarded traffic")?;

    info!("Egress filter rules added for bridge {}", bridge);
    Ok(())
}

/// Remove proxy redirect and egress filter rules (best-effort, for cleanup).
pub fn remove_proxy_rules(bridge: &str) {
    let rules: &[&[&str]] = &[
        &["-t", "nat", "-D", "PREROUTING", "-i", bridge, "-p", "tcp", "--dport", "80", "-j", "REDIRECT", "--to-port", "10080"],
        &["-t", "nat", "-D", "PREROUTING", "-i", bridge, "-p", "tcp", "--dport", "443", "-j", "REDIRECT", "--to-port", "10443"],
        &["-t", "nat", "-D", "PREROUTING", "-i", bridge, "-p", "udp", "--dport", "53", "-j", "REDIRECT", "--to-port", "10053"],
        &["-t", "nat", "-D", "PREROUTING", "-i", bridge, "-p", "tcp", "--dport", "53", "-j", "REDIRECT", "--to-port", "10053"],
        &["-D", "FORWARD", "-i", bridge, "-j", "DROP"],
    ];

    for rule in rules {
        let output = Command::new("iptables").args(*rule).output();
        match output {
            Ok(o) if !o.status.success() => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                warn!("Failed to remove iptables rule (may not exist): {}", stderr.trim());
            }
            Err(e) => warn!("Failed to execute iptables: {}", e),
            _ => {}
        }
    }

    info!("Proxy iptables rules removed (best-effort) for bridge {}", bridge);
}

fn run_iptables(args: &[&str], description: &str) -> Result<()> {
    let output = Command::new("iptables")
        .args(args)
        .output()
        .context("Failed to execute iptables command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "iptables rule '{}' failed: {}",
            description,
            stderr
        ));
    }

    info!("iptables: {}", description);
    Ok(())
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

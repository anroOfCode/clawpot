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

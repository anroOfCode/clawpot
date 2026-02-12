use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

/// Create a TAP network device
pub fn create_tap(name: &str) -> Result<()> {
    // Create TAP device
    let output = Command::new("ip")
        .args(["tuntap", "add", name, "mode", "tap"])
        .output()
        .context("Failed to execute 'ip tuntap add' command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to create TAP device {}: {}",
            name,
            stderr
        ));
    }

    info!("Created TAP device: {}", name);

    // Bring the device up
    bring_up(name)?;

    Ok(())
}

/// Bring a network device up
pub fn bring_up(name: &str) -> Result<()> {
    let output = Command::new("ip")
        .args(["link", "set", name, "up"])
        .output()
        .context("Failed to execute 'ip link set up' command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Failed to bring up device {}: {}",
            name,
            stderr
        ));
    }

    info!("Brought up TAP device: {}", name);
    Ok(())
}

/// Delete a TAP network device
pub fn delete_tap(name: &str) -> Result<()> {
    let output = Command::new("ip")
        .args(["tuntap", "del", name, "mode", "tap"])
        .output()
        .context("Failed to execute 'ip tuntap del' command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Failed to delete TAP device {}: {}", name, stderr);
        // Don't return error - device might already be deleted
        return Ok(());
    }

    info!("Deleted TAP device: {}", name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires root privileges
    fn test_create_and_delete_tap() {
        let tap_name = "test-tap-device";

        // Create TAP device
        create_tap(tap_name).expect("Failed to create TAP device");

        // Verify it exists (this would require parsing `ip link show`)

        // Delete TAP device
        delete_tap(tap_name).expect("Failed to delete TAP device");
    }
}

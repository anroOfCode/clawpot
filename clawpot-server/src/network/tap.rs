use anyhow::{Context, Result};
use nix::libc;
use rtnetlink::{Handle, LinkUnspec};
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use tracing::{info, warn};

use super::bridge::get_link_index;

// TUN/TAP ioctl constants
const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
const IFF_TAP: libc::c_short = 0x0002;
const IFF_NO_PI: libc::c_short = 0x1000;

/// ifreq struct for ioctl - only the fields we need
#[repr(C)]
struct Ifreq {
    ifr_name: [libc::c_char; libc::IFNAMSIZ],
    ifr_flags: libc::c_short,
    _padding: [u8; 22], // pad to full ifreq size
}

nix::ioctl_write_ptr_bad!(tunsetiff, TUNSETIFF, Ifreq);

/// Create a TAP network device using ioctl on /dev/net/tun
pub async fn create_tap(handle: &Handle, name: &str) -> Result<()> {
    // Validate name length (Linux interface names max 15 chars)
    anyhow::ensure!(
        name.len() < libc::IFNAMSIZ,
        "TAP device name '{}' too long (max {} chars)",
        name,
        libc::IFNAMSIZ - 1
    );

    // Open /dev/net/tun
    let tun_fd = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .context("Failed to open /dev/net/tun")?;

    // Build ifreq with TAP + NO_PI flags
    let mut ifr = Ifreq {
        ifr_name: [0; libc::IFNAMSIZ],
        ifr_flags: IFF_TAP | IFF_NO_PI,
        _padding: [0; 22],
    };

    // Copy name into ifreq
    for (i, byte) in name.bytes().enumerate() {
        ifr.ifr_name[i] = byte as libc::c_char;
    }

    // Create TAP device via ioctl
    // SAFETY: ifreq is correctly sized and initialized; fd is valid
    #[allow(unsafe_code)]
    unsafe {
        tunsetiff(tun_fd.as_raw_fd(), &raw const ifr)
            .context(format!("ioctl TUNSETIFF failed for TAP device {name}"))?;
    }

    // The fd must be kept open for the TAP device to persist... but actually
    // once the ioctl succeeds and the device is created, we need to set PERSIST
    // flag so it survives fd close. Use TUNSETPERSIST ioctl.
    const TUNSETPERSIST: libc::c_ulong = 0x4004_54cb;
    // SAFETY: fd is valid; setting TUNSETPERSIST to keep the device alive
    #[allow(unsafe_code)]
    unsafe {
        let ret = libc::ioctl(tun_fd.as_raw_fd(), TUNSETPERSIST as _, 1 as libc::c_int);
        if ret < 0 {
            return Err(anyhow::anyhow!(
                "ioctl TUNSETPERSIST failed for TAP device {}: {}",
                name,
                std::io::Error::last_os_error()
            ));
        }
    }

    info!("Created TAP device: {}", name);

    // Bring the device up via rtnetlink
    bring_up(handle, name).await?;

    Ok(())
}

/// Bring a network device up via rtnetlink
pub async fn bring_up(handle: &Handle, name: &str) -> Result<()> {
    let index = get_link_index(handle, name)
        .await
        .context(format!("Failed to get index for device {name}"))?;

    handle
        .link()
        .set(LinkUnspec::new_with_index(index).up().build())
        .execute()
        .await
        .context(format!("Failed to bring up device {name}"))?;

    info!("Brought up TAP device: {}", name);
    Ok(())
}

/// Delete a TAP network device via rtnetlink
pub async fn delete_tap(handle: &Handle, name: &str) -> Result<()> {
    match get_link_index(handle, name).await {
        Ok(index) => {
            handle
                .link()
                .del(index)
                .execute()
                .await
                .context(format!("Failed to delete TAP device {name}"))?;
            info!("Deleted TAP device: {}", name);
        }
        Err(_) => {
            warn!("TAP device {} not found (may already be deleted)", name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires root privileges
    async fn test_create_and_delete_tap() {
        let (connection, handle, _) = rtnetlink::new_connection().unwrap();
        tokio::spawn(connection);

        let tap_name = "test-tap-device";

        // Create TAP device
        create_tap(&handle, tap_name)
            .await
            .expect("Failed to create TAP device");

        // Delete TAP device
        delete_tap(&handle, tap_name)
            .await
            .expect("Failed to delete TAP device");
    }
}

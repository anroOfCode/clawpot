use anyhow::{anyhow, Result};
use bitvec::prelude::*;
use std::net::{IpAddr, Ipv4Addr};

/// IP address allocator for the 192.168.100.0/24 network
/// Gateway is at 192.168.100.1
/// Allocatable range: 192.168.100.2-254 (253 addresses)
pub struct IpAllocator {
    network_base: u32,      // 192.168.100.0 as u32
    gateway: Ipv4Addr,      // 192.168.100.1
    allocated: BitVec,      // Bitmap of allocated IPs
    next_index: usize,       // Hint for next allocation (round-robin)
}

impl IpAllocator {
    /// Create a new IP allocator for 192.168.100.0/24
    /// Gateway is 192.168.100.1, allocatable range is .2-.254
    pub fn new() -> Self {
        // 192.168.100.0 = 3232261120
        let network_base = u32::from(Ipv4Addr::new(192, 168, 100, 0));
        let gateway = Ipv4Addr::new(192, 168, 100, 1);

        // 253 allocatable IPs (.2 through .254)
        let allocated = bitvec![0; 253];

        Self {
            network_base,
            gateway,
            allocated,
            next_index: 0,
        }
    }

    /// Allocate the next available IP address
    /// Returns error if no IPs are available
    pub fn allocate(&mut self) -> Result<IpAddr> {
        // Find first unallocated IP using round-robin
        for i in 0..self.allocated.len() {
            let index = (self.next_index + i) % self.allocated.len();

            if !self.allocated[index] {
                // Mark as allocated
                self.allocated.set(index, true);

                // Update next_index for next allocation
                self.next_index = (index + 1) % self.allocated.len();

                // Convert index to IP address
                // Index 0 = 192.168.100.2, Index 1 = 192.168.100.3, etc.
                let ip_u32 = self.network_base + 2 + index as u32;
                let ip = Ipv4Addr::from(ip_u32);

                return Ok(IpAddr::V4(ip));
            }
        }

        Err(anyhow!("No available IP addresses in the pool"))
    }

    /// Release an IP address back to the pool
    pub fn release(&mut self, ip: IpAddr) -> Result<()> {
        let ipv4 = match ip {
            IpAddr::V4(v4) => v4,
            IpAddr::V6(_) => return Err(anyhow!("IPv6 addresses are not supported")),
        };

        let ip_u32 = u32::from(ipv4);

        // Check if IP is in our network range
        if ip_u32 < self.network_base + 2 || ip_u32 > self.network_base + 254 {
            return Err(anyhow!(
                "IP address {} is not in the allocatable range (192.168.100.2-254)",
                ipv4
            ));
        }

        // Calculate index in the bitmap
        let index = (ip_u32 - self.network_base - 2) as usize;

        if index >= self.allocated.len() {
            return Err(anyhow!("IP address {} is out of range", ipv4));
        }

        // Mark as unallocated
        self.allocated.set(index, false);

        Ok(())
    }

    /// Get the gateway IP address
    pub fn gateway(&self) -> IpAddr {
        IpAddr::V4(self.gateway)
    }

    /// Get the number of allocated IPs
    pub fn allocated_count(&self) -> usize {
        self.allocated.count_ones()
    }

    /// Get the number of available IPs
    pub fn available_count(&self) -> usize {
        self.allocated.len() - self.allocated_count()
    }
}

impl Default for IpAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_first_ip() {
        let mut allocator = IpAllocator::new();
        let ip = allocator.allocate().unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 100, 2)));
    }

    #[test]
    fn test_allocate_multiple_ips() {
        let mut allocator = IpAllocator::new();
        let ip1 = allocator.allocate().unwrap();
        let ip2 = allocator.allocate().unwrap();
        let ip3 = allocator.allocate().unwrap();

        assert_eq!(ip1, IpAddr::V4(Ipv4Addr::new(192, 168, 100, 2)));
        assert_eq!(ip2, IpAddr::V4(Ipv4Addr::new(192, 168, 100, 3)));
        assert_eq!(ip3, IpAddr::V4(Ipv4Addr::new(192, 168, 100, 4)));
    }

    #[test]
    fn test_release_and_reallocate() {
        let mut allocator = IpAllocator::new();
        let ip1 = allocator.allocate().unwrap();
        let _ip2 = allocator.allocate().unwrap();

        // Release first IP
        allocator.release(ip1).unwrap();

        // Should have 2 available (253 - 1 allocated)
        assert_eq!(allocator.available_count(), 252);

        // Allocate again - should get a different IP (round-robin)
        let ip3 = allocator.allocate().unwrap();
        assert_ne!(ip3, ip1);  // Round-robin, so we get the next available
    }

    #[test]
    fn test_allocate_all_ips() {
        let mut allocator = IpAllocator::new();

        // Allocate all 253 IPs
        for i in 0..253 {
            let ip = allocator.allocate();
            assert!(ip.is_ok(), "Failed to allocate IP {}", i);
        }

        // 254th allocation should fail
        let result = allocator.allocate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No available IP"));
    }

    #[test]
    fn test_release_invalid_ip() {
        let mut allocator = IpAllocator::new();

        // Try to release IP outside range
        let result = allocator.release(IpAddr::V4(Ipv4Addr::new(192, 168, 100, 1)));  // Gateway
        assert!(result.is_err());

        let result = allocator.release(IpAddr::V4(Ipv4Addr::new(192, 168, 100, 255)));  // Broadcast
        assert!(result.is_err());

        let result = allocator.release(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));  // Different network
        assert!(result.is_err());
    }

    #[test]
    fn test_gateway() {
        let allocator = IpAllocator::new();
        assert_eq!(allocator.gateway(), IpAddr::V4(Ipv4Addr::new(192, 168, 100, 1)));
    }
}

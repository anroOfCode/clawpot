use anyhow::{anyhow, Result};
use clawpot_common::vm::VmManager;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use uuid::Uuid;

pub type VmId = Uuid;

/// Entry in the VM registry containing VM metadata and manager
pub struct VmEntry {
    pub id: VmId,
    pub manager: VmManager,
    pub ip_address: IpAddr,
    pub tap_name: String,
    pub created_at: SystemTime,
    pub vcpu_count: u8,
    pub mem_size_mib: u32,
    pub vsock_uds_path: String,
}

/// Thread-safe VM registry for managing multiple VMs
pub struct VmRegistry {
    vms: Arc<RwLock<HashMap<VmId, VmEntry>>>,
}

impl VmRegistry {
    /// Create a new empty VM registry
    pub fn new() -> Self {
        Self {
            vms: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert a new VM into the registry
    /// Returns error if VM ID already exists
    pub async fn insert(&self, id: VmId, entry: VmEntry) -> Result<()> {
        let mut vms = self.vms.write().await;

        if vms.contains_key(&id) {
            return Err(anyhow!("VM with ID {} already exists", id));
        }

        vms.insert(id, entry);
        Ok(())
    }

    /// Remove a VM from the registry and return it
    /// Returns error if VM doesn't exist
    pub async fn remove(&self, id: &VmId) -> Result<VmEntry> {
        let mut vms = self.vms.write().await;

        vms.remove(id)
            .ok_or_else(|| anyhow!("VM with ID {} not found", id))
    }

    /// Get a reference to a VM entry
    /// Returns error if VM doesn't exist
    /// Note: This returns a clone since we can't hold a read lock across await points
    pub async fn get(&self, id: &VmId) -> Result<VmId> {
        let vms = self.vms.read().await;

        if vms.contains_key(id) {
            Ok(*id)
        } else {
            Err(anyhow!("VM with ID {} not found", id))
        }
    }

    /// List all VM IDs and their metadata
    /// Returns a vector of tuples (id, ip, tap_name, vcpus, memory, created_at)
    pub async fn list(&self) -> Vec<(VmId, IpAddr, String, u8, u32, SystemTime)> {
        let vms = self.vms.read().await;

        vms.iter()
            .map(|(id, entry)| {
                (
                    *id,
                    entry.ip_address,
                    entry.tap_name.clone(),
                    entry.vcpu_count,
                    entry.mem_size_mib,
                    entry.created_at,
                )
            })
            .collect()
    }

    /// Get the count of registered VMs
    pub async fn count(&self) -> usize {
        let vms = self.vms.read().await;
        vms.len()
    }

    /// Get VM entry for status query (returns subset of info without holding lock)
    pub async fn get_vm_info(&self, id: &VmId) -> Result<(IpAddr, String, u8, u32, SystemTime)> {
        let vms = self.vms.read().await;

        let entry = vms
            .get(id)
            .ok_or_else(|| anyhow!("VM with ID {} not found", id))?;

        Ok((
            entry.ip_address,
            entry.tap_name.clone(),
            entry.vcpu_count,
            entry.mem_size_mib,
            entry.created_at,
        ))
    }

    /// Get the vsock UDS path for a VM
    pub async fn get_vsock_path(&self, id: &VmId) -> Result<String> {
        let vms = self.vms.read().await;
        let entry = vms
            .get(id)
            .ok_or_else(|| anyhow!("VM with ID {} not found", id))?;
        Ok(entry.vsock_uds_path.clone())
    }
}

impl Default for VmRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_insert_and_get() {
        let registry = VmRegistry::new();
        let id = Uuid::new_v4();

        let entry = VmEntry {
            id,
            manager: VmManager::new(PathBuf::from("/tmp/test.sock")),
            ip_address: "192.168.100.2".parse().unwrap(),
            tap_name: "tap-test".to_string(),
            created_at: SystemTime::now(),
            vcpu_count: 2,
            mem_size_mib: 512,
            vsock_uds_path: "/tmp/test-vsock.sock".to_string(),
        };

        registry.insert(id, entry).await.unwrap();

        assert_eq!(registry.count().await, 1);
        assert!(registry.get(&id).await.is_ok());
    }

    #[tokio::test]
    async fn test_remove() {
        let registry = VmRegistry::new();
        let id = Uuid::new_v4();

        let entry = VmEntry {
            id,
            manager: VmManager::new(PathBuf::from("/tmp/test.sock")),
            ip_address: "192.168.100.2".parse().unwrap(),
            tap_name: "tap-test".to_string(),
            created_at: SystemTime::now(),
            vcpu_count: 2,
            mem_size_mib: 512,
            vsock_uds_path: "/tmp/test-vsock.sock".to_string(),
        };

        registry.insert(id, entry).await.unwrap();
        let removed = registry.remove(&id).await.unwrap();

        assert_eq!(removed.id, id);
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn test_list() {
        let registry = VmRegistry::new();

        for i in 0..3 {
            let id = Uuid::new_v4();
            let entry = VmEntry {
                id,
                manager: VmManager::new(PathBuf::from(format!("/tmp/test-{}.sock", i))),
                ip_address: format!("192.168.100.{}", i + 2).parse().unwrap(),
                tap_name: format!("tap-test-{}", i),
                created_at: SystemTime::now(),
                vcpu_count: 1,
                mem_size_mib: 256,
                vsock_uds_path: format!("/tmp/test-{}-vsock.sock", i),
            };
            registry.insert(id, entry).await.unwrap();
        }

        let list = registry.list().await;
        assert_eq!(list.len(), 3);
    }
}

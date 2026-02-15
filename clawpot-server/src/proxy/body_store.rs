use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const DEFAULT_INLINE_THRESHOLD: usize = 64 * 1024; // 64KB

pub enum StoredBody {
    Inline(Vec<u8>),
    External(PathBuf),
}

impl StoredBody {
    pub fn inline_bytes(&self) -> Option<&[u8]> {
        match self {
            StoredBody::Inline(b) => Some(b),
            StoredBody::External(_) => None,
        }
    }

    pub fn external_path(&self) -> Option<&str> {
        match self {
            StoredBody::External(p) => p.to_str(),
            StoredBody::Inline(_) => None,
        }
    }
}

pub struct BodyStore {
    storage_dir: PathBuf,
    inline_threshold: usize,
}

impl BodyStore {
    pub fn new(storage_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(storage_dir)
            .with_context(|| format!("Failed to create body storage dir: {}", storage_dir.display()))?;
        Ok(Self {
            storage_dir: storage_dir.to_path_buf(),
            inline_threshold: DEFAULT_INLINE_THRESHOLD,
        })
    }

    /// Store a body, either inline or externalized to disk.
    /// `suffix` should be "req" or "resp".
    pub fn store(&self, request_id: i64, suffix: &str, body: &[u8]) -> Result<StoredBody> {
        if body.len() <= self.inline_threshold {
            return Ok(StoredBody::Inline(body.to_vec()));
        }

        let path = self.storage_dir.join(format!("{}_{}.bin", request_id, suffix));
        std::fs::write(&path, body)
            .with_context(|| format!("Failed to write body to {}", path.display()))?;
        Ok(StoredBody::External(path))
    }
}

//! `<state_dir>/usage/cache.json`：每个日志文件的大小、修改时间和解析出的记录。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::record::FileEntry;

/// 格式变了就加一；读到旧版本时整体重建。
pub(crate) const CACHE_VERSION: u32 = 1;

#[derive(Deserialize)]
struct CacheDisk {
    version: u32,
    files: Vec<FileEntry>,
}

#[derive(Serialize)]
struct CacheDiskRef<'a> {
    version: u32,
    files: Vec<&'a FileEntry>,
}

pub(crate) type Files = HashMap<String, FileEntry>;

pub(crate) fn load(path: &Path) -> Files {
    let Ok(bytes) = std::fs::read(path) else {
        return Files::new();
    };
    match serde_json::from_slice::<CacheDisk>(&bytes) {
        Ok(c) if c.version == CACHE_VERSION => {
            c.files.into_iter().map(|f| (f.path.clone(), f)).collect()
        }
        _ => Files::new(),
    }
}

pub(crate) fn save(path: &Path, files: &Files) -> std::io::Result<()> {
    let mut list: Vec<&FileEntry> = files.values().collect();
    list.sort_by(|a, b| a.path.cmp(&b.path));
    let bytes = serde_json::to_vec(&CacheDiskRef {
        version: CACHE_VERSION,
        files: list,
    })
    .map_err(std::io::Error::other)?;
    crate::pricing::write_atomic(path, &bytes)
}

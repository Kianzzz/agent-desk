//! 并行遍历目录（rayon 递归），统计占用空间、文件数和最新修改时间。
//! 不跟随符号链接；硬链接只算一次；大小按实际占用的磁盘块（和 `du` 一致）。

use crate::util::system_time_secs;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Mutex;

/// 树里保留的单个文件的最小大小（更小的文件只计入父目录的合计）
pub(crate) const KEEP_FILE_MIN: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Stats {
    pub bytes: u64,
    pub files: u64,
    /// 最新修改时间（Unix 秒），0 = 未知
    pub newest: i64,
}

impl Stats {
    pub fn add(&mut self, o: &Stats) {
        self.bytes += o.bytes;
        self.files += o.files;
        self.newest = self.newest.max(o.newest);
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    pub name: String,
    pub is_dir: bool,
    pub stats: Stats,
    /// 只保留到一定深度：目录全部保留，文件只保留 >= KEEP_FILE_MIN 的
    pub children: Vec<Node>,
}

#[cfg(test)]
impl Node {
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
}

struct Ctx {
    seen: Mutex<HashSet<(u64, u64)>>,
}

impl Ctx {
    fn new() -> Self {
        Ctx {
            seen: Mutex::new(HashSet::new()),
        }
    }

    fn file_bytes(&self, m: &Metadata) -> u64 {
        if m.nlink() > 1 && !m.is_dir() {
            let key = (m.dev(), m.ino());
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            if !seen.insert(key) {
                return 0;
            }
        }
        m.blocks() * 512
    }
}

fn mtime(m: &Metadata) -> i64 {
    m.modified().map(system_time_secs).unwrap_or(0)
}

/// 遍历 `path`，返回带子树（深度 `depth`）的节点。路径不存在时返回 None。
pub(crate) fn walk_tree(path: &Path, depth: usize) -> Option<Node> {
    let meta = fs::symlink_metadata(path).ok()?;
    let ctx = Ctx::new();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(node_from(&ctx, path, name, &meta, depth))
}

fn node_from(ctx: &Ctx, path: &Path, name: String, meta: &Metadata, depth: usize) -> Node {
    if !meta.is_dir() {
        return Node {
            name,
            is_dir: false,
            stats: Stats {
                bytes: ctx.file_bytes(meta),
                files: 1,
                newest: mtime(meta),
            },
            children: Vec::new(),
        };
    }
    let mut stats = Stats {
        bytes: meta.blocks() * 512,
        files: 0,
        newest: mtime(meta),
    };
    let entries: Vec<_> = match fs::read_dir(path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => Vec::new(),
    };
    let kids: Vec<Node> = entries
        .par_iter()
        .filter_map(|e| {
            let m = e.metadata().ok()?; // 不跟随符号链接
            let child_name = e.file_name().to_string_lossy().into_owned();
            let child_path = e.path();
            if m.is_dir() {
                if depth > 0 {
                    Some(node_from(ctx, &child_path, child_name, &m, depth - 1))
                } else {
                    let s = stats_inner(ctx, &child_path, &m);
                    Some(Node {
                        name: child_name,
                        is_dir: true,
                        stats: s,
                        children: Vec::new(),
                    })
                }
            } else {
                Some(Node {
                    name: child_name,
                    is_dir: false,
                    stats: Stats {
                        bytes: ctx.file_bytes(&m),
                        files: 1,
                        newest: mtime(&m),
                    },
                    children: Vec::new(),
                })
            }
        })
        .collect();
    let mut children = Vec::new();
    for k in kids {
        stats.add(&k.stats);
        if depth > 0 && (k.is_dir || k.stats.bytes >= KEEP_FILE_MIN) {
            children.push(k);
        }
    }
    Node {
        name,
        is_dir: true,
        stats,
        children,
    }
}

fn stats_inner(ctx: &Ctx, path: &Path, meta: &Metadata) -> Stats {
    if !meta.is_dir() {
        return Stats {
            bytes: ctx.file_bytes(meta),
            files: 1,
            newest: mtime(meta),
        };
    }
    let mut s = Stats {
        bytes: meta.blocks() * 512,
        files: 0,
        newest: mtime(meta),
    };
    let entries: Vec<_> = match fs::read_dir(path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return s,
    };
    let sub = entries
        .par_iter()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some(stats_inner(ctx, &e.path(), &m))
        })
        .reduce(Stats::default, |mut a, b| {
            a.add(&b);
            a
        });
    s.add(&sub);
    s
}

/// 只要合计，不要子树。路径不存在时返回 None。
pub(crate) fn dir_stats(path: &Path) -> Option<Stats> {
    let meta = fs::symlink_metadata(path).ok()?;
    let ctx = Ctx::new();
    Some(stats_inner(&ctx, path, &meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_counts_and_tree() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/f1"), vec![1u8; 10_000]).unwrap();
        fs::write(root.join("a/f2"), vec![1u8; 2 * 1024 * 1024]).unwrap();
        fs::write(root.join("small"), b"x").unwrap();
        std::os::unix::fs::symlink("/", root.join("link")).unwrap();
        let n = walk_tree(root, 3).unwrap();
        assert_eq!(n.stats.files, 4); // 含符号链接本身
        let a = n.child("a").unwrap();
        assert!(a.child("b").is_some());
        assert!(a.child("f2").is_some(), "大文件保留在树里");
        assert!(n.child("small").is_none(), "小文件不进树");
        let s = dir_stats(root).unwrap();
        assert_eq!(s.files, 4);
        assert!(s.bytes >= 2 * 1024 * 1024);
        assert_eq!(s.bytes, n.stats.bytes);
    }
}

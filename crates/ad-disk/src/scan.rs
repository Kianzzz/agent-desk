//! 扫描入口：并行发现对话记录、遍历各数据目录，再按项目汇总。

use crate::locations::{
    build_location, current_claude_cli, location_defs, walk_location, Ctx, Env,
};
use crate::projects::{build_projects, ProjectEnv};
use crate::transcripts::{discover_all, Transcript};
use crate::util::rfc3339;
use crate::{DiskItem, DiskReport, DiskScanOptions};
use rayon::prelude::*;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

/// 遍历主要卡在文件系统的元数据调用上（IO 等待），线程多一些明显更快
const IO_THREADS: usize = 32;

pub(crate) fn run(opts: &DiskScanOptions) -> anyhow::Result<DiskReport> {
    if !opts.home.is_dir() {
        anyhow::bail!("主目录不存在：{}", opts.home.display());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(IO_THREADS)
        .thread_name(|i| format!("ad-disk-{i}"))
        .build()
        .map_err(|e| anyhow::anyhow!("无法创建扫描线程：{e}"))?;
    pool.install(|| run_inner(opts))
}

fn run_inner(opts: &DiskScanOptions) -> anyhow::Result<DiskReport> {
    let start = Instant::now();
    let env = Env::new(&opts.home);
    let defs = location_defs(&env);
    let (transcripts, nodes) = rayon::join(
        || discover_all(&opts.home),
        || defs.par_iter().map(walk_location).collect::<Vec<_>>(),
    );
    let ctx = build_ctx(&env, &transcripts);

    let mut locations: Vec<DiskItem> = defs
        .iter()
        .zip(nodes.iter())
        .filter_map(|(d, n)| n.as_ref().map(|n| build_location(&ctx, d, n)))
        .filter(|item| item.size_bytes > 0)
        .collect();
    locations.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    let mut scratch_items: HashMap<String, DiskItem> = HashMap::new();
    for loc in &locations {
        collect_scratch(loc, &mut scratch_items);
    }

    let penv = ProjectEnv {
        home: &opts.home,
        now: env.now,
        include_folders: opts.include_project_folders,
        claude_tmp: env.claude_tmp.as_deref(),
    };
    let projects = build_projects(&penv, &transcripts, &scratch_items);

    let total_ai_bytes = locations.iter().map(|l| l.size_bytes).sum();
    let (disk_total_bytes, disk_free_bytes) = disk_space(&opts.home);
    Ok(DiskReport {
        scanned_at: rfc3339(env.now),
        duration_ms: start.elapsed().as_millis() as u64,
        total_ai_bytes,
        disk_total_bytes,
        disk_free_bytes,
        locations,
        projects,
    })
}

fn collect_scratch(item: &DiskItem, out: &mut HashMap<String, DiskItem>) {
    if item.category == "claude_scratch" && item.children.is_empty() {
        if let Some(p) = &item.project {
            out.insert(p.clone(), item.clone());
        }
    }
    for c in &item.children {
        collect_scratch(c, out);
    }
}

fn build_ctx(env: &Env, transcripts: &[Transcript]) -> Ctx {
    let mut ctx = Ctx {
        now: env.now,
        current_claude_cli: current_claude_cli(&env.home),
        ..Default::default()
    };
    // 新的对话优先提供标题
    let mut sorted: Vec<&Transcript> = transcripts.iter().collect();
    sorted.sort_by(|a, b| b.stats.newest.cmp(&a.stats.newest));
    for t in sorted {
        ctx.sessions
            .entry(t.session_id.clone())
            .or_insert_with(|| (t.title.clone(), t.cwd.clone()));
        let dir_name = |p: Option<&Path>| {
            p.and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        };
        match t.tool {
            "claude" => {
                if let (Some(enc), Some(cwd)) = (dir_name(t.path.parent()), &t.cwd) {
                    ctx.claude_enc.entry(enc).or_insert_with(|| cwd.clone());
                }
            }
            "gemini" => {
                if let (Some(d), Some(cwd)) =
                    (dir_name(t.path.parent().and_then(|p| p.parent())), &t.cwd)
                {
                    ctx.gemini_dirs.entry(d).or_insert_with(|| cwd.clone());
                }
            }
            _ => {}
        }
        if let (Some(cwd), Some(title)) = (&t.cwd, &t.title) {
            ctx.title_by_cwd
                .entry(cwd.clone())
                .or_insert_with(|| title.clone());
        }
    }
    ctx
}

/// $HOME 所在卷的总空间和可用空间
pub(crate) fn disk_space(path: &Path) -> (u64, u64) {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return (0, 0);
    };
    // SAFETY: statvfs 只写入我们提供的结构体
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return (0, 0);
    }
    #[allow(clippy::useless_conversion)] // 各平台字段宽度不同
    let frsize = u64::from(st.f_frsize);
    (
        u64::from(st.f_blocks) * frsize,
        u64::from(st.f_bavail) * frsize,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiskScanOptions, Safety};
    use std::fs;

    #[test]
    fn scan_fake_home() {
        let d = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(d.path()).unwrap();
        let proj = home.join("code/app");
        fs::create_dir_all(proj.join("node_modules")).unwrap();
        fs::write(proj.join("node_modules/a.js"), vec![1u8; 2 * 1024 * 1024]).unwrap();
        let enc = crate::transcripts::claude_encode(&proj.to_string_lossy());
        let pd = home.join(".claude/projects").join(&enc);
        fs::create_dir_all(&pd).unwrap();
        fs::write(
            pd.join("s1.jsonl"),
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\",\"message\":{{\"content\":\"修个 bug\"}}}}\n{}",
                proj.display(),
                "x".repeat(2 * 1024 * 1024)
            ),
        )
        .unwrap();
        fs::create_dir_all(home.join(".codex/cache")).unwrap();
        fs::write(home.join(".codex/cache/big"), vec![0u8; 3 * 1024 * 1024]).unwrap();
        fs::write(home.join(".codex/auth.json"), "{}").unwrap();

        let r = run(&DiskScanOptions {
            home: home.clone(),
            include_project_folders: true,
        })
        .unwrap();
        assert!(r.disk_total_bytes > 0);
        assert_eq!(
            r.total_ai_bytes,
            r.locations.iter().map(|l| l.size_bytes).sum::<u64>()
        );
        let tools: Vec<&str> = r.locations.iter().map(|l| l.tool.as_str()).collect();
        assert!(tools.contains(&"claude") && tools.contains(&"codex"));
        let claude = r
            .locations
            .iter()
            .find(|l| l.label == "Claude Code")
            .unwrap();
        let projects = claude
            .children
            .iter()
            .find(|c| c.category == "claude_transcripts")
            .unwrap();
        assert_eq!(projects.children.len(), 1);
        assert_eq!(projects.children[0].label, "对话记录 · app");
        assert_eq!(r.projects.len(), 1);
        let p = &r.projects[0];
        assert_eq!(p.display_name, "app");
        assert!(p.exists);
        assert_eq!(p.transcripts[0].title.as_deref(), Some("修个 bug"));
        // 刚写的对话：在用 → 受保护
        assert_eq!(p.transcripts[0].safety, Safety::Protected);
        assert_eq!(p.artifacts.len(), 1);
        assert!(p.folder_bytes.unwrap() >= 50_000);
        // JSON 字段是 camelCase
        let j = serde_json::to_value(&r).unwrap();
        assert!(j.get("totalAiBytes").is_some());
        assert!(j["projects"][0].get("displayName").is_some());
        assert_eq!(j["projects"][0]["transcripts"][0]["safety"], "protected");
    }
}

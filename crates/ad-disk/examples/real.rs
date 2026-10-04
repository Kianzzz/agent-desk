//! 对本机真实 $HOME 只读扫描，打印摘要。**不调用任何修改类函数。**
//!
//! CARGO_TARGET_DIR=target-agents/ad-disk cargo run --release -p ad-disk --example real

use ad_disk::{scan, DiskItem, DiskReport, DiskScanOptions, Safety};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

fn human(b: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

fn sname(s: Safety) -> &'static str {
    match s {
        Safety::Safe => "Safe",
        Safety::Review => "Review",
        Safety::Keep => "Keep",
        Safety::Protected => "Protected",
    }
}

/// 每个条目自身（扣掉已列出的子项）的大小记到它自己的 safety 上，合起来正好是总量
fn attribute(item: &DiskItem, acc: &mut BTreeMap<&'static str, u64>) {
    let kids: u64 = item.children.iter().map(|c| c.size_bytes).sum();
    *acc.entry(sname(item.safety)).or_default() += item.size_bytes.saturating_sub(kids);
    for c in &item.children {
        attribute(c, acc);
    }
}

fn print_report(r: &DiskReport, show_tree: bool) {
    println!(
        "扫描用时 {} ms；AI 文件合计 {}；磁盘 {} / 可用 {}",
        r.duration_ms,
        human(r.total_ai_bytes),
        human(r.disk_total_bytes),
        human(r.disk_free_bytes)
    );
    if show_tree {
        println!("\n== 按位置（一级 + 二级）");
        for l in &r.locations {
            println!(
                "{:>10}  [{:<9}] {}  ({})",
                human(l.size_bytes),
                sname(l.safety),
                l.label,
                l.path
            );
            for c in &l.children {
                println!(
                    "    {:>10}  [{:<9}]{} {}  — {}",
                    human(c.size_bytes),
                    sname(c.safety),
                    if c.in_use { "*" } else { " " },
                    c.label,
                    c.reason
                );
            }
        }
    }
    let mut acc = BTreeMap::new();
    for l in &r.locations {
        attribute(l, &mut acc);
    }
    println!("\n== 各类合计（按位置）");
    for k in ["Safe", "Review", "Keep", "Protected"] {
        println!("{:>10}  {k}", human(*acc.get(k).unwrap_or(&0)));
    }

    // 项目里的可清理项（不在位置里的构建产物/worktree）
    let mut proj_acc: BTreeMap<&'static str, u64> = BTreeMap::new();
    for p in &r.projects {
        for a in &p.artifacts {
            if a.category == "build_artifact" || a.category == "worktree" {
                *proj_acc.entry(sname(a.safety)).or_default() += a.size_bytes;
            }
        }
    }
    if !proj_acc.is_empty() {
        println!("项目里的构建产物/worktree：{proj_acc:?}");
    }

    println!("\n== 按项目（前 15，共 {} 个）", r.projects.len());
    for p in r.projects.iter().take(15) {
        let safe_t: u64 = p
            .transcripts
            .iter()
            .filter(|t| t.safety == Safety::Safe)
            .map(|t| t.size_bytes)
            .sum();
        println!(
            "{:>10}  {}{}  对话 {} 个 {}（Safe {}），文件夹 {}，可清理项 {} 个 {}  工具 {:?}  最近 {}",
            human(p.total_bytes),
            p.display_name,
            if p.exists { "" } else { "（已不存在）" },
            p.sessions,
            human(p.transcripts_bytes),
            human(safe_t),
            p.folder_bytes.map(human).unwrap_or_else(|| "-".into()),
            p.artifacts.len(),
            human(p.artifacts.iter().map(|a| a.size_bytes).sum()),
            p.tools,
            &p.last_active[..10.min(p.last_active.len())]
        );
        for t in p.transcripts.iter().take(2) {
            println!(
                "              · [{:<9}] {} {}",
                sname(t.safety),
                human(t.size_bytes),
                t.title.as_deref().unwrap_or("（无标题）")
            );
        }
        for a in p.artifacts.iter().take(3) {
            println!(
                "              + [{:<9}] {} {}",
                sname(a.safety),
                human(a.size_bytes),
                a.label
            );
        }
    }
    let no_title = r
        .projects
        .iter()
        .flat_map(|p| p.transcripts.iter())
        .filter(|t| t.title.is_none())
        .count();
    let total_t: usize = r.projects.iter().map(|p| p.transcripts.len()).sum();
    println!("\n对话记录 {total_t} 个，其中没取到标题的 {no_title} 个");
}

fn main() -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var("HOME")?);
    for (include, tree) in [(false, true), (true, false)] {
        println!("\n################ include_project_folders = {include}");
        let t = Instant::now();
        let r = scan(&DiskScanOptions {
            home: home.clone(),
            include_project_folders: include,
        })?;
        print_report(&r, tree);
        println!("（含构建报告总耗时 {:.1} 秒）", t.elapsed().as_secs_f64());
        let json = serde_json::to_string(&r)?;
        println!("JSON 大小 {}", human(json.len() as u64));
        // 需要细看时：AD_DISK_JSON=/某个目录 会把完整结果写成 JSON
        if let Some(dir) = std::env::var_os("AD_DISK_JSON") {
            let p = PathBuf::from(dir).join(format!("report-{include}.json"));
            std::fs::write(&p, &json)?;
            println!("已写入 {}", p.display());
        }
    }
    Ok(())
}

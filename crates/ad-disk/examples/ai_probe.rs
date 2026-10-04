//! 用几条**虚构的**条目真实调用一次 `claude`，验证参数和解析。会用到订阅额度，别频繁跑。
//!
//! CARGO_TARGET_DIR=target-agents/ad-disk cargo run -p ad-disk --example ai_probe

use ad_disk::{assess_with_ai, AssessInput, Safety};
use std::time::Instant;

#[allow(clippy::too_many_arguments)]
fn item(
    id: &str,
    path: &str,
    label: &str,
    category: &str,
    size_mb: u64,
    modified: &str,
    safety: Safety,
    reason: &str,
) -> AssessInput {
    AssessInput {
        id: id.into(),
        path: path.into(),
        label: label.into(),
        category: category.into(),
        project: Some("/Users/demo/code/old-site".into()),
        project_exists: Some(false),
        size_bytes: size_mb * 1024 * 1024,
        modified: modified.into(),
        title: None,
        safety,
        reason: reason.into(),
    }
}

fn main() -> anyhow::Result<()> {
    let mut items = vec![
        item(
            "demo-cache",
            "/Users/demo/Library/Application Support/Claude/Cache",
            "网页缓存",
            "cache",
            800,
            "2026-10-03T10:00:00+08:00",
            Safety::Safe,
            "缓存，删除后会自动重建",
        ),
        item(
            "demo-chat",
            "/Users/demo/.claude/projects/-Users-demo-code-old-site/abc.jsonl",
            "Claude Code 对话",
            "claude_transcripts",
            45,
            "2026-07-01T10:00:00+08:00",
            Safety::Safe,
            "项目文件夹已经不在了，而且 30 天以上没动过",
        ),
        item(
            "demo-db",
            "/Users/demo/.codex/state_5.sqlite",
            "state_5.sqlite",
            "database",
            2,
            "2026-10-04T09:00:00+08:00",
            Safety::Protected,
            "Codex 正在使用的数据库",
        ),
        item(
            "demo-img",
            "/Users/demo/.codex/generated_images/0001",
            "生成的图片",
            "codex_images",
            150,
            "2026-09-20T10:00:00+08:00",
            Safety::Review,
            "AI 生成的图片，确认没用再删",
        ),
    ];
    items[1].title = Some("帮我把旧网站的首页改成响应式".into());
    let t = Instant::now();
    let r = assess_with_ai(&items)?;
    println!(
        "用时 {:.1} 秒，返回 {} 条",
        t.elapsed().as_secs_f64(),
        r.len()
    );
    for a in r {
        println!("{:<12} {:<7} {}", a.id, a.verdict, a.reason);
    }
    Ok(())
}

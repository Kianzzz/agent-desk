//! 对本机真实 `$HOME` 只读运行（缓存写在临时目录），打印摘要用来核对。
//!
//! `cargo run --release -p ad-usage --example real`
//! 可选：`AD_USAGE_STATE=/某个目录` 复用缓存，`AD_USAGE_DAYS=14` 改变按天明细的天数，
//! `AD_USAGE_JSON=/某个文件` 把完整快照写成 JSON。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use ad_usage::{Tool, UsageEngine, UsageSnapshot};

const ALL_TOOLS: [Tool; 18] = [
    Tool::Claude,
    Tool::Codex,
    Tool::Gemini,
    Tool::Grok,
    Tool::Opencode,
    Tool::Kilo,
    Tool::Qwen,
    Tool::Copilot,
    Tool::Cline,
    Tool::Roo,
    Tool::Kimi,
    Tool::Droid,
    Tool::Amp,
    Tool::Pi,
    Tool::Openclaw,
    Tool::Codebuddy,
    Tool::Crush,
    Tool::Goose,
];

fn tool_name(t: Tool) -> &'static str {
    match t {
        Tool::Claude => "Claude",
        Tool::Codex => "Codex",
        Tool::Gemini => "Gemini",
        Tool::Grok => "Grok",
        Tool::Opencode => "OpenCode",
        Tool::Kilo => "Kilo",
        Tool::Qwen => "Qwen",
        Tool::Copilot => "Copilot",
        Tool::Cline => "Cline",
        Tool::Roo => "Roo",
        Tool::Kimi => "Kimi",
        Tool::Droid => "Droid",
        Tool::Amp => "Amp",
        Tool::Pi => "Pi",
        Tool::Openclaw => "OpenClaw",
        Tool::Codebuddy => "CodeBuddy",
        Tool::Crush => "Crush",
        Tool::Goose => "Goose",
    }
}

fn mtok(n: u64) -> String {
    format!("{:.2}M", n as f64 / 1e6)
}

fn main() -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var("HOME")?);
    let tmp = tempfile::tempdir()?;
    let state = std::env::var("AD_USAGE_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let days_back: i64 = std::env::var("AD_USAGE_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);

    let mut eng = UsageEngine::new(home.clone(), state.clone());
    let t = Instant::now();
    let _first = eng.refresh()?;
    let d1 = t.elapsed();
    let t = Instant::now();
    let s = eng.refresh()?;
    let d2 = t.elapsed();
    // 新进程：从磁盘缓存冷启动
    let t = Instant::now();
    let _cold = UsageEngine::new(home, state.clone()).refresh()?;
    let d3 = t.elapsed();
    let cache_size = std::fs::metadata(state.join("usage/cache.json"))
        .map(|m| m.len())
        .unwrap_or(0);

    if let Ok(out) = std::env::var("AD_USAGE_JSON") {
        std::fs::write(out, serde_json::to_vec_pretty(&s)?)?;
    }

    println!("== 耗时 ==");
    println!("第一次 refresh（全量）：{:.2?}", d1);
    println!("第二次 refresh（无变化）：{:.2?}", d2);
    println!("新实例从缓存启动：{:.2?}", d3);
    println!("缓存文件：{:.1} MB", cache_size as f64 / 1e6);
    println!("时区：{}  生成于：{}", s.timezone, s.generated_at);
    println!("价格表：{:?}", s.pricing_updated_at);

    println!("\n== 数据源 ==");
    for src in &s.sources {
        println!(
            "{:9} 文件 {:4}  记录 {:7}  已删除文件里的记录 {:5}  最新 {:?}  错误 {:?}",
            tool_name(src.tool),
            src.files,
            src.records,
            src.archived_records,
            src.last_record_at,
            src.errors
        );
    }

    print_totals(&s, None);
    let cutoff = (chrono::Local::now() - chrono::Duration::days(days_back - 1))
        .format("%Y-%m-%d")
        .to_string();
    print_totals(&s, Some(&cutoff));

    println!("\n== 最近 {days_back} 天按天/模型 ==");
    let mut per_day: BTreeMap<&str, f64> = BTreeMap::new();
    for d in s.days.iter().filter(|d| d.date.as_str() >= cutoff.as_str()) {
        *per_day.entry(d.date.as_str()).or_default() += d.cost_usd;
        println!(
            "{} {:9} {:24} 请求 {:5}  输入 {:>8}  输出 {:>7}  缓存读 {:>9}  缓存写 {:>8}  ${:9.2}{}",
            d.date,
            tool_name(d.tool),
            d.model,
            d.requests,
            mtok(d.tokens.input),
            mtok(d.tokens.output),
            mtok(d.tokens.cache_read),
            mtok(d.tokens.cache_write),
            d.cost_usd,
            if d.unpriced_requests > 0 {
                format!("  （{} 次未定价）", d.unpriced_requests)
            } else {
                String::new()
            }
        );
    }
    println!("-- 每天合计 --");
    for (day, c) in &per_day {
        println!("{day}  ${c:.2}");
    }

    println!("\n== 未定价模型 ==\n{:?}", s.unpriced_models);

    println!("\n== 额度 ==");
    for q in &s.quotas {
        println!(
            "{} {}：{:.0}%（{} 分钟窗口）重置 {:?}，读取于 {}",
            tool_name(q.tool),
            q.label,
            q.used_percent,
            q.window_minutes,
            q.resets_at,
            q.observed_at
        );
    }

    println!("\n== 活跃 5 小时窗口 ==");
    if s.active_blocks.is_empty() {
        println!("（无）");
    }
    for b in &s.active_blocks {
        println!(
            "{} {} → {}  ${:.2}  请求 {}  每小时 ${:.2}",
            tool_name(b.tool),
            b.start,
            b.end,
            b.cost_usd,
            b.requests,
            b.burn_rate_usd_per_hour
        );
    }

    println!("\n== 费用最高的项目 ==");
    for p in s.projects.iter().take(8) {
        println!(
            "{:9} ${:9.2}  会话 {:3}  {}",
            tool_name(p.tool),
            p.cost_usd,
            p.sessions,
            p.project
        );
    }

    println!("\n== 最近的会话（共输出 {} 个）==", s.sessions.len());
    for x in s.sessions.iter().take(8) {
        println!(
            "{:9} {}  ${:8.2}  {:?}  {:?}",
            tool_name(x.tool),
            x.last_active,
            x.cost_usd,
            x.models,
            x.title
        );
    }
    Ok(())
}

fn print_totals(s: &UsageSnapshot, since: Option<&str>) {
    match since {
        None => println!("\n== 各工具总额（全部历史）=="),
        Some(d) => println!("\n== 各工具总额（{d} 起）=="),
    }
    let mut sum = 0.0;
    for tool in ALL_TOOLS {
        let rows = s
            .days
            .iter()
            .filter(|d| d.tool == tool && since.is_none_or(|c| d.date.as_str() >= c));
        let (mut cost, mut req, mut unpriced, mut tok) = (0.0, 0, 0, 0);
        // 会话数取项目汇总（不受会话列表 300 条的限制），只在全部历史时有意义
        let sessions: u32 = s
            .projects
            .iter()
            .filter(|p| p.tool == tool)
            .map(|p| p.sessions)
            .sum();
        for d in rows {
            cost += d.cost_usd;
            req += d.requests;
            unpriced += d.unpriced_requests;
            tok += d.tokens.input + d.tokens.output + d.tokens.cache_read + d.tokens.cache_write;
        }
        sum += cost;
        println!(
            "{:9} ${:10.2}  请求 {:7}  token {:>10}  未定价请求 {:5}  会话 {}",
            tool_name(tool),
            cost,
            req,
            mtok(tok),
            unpriced,
            if since.is_none() {
                sessions.to_string()
            } else {
                "-".to_string()
            }
        );
    }
    println!("合计      ${sum:10.2}");
}

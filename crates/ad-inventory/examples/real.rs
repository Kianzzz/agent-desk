//! 对本机真实的 $HOME 只读运行 scan，打印摘要。不调用任何开关或删除函数。
//!
//! 运行：CARGO_TARGET_DIR=target-agents/ad-inventory cargo run -p ad-inventory --example real --release

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let home = std::env::var("HOME").map(PathBuf::from).expect("需要 HOME");
    // scan 会在 state_dir 下写哈希缓存。为了不在真实 $HOME 里留下任何文件，这里用临时状态目录，
    // 只把真实的停用存档（如果有）复制过去，好让被停用的条目也能显示出来。
    let state_dir = std::env::temp_dir().join(format!("ad-inventory-real-{}", std::process::id()));
    let real_store = home.join(".agent-desk/inventory/disabled.json");
    if real_store.is_file() {
        std::fs::create_dir_all(state_dir.join("inventory")).expect("创建临时目录");
        std::fs::copy(&real_store, state_dir.join("inventory/disabled.json"))
            .expect("复制停用存档");
    }

    let t = Instant::now();
    let inv = ad_inventory::scan(&home, &state_dir);
    let cold = t.elapsed();
    let t = Instant::now();
    let _ = ad_inventory::scan(&home, &state_dir);
    let warm = t.elapsed();
    std::fs::remove_dir_all(&state_dir).ok();

    println!("扫描时间：{}", inv.scanned_at);
    println!(
        "耗时：首次 {:.3} 秒（无哈希缓存），再次 {:.3} 秒（有缓存）",
        cold.as_secs_f64(),
        warm.as_secs_f64()
    );

    println!("\n== MCP 服务（{} 个）", inv.mcp.len());
    let mut by_client: BTreeMap<String, usize> = BTreeMap::new();
    for m in &inv.mcp {
        *by_client.entry(format!("{:?}", m.client)).or_default() += 1;
    }
    for (c, n) in &by_client {
        println!("  {c}: {n}");
    }
    for m in &inv.mcp {
        println!(
            "  - [{:?}/{}] {} transport={} enabled={} manageable={} package={:?} env={:?} headers={:?}{}",
            m.client,
            m.scope,
            m.name,
            m.transport,
            m.enabled,
            m.manageable,
            m.package,
            m.env_keys,
            m.header_keys,
            m.scope_path.as_ref().map(|p| format!(" @ {p}")).unwrap_or_default()
        );
    }

    println!("\n== 技能根目录");
    for r in &inv.skill_roots {
        println!(
            "  {} ({}) exists={} count={}  {}",
            r.label, r.id, r.exists, r.count, r.path
        );
    }
    let symlinks = inv.skills.iter().filter(|s| s.is_symlink).count();
    let scripts = inv.skills.iter().filter(|s| s.has_scripts).count();
    let disabled = inv.skills.iter().filter(|s| !s.enabled).count();
    let synced = inv.skills.iter().filter(|s| s.synced_by.is_some()).count();
    let total_bytes: u64 = inv
        .skills
        .iter()
        .filter(|s| !s.is_symlink)
        .map(|s| s.size_bytes)
        .sum();
    println!(
        "  技能共 {} 个；符号链接 {}；含脚本 {}；未启用 {}；ChatCut 同步 {}；非链接目录合计 {:.1} MB",
        inv.skills.len(),
        symlinks,
        scripts,
        disabled,
        synced,
        total_bytes as f64 / 1e6
    );
    let no_desc = inv
        .skills
        .iter()
        .filter(|s| s.description.is_empty())
        .count();
    println!("  没有 description 的技能：{no_desc}");
    for s in inv.skills.iter().filter(|s| !s.enabled) {
        println!("  未启用：[{}] {} ({})", s.root_id, s.name, s.path);
    }

    let identical = inv.skill_groups.iter().filter(|g| g.identical).count();
    println!(
        "\n== 同名技能组：{} 组（一致 {}，不一致 {}）",
        inv.skill_groups.len(),
        identical,
        inv.skill_groups.len() - identical
    );
    for g in inv.skill_groups.iter().filter(|g| !g.identical) {
        let roots: Vec<String> = g
            .entry_ids
            .iter()
            .filter_map(|id| inv.skills.iter().find(|s| &s.id == id))
            .map(|s| s.root_id.clone())
            .collect();
        println!("  不一致：{} -> {:?}", g.name, roots);
    }

    println!("\n== 钩子（{} 个）", inv.hooks.len());
    for h in &inv.hooks {
        println!(
            "  - [{:?}/{}] {} matcher={:?} enabled={} manageable={} cmd={}",
            h.client, h.scope, h.event, h.matcher, h.enabled, h.manageable, h.command
        );
    }

    println!("\n== 插件（{} 个）", inv.plugins.len());
    for p in &inv.plugins {
        println!(
            "  - {} @ {:?} v{:?} enabled={} skills={} mcp={} hooks={} commands={} agents={}",
            p.name,
            p.marketplace,
            p.version,
            p.enabled,
            p.skills,
            p.mcp_servers,
            p.hooks,
            p.commands,
            p.agents
        );
    }

    println!("\n== 提示（{} 条）", inv.warnings.len());
    for w in &inv.warnings {
        println!("  - {w}");
    }
}

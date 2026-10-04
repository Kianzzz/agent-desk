//! 对本机真实的 $HOME 只读运行一遍扫描，打印摘要。只读，不修改任何文件。
//!
//! 运行：`CARGO_TARGET_DIR=target-agents/ad-security cargo run -p ad-security --example real`

use std::collections::BTreeMap;
use std::path::PathBuf;

use ad_security::{scan, ScanOptions};

fn main() {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("没有 HOME");
    let all = std::env::args().any(|a| a == "--all");
    let report = scan(&ScanOptions {
        home,
        extra_project_dirs: Vec::new(),
    });
    println!("扫描文件数：{}", report.files_scanned);
    println!("耗时：{} ms", report.duration_ms);
    println!("发现：{} 条", report.findings.len());

    let mut by_sev: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_cat: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_rule: BTreeMap<String, usize> = BTreeMap::new();
    for f in &report.findings {
        *by_sev.entry(format!("{:?}", f.severity)).or_default() += 1;
        *by_cat.entry(format!("{:?}", f.category)).or_default() += 1;
        *by_rule
            .entry(format!("{:?}/{}", f.severity, f.rule_id))
            .or_default() += 1;
    }
    println!("\n按严重程度：");
    for (k, v) in &by_sev {
        println!("  {k:<8} {v}");
    }
    println!("\n按类别：");
    for (k, v) in &by_cat {
        println!("  {k:<18} {v}");
    }
    println!("\n按规则：");
    for (k, v) in &by_rule {
        println!("  {k:<40} {v}");
    }

    let n = if all { report.findings.len() } else { 30 };
    println!("\n前 {n} 条：");
    for (i, f) in report.findings.iter().take(n).enumerate() {
        println!(
            "\n{:>2}. [{:?}] [{}] {}",
            i + 1,
            f.severity,
            f.rule_id,
            f.title
        );
        println!(
            "    {}{}",
            f.path,
            f.line.map(|l| format!(":{l}")).unwrap_or_default()
        );
        if let Some(e) = &f.excerpt {
            println!("    > {e}");
        }
        println!("    {}", f.detail);
    }
}

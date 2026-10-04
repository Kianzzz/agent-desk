//! 拼装 `Finding`：稳定 id、去重、限流、排序。

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::{Category, Finding, Severity};

/// 发现属于哪个对象（界面上按它分组）。
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// "skill" | "hook" | "mcp" | "settings" | "instructions" | "credentials" | "shell"
    pub kind: &'static str,
    pub name: Option<String>,
}

impl Target {
    pub(crate) fn new(kind: &'static str, name: impl Into<String>) -> Self {
        Target {
            kind,
            name: Some(name.into()),
        }
    }
}

/// 一条发现的全部材料。
pub(crate) struct Draft<'a> {
    pub rule_id: &'a str,
    pub severity: Severity,
    pub category: Category,
    pub title: String,
    pub detail: String,
    pub path: &'a str,
    pub line: Option<usize>,
    /// 命中的原文，只用来算 id，不会输出
    pub matched: &'a str,
    /// 已经打过码的摘录
    pub excerpt: Option<String>,
    pub target: &'a Target,
}

impl Draft<'_> {
    pub(crate) fn build(self) -> Finding {
        Finding {
            id: stable_id(self.rule_id, self.path, self.line, self.matched),
            rule_id: self.rule_id.to_string(),
            severity: self.severity,
            category: self.category,
            title: crate::text::spaced(&self.title),
            detail: crate::text::spaced(&self.detail),
            path: self.path.to_string(),
            line: self.line.map(|l| l as u32),
            excerpt: self.excerpt,
            target_kind: self.target.kind.to_string(),
            target_name: self.target.name.clone(),
        }
    }
}

/// sha256(规则 + 路径 + 行 + 命中内容) 的前 16 位十六进制。
pub(crate) fn stable_id(rule_id: &str, path: &str, line: Option<usize>, matched: &str) -> String {
    let mut h = Sha256::new();
    h.update(rule_id.as_bytes());
    h.update([0]);
    h.update(path.as_bytes());
    h.update([0]);
    h.update(line.unwrap_or(0).to_string().as_bytes());
    h.update([0]);
    h.update(matched.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

fn category_rank(c: Category) -> u8 {
    match c {
        Category::DangerousCommand => 0,
        Category::HiddenChars => 1,
        Category::PromptInjection => 2,
        Category::BroadPermission => 3,
        Category::PlaintextSecret => 4,
        Category::InsecureTransport => 5,
        Category::SupplyChain => 6,
        Category::FilePermission => 7,
    }
}

/// 同一文件同一规则最多列几条。
pub(crate) const PER_FILE_RULE_MAX: usize = 5;

/// 去重（同一文件同一行同一规则只留最严重的一条）、限流、排序。
pub(crate) fn finalize(mut all: Vec<Finding>) -> Vec<Finding> {
    // 先按严重程度排，去重时保留排在前面的
    all.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    let mut seen_line: std::collections::HashSet<(String, Option<u32>, String)> =
        Default::default();
    let mut seen_id: std::collections::HashSet<String> = Default::default();
    let mut kept: Vec<Finding> = Vec::new();
    for f in all {
        let key = (f.path.clone(), f.line, f.rule_id.clone());
        if f.line.is_some() && !seen_line.insert(key) {
            continue;
        }
        if !seen_id.insert(f.id.clone()) {
            continue;
        }
        kept.push(f);
    }
    // 同一行上被更具体的规则覆盖的：外发凭据已经包含了读私钥
    const SUBSUMED: &[(&str, &str)] = &[("read-private-key", "exfil-credentials")];
    let present: std::collections::HashSet<(String, Option<u32>, String)> = kept
        .iter()
        .map(|f| (f.path.clone(), f.line, f.rule_id.clone()))
        .collect();
    kept.retain(|f| {
        !SUBSUMED.iter().any(|(weak, strong)| {
            f.rule_id == *weak && present.contains(&(f.path.clone(), f.line, strong.to_string()))
        })
    });
    // 同一文件同一规则限流
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for f in &kept {
        *counts
            .entry((f.path.clone(), f.rule_id.clone()))
            .or_default() += 1;
    }
    let mut emitted: HashMap<(String, String), usize> = HashMap::new();
    let mut out: Vec<Finding> = Vec::new();
    for mut f in kept {
        let key = (f.path.clone(), f.rule_id.clone());
        let n = emitted.entry(key.clone()).or_default();
        *n += 1;
        if *n > PER_FILE_RULE_MAX {
            continue;
        }
        let total = counts[&key];
        if *n == PER_FILE_RULE_MAX && total > PER_FILE_RULE_MAX {
            f.detail.push_str(&format!(
                "（这个文件里还有 {} 处同类问题，没有逐条列出。）",
                total - PER_FILE_RULE_MAX
            ));
        }
        out.push(f);
    }
    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| category_rank(a.category).cmp(&category_rank(b.category)))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_short() {
        let a = stable_id("r", "/p", Some(3), "x");
        assert_eq!(a.len(), 16);
        assert_eq!(a, stable_id("r", "/p", Some(3), "x"));
        assert_ne!(a, stable_id("r", "/p", Some(4), "x"));
    }
}

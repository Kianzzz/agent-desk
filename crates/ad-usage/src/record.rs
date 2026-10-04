//! 解析出来的紧凑记录，以及每个日志文件在缓存里的条目。

use serde::{Deserialize, Serialize};

use crate::Tool;

/// Claude `speed: "fast"`
pub(crate) const FLAG_FAST: u8 = 1;
/// Codex `service_tier: "priority"`
pub(crate) const FLAG_PRIORITY: u8 = 2;

/// 一次计费请求。缓存里序列化成数组以减小体积。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RecRepr", into = "RecRepr")]
pub(crate) struct Rec {
    /// Unix 毫秒
    pub ts: i64,
    /// 全局去重键（0 表示不去重）
    pub key: u64,
    /// `FileEntry::models` 的下标
    pub model: u32,
    /// `FileEntry::sessions` 的下标
    pub session: u32,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub reasoning: u64,
    pub flags: u8,
}

type RecRepr = (i64, u64, u32, u32, u64, u64, u64, u64, u64, u64, u8);

impl From<RecRepr> for Rec {
    fn from(r: RecRepr) -> Self {
        Rec {
            ts: r.0,
            key: r.1,
            model: r.2,
            session: r.3,
            input: r.4,
            output: r.5,
            cache_read: r.6,
            cache_write_5m: r.7,
            cache_write_1h: r.8,
            reasoning: r.9,
            flags: r.10,
        }
    }
}

impl From<Rec> for RecRepr {
    fn from(r: Rec) -> Self {
        (
            r.ts,
            r.key,
            r.model,
            r.session,
            r.input,
            r.output,
            r.cache_read,
            r.cache_write_5m,
            r.cache_write_1h,
            r.reasoning,
            r.flags,
        )
    }
}

impl Rec {
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    pub fn is_empty(&self) -> bool {
        self.input == 0
            && self.output == 0
            && self.cache_read == 0
            && self.cache_write_5m == 0
            && self.cache_write_1h == 0
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SessionMeta {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 标题来自子代理/旁支对话（优先级低于主对话）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub title_side: bool,
    #[serde(default)]
    pub title_ts: i64,
}

impl SessionMeta {
    /// 这条候选标题是否应该替换现有标题。
    pub fn wants_title(&self, side: bool) -> bool {
        match &self.title {
            None => true,
            Some(_) => self.title_side && !side,
        }
    }

    pub fn set_title(&mut self, title: String, side: bool, ts: i64) {
        if self.wants_title(side) {
            self.title = Some(title);
            self.title_side = side;
            self.title_ts = ts;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QuotaWin {
    pub used_percent: f64,
    pub window_minutes: u64,
    /// Unix 秒
    #[serde(default)]
    pub resets_at: Option<i64>,
}

/// 某一刻从日志里读到的额度。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QuotaObs {
    /// Unix 毫秒
    pub ts: i64,
    #[serde(default)]
    pub limit_id: Option<String>,
    pub windows: Vec<QuotaWin>,
}

/// Codex 增量续读时需要接上的解析状态。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CodexState {
    #[serde(default)]
    pub session: Option<u32>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub priority: bool,
    /// input, cached, cache_write, output, reasoning
    #[serde(default)]
    pub prev_total: Option<[u64; 5]>,
    /// fork 出来的会话开头会把父会话的历史（含 token_count）原样重放一遍，这段不计费
    #[serde(default)]
    pub replay: bool,
    #[serde(default)]
    pub last_line_ts: i64,
}

/// 追加写入的 jsonl 文件可以从上次读到的位置继续。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Resume {
    /// 已完整解析到的字节位置（最后一个换行之后）
    pub offset: u64,
    /// 用文件开头若干字节确认还是同一个文件
    pub head_len: u64,
    pub head_hash: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FileEntry {
    pub tool: Tool,
    pub path: String,
    pub size: u64,
    pub mtime_ns: i64,
    /// 原文件已经不在了，记录仍然保留
    #[serde(default)]
    pub missing: bool,
    pub models: Vec<String>,
    pub sessions: Vec<SessionMeta>,
    pub recs: Vec<Rec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<QuotaObs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Resume>,
}

impl FileEntry {
    pub fn new(tool: Tool, path: String, size: u64, mtime_ns: i64) -> Self {
        FileEntry {
            tool,
            path,
            size,
            mtime_ns,
            missing: false,
            models: Vec::new(),
            sessions: Vec::new(),
            recs: Vec::new(),
            quota: None,
            resume: None,
        }
    }

    pub fn model_idx(&mut self, name: &str) -> u32 {
        if let Some(i) = self.models.iter().position(|m| m == name) {
            return i as u32;
        }
        self.models.push(name.to_string());
        (self.models.len() - 1) as u32
    }

    pub fn session_idx(&mut self, id: &str) -> u32 {
        if let Some(i) = self.sessions.iter().rposition(|s| s.id == id) {
            return i as u32;
        }
        self.sessions.push(SessionMeta {
            id: id.to_string(),
            ..Default::default()
        });
        (self.sessions.len() - 1) as u32
    }

    pub fn find_session(&self, id: &str) -> Option<&SessionMeta> {
        self.sessions.iter().rev().find(|s| s.id == id)
    }

    /// 同一文件内按去重键合并，保留 output 最大的那条。
    pub fn push_dedup(&mut self, keys: &mut std::collections::HashMap<u64, usize>, rec: Rec) {
        if rec.key == 0 {
            self.recs.push(rec);
            return;
        }
        match keys.get(&rec.key) {
            Some(&i) => {
                if rec.output > self.recs[i].output {
                    self.recs[i] = rec;
                }
            }
            None => {
                keys.insert(rec.key, self.recs.len());
                self.recs.push(rec);
            }
        }
    }

    pub fn key_index(&self) -> std::collections::HashMap<u64, usize> {
        self.recs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.key != 0)
            .map(|(i, r)| (r.key, i))
            .collect()
    }
}

/// 稳定的 64 位 FNV-1a，用来做去重键和文件头指纹（跨版本不变）。
pub(crate) fn fnv64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in *p {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    if h == 0 {
        1
    } else {
        h
    }
}

/// RFC 3339 → Unix 毫秒
pub(crate) fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// `[文字](链接)` → `文字`，避免标题被长路径占满。
fn strip_md_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let parsed = after.find("](").and_then(|close| {
            let label = &after[..close];
            let tail = &after[close + 2..];
            let end = tail.find(')')?;
            (!label.contains('[')
                && !label.contains('\n')
                && !tail[..end].contains(char::is_whitespace))
            .then_some((label, &tail[end + 1..]))
        });
        match parsed {
            Some((label, next)) => {
                out.push_str(&rest[..open]);
                out.push_str(label);
                rest = next;
            }
            None => {
                out.push_str(&rest[..open + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 第一条用户消息 → 标题：去掉 Markdown 链接地址、合并空白，取前 80 个字符。
pub(crate) fn clean_title(s: &str) -> Option<String> {
    let head: String = s.chars().take(2000).collect();
    let head = strip_md_links(&head);
    let collapsed = head.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(80).collect())
}

/// 以 `<标签…>` 开头的文本（系统注入的提示、命令输出、任务通知等），不当作用户输入。
pub(crate) fn starts_with_tag(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('<') else {
        return false;
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let name_len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_' || *b == b':')
        .count();
    name_len > 0
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && matches!(
            rest.as_bytes().get(name_len),
            Some(b'>' | b' ' | b'/' | b'\n')
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(
            clean_title("[$cover](/Users/x/SKILL.md) 抖音封面  标题\n第二行").as_deref(),
            Some("$cover 抖音封面 标题 第二行")
        );
        assert_eq!(
            clean_title("数组 a[0] 和 [链接](有 空格)").as_deref(),
            Some("数组 a[0] 和 [链接](有 空格)")
        );
        assert_eq!(clean_title("   \n ").as_deref(), None);
        assert_eq!(
            clean_title(&"字".repeat(200)).map(|t| t.chars().count()),
            Some(80)
        );
        assert!(starts_with_tag("<task-notification> <task-id>1</task-id>"));
        assert!(starts_with_tag("<system-reminder>\nx"));
        assert!(starts_with_tag("<image name=[Image #1]>"));
        assert!(!starts_with_tag("<3 谢谢"));
        assert!(!starts_with_tag("< 不是标签"));
        assert!(!starts_with_tag("普通文字"));
    }
}

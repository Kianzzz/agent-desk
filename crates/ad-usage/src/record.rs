//! 解析出来的紧凑记录，以及每个日志文件在缓存里的条目。

use serde::{Deserialize, Serialize};

use crate::Tool;

/// Claude `speed: "fast"`
pub(crate) const FLAG_FAST: u8 = 1;
/// Codex `service_tier: "priority"`
pub(crate) const FLAG_PRIORITY: u8 = 2;

/// 一次计费请求。缓存里序列化成数组以减小体积。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// 工具自己记下的费用，单位是 1e-10 美元；0 表示没有，按价格表折算
    pub known_cost: u64,
}

/// 美元 → `Rec::known_cost` 的单位（负数、非有限值当作没有）
pub(crate) fn usd_to_cost(usd: f64) -> u64 {
    if usd.is_finite() && usd > 0.0 {
        (usd * COST_UNITS_PER_USD).round() as u64
    } else {
        0
    }
}

pub(crate) const COST_UNITS_PER_USD: f64 = 1e10;

// 缓存格式：前 11 项和旧版本一样；有已知费用时追加第 12 项。旧缓存照样能读。
impl Serialize for Rec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let n = if self.known_cost > 0 { 12 } else { 11 };
        let mut t = s.serialize_tuple(n)?;
        t.serialize_element(&self.ts)?;
        t.serialize_element(&self.key)?;
        t.serialize_element(&self.model)?;
        t.serialize_element(&self.session)?;
        t.serialize_element(&self.input)?;
        t.serialize_element(&self.output)?;
        t.serialize_element(&self.cache_read)?;
        t.serialize_element(&self.cache_write_5m)?;
        t.serialize_element(&self.cache_write_1h)?;
        t.serialize_element(&self.reasoning)?;
        t.serialize_element(&self.flags)?;
        if self.known_cost > 0 {
            t.serialize_element(&self.known_cost)?;
        }
        t.end()
    }
}

impl<'de> Deserialize<'de> for Rec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Rec;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("11 或 12 项的数组")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Rec, A::Error> {
                use serde::de::Error;
                macro_rules! next {
                    ($i:expr) => {
                        a.next_element()?
                            .ok_or_else(|| A::Error::invalid_length($i, &self))?
                    };
                }
                Ok(Rec {
                    ts: next!(0),
                    key: next!(1),
                    model: next!(2),
                    session: next!(3),
                    input: next!(4),
                    output: next!(5),
                    cache_read: next!(6),
                    cache_write_5m: next!(7),
                    cache_write_1h: next!(8),
                    reasoning: next!(9),
                    flags: next!(10),
                    known_cost: a.next_element()?.unwrap_or(0),
                })
            }
        }
        d.deserialize_seq(V)
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

/// 各家时间字段的写法不一：RFC 3339、`2026-10-01 12:00:00`（按 UTC）、秒或毫秒数字。统一成 Unix 毫秒。
pub(crate) fn parse_time_str(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(ms) = parse_ts(s) {
        return Some(ms);
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(d.and_utc().timestamp_millis());
        }
    }
    s.parse::<f64>().ok().and_then(num_to_ms)
}

/// 数字时间：大于 1e11 当毫秒（1973 年以后的毫秒数），否则当秒。
pub(crate) fn num_to_ms(n: f64) -> Option<i64> {
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    Some(if n > 1e14 {
        // 微秒
        (n / 1000.0) as i64
    } else if n > 1e11 {
        n as i64
    } else {
        (n * 1000.0) as i64
    })
}

pub(crate) fn parse_time_value(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64().and_then(num_to_ms),
        serde_json::Value::String(s) => parse_time_str(s),
        _ => None,
    }
}

/// `%2FUsers%2Fx` → `/Users/x`（解不出来的字节原样保留）
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// 用户消息内容（字符串，或带 `text` 字段的片段数组）里第一段像用户输入的文字 → 标题
pub(crate) fn first_text_title(v: &serde_json::Value) -> Option<String> {
    let usable = |s: &str| {
        let t = s.trim_start();
        (!t.is_empty() && !starts_with_tag(t))
            .then(|| clean_title(t))
            .flatten()
    };
    match v {
        serde_json::Value::String(s) => usable(s),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(serde_json::Value::as_str))
            .find_map(usable),
        _ => None,
    }
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
    fn rec_cache_format_is_backward_compatible() {
        // 旧版本写的 11 项数组照样能读，已知费用为 0
        let old: Rec = serde_json::from_str("[1,2,3,4,5,6,7,8,9,10,1]").unwrap();
        assert_eq!((old.ts, old.flags, old.known_cost), (1, 1, 0));
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            "[1,2,3,4,5,6,7,8,9,10,1]"
        );
        let with_cost = Rec {
            known_cost: 123,
            ..old
        };
        let text = serde_json::to_string(&with_cost).unwrap();
        assert_eq!(text, "[1,2,3,4,5,6,7,8,9,10,1,123]");
        assert_eq!(serde_json::from_str::<Rec>(&text).unwrap(), with_cost);
        assert!(serde_json::from_str::<Rec>("[1,2,3]").is_err());
        assert_eq!(usd_to_cost(0.0123), 123_000_000);
        assert_eq!(usd_to_cost(-1.0), 0);
        assert_eq!(usd_to_cost(f64::NAN), 0);
    }

    #[test]
    fn time_and_decode_helpers() {
        let iso = parse_time_str("2026-10-01T12:00:00Z").unwrap();
        assert_eq!(parse_time_str("2026-10-01 12:00:00"), Some(iso));
        assert_eq!(parse_time_str("1790000000"), Some(1_790_000_000_000));
        assert_eq!(num_to_ms(1_790_000_000_123.0), Some(1_790_000_000_123));
        assert_eq!(num_to_ms(1_770_983_426.42), Some(1_770_983_426_420));
        assert_eq!(parse_time_str(""), None);
        assert_eq!(percent_decode("%2FUsers%2Fa%20b%zz"), "/Users/a b%zz");
    }

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

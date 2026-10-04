//! Codex：`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`、`~/.codex/archived_sessions/`
//!
//! 文件可能有上 GB、行里有 base64 图片，只看行首的 `type`，挑需要的行解析。

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde::Deserialize;

use crate::record::{
    clean_title, fnv64, parse_ts, CodexState, FileEntry, QuotaObs, QuotaWin, Rec, FLAG_PRIORITY,
};
use crate::scan::{contains, find_str_value, scan_lines, LineSink};

const P_SESSION_META: &[u8] = b"\"type\":\"session_meta\"";
const P_TURN_CONTEXT: &[u8] = b"\"type\":\"turn_context\"";
const P_TOKEN_COUNT: &[u8] = b"\"type\":\"token_count\"";
const P_SETTINGS: &[u8] = b"\"type\":\"thread_settings_applied\"";
const P_USER_MESSAGE: &[u8] = b"\"type\":\"user_message\"";
const P_ROLE_USER: &[u8] = b"\"role\":\"user\"";

#[derive(Deserialize)]
struct Line<P> {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    payload: Option<P>,
}

#[derive(Deserialize)]
struct SessionMetaP {
    id: Option<String>,
    cwd: Option<String>,
    forked_from_id: Option<String>,
}

/// fork 重放是一次性写入的，行与行的时间间隔是毫秒级；出现超过这个间隔的停顿就说明重放结束了。
const REPLAY_GAP_MS: i64 = 2000;

#[derive(Deserialize)]
struct TurnContextP {
    model: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct TokenCountP {
    #[serde(rename = "type")]
    kind: Option<String>,
    info: Option<TokenInfo>,
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct TokenInfo {
    total_token_usage: Option<TokenUsage>,
    last_token_usage: Option<TokenUsage>,
}

#[derive(Deserialize, Clone, Copy)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    cache_write_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

impl TokenUsage {
    fn arr(&self) -> [u64; 5] {
        [
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens,
        ]
    }
}

#[derive(Deserialize)]
struct RateLimits {
    limit_id: Option<String>,
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
}

#[derive(Deserialize)]
struct RateWindow {
    used_percent: Option<f64>,
    window_minutes: Option<u64>,
    resets_at: Option<f64>,
    resets_in_seconds: Option<f64>,
}

#[derive(Deserialize)]
struct SettingsP {
    #[serde(rename = "type")]
    kind: Option<String>,
    thread_settings: Option<ThreadSettings>,
}

#[derive(Deserialize)]
struct ThreadSettings {
    model: Option<String>,
    service_tier: Option<String>,
}

#[derive(Deserialize)]
struct UserMessageP {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct ResponseItemP {
    #[serde(rename = "type")]
    kind: Option<String>,
    role: Option<String>,
    #[serde(default)]
    content: Vec<ContentItem>,
}

#[derive(Deserialize)]
struct ContentItem {
    text: Option<String>,
}

/// 跳过 `<environment_context>` 这类以 `<` 开头的说明块和 AGENTS.md 注入；
/// 带附件的消息只取 `## My request…:` 之后用户自己写的部分。
fn usable_text(s: &str) -> Option<String> {
    let mut t = s.trim_start();
    if t.is_empty() || t.starts_with('<') || t.starts_with("# AGENTS.md instructions") {
        return None;
    }
    if let Some(i) = t.find("## My request") {
        let after = &t[i..];
        t = after
            .find('\n')
            .map_or("", |n| &after[n + 1..])
            .trim_start();
    }
    clean_title(t)
}

struct Sink<'a> {
    entry: &'a mut FileEntry,
    st: CodexState,
    keys: HashMap<u64, usize>,
    fallback_session: String,
}

impl Sink<'_> {
    fn session(&mut self) -> u32 {
        match self.st.session {
            Some(s) => s,
            None => {
                let id = self.fallback_session.clone();
                let s = self.entry.session_idx(&id);
                self.st.session = Some(s);
                s
            }
        }
    }

    fn title_done(&self) -> bool {
        self.st
            .session
            .is_some_and(|s| self.entry.sessions[s as usize].title.is_some())
    }

    fn set_title(&mut self, title: String, ts: i64) {
        let s = self.session();
        self.entry.sessions[s as usize].set_title(title, false, ts);
    }

    fn set_project(&mut self, cwd: Option<String>) {
        let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
            return;
        };
        let s = self.session();
        let meta = &mut self.entry.sessions[s as usize];
        if meta.project.is_none() {
            meta.project = Some(cwd);
        }
    }

    fn session_meta(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<Line<SessionMetaP>>(line) else {
            return false;
        };
        if l.kind != "session_meta" {
            return false;
        }
        let Some(p) = l.payload else { return true };
        if let Some(id) = p.id.filter(|i| !i.is_empty()) {
            self.st.session = Some(self.entry.session_idx(&id));
        }
        if p.forked_from_id.is_some_and(|f| !f.is_empty()) {
            if let Some(ts) = l.timestamp.as_deref().and_then(parse_ts) {
                self.st.replay = true;
                self.st.last_line_ts = ts;
            }
        }
        self.set_project(p.cwd);
        true
    }

    fn turn_context(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<Line<TurnContextP>>(line) else {
            return false;
        };
        if l.kind != "turn_context" {
            return false;
        }
        let Some(p) = l.payload else { return true };
        if let Some(m) = p.model.filter(|m| !m.is_empty()) {
            self.st.model = Some(m);
        }
        self.set_project(p.cwd);
        true
    }

    fn settings(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<Line<SettingsP>>(line) else {
            return false;
        };
        let Some(p) = l.payload else { return true };
        if p.kind.as_deref() != Some("thread_settings_applied") {
            return false;
        }
        if let Some(ts) = p.thread_settings {
            if let Some(m) = ts.model.filter(|m| !m.is_empty()) {
                self.st.model = Some(m);
            }
            self.st.priority = ts.service_tier.as_deref() == Some("priority");
        }
        true
    }

    fn token_count(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<Line<TokenCountP>>(line) else {
            return false;
        };
        let Some(p) = l.payload else { return true };
        if l.kind != "event_msg" || p.kind.as_deref() != Some("token_count") {
            return false;
        }
        let Some(ts) = l.timestamp.as_deref().and_then(parse_ts) else {
            return true;
        };
        let replay = self.st.replay;
        if let Some(rl) = p.rate_limits.filter(|_| !replay) {
            self.quota(ts, rl);
        }
        let Some(info) = p.info else { return true };
        let Some(total) = info.total_token_usage else {
            return true;
        };
        let tot = total.arr();
        let last = info.last_token_usage.map(|u| u.arr());
        let delta = match self.st.prev_total {
            // 同一累计值重复上报
            Some(prev) if prev == tot => return true,
            Some(prev) if tot.iter().zip(prev.iter()).all(|(a, b)| a >= b) => {
                let mut d = [0u64; 5];
                for i in 0..5 {
                    d[i] = tot[i] - prev[i];
                }
                // 差值和这次请求自己的用量对不上：累计值是从别处接过来的（分页续接、fork），以 last 为准
                match last {
                    Some(l) if l != d => l,
                    _ => d,
                }
            }
            // 文件里第一次出现，或累计值变小（重置）：用这一次请求自己的用量
            _ => last.unwrap_or(tot),
        };
        self.st.prev_total = Some(tot);
        if replay {
            return true;
        }
        let model = self
            .st
            .model
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let session = self.session();
        let ts_bytes = ts.to_le_bytes();
        let tot_bytes: Vec<u8> = tot.iter().flat_map(|v| v.to_le_bytes()).collect();
        let rec = Rec {
            ts,
            key: fnv64(&[b"codex", &ts_bytes, &tot_bytes]),
            model: self.entry.model_idx(&model),
            session,
            // input_tokens 含缓存命中部分
            input: delta[0].saturating_sub(delta[1]),
            cache_read: delta[1],
            cache_write_5m: delta[2],
            cache_write_1h: 0,
            // output_tokens 已含推理
            output: delta[3],
            reasoning: delta[4],
            flags: if self.st.priority { FLAG_PRIORITY } else { 0 },
        };
        if !rec.is_empty() {
            self.entry.push_dedup(&mut self.keys, rec);
        }
        true
    }

    fn quota(&mut self, ts: i64, rl: RateLimits) {
        let mut windows = Vec::new();
        for w in [rl.primary, rl.secondary].into_iter().flatten() {
            let (Some(used), Some(minutes)) = (w.used_percent, w.window_minutes) else {
                continue;
            };
            let resets_at = w
                .resets_at
                .map(|s| s as i64)
                .or_else(|| w.resets_in_seconds.map(|s| ts / 1000 + s as i64));
            windows.push(QuotaWin {
                used_percent: used,
                window_minutes: minutes,
                resets_at,
            });
        }
        if windows.is_empty() {
            return;
        }
        if self.entry.quota.as_ref().is_none_or(|q| ts >= q.ts) {
            self.entry.quota = Some(QuotaObs {
                ts,
                limit_id: rl.limit_id,
                windows,
            });
        }
    }

    fn user_message(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<Line<UserMessageP>>(line) else {
            return false;
        };
        let Some(p) = l.payload else { return true };
        if p.kind.as_deref() != Some("user_message") {
            return false;
        }
        if let Some(t) = p.message.as_deref().and_then(usable_text) {
            let ts = l.timestamp.as_deref().and_then(parse_ts).unwrap_or(0);
            self.set_title(t, ts);
        }
        true
    }

    fn response_user(&mut self, line: &[u8]) -> bool {
        let Ok(l) = serde_json::from_slice::<Line<ResponseItemP>>(line) else {
            return false;
        };
        if l.kind != "response_item" {
            return false;
        }
        let Some(p) = l.payload else { return true };
        if p.kind.as_deref() != Some("message") || p.role.as_deref() != Some("user") {
            return false;
        }
        if let Some(t) = p
            .content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .find_map(usable_text)
        {
            let ts = l.timestamp.as_deref().and_then(parse_ts).unwrap_or(0);
            self.set_title(t, ts);
        }
        true
    }
}

impl LineSink for Sink<'_> {
    fn want(&mut self, head: &[u8]) -> bool {
        if self.st.replay {
            if let Some(ts) = find_str_value(head, b"\"timestamp\":\"").and_then(parse_ts) {
                if ts - self.st.last_line_ts > REPLAY_GAP_MS {
                    self.st.replay = false;
                }
                self.st.last_line_ts = self.st.last_line_ts.max(ts);
            }
        }
        contains(head, P_TOKEN_COUNT)
            || contains(head, P_TURN_CONTEXT)
            || contains(head, P_SESSION_META)
            || contains(head, P_SETTINGS)
            || (!self.title_done()
                && (contains(head, P_USER_MESSAGE) || contains(head, P_ROLE_USER)))
    }

    fn line(&mut self, line: &[u8]) -> bool {
        let head = &line[..line.len().min(crate::scan::HEAD)];
        if contains(head, P_TOKEN_COUNT) {
            self.token_count(line)
        } else if contains(head, P_TURN_CONTEXT) {
            self.turn_context(line)
        } else if contains(head, P_SESSION_META) {
            self.session_meta(line)
        } else if contains(head, P_SETTINGS) {
            self.settings(line)
        } else if self.title_done() {
            false
        } else if contains(head, P_USER_MESSAGE) {
            self.user_message(line)
        } else if contains(head, P_ROLE_USER) {
            self.response_user(line)
        } else {
            false
        }
    }
}

/// 从 `offset` 开始解析，结果追加到 `entry`。返回读到的位置和续读状态。
pub(crate) fn parse(
    path: &Path,
    entry: &mut FileEntry,
    offset: u64,
    state: CodexState,
) -> io::Result<(u64, CodexState)> {
    let fallback_session = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let keys = entry.key_index();
    let mut sink = Sink {
        entry,
        st: state,
        keys,
        fallback_session,
    };
    let off = scan_lines(path, offset, &mut sink)?;
    Ok((off, sink.st))
}

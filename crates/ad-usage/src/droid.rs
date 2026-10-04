//! Factory Droid：`~/.factory/sessions/**/<id>.settings.json` + 同名 `<id>.jsonl`
//!
//! 用量只有 settings 里的会话累计值 `tokenUsage`（每次重写），没有逐条记录，也没有模型字段。
//! 按 jsonl 里每条助手消息的时间把累计值分摊开：输入和缓存按“这条消息之前的上下文有多长”加权
//! （遇到 `compaction_state` 重新计），输出和思考按这条消息自身的长度加权，分摊结果加起来等于累计值。
//! `inputTokens` 不含缓存；`thinkingTokens` 当作 `outputTokens` 的一部分（两家接口的输出都已含思考）。
//! 模型：settings 里有 `model` 就用；否则取第一条用户消息里系统信息的 `Model: …`；
//! 都没有时用 `droid-<provider>`（没有价格）。

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::discover::{self, Ctx, Found, Kind, Source};
use crate::record::{clean_title, parse_time_str, FileEntry, Rec};
use crate::scan::find_str_value;
use crate::Tool;

pub(crate) fn root(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".factory").join("sessions")
}

fn transcript_of(settings: &Path) -> Option<PathBuf> {
    let name = settings.file_name()?.to_str()?;
    let stem = name.strip_suffix(".settings.json")?;
    Some(settings.with_file_name(format!("{stem}.jsonl")))
}

pub(crate) fn discover(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let root = root(ctx);
    let mut src = Source::new(Tool::Droid, root.clone());
    let mut paths = Vec::new();
    discover::walk(
        &root,
        0,
        &|n| n.ends_with(".settings.json"),
        &mut paths,
        &mut src.errors,
    );
    for p in paths {
        let extra: Vec<PathBuf> = transcript_of(&p).into_iter().collect();
        discover::push_with(
            Tool::Droid,
            Kind::Droid,
            p,
            None,
            |p| discover::stat_multi(p, &extra),
            out,
            &mut src.errors,
        );
    }
    src
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Settings {
    token_usage: Option<TokenUsage>,
    model: Option<String>,
    provider_lock: Option<String>,
    api_provider_lock: Option<String>,
    provider_lock_timestamp: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    thinking_tokens: u64,
}

#[derive(Deserialize)]
struct Line {
    title: Option<String>,
    timestamp: Option<String>,
    message: Option<LineMsg>,
}

#[derive(Deserialize)]
struct LineMsg {
    role: Option<String>,
}

/// 一条助手消息：时间、上下文权重、输出权重
struct Turn {
    ts: i64,
    ctx_w: u64,
    out_w: u64,
}

#[derive(Default)]
struct Transcript {
    title: Option<String>,
    model_hint: Option<String>,
    cwd_hint: Option<String>,
    turns: Vec<Turn>,
}

/// 系统信息是 JSON 字符串里的一段文字：取 `Model: ` 之后到换行（`\n` 转义）、引号或 `[` 为止。
fn text_after<'a>(line: &'a [u8], pat: &[u8]) -> Option<&'a str> {
    let at = memchr::memmem::find(line, pat)? + pat.len();
    let rest = &line[at..];
    let end = rest
        .iter()
        .position(|b| matches!(b, b'\\' | b'"' | b'[' | b'*'))
        .unwrap_or(rest.len());
    let s = std::str::from_utf8(&rest[..end]).ok()?.trim();
    (!s.is_empty()).then_some(s)
}

/// `Claude Sonnet 4.5` → `claude-sonnet-4-5`；`custom:Foo-[Anthropic]-0` → `foo-0`
fn normalize_model(raw: &str) -> Option<String> {
    let s = raw.trim();
    let s = s.strip_prefix("custom:").unwrap_or(s);
    let mut cleaned = String::with_capacity(s.len());
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            c if c.is_whitespace() => cleaned.push('-'),
            c => cleaned.extend(c.to_lowercase()),
        }
    }
    // Anthropic 的模型 id 用连字符分隔版本号
    if cleaned.starts_with("claude") {
        cleaned = cleaned.replace('.', "-");
    }
    let mut out = String::with_capacity(cleaned.len());
    for c in cleaned.chars() {
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    let out = out.trim_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

fn read_transcript(path: &Path) -> io::Result<Transcript> {
    let mut t = Transcript::default();
    let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut buf = Vec::new();
    // 这条消息之前的上下文长度（字节）
    let mut ctx_bytes: u64 = 0;
    let mut user_lines = 0;
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
        let len = line.len() as u64;
        let kind = find_str_value(&line[..line.len().min(64)], b"\"type\":\"");
        match kind {
            Some("message") => {
                if let Ok(l) = serde_json::from_slice::<Line>(line) {
                    let role = l.message.and_then(|m| m.role);
                    match role.as_deref() {
                        Some("assistant") => {
                            if let Some(ts) = l.timestamp.as_deref().and_then(parse_time_str) {
                                t.turns.push(Turn {
                                    ts,
                                    ctx_w: ctx_bytes.max(1),
                                    out_w: len.max(1),
                                });
                            }
                        }
                        Some("user") if user_lines < 3 => {
                            user_lines += 1;
                            if t.model_hint.is_none() {
                                t.model_hint =
                                    text_after(line, b"Model: ").and_then(normalize_model);
                            }
                            if t.cwd_hint.is_none() {
                                t.cwd_hint = text_after(line, b"Current folder: ")
                                    .filter(|s| s.starts_with('/'))
                                    .map(str::to_string);
                            }
                        }
                        _ => {}
                    }
                }
                ctx_bytes += len;
            }
            Some("session_start") => {
                if let Ok(l) = serde_json::from_slice::<Line>(line) {
                    t.title = l.title.as_deref().and_then(clean_title);
                }
                ctx_bytes += len;
            }
            // 压缩后上下文从摘要重新开始
            Some("compaction_state") => ctx_bytes = len,
            _ => ctx_bytes += len,
        }
    }
    Ok(t)
}

/// 按权重把 `total` 分成若干份，结果之和正好等于 `total`。
fn apportion(total: u64, weights: &[u64]) -> Vec<u64> {
    let sum: u128 = weights.iter().map(|w| *w as u128).sum();
    if sum == 0 {
        return vec![0; weights.len()];
    }
    let mut out = Vec::with_capacity(weights.len());
    let mut acc: u128 = 0;
    let mut prev: u128 = 0;
    for w in weights {
        acc += *w as u128;
        let upto = total as u128 * acc / sum;
        out.push((upto - prev) as u64);
        prev = upto;
    }
    out
}

pub(crate) fn parse(path: &Path, entry: &mut FileEntry) -> io::Result<()> {
    let settings: Settings = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let session_id = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".settings.json"))
        .unwrap_or_default()
        .to_string();
    let transcript = match transcript_of(path) {
        Some(p) if p.is_file() => read_transcript(&p)?,
        _ => Transcript::default(),
    };
    let sidx = entry.session_idx(&session_id);
    {
        let s = &mut entry.sessions[sidx as usize];
        s.project = transcript.cwd_hint.clone();
        if let Some(t) = transcript.title.clone() {
            s.set_title(t, false, 0);
        }
    }
    let Some(u) = settings.token_usage else {
        return Ok(());
    };
    let provider = settings
        .provider_lock
        .as_deref()
        .or(settings.api_provider_lock.as_deref())
        .filter(|p| !p.is_empty())
        .unwrap_or("unknown");
    let model = settings
        .model
        .as_deref()
        .and_then(normalize_model)
        .or(transcript.model_hint)
        .unwrap_or_else(|| format!("droid-{provider}"));
    let midx = entry.model_idx(&model);

    let mut turns = transcript.turns;
    if turns.is_empty() {
        // 没有逐条时间：记在会话锁定提供方的那一刻，再不行用文件修改时间
        let ts = settings
            .provider_lock_timestamp
            .as_deref()
            .and_then(parse_time_str)
            .or_else(|| {
                discover::stat(path)
                    .ok()
                    .map(|(_, ns)| ns / 1_000_000)
                    .filter(|ms| *ms > 0)
            });
        let Some(ts) = ts else { return Ok(()) };
        turns.push(Turn {
            ts,
            ctx_w: 1,
            out_w: 1,
        });
    }
    let ctx_w: Vec<u64> = turns.iter().map(|t| t.ctx_w).collect();
    let out_w: Vec<u64> = turns.iter().map(|t| t.out_w).collect();
    let input = apportion(u.input_tokens, &ctx_w);
    let cache_read = apportion(u.cache_read_tokens, &ctx_w);
    let cache_write = apportion(u.cache_creation_tokens, &ctx_w);
    let output = apportion(u.output_tokens, &out_w);
    let thinking = apportion(u.thinking_tokens.min(u.output_tokens), &out_w);
    for (i, t) in turns.iter().enumerate() {
        let rec = Rec {
            ts: t.ts,
            key: 0,
            model: midx,
            session: sidx,
            input: input[i],
            output: output[i],
            cache_read: cache_read[i],
            cache_write_5m: cache_write[i],
            reasoning: thinking[i].min(output[i]),
            ..Default::default()
        };
        if !rec.is_empty() {
            entry.recs.push(rec);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_home() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools/droid-home")
    }

    fn parsed(id: &str) -> FileEntry {
        let p = root(&Ctx::with_vars(&fixture_home(), &[])).join(format!("{id}.settings.json"));
        let mut e = FileEntry::new(Tool::Droid, p.to_string_lossy().into_owned(), 0, 0);
        parse(&p, &mut e).unwrap();
        e
    }

    #[test]
    fn spreads_cumulative_totals_over_turns() {
        let e = parsed("S1");
        assert_eq!(e.sessions[0].title.as_deref(), Some("修测试"));
        assert_eq!(
            e.sessions[0].project.as_deref(),
            Some("/Users/test/droidproj")
        );
        assert_eq!(e.models, vec!["claude-sonnet-4-5"]);
        // 三条助手消息（跨两天），分摊后总数不变
        assert_eq!(e.recs.len(), 3);
        let sum = |f: fn(&Rec) -> u64| e.recs.iter().map(f).sum::<u64>();
        assert_eq!(sum(|r| r.input), 110);
        assert_eq!(sum(|r| r.output), 6659);
        assert_eq!(sum(|r| r.cache_write_5m), 40180);
        assert_eq!(sum(|r| r.cache_read), 169919);
        assert_eq!(sum(|r| r.reasoning), 1274);
        assert!(e.recs.iter().all(|r| r.reasoning <= r.output));
        // 压缩之后的那条上下文变短，分到的缓存读比压缩前最后一条少
        assert!(e.recs[2].cache_read < e.recs[1].cache_read);
        let days: std::collections::BTreeSet<i64> =
            e.recs.iter().map(|r| r.ts / 86_400_000).collect();
        assert_eq!(days.len(), 2);
    }

    #[test]
    fn model_fallbacks_and_missing_parts() {
        // settings 里有 model 字段
        let e = parsed("S2");
        assert_eq!(e.models, vec!["claude-opus-4-5-thinking-0"]);
        // 没有 jsonl：整份记在 providerLockTimestamp；没有模型线索：droid-<provider>
        assert_eq!(e.recs.len(), 1);
        assert_eq!(
            e.recs[0].ts,
            parse_time_str("2026-09-02T09:00:00.000Z").unwrap()
        );
        // 没有 tokenUsage：没有记录
        let e = parsed("S3");
        assert!(e.recs.is_empty());
        assert_eq!(e.sessions[0].title.as_deref(), Some("空会话"));
        let e = parsed("S4");
        assert_eq!(e.models, vec!["droid-openai"]);
        assert_eq!(e.recs.len(), 1);
    }

    #[test]
    fn helpers() {
        assert_eq!(apportion(10, &[1, 1, 1]), vec![3, 3, 4]);
        assert_eq!(apportion(0, &[5, 5]), vec![0, 0]);
        assert_eq!(apportion(7, &[0, 0]), vec![0, 0]);
        assert_eq!(
            normalize_model("Claude Sonnet 4.5").as_deref(),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            normalize_model("gpt-5.1-codex").as_deref(),
            Some("gpt-5.1-codex")
        );
        assert_eq!(normalize_model("  ").as_deref(), None);
        let dir = tempfile::tempdir().unwrap();
        let mut found = Vec::new();
        let src = discover(&Ctx::with_vars(dir.path(), &[]), &mut found);
        assert!(found.is_empty() && src.errors.is_empty());
    }
}

//! 调用本机 `claude` 命令（走用户订阅，haiku 模型，禁用工具）做二次评估。只发元数据。

use crate::util::{human_bytes, now_secs, parse_rfc3339, tilde, DAY};
use crate::{AssessInput, Assessment, Safety};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BATCH: usize = 50;
const TIMEOUT: Duration = Duration::from_secs(180);
const SYSTEM_PROMPT: &str = "你是 macOS 磁盘清理助手，只根据给出的元数据判断，只输出 JSON。";

pub(crate) fn assess_with_ai(items: &[AssessInput]) -> Result<Vec<Assessment>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let home = home_dir();
    let claude = find_claude(&home).ok_or_else(|| {
        anyhow!("没有找到 claude 命令。请先安装 Claude Code 并登录（一般在 ~/.local/bin/claude）")
    })?;
    let mut out = Vec::new();
    for chunk in items.chunks(BATCH) {
        let prompt = build_prompt(&home, chunk, now_secs());
        let raw = run_claude(&claude, &prompt)?;
        let ids: HashSet<&str> = chunk.iter().map(|i| i.id.as_str()).collect();
        let parsed = parse_response(&raw)?;
        out.extend(parsed.into_iter().filter(|a| ids.contains(a.id.as_str())));
    }
    Ok(out)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// GUI 应用拿不到 shell 的 PATH：依次试常见位置，最后用登录 shell 的 `whence -p`
/// （用户可能把 claude alias 成带危险参数的命令，`whence -p` 只找真正的可执行文件）
pub(crate) fn find_claude(home: &Path) -> Option<PathBuf> {
    let candidates = [
        home.join(".local/bin/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".claude/local/claude"),
        home.join(".npm-global/bin/claude"),
    ];
    for c in candidates {
        if is_executable(&c) {
            return Some(c);
        }
    }
    let out = run_with_timeout(
        Command::new("/bin/zsh").args(["-lc", "whence -p claude"]),
        None,
        Duration::from_secs(5),
    )
    .ok()?;
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (p.is_absolute() && is_executable(&p)).then_some(p)
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

struct Output {
    status_ok: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_with_timeout(cmd: &mut Command, stdin: Option<&str>, timeout: Duration) -> Result<Output> {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("无法启动命令")?;
    let writer = stdin.map(|s| {
        let mut pipe = child.stdin.take();
        let data = s.to_string();
        std::thread::spawn(move || {
            if let Some(p) = pipe.as_mut() {
                let _ = p.write_all(data.as_bytes());
            }
            drop(pipe);
        })
    });
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let rd_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(p) = so.as_mut() {
            let _ = p.read_to_end(&mut b);
        }
        b
    });
    let rd_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(p) = se.as_mut() {
            let _ = p.read_to_end(&mut b);
        }
        b
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("命令超时（{} 秒）", timeout.as_secs());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    Ok(Output {
        status_ok: status.success(),
        stdout: rd_out.join().unwrap_or_default(),
        stderr: rd_err.join().unwrap_or_default(),
    })
}

fn run_claude(claude: &Path, prompt: &str) -> Result<String> {
    let mut cmd = Command::new(claude);
    cmd.args([
        "-p",
        "--output-format",
        "json",
        "--model",
        "haiku",
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--disable-slash-commands",
        "--system-prompt",
        SYSTEM_PROMPT,
    ])
    // 在临时目录里跑，避免读到某个项目的 CLAUDE.md
    .current_dir(std::env::temp_dir());
    let out = run_with_timeout(&mut cmd, Some(prompt), TIMEOUT)
        .map_err(|e| anyhow!("调用 claude 失败：{e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status_ok && stdout.trim().is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err: String = err.trim().chars().take(300).collect();
        bail!("claude 命令执行失败：{err}");
    }
    Ok(stdout)
}

fn safety_cn(s: Safety) -> &'static str {
    match s {
        Safety::Safe => "可以放心删",
        Safety::Review => "需要看一眼",
        Safety::Keep => "建议保留",
        Safety::Protected => "受保护，不允许删",
    }
}

pub(crate) fn build_prompt(home: &Path, items: &[AssessInput], now: i64) -> String {
    let mut s = String::new();
    s.push_str(
        "我在帮用户清理 AI 工具（Claude Code、Claude 桌面版、Codex、Gemini、ChatCut 等）在 Mac 上产生的文件，\
想知道哪些可以删、哪些该留。下面每一项只有元数据（没有文件内容）。\n\
判断原则：缓存、可重新下载或重新生成的东西可以删；用户自己的作品、近期还在用的对话、配置和数据库要留；拿不准就给 review。\n\
「对话记录」删了以后那段对话不能再继续，但不影响用量统计。规则初判是「受保护」的一律给 keep。\n\n",
    );
    for (i, it) in items.iter().enumerate() {
        let age = parse_rfc3339(&it.modified)
            .map(|t| ((now - t).max(0) / DAY).to_string())
            .unwrap_or_else(|| "未知".into());
        let project = match (&it.project, it.project_exists) {
            (Some(p), Some(true)) => format!("{}（文件夹还在）", tilde(home, p)),
            (Some(p), Some(false)) => format!("{}（文件夹已不存在）", tilde(home, p)),
            (Some(p), None) => tilde(home, p),
            (None, _) => "无".into(),
        };
        s.push_str(&format!(
            "{n}. id: {id}\n   路径: {path}\n   标签: {label}\n   类别: {cat}\n   所属项目: {project}\n   大小: {size}\n   修改时间: {modified}（{age} 天前）\n   标题: {title}\n   规则初判: {safety}，理由：{reason}\n",
            n = i + 1,
            id = it.id,
            path = tilde(home, &it.path),
            label = it.label,
            cat = it.category,
            size = human_bytes(it.size_bytes),
            modified = it.modified,
            title = it.title.as_deref().unwrap_or("无"),
            safety = safety_cn(it.safety),
            reason = it.reason,
        ));
    }
    s.push_str(
        "\n只输出一个 JSON 数组，不要任何其他文字，也不要代码块标记。每项格式：\n\
{\"id\":\"原样的 id\",\"verdict\":\"delete 或 keep 或 review\",\"reason\":\"40 字以内的中文理由\"}\n\
每个 id 都要有一项。",
    );
    s
}

/// 解析 claude 的输出：兼容 `--output-format json` 的外层对象、``` 围栏和前后多余文字。
pub(crate) fn parse_response(raw: &str) -> Result<Vec<Assessment>> {
    let mut text = raw.trim().to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if let Some(obj) = v.as_object() {
            if obj.get("is_error").and_then(|b| b.as_bool()) == Some(true) {
                let msg = obj
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("未知错误");
                bail!("Claude 返回错误：{msg}");
            }
            if let Some(r) = obj.get("result").and_then(|r| r.as_str()) {
                text = r.trim().to_string();
            }
        } else if v.is_array() {
            return Ok(normalize(&v));
        }
    }
    let text = strip_fences(&text);
    let start = text
        .find('[')
        .ok_or_else(|| anyhow!("AI 的回答里没有找到 JSON 数组"))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| anyhow!("AI 的回答里没有找到 JSON 数组"))?;
    if end <= start {
        bail!("AI 的回答格式不对");
    }
    let v: Value = serde_json::from_str(&text[start..=end])
        .map_err(|e| anyhow!("AI 返回的 JSON 解析失败：{e}"))?;
    Ok(normalize(&v))
}

fn strip_fences(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.split_once('\n').map(|x| x.1).unwrap_or("");
        let rest = rest.trim_end();
        return rest.strip_suffix("```").unwrap_or(rest).trim().to_string();
    }
    t.to_string()
}

fn normalize(v: &Value) -> Vec<Assessment> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|o| {
            let id = match o.get("id")? {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            let verdict = o
                .get("verdict")
                .and_then(|x| x.as_str())
                .unwrap_or("review")
                .trim()
                .to_ascii_lowercase();
            let verdict = match verdict.as_str() {
                "delete" | "remove" | "safe" => "delete",
                "keep" | "protected" => "keep",
                _ => "review",
            }
            .to_string();
            let reason: String = o
                .get("reason")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .chars()
                .take(80)
                .collect();
            Some(Assessment {
                id,
                verdict,
                reason,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outer_json_with_fences() {
        let raw = r#"{"type":"result","subtype":"success","is_error":false,"result":"```json\n[{\"id\":\"a1\",\"verdict\":\"delete\",\"reason\":\"缓存，可重建\"},{\"id\":\"b2\",\"verdict\":\"KEEP\",\"reason\":\"近期在用\"},{\"id\":\"c3\",\"verdict\":\"maybe\",\"reason\":\"\"}]\n```","total_cost_usd":0.001}"#;
        let r = parse_response(raw).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].verdict, "delete");
        assert_eq!(r[1].verdict, "keep");
        assert_eq!(r[2].verdict, "review");
        assert_eq!(r[0].reason, "缓存，可重建");
    }

    #[test]
    fn parse_plain_and_noisy() {
        let r = parse_response(r#"[{"id":"x","verdict":"review","reason":"看看"}]"#).unwrap();
        assert_eq!(r[0].id, "x");
        let r = parse_response(
            "好的，结果如下：\n[{\"id\":\"y\",\"verdict\":\"delete\",\"reason\":\"r\"}]\n以上。",
        )
        .unwrap();
        assert_eq!(r[0].id, "y");
        assert!(parse_response("没有数组").is_err());
        let e = parse_response(r#"{"type":"result","is_error":true,"result":"Not logged in"}"#)
            .unwrap_err();
        assert!(e.to_string().contains("Not logged in"));
    }

    #[test]
    fn prompt_has_metadata_only() {
        let home = Path::new("/Users/a");
        let it = AssessInput {
            id: "abc".into(),
            path: "/Users/a/.codex/cache".into(),
            label: "缓存".into(),
            category: "cache".into(),
            project: Some("/Users/a/gone".into()),
            project_exists: Some(false),
            size_bytes: 3 * 1024 * 1024,
            modified: "2026-09-01T00:00:00+08:00".into(),
            title: None,
            safety: Safety::Safe,
            reason: "缓存，删除后会自动重建".into(),
        };
        let now = parse_rfc3339("2026-10-01T00:00:00+08:00").unwrap();
        let p = build_prompt(home, &[it], now);
        assert!(p.contains("~/.codex/cache"));
        assert!(p.contains("3.0 MB"));
        assert!(p.contains("30 天前"));
        assert!(p.contains("文件夹已不存在"));
        assert!(p.contains("可以放心删"));
    }
}

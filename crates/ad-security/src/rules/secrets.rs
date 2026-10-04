//! 明文密钥：认得出前缀的令牌、私钥块、JWT，以及名字像密钥的变量里的长值。
//!
//! 前缀表和「在 token 边界上匹配」的做法移植自 ThinkWatch（tw-guard 的 redact/rules.rs，
//! MIT）。原则一样：只收前缀明确、长度有下限的；「一长串随机字符」本身不算密钥，
//! 只在打码时宁可多遮。

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

/// 认出来的一种令牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenKind {
    pub id: &'static str,
    /// 中文名，用在标题里
    pub label: &'static str,
}

/// (前缀, 前缀之后至少多少字符, 规则 id, 中文名)
const PREFIXES: &[(&str, usize, &str, &str)] = &[
    ("sk-ant-", 20, "anthropic-api-key", "Anthropic API 密钥"),
    ("sk-proj-", 20, "openai-project-key", "OpenAI 项目密钥"),
    (
        "sk-svcacct-",
        20,
        "openai-service-key",
        "OpenAI 服务账号密钥",
    ),
    ("sk-or-v1-", 30, "openrouter-api-key", "OpenRouter API 密钥"),
    ("ghp_", 30, "github-token", "GitHub 令牌"),
    ("gho_", 30, "github-token", "GitHub 令牌"),
    ("ghs_", 30, "github-token", "GitHub 令牌"),
    ("ghu_", 30, "github-token", "GitHub 令牌"),
    ("ghr_", 30, "github-token", "GitHub 令牌"),
    ("github_pat_", 30, "github-token", "GitHub 令牌"),
    ("xoxb-", 20, "slack-token", "Slack 令牌"),
    ("xoxp-", 20, "slack-token", "Slack 令牌"),
    ("xoxa-", 20, "slack-token", "Slack 令牌"),
    ("xoxr-", 20, "slack-token", "Slack 令牌"),
    ("xoxs-", 20, "slack-token", "Slack 令牌"),
    ("xoxe-", 20, "slack-token", "Slack 令牌"),
    ("AIza", 35, "google-api-key", "Google API 密钥"),
    ("ya29.", 20, "google-oauth-token", "Google 登录令牌"),
    ("glpat-", 20, "gitlab-token", "GitLab 令牌"),
    ("hf_", 30, "huggingface-token", "Hugging Face 令牌"),
    ("sk_live_", 20, "stripe-key", "Stripe 密钥"),
    ("rk_live_", 20, "stripe-key", "Stripe 密钥"),
    ("npm_", 36, "npm-token", "npm 令牌"),
    ("dop_v1_", 40, "digitalocean-token", "DigitalOcean 令牌"),
];

/// `sk-` 开头的老式/兼容服务密钥至少多长。DeepSeek、Moonshot 等兼容 OpenAI 的服务也用
/// `sk-` 前缀，最短的是 `sk-` + 32 位。
const SK_MIN: usize = 35;

fn is_tok(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
}

/// 明显是占位符：xxxx、your-key、example……
pub(crate) fn placeholder(s: &str) -> bool {
    let low = s.to_ascii_lowercase();
    const WORDS: &[&str] = &[
        "xxxx",
        "****",
        "...",
        "…",
        "your",
        "example",
        "placeholder",
        "dummy",
        "redacted",
        "replace",
        "changeme",
        "insert",
        "<",
        ">",
        "fake",
        "sample",
        "here",
        "abcdefgh",
        "12345678",
        "test_key",
        "testkey",
    ];
    if WORDS.iter().any(|w| low.contains(w)) {
        return true;
    }
    // 同一个字符连着 6 个以上
    let b = s.as_bytes();
    let mut run = 1;
    for i in 1..b.len() {
        if b[i] == b[i - 1] {
            run += 1;
            if run >= 6 {
                return true;
            }
        } else {
            run = 1;
        }
    }
    distinct(s) < 8
}

fn distinct(s: &str) -> usize {
    let mut seen = [false; 256];
    let mut n = 0;
    for b in s.bytes() {
        if !seen[b as usize] {
            seen[b as usize] = true;
            n += 1;
        }
    }
    n
}

/// 一个 token 是哪种令牌。
pub(crate) fn classify_token(tok: &str) -> Option<TokenKind> {
    for &(prefix, min_tail, id, label) in PREFIXES {
        if let Some(tail) = tok.strip_prefix(prefix) {
            if tail.len() >= min_tail && !placeholder(tail) {
                return Some(TokenKind { id, label });
            }
            return None;
        }
    }
    // AWS：AKIA/ASIA + 16 位大写字母数字
    for p in ["AKIA", "ASIA"] {
        if let Some(tail) = tok.strip_prefix(p) {
            if tail.len() == 16
                && tail
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                && !placeholder(tail)
            {
                return Some(TokenKind {
                    id: "aws-access-key",
                    label: "AWS 访问密钥",
                });
            }
            return None;
        }
    }
    if let Some(tail) = tok.strip_prefix("sk-") {
        if tok.len() >= SK_MIN
            && tail
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && tail.chars().any(|c| c.is_ascii_digit())
            && tail.chars().any(|c| c.is_ascii_alphabetic())
            && !placeholder(tail)
        {
            return Some(TokenKind {
                id: "openai-api-key",
                label: "OpenAI 或兼容服务的 API 密钥",
            });
        }
    }
    if looks_like_jwt(tok) {
        return Some(TokenKind {
            id: "jwt",
            label: "JWT 登录令牌",
        });
    }
    None
}

/// 把文本切成 token，回调每个 token 和它的区间。反斜杠转义（`\n`、`\uXXXX`）算分隔。
fn for_each_token(text: &str, mut f: impl FnMut(&str, Range<usize>)) {
    let mut start: Option<usize> = None;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            if let Some(s) = start.take() {
                f(&text[s..i], s..i);
            }
            if let Some((_, 'u')) = chars.next() {
                for _ in 0..4 {
                    if chars.next_if(|(_, h)| h.is_ascii_hexdigit()).is_none() {
                        break;
                    }
                }
            }
            continue;
        }
        if is_tok(c) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            f(&text[s..i], s..i);
        }
    }
    if let Some(s) = start {
        f(&text[s..], s..text.len());
    }
}

/// 一处密钥命中。
#[derive(Debug, Clone)]
pub(crate) struct Hit {
    pub kind: TokenKind,
    /// 命中的字节区间（令牌本身，或者私钥块的 BEGIN 那一行）
    pub range: Range<usize>,
}

/// 找出所有认得出前缀的令牌、JWT 和私钥块。
pub(crate) fn find_tokens(text: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    for_each_token(text, |tok, r| {
        // token 两头的点不属于令牌（句号）
        let trimmed = tok.trim_end_matches('.');
        let r = r.start..r.start + trimmed.len();
        if let Some(kind) = classify_token(trimmed) {
            out.push(Hit { kind, range: r });
        }
    });
    private_keys(text, &mut out);
    out
}

/// `-----BEGIN ... PRIVATE KEY-----` 到 END，中间要有足够长的 base64 内容；文档里的示意
/// （中间是省略号、或者只有几个字符）不算。
fn private_keys(text: &str, out: &mut Vec<Hit>) {
    const BEGIN: &str = "-----BEGIN ";
    let mut from = 0;
    while let Some(b) = text[from..].find(BEGIN) {
        let begin = from + b;
        let head = begin + BEGIN.len();
        let Some(h) = text[head..].find("-----") else {
            break;
        };
        let head_end = head + h + 5;
        from = head_end;
        let label = &text[head..head + h];
        if !label.trim_end().ends_with("PRIVATE KEY") || label.len() > 40 {
            continue;
        }
        let Some(e) = text[head_end..].find("-----END ") else {
            continue;
        };
        let body = &text[head_end..head_end + e];
        let b64 = body
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/')
            .count();
        if b64 >= 100 && !body.contains("...") && !body.contains('…') {
            out.push(Hit {
                kind: TokenKind {
                    id: "private-key",
                    label: "私钥",
                },
                range: begin..head_end,
            });
        }
        from = head_end + e;
    }
}

fn is_b64url(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'=')
}

/// 三段 base64url，首段解码后含 `"alg"`。jwt.io 上那个人人都见过的示例不算。
fn looks_like_jwt(tok: &str) -> bool {
    if !tok.starts_with("eyJ") || tok.bytes().filter(|&b| b == b'.').count() != 2 {
        return false;
    }
    let parts: Vec<&str> = tok.split('.').collect();
    if !parts.iter().all(|p| is_b64url(p)) || parts[1].len() < 10 || parts[2].len() < 16 {
        return false;
    }
    if parts[2].starts_with("SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV") {
        return false;
    }
    match b64url_decode(parts[0]) {
        Some(h) => String::from_utf8_lossy(&h).contains("\"alg\""),
        None => false,
    }
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// 名字看起来是放密钥的：按 `_`、`-`、`.` 和驼峰拆成词，有一个词是 KEY / TOKEN / SECRET /
/// PASSWORD 之类。
pub(crate) fn secretish_name(name: &str) -> bool {
    const WORDS: &[&str] = &[
        "KEY",
        "APIKEY",
        "ACCESSKEY",
        "SECRETKEY",
        "PRIVATEKEY",
        "TOKEN",
        "AUTHTOKEN",
        "ACCESSTOKEN",
        "REFRESHTOKEN",
        "SECRET",
        "CLIENTSECRET",
        "PASSWORD",
        "PASSWD",
        "PWD",
        "CREDENTIAL",
        "CREDENTIALS",
        "AUTHORIZATION",
        "BEARER",
        "COOKIE",
    ];
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' || c == '-' || c == '.' || c == ' ' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_uppercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.iter().any(|w| {
        WORDS.contains(&w.as_str())
            || ["SECRET", "TOKEN", "APIKEY", "PASSWORD", "PASSWD"]
                .iter()
                .any(|suffix| w.len() > suffix.len() && w.ends_with(suffix))
    })
}

/// 值看起来是真的密钥：够长、没有空格、不是变量引用、不是路径或网址、不是占位符。
pub(crate) fn looks_secret(value: &str) -> bool {
    let v = value.trim();
    let v = v.strip_prefix("Bearer ").unwrap_or(v).trim();
    if v.len() < 16 || v.len() > 4096 {
        return false;
    }
    if v.chars().any(|c| c.is_whitespace())
        || v.starts_with('$')
        || v.contains("${")
        || v.contains("$(")
        || v.starts_with('%')
        || v.starts_with("{{")
        || v.starts_with('/')
        || v.starts_with("~/")
        || v.starts_with("./")
        || v.starts_with("op://")
        || v.contains("://")
    {
        return false;
    }
    let has_digit = v.chars().any(|c| c.is_ascii_digit());
    let has_alpha = v.chars().any(|c| c.is_ascii_alphabetic());
    if !(has_digit && has_alpha) {
        return false;
    }
    !placeholder(v)
}

/// 打码：前 4 个字符 + 「…」。
pub(crate) fn mask_value(s: &str) -> String {
    let head: String = s.chars().take(4).collect();
    format!("{head}…")
}

static ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)([A-Za-z0-9_\-\.]*(?:key|token|secret|passw(?:or)?d|pwd|credential|authorization|bearer|cookie)[A-Za-z0-9_\-]*)["']?\s*(?:[:=]|=>)\s*["']?(?:Bearer\s+)?([^\s"',;}{)(\]\[<>`]{6,})"#,
    )
    .unwrap()
});

static URL_CRED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z][A-Za-z0-9+.\-]*://[^\s:/@]+:([^\s@/]{3,})@").unwrap());

static FLAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)--?[a-z0-9\-]*(?:key|token|secret|password)[a-z0-9\-]*[ =]([^\s\-][^\s]{5,})")
        .unwrap()
});

/// 一段文字里要打码的区间（令牌、名字像密钥的变量的值、网址里的口令、长随机串）。
pub(crate) fn mask_ranges(s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for_each_token(s, |tok, r| {
        if classify_token(tok.trim_end_matches('.')).is_some() || random_looking(tok) {
            out.push(r);
        }
    });
    for re in [&*ASSIGN_RE, &*URL_CRED_RE, &*FLAG_RE] {
        for c in re.captures_iter(s) {
            let m = c.get(c.len() - 1).unwrap();
            let v = m.as_str();
            if v.starts_with('$') || v.len() < 6 {
                continue;
            }
            // 值本身是文件路径或常见的非密钥词就不遮
            if v.starts_with('/') || v.starts_with("~/") || v == "true" || v == "false" {
                continue;
            }
            out.push(m.range());
        }
    }
    // 私钥块的正文行
    if s.len() >= 60
        && s.trim()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    {
        let t = s.trim();
        let start = s.find(t).unwrap_or(0);
        out.push(start..start + t.len());
    }
    out.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in out {
        match merged.last_mut() {
            Some(last) if r.start < last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// 长而随机的一串：打码时宁可多遮。
fn random_looking(tok: &str) -> bool {
    tok.len() >= 28
        && tok.chars().any(|c| c.is_ascii_digit())
        && tok.chars().any(|c| c.is_ascii_alphabetic())
        && distinct(tok) >= 14
        && !tok.contains("..")
        && tok.matches('.').count() != 1
        && !tok.contains('/')
}

/// 名字像密钥的赋值（`API_KEY = "…"`、`apiKey: '…'`、`export TOKEN=…`）。
#[derive(Debug, Clone)]
pub(crate) struct Assignment {
    pub name: String,
    pub value_range: Range<usize>,
}

static ASSIGN_FIND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)(?:^|[\s{,;(])(?:export\s+)?["']?([A-Za-z0-9_\-]*(?i:key|token|secret|passw(?:or)?d|pwd|credential|authorization|bearer|cookie)[A-Za-z0-9_\-]*)["']?\s*(?:=|:|=>)\s*(["']?)([A-Za-z0-9_\-\.+/=:]{16,})(["']?)"#,
    )
    .unwrap()
});

/// 设计上就公开的密钥（前端埋点用的 PostHog 项目密钥、Stripe 可发布密钥），不算泄露。
fn public_by_design(v: &str) -> bool {
    ["phc_", "pk_live_", "pk_test_", "sk_test_", "rk_test_"]
        .iter()
        .any(|p| v.starts_with(p))
}

/// 找出名字像密钥、值像真密钥的赋值。`quoted_only` 为真时只认带引号的值（代码文件里
/// `token = get_token()` 这类不是字面量）。
pub(crate) fn find_assignments(text: &str, quoted_only: bool) -> Vec<Assignment> {
    let mut out = Vec::new();
    for c in ASSIGN_FIND_RE.captures_iter(text) {
        let name = c.get(1).unwrap().as_str();
        let open = c.get(2).map(|m| m.as_str()).unwrap_or("");
        let close = c.get(4).map(|m| m.as_str()).unwrap_or("");
        let value = c.get(3).unwrap();
        if quoted_only && (open.is_empty() || open != close) {
            continue;
        }
        if !secretish_name(name)
            || !looks_secret(value.as_str())
            || public_by_design(value.as_str())
        {
            continue;
        }
        // 值后面紧跟 ( 或 . 调用，说明是表达式
        let after = &text[value.end()..];
        if open.is_empty() && (after.starts_with('(') || after.starts_with('[')) {
            continue;
        }
        out.push(Assignment {
            name: name.to_string(),
            value_range: value.range(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_ANT: &str = "sk-ant-api03-Zq8vN2kLmP4xR7tYbC1dF9gH3jK6wS0eU5iO";

    #[test]
    fn prefixes_are_recognized() {
        assert_eq!(classify_token(FAKE_ANT).unwrap().id, "anthropic-api-key");
        assert_eq!(
            classify_token("ghp_Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0UvWx")
                .unwrap()
                .id,
            "github-token"
        );
        assert_eq!(
            classify_token("AKIAZ7Q3M9X2LK4PB8RT").unwrap().id,
            "aws-access-key"
        );
        assert!(classify_token("sk-ant-xxxxxxxxxxxxxxxxxxxxxxxxxxxx").is_none());
        assert!(classify_token("sk-your-api-key-here-0000000000000000").is_none());
        assert!(classify_token("sk-learn").is_none());
    }

    #[test]
    fn names_and_values() {
        assert!(secretish_name("OPENAI_API_KEY"));
        assert!(secretish_name("apiKey"));
        assert!(secretish_name("x-api-key"));
        assert!(secretish_name("GITHUB_TOKEN"));
        assert!(!secretish_name("MAX_TOKENS"));
        assert!(!secretish_name("MONKEY_PATH"));
        assert!(!secretish_name("KEYCHAIN_NAME"));
        assert!(secretish_name("appsecret"));
        assert!(secretish_name("tmdb_bearer_token"));
        assert!(!secretish_name("key_path") || !looks_secret("~/.ssh/id_ed25519"));
        assert!(looks_secret("a8f3k29dk3m4n5b6v7c8"));
        assert!(!looks_secret("${OPENAI_API_KEY}"));
        assert!(!looks_secret("your-api-key-goes-here-123"));
        assert!(!looks_secret("/Users/me/.ssh/id_rsa"));
    }

    #[test]
    fn masking_covers_tokens_and_assignments() {
        let s = format!("export OPENROUTER_API_KEY=\"{FAKE_ANT}\" && PASSWORD=hunter2hunter2");
        let r = mask_ranges(&s);
        let covered = |needle: &str| {
            let at = s.find(needle).unwrap();
            r.iter()
                .any(|m| m.start <= at && at + needle.len() <= m.end)
        };
        assert!(covered(&FAKE_ANT[8..]));
        assert!(covered("hunter2hunter2"));
    }

    #[test]
    fn assignments() {
        let t = "API_KEY = \"k29dk3m4n5b6v7c8x9z0\"\ntoken = response.json()[\"access_token\"]\n";
        let a = find_assignments(t, true);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "API_KEY");
    }
}

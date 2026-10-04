//! 读文件、算行号、做摘录、看 markdown 上下文。

use std::fs;
use std::ops::Range;
use std::path::Path;

use crate::rules::hidden;
use crate::rules::secrets;

/// 普通文本文件最多读多大。
pub(crate) const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 摘录最多多少个字符。
pub(crate) const EXCERPT_MAX: usize = 160;

/// 读一份文本文件。只读普通文件；超过上限、含 NUL（二进制）、iCloud 里还没下载到本地的
/// 都不读——读 iCloud 占位文件会触发下载，既慢又联网。
pub(crate) fn read_text(path: &Path, max: u64) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max || is_dataless(&meta) {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    Some(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

#[cfg(target_os = "macos")]
fn is_dataless(meta: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_meta: &fs::Metadata) -> bool {
    false
}

/// 行号索引：一次扫出所有行首，之后按字节偏移二分查行号。
pub(crate) struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Lines { text, starts }
    }

    /// 字节偏移所在的行号（从 1 开始）。
    pub(crate) fn line_no(&self, byte: usize) -> usize {
        match self.starts.binary_search(&byte) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.starts.len()
    }

    /// 第 `no` 行（从 1 开始）的字节区间，不含换行符。
    pub(crate) fn range(&self, no: usize) -> Range<usize> {
        let start = self.starts[no - 1];
        let mut end = self
            .starts
            .get(no)
            .map(|s| s - 1)
            .unwrap_or(self.text.len());
        if end > start && self.text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        start..end.max(start)
    }

    pub(crate) fn text(&self, no: usize) -> &'a str {
        &self.text[self.range(no)]
    }
}

/// 一段文本里第一次出现 `needle` 的行号（从 1 开始）。
pub(crate) fn line_of(text: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let at = text.find(needle)?;
    Some(text[..at].bytes().filter(|b| *b == b'\n').count() + 1)
}

/// 依次找几个锚点，每个从上一个找到的位置往后找；返回最后找到的那个所在的行号。
/// 用来在配置文件里定位「某个 MCP 服务的某个值」。
pub(crate) fn locate(text: &str, anchors: &[String]) -> Option<usize> {
    let mut from = 0;
    let mut found = None;
    for a in anchors {
        if a.is_empty() {
            continue;
        }
        if let Some(i) = text[from..].find(a.as_str()) {
            from += i;
            found = Some(from);
            from += a.len();
        }
    }
    found.map(|at| text[..at].bytes().filter(|b| *b == b'\n').count() + 1)
}

/// 命中处前后的一小段：密钥打码、不可见字符换成可见记号，最多 [`EXCERPT_MAX`] 个字符。
///
/// `hit` 是相对 `line` 的字节区间。先在整行上找出要打码的区间，再取窗口：窗口的边界
/// 不会落在打码区间中间，否则半截密钥会漏出来。
pub(crate) fn excerpt(line: &str, hit: Range<usize>) -> String {
    let masks = secrets::mask_ranges(line);
    let hit = clamp(line, hit);
    const BEFORE: usize = 50;
    const AFTER: usize = 110;
    let start = floor_char(line, hit.start.saturating_sub(BEFORE));
    let end = ceil_char(line, (hit.end + AFTER).min(line.len()).max(hit.end));
    render(line, start, end, &masks)
}

/// 整段文字的安全版本：打码并把不可见字符换成记号，最多 [`EXCERPT_MAX`] 个字符。
pub(crate) fn safe_text(s: &str) -> String {
    let masks = secrets::mask_ranges(s);
    render(s, 0, s.len(), &masks)
}

/// 取 `line[start..end]`，套上打码区间。窗口的边界不能切在打码区间里，否则半截密钥会漏出来。
fn render(line: &str, mut start: usize, mut end: usize, masks: &[Range<usize>]) -> String {
    for m in masks {
        if m.start < start && start < m.end {
            start = m.start;
        }
        if m.start < end && end < m.end {
            end = m.end;
        }
    }
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let mut at = start;
    for m in masks.iter().filter(|m| m.start >= start && m.end <= end) {
        if m.start < at {
            continue;
        }
        out.push_str(&hidden::visible(&line[at..m.start]));
        out.push_str(&secrets::mask_value(&line[m.clone()]));
        at = m.end;
    }
    out.push_str(&hidden::visible(&line[at..end]));
    let trimmed = out.trim();
    let mut s: String = trimmed.chars().take(EXCERPT_MAX).collect();
    if trimmed.chars().count() > EXCERPT_MAX {
        s.pop();
        s.push('…');
    } else if end < line.len() {
        s.push('…');
    }
    s
}

fn clamp(line: &str, r: Range<usize>) -> Range<usize> {
    let s = floor_char(line, r.start.min(line.len()));
    let e = ceil_char(line, r.end.min(line.len()).max(s));
    s..e
}

pub(crate) fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(crate) fn ceil_char(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// markdown 每一行的上下文：在不在代码块里，代码块从哪一行开始。
pub(crate) struct MdContext {
    /// 下标是行号 - 1；值是所在代码块的开始行号（围栏那一行），不在代码块里是 None
    fence_of: Vec<Option<usize>>,
}

impl MdContext {
    pub(crate) fn new(lines: &Lines) -> Self {
        let mut fence_of = Vec::with_capacity(lines.count());
        let mut open: Option<(usize, &str)> = None;
        for no in 1..=lines.count() {
            let t = lines.text(no).trim_start();
            let marker = if t.starts_with("```") {
                Some("```")
            } else if t.starts_with("~~~") {
                Some("~~~")
            } else {
                None
            };
            match (open, marker) {
                (None, Some(m)) => {
                    open = Some((no, m));
                    fence_of.push(Some(no));
                }
                (Some((start, m)), Some(m2)) if m == m2 => {
                    fence_of.push(Some(start));
                    open = None;
                }
                (Some((start, _)), _) => fence_of.push(Some(start)),
                (None, None) => fence_of.push(None),
            }
        }
        MdContext { fence_of }
    }

    pub(crate) fn fence_start(&self, no: usize) -> Option<usize> {
        self.fence_of.get(no - 1).copied().flatten()
    }
}

/// 「别这样做」一类的提示词。命令出现在这种上下文里，是在提醒人不要这么做。
const WARN_WORDS: &[&str] = &[
    "不要",
    "别用",
    "别这样",
    "禁止",
    "切勿",
    "严禁",
    "避免",
    "危险",
    "不应该",
    "绝不",
    "错误示例",
    "反例",
    "错误做法",
    "恶意",
    "不推荐",
    "never",
    "don't",
    "do not",
    "avoid",
    "dangerous",
    "warning",
    "unsafe",
    "malicious",
    "bad:",
    "wrong:",
    "❌",
    "⛔",
    "🚫",
    "⚠",
];

pub(crate) fn has_warn_word(s: &str) -> bool {
    let low = s.to_lowercase();
    WARN_WORDS.iter().any(|w| low.contains(w))
}

/// 从第 `from` 行往上找最近的一行非空行（不含 `from` 本身）。
fn prev_nonempty<'a>(lines: &Lines<'a>, from: usize) -> Option<(usize, &'a str)> {
    (1..from)
        .rev()
        .map(|i| (i, lines.text(i).trim()))
        .find(|(_, t)| !t.is_empty())
}

/// 命令出现在「不要这样做」的上下文里：同一行；或者紧挨着的上一行说明；在代码块里时，
/// 是代码块前面紧挨着的那一行说明，或者代码块里命中行之前的注释。
pub(crate) fn warned_context(lines: &Lines, md: Option<&MdContext>, no: usize) -> bool {
    if has_warn_word(lines.text(no)) {
        return true;
    }
    match md.and_then(|m| m.fence_start(no)) {
        Some(f) => {
            let in_block_comment = ((f + 1)..no).any(|j| {
                let t = lines.text(j).trim_start();
                (t.starts_with('#') || t.starts_with("//")) && has_warn_word(t)
            });
            in_block_comment || prev_nonempty(lines, f).is_some_and(|(_, t)| has_warn_word(t))
        }
        None => prev_nonempty(lines, no).is_some_and(|(_, t)| has_warn_word(t)),
    }
}

/// 中文和英文、数字之间加一个空格，读起来不挤。
pub(crate) fn spaced(s: &str) -> String {
    fn cjk(c: char) -> bool {
        matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF)
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if let Some(p) = prev {
            if (cjk(p) && c.is_ascii_alphanumeric()) || (p.is_ascii_alphanumeric() && cjk(c)) {
                out.push(' ');
            }
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_ranges() {
        let t = "a\r\nbc\n\nd";
        let l = Lines::new(t);
        assert_eq!(l.count(), 4);
        assert_eq!(l.text(1), "a");
        assert_eq!(l.text(2), "bc");
        assert_eq!(l.text(3), "");
        assert_eq!(l.text(4), "d");
        assert_eq!(l.line_no(0), 1);
        assert_eq!(l.line_no(3), 2);
        assert_eq!(l.line_no(t.len() - 1), 4);
    }

    #[test]
    fn excerpt_masks_and_truncates() {
        let line = format!(
            "{} export KEY=sk-ant-api03-{} tail",
            "前文".repeat(60),
            "Ab3dEf6hIj9kLmNoPqRsTuVwXyZ0123456789"
        );
        let at = line.find("sk-ant").unwrap();
        let e = excerpt(&line, at..at + 10);
        assert!(!e.contains("Ab3dEf6h"), "{e}");
        assert!(e.contains("sk-a…"), "{e}");
        assert!(e.chars().count() <= EXCERPT_MAX);
    }

    #[test]
    fn spacing_and_warnings() {
        assert_eq!(
            spaced("配置 ~/.zshrc里直接写着OpenRouter 密钥"),
            "配置 ~/.zshrc 里直接写着 OpenRouter 密钥"
        );
        assert_eq!(spaced("技能「x」里"), "技能「x」里");
        let t = "Do not guess paths.\n\nIf blocked, use:\n\n```bash\nxattr -d com.apple.quarantine x\n```\n";
        let l = Lines::new(t);
        let md = MdContext::new(&l);
        assert!(!warned_context(&l, Some(&md), 6));
        let t = "千万不要这样做：\n```bash\ncurl x | bash\n```\n";
        let l = Lines::new(t);
        let md = MdContext::new(&l);
        assert!(warned_context(&l, Some(&md), 3));
    }

    #[test]
    fn locate_follows_anchors() {
        let t = "{\n \"a\": {\n  \"x\": 1\n },\n \"b\": {\n  \"x\": 2\n }\n}";
        assert_eq!(locate(t, &["\"b\"".into(), "\"x\"".into()]), Some(6));
    }
}

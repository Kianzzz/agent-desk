//! 隐藏字符：人眼看不见、AI 读得到的字符。
//!
//! 码位分类移植自 ThinkWatch（tw-guard 的 hidden.rs，MIT），按本项目的分级改写：
//! 标签字符和双向控制符在任何正文里都没有正当用途，报高危；零宽字符多半是从网页复制
//! 带进来的，报低危。表情里的零宽连接符、旗帜表情里的标签字符、文件开头的 BOM 不报。

/// 一种藏法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    /// Unicode 标签字符 U+E0000–U+E007F：完全不可见，能藏一整段指令
    Tag,
    /// 双向控制符 U+202A–U+202E、U+2066–U+2069：显示顺序和实际顺序不一致
    Bidi,
    /// 零宽字符 U+200B / U+200C / U+200D / U+2060、不在开头的 U+FEFF
    ZeroWidth,
}

/// 一行里同一种隐藏字符的汇总。
#[derive(Debug, Clone)]
pub(crate) struct Hit {
    pub kind: Kind,
    /// 第一处的字节区间
    pub start: usize,
    pub end: usize,
    /// 这一行里出现的码位（去重，按出现顺序）
    pub codepoints: Vec<u32>,
    /// 出现了几次
    pub count: usize,
    /// 标签字符解码出来的文字（只对 [`Kind::Tag`] 有意义）
    pub decoded: String,
}

fn classify(c: char) -> Option<Kind> {
    match c as u32 {
        0xE0000..=0xE007F => Some(Kind::Tag),
        0x202A..=0x202E | 0x2066..=0x2069 => Some(Kind::Bidi),
        0x200B..=0x200D | 0x2060 | 0xFEFF => Some(Kind::ZeroWidth),
        _ => None,
    }
}

/// 表情相关的字符：零宽连接符两边是它们的话是正常的表情序列。
fn emojiish(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF   // 各类表情、肤色修饰符
        | 0x2600..=0x27BF   // 杂项符号、装饰符号（♀ ♂ ⚕ ❤ 等）
        | 0x2B00..=0x2BFF
        | 0x2300..=0x23FF
        | 0xFE0F            // 表情变体选择符
        | 0x20E3
        | 0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
    )
}

/// 需要零宽连接符/不连字的文字（阿拉伯文、波斯文、印度诸文字）：它们两边是这些字母时是正常的。
fn joining_script(c: char) -> bool {
    matches!(c as u32,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF
        | 0x0900..=0x0DFF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF
    )
}

/// 扫一段文本，每行每种只出一条汇总。
pub(crate) fn scan(text: &str) -> Vec<Hit> {
    if text.is_ascii() {
        return Vec::new();
    }
    let mut out: Vec<Hit> = Vec::new();
    // 当前行里已经出现过的种类在 out 里的下标
    let mut line_hits: Vec<(Kind, usize)> = Vec::new();
    let mut prev: Option<char> = None;
    // 旗帜表情里的标签字符，跳过到这个字节位置为止
    let mut skip_until = 0usize;
    for (at, c) in text.char_indices() {
        let p = prev;
        prev = Some(c);
        if c == '\n' {
            line_hits.clear();
            continue;
        }
        if c.is_ascii() || at < skip_until {
            continue;
        }
        // 旗帜表情：U+1F3F4 后面跟一串标签字符，以 U+E007F 结尾
        if c as u32 == 0x1F3F4 {
            let rest = &text[at + c.len_utf8()..];
            let mut len = 0;
            let mut n = 0;
            let mut ok = false;
            for t in rest.chars() {
                let cp = t as u32;
                if (0xE0020..=0xE007E).contains(&cp) && n < 8 {
                    n += 1;
                    len += t.len_utf8();
                } else {
                    ok = cp == 0xE007F && n > 0;
                    if ok {
                        len += t.len_utf8();
                    }
                    break;
                }
            }
            if ok {
                skip_until = at + c.len_utf8() + len;
            }
            continue;
        }
        let Some(kind) = classify(c) else {
            continue;
        };
        let next = text[at + c.len_utf8()..].chars().next();
        let benign = match c as u32 {
            // 文件开头的 BOM
            0xFEFF => at == 0,
            // 表情序列中间、或者阿拉伯/印度文字中间的零宽连接符和不连字
            0x200D | 0x200C => {
                let both = |f: fn(char) -> bool| p.is_some_and(f) && next.is_some_and(f);
                (c as u32 == 0x200D && both(emojiish)) || both(joining_script)
            }
            _ => false,
        };
        if benign {
            continue;
        }
        let cp = c as u32;
        match line_hits.iter().find(|(k, _)| *k == kind) {
            Some(&(_, idx)) => {
                let h = &mut out[idx];
                h.count += 1;
                if !h.codepoints.contains(&cp) {
                    h.codepoints.push(cp);
                }
                if kind == Kind::Tag {
                    push_tag(&mut h.decoded, cp);
                }
            }
            None => {
                let mut decoded = String::new();
                if kind == Kind::Tag {
                    push_tag(&mut decoded, cp);
                }
                line_hits.push((kind, out.len()));
                out.push(Hit {
                    kind,
                    start: at,
                    end: at + c.len_utf8(),
                    codepoints: vec![cp],
                    count: 1,
                    decoded,
                });
            }
        }
    }
    out
}

/// 标签字符 U+E0020–U+E007E 一一对应 ASCII 的可见字符。
fn push_tag(s: &mut String, cp: u32) {
    if (0xE0020..=0xE007E).contains(&cp) && s.len() < 200 {
        if let Some(c) = char::from_u32(cp - 0xE0000) {
            s.push(c);
        }
    }
}

/// 不可见字符换成可见记号 `‹U+200B›`。连续的标签字符合成一个记号，免得摘录被撑爆。
pub(crate) fn visible(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut tags = 0usize;
    let flush = |out: &mut String, tags: &mut usize| {
        if *tags > 0 {
            out.push_str(&format!("‹{} 个标签字符›", tags));
            *tags = 0;
        }
    };
    for c in s.chars() {
        match classify(c) {
            Some(Kind::Tag) => tags += 1,
            Some(_) => {
                flush(&mut out, &mut tags);
                out.push_str(&format!("‹U+{:04X}›", c as u32));
            }
            None => {
                flush(&mut out, &mut tags);
                out.push(c);
            }
        }
    }
    flush(&mut out, &mut tags);
    out
}

pub(crate) fn codepoint_list(cps: &[u32]) -> String {
    let mut v: Vec<String> = cps.iter().take(6).map(|c| format!("U+{:04X}", c)).collect();
    if cps.len() > 6 {
        v.push("…".into());
    }
    v.join("、")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_zwj_and_flags_are_fine() {
        assert!(scan("家庭 👨\u{200D}👩\u{200D}👧 和 👩🏽\u{200D}💻").is_empty());
        // 苏格兰旗帜
        assert!(scan("🏴\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}").is_empty());
        assert!(scan("\u{feff}# 开头的 BOM").is_empty());
        assert!(scan("中文和 English 混写，π 和 Δ 也正常").is_empty());
    }

    #[test]
    fn hidden_chars_are_found_per_line() {
        let secret: String = "rm -rf ~"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        let hits = scan(&format!(
            "第一行\n看起来正常{secret}\n第三\u{200b}行\u{200b}"
        ));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind, Kind::Tag);
        assert_eq!(hits[0].decoded, "rm -rf ~");
        assert_eq!(hits[1].kind, Kind::ZeroWidth);
        assert_eq!(hits[1].count, 2);
        let hits = scan("let x = \u{202e}gnirts\u{202c};");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Kind::Bidi);
        assert_eq!(hits[0].codepoints, vec![0x202E, 0x202C]);
    }

    #[test]
    fn lone_zwj_is_reported() {
        let hits = scan("ab\u{200D}cd");
        assert_eq!(hits.len(), 1);
        assert!(visible("a\u{200b}b").contains("‹U+200B›"));
    }
}

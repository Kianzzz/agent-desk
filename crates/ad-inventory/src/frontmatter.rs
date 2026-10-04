//! SKILL.md 的 YAML frontmatter 解析（name / description）。

use yaml_rust2::{Yaml, YamlLoader};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// 取出 `---` 包住的头部文本。
fn split_block(text: &str) -> Option<&str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let start = first.len();
    let mut offset = start;
    for line in lines {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            return Some(&text[start..offset]);
        }
        offset += line.len();
    }
    None
}

fn yaml_scalar(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Real(r) => Some(r.clone()),
        Yaml::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

pub(crate) fn parse(text: &str) -> Frontmatter {
    let Some(block) = split_block(text) else {
        return Frontmatter::default();
    };
    if let Ok(docs) = YamlLoader::load_from_str(block) {
        if let Some(Yaml::Hash(h)) = docs.first() {
            let get = |k: &str| {
                h.get(&Yaml::String(k.to_string()))
                    .and_then(yaml_scalar)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            };
            return Frontmatter {
                name: get("name"),
                description: get("description"),
            };
        }
    }
    fallback(block)
}

/// YAML 不合法时的宽松解析：逐行找 `key: value`，支持 `|` / `>` 多行块和引号。
fn fallback(block: &str) -> Frontmatter {
    let lines: Vec<&str> = block.lines().collect();
    let mut out = Frontmatter::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim();
        if key != "name" && key != "description" {
            continue;
        }
        let v = v.trim();
        let value = if v.starts_with('|') || v.starts_with('>') {
            let folded = v.starts_with('>');
            let mut parts = Vec::new();
            while i < lines.len()
                && (lines[i].starts_with(' ')
                    || lines[i].starts_with('\t')
                    || lines[i].trim().is_empty())
            {
                parts.push(lines[i].trim());
                i += 1;
            }
            if folded {
                parts
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                parts.join("\n").trim().to_string()
            }
        } else {
            let mut s = v.to_string();
            // 多行的普通标量：后续缩进行接在后面
            while i < lines.len() && (lines[i].starts_with(' ') || lines[i].starts_with('\t')) {
                s.push(' ');
                s.push_str(lines[i].trim());
                i += 1;
            }
            unquote(&s)
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        if key == "name" {
            out.name = Some(value);
        } else {
            out.description = Some(value);
        }
    }
    out
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' && b[s.len() - 1] == b'"') || (b[0] == b'\'' && b[s.len() - 1] == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain() {
        let f = parse("---\nname: foo\ndescription: 一句话描述\n---\n# body\n");
        assert_eq!(f.name.as_deref(), Some("foo"));
        assert_eq!(f.description.as_deref(), Some("一句话描述"));
    }

    #[test]
    fn literal_block() {
        let f = parse("---\nname: foo\ndescription: |\n  第一行\n  第二行\nlicense: MIT\n---\n");
        assert_eq!(f.description.as_deref(), Some("第一行\n第二行"));
    }

    #[test]
    fn folded_block() {
        let f = parse("---\nname: foo\ndescription: >\n  Use this when\n  you need it.\n---\n");
        assert_eq!(f.description.as_deref(), Some("Use this when you need it."));
    }

    #[test]
    fn quoted() {
        let f = parse("---\nname: 'foo'\ndescription: \"带: 冒号的描述\"\n---\n");
        assert_eq!(f.name.as_deref(), Some("foo"));
        assert_eq!(f.description.as_deref(), Some("带: 冒号的描述"));
    }

    #[test]
    fn invalid_yaml_falls_back() {
        // 未加引号的 `: ` 让 YAML 解析失败
        let f = parse("---\nname: foo\ndescription: 用法: 做这个: 再做那个\n  [不闭合\n---\n");
        assert_eq!(f.name.as_deref(), Some("foo"));
        assert!(f.description.unwrap().starts_with("用法: 做这个"));
    }

    #[test]
    fn bom_and_crlf() {
        let f = parse("\u{feff}---\r\nname: foo\r\ndescription: bar\r\n---\r\nbody");
        assert_eq!(f.name.as_deref(), Some("foo"));
        assert_eq!(f.description.as_deref(), Some("bar"));
    }

    #[test]
    fn no_frontmatter() {
        assert_eq!(parse("# 标题\n正文"), Frontmatter::default());
    }
}

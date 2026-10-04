//! 一次 refresh：找文件 → 只重读变过的文件（能续读就续读）→ 写缓存 → 汇总。

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use anyhow::Context;
use rayon::prelude::*;

use crate::cache::{self, Files};
use crate::discover::{self, Ctx, Found, Kind};
use crate::pricing::{self, Pricing};
use crate::record::{CodexState, FileEntry, Resume};
use crate::{aggregate, claude, codex, gemini, scan, tools, Tool, UsageSnapshot};

/// 文件头指纹最多取这么多字节
const HEAD_FINGERPRINT: u64 = 4096;

#[derive(Default)]
pub(crate) struct State {
    pub files: Option<Files>,
    pub pricing: Option<Pricing>,
}

struct Task {
    found: Found,
    prev: Option<FileEntry>,
}

enum Outcome {
    Parsed(FileEntry),
    Failed {
        tool: Tool,
        msg: Option<String>,
        prev: Option<FileEntry>,
    },
}

fn parse_jsonl(f: &Found, prev: Option<&FileEntry>) -> io::Result<FileEntry> {
    // 只追加过的文件从上次读到的位置接着读
    let resumable = prev.filter(|p| {
        let Some(r) = &p.resume else { return false };
        p.tool == f.tool
            && f.size >= p.size
            && r.offset > 0
            && r.offset <= f.size
            && r.head_len > 0
            && scan::head_hash(&f.path, r.head_len).ok() == Some(r.head_hash)
    });
    let (mut entry, offset, cstate) = match resumable {
        Some(p) => {
            let mut e = p.clone();
            let r = e.resume.take().unwrap_or_default();
            (e, r.offset, r.codex.unwrap_or_default())
        }
        None => (
            FileEntry::new(
                f.tool,
                f.path.to_string_lossy().into_owned(),
                f.size,
                f.mtime_ns,
            ),
            0,
            CodexState::default(),
        ),
    };
    entry.size = f.size;
    entry.mtime_ns = f.mtime_ns;
    entry.missing = false;
    let (committed, codex_state) = match f.tool {
        Tool::Claude => (claude::parse(&f.path, &mut entry, offset)?, None),
        _ => {
            let (o, s) = codex::parse(&f.path, &mut entry, offset, cstate)?;
            (o, Some(s))
        }
    };
    let head_len = committed.min(HEAD_FINGERPRINT);
    let head_hash = if head_len > 0 {
        scan::head_hash(&f.path, head_len)?
    } else {
        0
    };
    entry.resume = Some(Resume {
        offset: committed,
        head_len,
        head_hash,
        codex: codex_state,
    });
    Ok(entry)
}

fn run_task(t: Task, gemini_hashes: &HashMap<String, String>) -> Outcome {
    let Task { found: f, prev } = t;
    let fresh = || {
        FileEntry::new(
            f.tool,
            f.path.to_string_lossy().into_owned(),
            f.size,
            f.mtime_ns,
        )
    };
    let result = match f.kind {
        Kind::Gemini => {
            let mut e = fresh();
            gemini::parse(&f.path, &mut e, f.project.clone(), gemini_hashes).map(|_| e)
        }
        Kind::Claude | Kind::Codex => parse_jsonl(&f, prev.as_ref()),
        // 其余工具：文件变了就整份重读
        _ => {
            let mut e = fresh();
            tools::parse(&f, &mut e).map(|_| e)
        }
    };
    match result {
        Ok(e) => Outcome::Parsed(e),
        Err(err) => Outcome::Failed {
            tool: f.tool,
            // 扫描期间刚好被删掉的文件不算错误，下次会按“已删除”处理
            msg: (err.kind() != io::ErrorKind::NotFound)
                .then(|| format!("读取 {} 失败：{}", f.path.display(), err)),
            prev,
        },
    }
}

pub(crate) fn refresh(
    home: &Path,
    state_dir: &Path,
    st: &mut State,
) -> anyhow::Result<UsageSnapshot> {
    let usage_dir = state_dir.join("usage");
    std::fs::create_dir_all(&usage_dir)
        .with_context(|| format!("无法创建目录 {}", usage_dir.display()))?;
    let cache_path = usage_dir.join("cache.json");
    let files = st.files.get_or_insert_with(|| cache::load(&cache_path));
    let pricing = st.pricing.get_or_insert_with(|| pricing::load(state_dir));

    let disc = discover::discover(&Ctx::from_process(home));
    let mut present: HashMap<Tool, u32> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::with_capacity(disc.files.len());
    let mut tasks = Vec::new();
    let mut dirty = false;
    for f in disc.files {
        let key = f.path.to_string_lossy().into_owned();
        *present.entry(f.tool).or_default() += 1;
        match files.get_mut(&key) {
            Some(e) if e.tool == f.tool && e.size == f.size && e.mtime_ns == f.mtime_ns => {
                if e.missing {
                    e.missing = false;
                    dirty = true;
                }
            }
            _ => {
                let prev = files.remove(&key);
                tasks.push(Task { found: f, prev });
            }
        }
        seen.insert(key);
    }
    // 原文件没了：记录留着，标记为已删除
    for (k, e) in files.iter_mut() {
        if !seen.contains(k) && !e.missing {
            e.missing = true;
            dirty = true;
        }
    }

    tasks.sort_by_key(|t| Reverse(t.found.size));
    let hashes = &disc.gemini_hashes;
    let outcomes: Vec<Outcome> = tasks.into_par_iter().map(|t| run_task(t, hashes)).collect();

    let mut sources = disc.sources;
    for o in outcomes {
        dirty = true;
        match o {
            Outcome::Parsed(e) => {
                files.insert(e.path.clone(), e);
            }
            Outcome::Failed { tool, msg, prev } => {
                if let Some(m) = msg {
                    if let Some(s) = sources.iter_mut().find(|s| s.tool == tool) {
                        s.errors.push(m);
                    }
                }
                if let Some(p) = prev {
                    files.insert(p.path.clone(), p);
                }
            }
        }
    }
    if dirty {
        if let Err(e) = cache::save(&cache_path, files) {
            if let Some(s) = sources.first_mut() {
                s.errors.push(format!("用量缓存保存失败：{e}"));
            }
        }
    }

    let inputs = sources
        .into_iter()
        .map(|s| aggregate::SourceInput {
            tool: s.tool,
            root: s.root.to_string_lossy().into_owned(),
            files: present.get(&s.tool).copied().unwrap_or(0),
            errors: s.errors,
        })
        .collect();
    Ok(aggregate::build(
        files,
        pricing,
        inputs,
        chrono::Local::now(),
    ))
}

pub(crate) fn update_pricing(state_dir: &Path, st: &mut State) -> anyhow::Result<usize> {
    let p = pricing::fetch_and_store(state_dir)?;
    let n = p.len();
    st.pricing = Some(p);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn found(path: &Path, tool: Tool) -> Found {
        let md = std::fs::metadata(path).unwrap();
        Found {
            tool,
            kind: Kind::Codex,
            path: path.to_path_buf(),
            size: md.len(),
            mtime_ns: 1,
            project: None,
        }
    }

    #[test]
    fn appended_file_resumes_from_offset() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "tests/fixtures/home/.codex/sessions/2026/10/01/rollout-2026-10-01T12-00-00-cdx1.jsonl",
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rollout-x.jsonl");
        std::fs::copy(&src, &p).unwrap();

        let first = parse_jsonl(&found(&p, Tool::Codex), None).unwrap();
        let r = first.resume.clone().unwrap();
        assert_eq!(r.offset, std::fs::metadata(&p).unwrap().len());
        assert_eq!(r.head_len, HEAD_FINGERPRINT);
        assert_eq!(first.recs.len(), 4);

        // 把文件头指纹之后的一行 token_count 换成等长空白：续读不会回头看它
        let mut text = std::fs::read_to_string(&p).unwrap();
        let at = text.find(r#""input_tokens":3000"#).unwrap();
        assert!(at as u64 > HEAD_FINGERPRINT);
        let start = text[..at].rfind('\n').unwrap() + 1;
        let end = start + text[start..].find('\n').unwrap();
        text.replace_range(start..end, &" ".repeat(end - start));
        text.push_str(r#"{"timestamp":"2026-10-01T04:00:20.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900,"cached_input_tokens":100,"output_tokens":45},"last_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":5}}}}"#);
        text.push('\n');
        std::fs::File::create(&p)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();

        let resumed = parse_jsonl(&found(&p, Tool::Codex), Some(&first)).unwrap();
        assert_eq!(resumed.recs.len(), 5, "续读：保留旧记录并追加新的一条");
        let last = resumed.recs.last().unwrap();
        assert_eq!((last.input, last.output), (100, 5));
        assert_eq!(resumed.resume.unwrap().offset, text.len() as u64);

        // 全量重读才会发现被抹掉的那行
        let full = parse_jsonl(&found(&p, Tool::Codex), None).unwrap();
        assert_eq!(full.recs.len(), 4);

        // 文件头变了就不续读
        let mut changed = text.clone();
        changed.replace_range(1..2, "\"");
        changed.replace_range(0..1, " ");
        std::fs::write(&p, changed.as_bytes()).unwrap();
        let after = parse_jsonl(&found(&p, Tool::Codex), Some(&first)).unwrap();
        assert_eq!(after.recs.len(), 4);
    }
}

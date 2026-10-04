//! 移到废纸篓：每个路径重新校验，只允许已知 AI 数据目录里非受保护的条目，
//! 以及已发现项目里的构建产物和 worktree。

use crate::locations::{classify_single, current_claude_cli, location_defs, Ctx, Env};
use crate::projects::{artifact_kind, folder_walkable};
use crate::transcripts::discover_all;
use crate::util::{canonical_parent_join, has_parent_component, IN_USE_SECS};
use crate::walk::{dir_stats, Node, Stats};
use crate::{Safety, TrashResult};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn move_to_trash(home: &Path, paths: &[String]) -> Vec<TrashResult> {
    let env = Env::new(home);
    let defs = location_defs(&env);
    let canon_roots: Vec<Option<PathBuf>> = defs
        .iter()
        .map(|d| fs::canonicalize(&d.root).ok())
        .collect();
    let canon_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let ctx = Ctx {
        now: env.now,
        current_claude_cli: current_claude_cli(home),
        ..Default::default()
    };
    let mut checker = Checker {
        home,
        canon_home,
        env: &env,
        defs: &defs,
        canon_roots: &canon_roots,
        ctx: &ctx,
        project_dirs: None,
    };
    paths
        .iter()
        .map(|p| match checker.check_and_trash(p) {
            Ok(freed) => TrashResult {
                path: p.clone(),
                ok: true,
                freed_bytes: freed,
                error: None,
            },
            Err(e) => TrashResult {
                path: p.clone(),
                ok: false,
                freed_bytes: 0,
                error: Some(e),
            },
        })
        .collect()
}

struct Checker<'a> {
    home: &'a Path,
    canon_home: PathBuf,
    env: &'a Env,
    defs: &'a [crate::locations::LocDef],
    canon_roots: &'a [Option<PathBuf>],
    ctx: &'a Ctx,
    /// 已发现项目的目录（规范化后，只含会遍历的）
    project_dirs: Option<Vec<PathBuf>>,
}

impl Checker<'_> {
    fn projects(&mut self) -> &[PathBuf] {
        if self.project_dirs.is_none() {
            let mut dirs: Vec<PathBuf> = discover_all(self.home)
                .into_iter()
                .filter_map(|t| t.cwd)
                .map(PathBuf::from)
                .filter(|p| p.is_dir() && folder_walkable(self.home, p))
                .filter_map(|p| fs::canonicalize(p).ok())
                .filter(|p| folder_walkable(&self.canon_home, p))
                .collect();
            dirs.sort();
            dirs.dedup();
            self.project_dirs = Some(dirs);
        }
        self.project_dirs.as_deref().unwrap_or(&[])
    }

    fn check_and_trash(&mut self, raw: &str) -> Result<u64, String> {
        if raw.trim().is_empty() {
            return Err("路径为空".into());
        }
        let path = Path::new(raw);
        if !path.is_absolute() {
            return Err("必须是完整的绝对路径".into());
        }
        if has_parent_component(path) {
            return Err("路径里不能有「..」或「.」这样的成分".into());
        }
        let meta = fs::symlink_metadata(path).map_err(|_| "文件或文件夹不存在".to_string())?;
        let canon = canonical_parent_join(path).ok_or_else(|| "无法解析这个路径".to_string())?;
        if canon == self.canon_home || self.canon_home.starts_with(&canon) {
            return Err("不能删除主目录或它的上级目录".into());
        }

        // 先用不含统计的探针判断「受保护」（不依赖大小和时间），避免对大目录白白遍历
        let probe = Node {
            name: canon
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            is_dir: meta.is_dir(),
            stats: Stats::default(),
            children: Vec::new(),
        };
        match classify_single(self.ctx, self.defs, self.canon_roots, &canon, &probe) {
            Some((loc, Some(class))) => {
                if class.safety == Safety::Protected {
                    if self
                        .canon_roots
                        .iter()
                        .any(|r| r.as_deref() == Some(canon.as_path()))
                    {
                        return Err(format!(
                            "「{}」是整个数据目录，不能整体删除，请展开后删除具体项目",
                            loc.label
                        ));
                    }
                    return Err(format!("受保护的项目，不能删除：{}", class.reason));
                }
            }
            Some((_, None)) => {
                return Err("不在已知的 AI 数据目录里，为安全起见拒绝删除".into());
            }
            None => {
                if !self.is_project_artifact(&canon) {
                    if canon.as_path() != path && self.literal_inside_allowed(path) {
                        return Err("路径经过符号链接指向了允许范围之外，拒绝删除".into());
                    }
                    return Err(
                        "不在已知的 AI 数据目录或项目的可清理产物里，为安全起见拒绝删除".into(),
                    );
                }
            }
        }

        // Claude 对话记录：<session>.jsonl 和同名目录（subagents 等）一起删
        let mut targets = vec![canon.clone()];
        if let Some(dir) = self.claude_transcript_dir(&canon) {
            targets.push(dir);
        }
        let mut freed = 0u64;
        for t in &targets {
            let st = dir_stats(t).unwrap_or_default();
            if st.newest > 0 && self.env.now - st.newest < IN_USE_SECS {
                return Err("最近 30 分钟内还有写入，可能正在使用，请稍后再试".into());
            }
            freed += st.bytes;
        }
        trash_paths(&targets).map_err(|e| format!("移到废纸篓失败：{e}"))?;
        Ok(freed)
    }

    fn claude_transcript_dir(&self, canon: &Path) -> Option<PathBuf> {
        if canon.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            return None;
        }
        let projects = fs::canonicalize(self.home.join(".claude/projects")).ok()?;
        if canon.parent()?.parent()? != projects {
            return None;
        }
        let dir = canon.with_extension("");
        fs::symlink_metadata(&dir)
            .ok()
            .filter(|m| m.is_dir())
            .map(|_| dir)
    }

    fn is_project_artifact(&mut self, canon: &Path) -> bool {
        let projects = self.projects().to_vec();
        for cwd in projects
            .iter()
            .filter(|c| canon.starts_with(c) && canon != c.as_path())
        {
            let mut cur = Some(canon);
            while let Some(a) = cur {
                if a == cwd.as_path() {
                    break;
                }
                let Some(parent) = a.parent() else { break };
                let name = a
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let is_dir = fs::symlink_metadata(a).map(|m| m.is_dir()).unwrap_or(false);
                if is_dir && artifact_kind(a, &name, |n| parent.join(n).exists()).is_some() {
                    return true;
                }
                if parent == cwd.join(".worktrees") || parent == cwd.join(".claude/worktrees") {
                    return is_dir;
                }
                cur = Some(parent);
            }
        }
        false
    }

    fn literal_inside_allowed(&mut self, path: &Path) -> bool {
        if self.defs.iter().any(|d| path.starts_with(&d.root)) {
            return true;
        }
        let home = self.home.to_path_buf();
        discover_all(&home)
            .into_iter()
            .filter_map(|t| t.cwd)
            .any(|c| path.starts_with(&c) && folder_walkable(&home, Path::new(&c)))
    }
}

fn trash_paths(paths: &[PathBuf]) -> Result<(), trash::Error> {
    #[allow(unused_mut)]
    let mut tc = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        // 用系统 NSFileManager 移到废纸篓：不需要「控制 Finder」的权限，也不弹窗
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        tc.set_delete_method(DeleteMethod::NsFileManager);
    }
    tc.delete_all(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcripts::claude_encode;
    use std::time::{Duration, UNIX_EPOCH};

    fn set_old(p: &Path) {
        let t = UNIX_EPOCH + Duration::from_secs((crate::util::now_secs() - 40 * 86_400) as u64);
        let walk = |p: &Path| {
            if let Ok(f) = fs::File::open(p) {
                let _ = f.set_times(fs::FileTimes::new().set_modified(t).set_accessed(t));
            }
        };
        fn rec(p: &Path, f: &dyn Fn(&Path)) {
            if let Ok(rd) = fs::read_dir(p) {
                for e in rd.flatten() {
                    rec(&e.path(), f);
                }
            }
            f(p);
        }
        rec(p, &walk);
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        home: PathBuf,
        project: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(dir.path()).unwrap().join("home");
        let project = home.join("code/app");
        fs::create_dir_all(project.join("node_modules/x")).unwrap();
        fs::write(project.join("node_modules/x/i.js"), vec![0u8; 8192]).unwrap();
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/main.rs"), "fn main(){}").unwrap();
        fs::create_dir_all(project.join("target")).unwrap(); // 没有 Cargo.toml
        let enc = claude_encode(&project.to_string_lossy());
        let pd = home.join(".claude/projects").join(&enc);
        fs::create_dir_all(pd.join("sess1/subagents")).unwrap();
        fs::write(pd.join("sess1/subagents/a.jsonl"), "{}").unwrap();
        fs::write(
            pd.join("sess1.jsonl"),
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\",\"message\":{{\"content\":\"hi\"}}}}\n",
                project.display()
            ),
        )
        .unwrap();
        fs::create_dir_all(home.join(".codex/cache/c")).unwrap();
        fs::write(home.join(".codex/cache/c/f"), vec![0u8; 4096]).unwrap();
        fs::write(home.join(".codex/auth.json"), "{}").unwrap();
        fs::write(home.join(".codex/state_5.sqlite"), "x").unwrap();
        fs::create_dir_all(home.join(".codex/sqlite")).unwrap();
        fs::create_dir_all(home.join("Library/Caches/com.apple.Safari")).unwrap();
        fs::create_dir_all(home.join("Library/Caches/ms-playwright/x")).unwrap();
        fs::create_dir_all(home.join("Documents")).unwrap();
        fs::write(home.join("Documents/secret.txt"), "x").unwrap();
        set_old(&home);
        Fixture {
            _dir: dir,
            home,
            project,
        }
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn err_of(home: &Path, p: &str) -> String {
        let r = move_to_trash(home, &[p.to_string()]);
        assert!(!r[0].ok, "应当拒绝 {p}");
        r[0].error.clone().unwrap()
    }

    #[test]
    fn rejections() {
        let f = fixture();
        let h = &f.home;
        assert!(err_of(h, "").contains("为空"));
        assert!(err_of(h, "relative/path").contains("绝对路径"));
        assert!(err_of(h, &format!("{}/.codex/cache/../auth.json", s(h))).contains(".."));
        assert!(err_of(h, &format!("{}/.codex/nope", s(h))).contains("不存在"));
        assert!(err_of(h, &s(h)).contains("主目录"));
        assert!(err_of(h, &format!("{}/.codex", s(h))).contains("整个数据目录"));
        assert!(err_of(h, &format!("{}/.codex/auth.json", s(h))).contains("受保护"));
        assert!(err_of(h, &format!("{}/.codex/state_5.sqlite", s(h))).contains("受保护"));
        assert!(err_of(h, &format!("{}/.codex/sqlite", s(h))).contains("受保护"));
        assert!(
            err_of(h, &format!("{}/Library/Caches/com.apple.Safari", s(h))).contains("不在已知")
        );
        assert!(err_of(h, &format!("{}/Documents/secret.txt", s(h))).contains("不在已知"));
        // 项目文件夹本身、项目里的普通文件、没有旁证的 target 都不行
        assert!(err_of(h, &s(&f.project)).contains("不在已知"));
        assert!(err_of(h, &s(&f.project.join("src/main.rs"))).contains("不在已知"));
        assert!(err_of(h, &s(&f.project.join("target"))).contains("不在已知"));
        // 符号链接逃逸：AI 目录里的链接指向外面，再从链接往里走
        std::os::unix::fs::symlink(h.join("Documents"), h.join(".codex/cache/escape")).unwrap();
        let e = err_of(h, &format!("{}/.codex/cache/escape/secret.txt", s(h)));
        assert!(e.contains("符号链接"), "{e}");
        assert!(h.join("Documents/secret.txt").exists());
        // 30 分钟内写过
        fs::write(h.join(".codex/cache/c/new"), "x").unwrap();
        assert!(err_of(h, &format!("{}/.codex/cache/c", s(h))).contains("30 分钟"));
        assert!(h.join(".codex/cache/c/new").exists());
    }

    #[test]
    fn real_trash_in_tempdir() {
        let f = fixture();
        let h = &f.home;
        // AI 目录里的缓存
        let r = move_to_trash(h, &[format!("{}/Library/Caches/ms-playwright", s(h))]);
        assert!(r[0].ok, "{:?}", r[0].error);
        assert!(!h.join("Library/Caches/ms-playwright").exists());
        // 项目里的 node_modules
        let nm = f.project.join("node_modules");
        let r = move_to_trash(h, &[s(&nm)]);
        assert!(r[0].ok, "{:?}", r[0].error);
        assert!(r[0].freed_bytes >= 8192);
        assert!(!nm.exists());
        // Claude 对话记录连同同名目录
        let enc = claude_encode(&f.project.to_string_lossy());
        let j = h.join(".claude/projects").join(&enc).join("sess1.jsonl");
        let r = move_to_trash(h, &[s(&j)]);
        assert!(r[0].ok, "{:?}", r[0].error);
        assert!(!j.exists());
        assert!(!h.join(".claude/projects").join(&enc).join("sess1").exists());
    }
}

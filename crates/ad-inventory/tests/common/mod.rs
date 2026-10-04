//! 测试公用：把 fixtures/home 复制到临时目录（替换 __HOME__），再补上符号链接等。

#![allow(dead_code)]

use ad_inventory::{ClaudeCli, CliOutput};
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub state: PathBuf,
}

fn copy_tree(from: &Path, to: &Path, home: &str) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let src = e.path();
        let dst = to.join(e.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst, home);
        } else {
            let bytes = fs::read(&src).unwrap();
            match String::from_utf8(bytes) {
                Ok(text) => fs::write(&dst, text.replace("__HOME__", home)).unwrap(),
                Err(e) => fs::write(&dst, e.into_bytes()).unwrap(),
            }
        }
    }
}

pub fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home");
    copy_tree(&fixtures, &home, home.to_str().unwrap());
    // git 不保存 .DS_Store 和空目录，这里补上
    fs::write(home.join(".claude/skills/alpha/.DS_Store"), "junk-a").unwrap();
    fs::write(
        home.join(".codex/skills/alpha/.DS_Store"),
        "junk-b-different",
    )
    .unwrap();
    // 符号链接：相对、绝对、失效
    std::os::unix::fs::symlink(
        "../../.agents/skills/shared",
        home.join(".claude/skills/shared"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        home.join(".agents/skills/shared"),
        home.join(".codex/skills/shared"),
    )
    .unwrap();
    std::os::unix::fs::symlink("../../nowhere", home.join(".claude/skills/broken")).unwrap();
    Env {
        _tmp: tmp,
        home,
        state,
    }
}

pub fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

/// 备份目录里所有文件
pub fn backups(state: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(p: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
    }
    walk(&state.join("backups"), &mut out);
    out
}

/// 假的 claude 命令：像真命令一样改 `~/.claude.json`，并记录调用。
pub struct FakeCli {
    pub home: PathBuf,
    pub calls: RefCell<Vec<(Vec<String>, PathBuf)>>,
    pub fail: bool,
}

impl FakeCli {
    pub fn new(home: &Path) -> Self {
        FakeCli {
            home: home.to_path_buf(),
            calls: RefCell::new(Vec::new()),
            fail: false,
        }
    }
}

impl ClaudeCli for FakeCli {
    fn run(&self, args: &[String], cwd: &Path) -> anyhow::Result<CliOutput> {
        self.calls
            .borrow_mut()
            .push((args.to_vec(), cwd.to_path_buf()));
        if self.fail {
            return Ok(CliOutput {
                success: false,
                stdout: String::new(),
                stderr: "Error: something went wrong with secret-token-123456".into(),
            });
        }
        let path = self.home.join(".claude.json");
        let mut v: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
        let scope = args
            .iter()
            .position(|a| a == "-s")
            .map(|i| args[i + 1].clone())
            .unwrap();
        let servers = if scope == "user" {
            v.as_object_mut()
                .unwrap()
                .entry("mcpServers")
                .or_insert(serde_json::json!({}))
        } else {
            let key = cwd.to_string_lossy().into_owned();
            v["projects"][&key]
                .as_object_mut()
                .unwrap()
                .entry("mcpServers")
                .or_insert(serde_json::json!({}))
        };
        let servers = servers.as_object_mut().unwrap();
        match args[1].as_str() {
            "remove" => {
                servers.shift_remove(&args[2]);
            }
            "add-json" => {
                servers.insert(args[2].clone(), serde_json::from_str(&args[3]).unwrap());
            }
            other => panic!("unexpected {other}"),
        }
        fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        Ok(CliOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

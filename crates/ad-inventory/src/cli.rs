//! 调用本机 `claude` 命令（只用于 `claude mcp add-json` / `claude mcp remove`）。

use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 一次命令调用的结果。
#[derive(Debug, Clone, Default)]
pub struct CliOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// `claude` 命令行的抽象，测试里用假实现。
pub trait ClaudeCli {
    /// 在 `cwd` 下运行 `claude <args...>`。
    fn run(&self, args: &[String], cwd: &Path) -> Result<CliOutput>;
}

/// 真实的 `claude` 可执行文件。直接执行文件本身，不经过 shell alias。
#[derive(Debug, Clone)]
pub struct SystemClaudeCli {
    pub binary: PathBuf,
    pub timeout: Duration,
}

impl SystemClaudeCli {
    /// 按固定顺序找 `claude`，都找不到再问登录 shell（`whence -p` 会跳过 alias）。
    pub fn locate(home: &Path) -> Result<Self> {
        let binary = find_claude_binary(home).ok_or_else(|| {
            anyhow!(
                "没有找到 claude 命令。请确认已安装 Claude Code（通常在 ~/.local/bin/claude）。"
            )
        })?;
        Ok(SystemClaudeCli {
            binary,
            timeout: Duration::from_secs(60),
        })
    }
}

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub(crate) fn find_claude_binary(home: &Path) -> Option<PathBuf> {
    let candidates = [
        home.join(".local/bin/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".claude/local/claude"),
        home.join(".npm-global/bin/claude"),
    ];
    if let Some(p) = candidates.iter().find(|p| is_executable(p)) {
        return Some(p.clone());
    }
    let out = run_with_timeout(
        Command::new("/bin/zsh").args(["-lc", "whence -p claude"]),
        Duration::from_secs(10),
    )
    .ok()?;
    let line = out.stdout.lines().next()?.trim().to_string();
    let p = PathBuf::from(line);
    (p.is_absolute() && is_executable(&p)).then_some(p)
}

impl ClaudeCli for SystemClaudeCli {
    fn run(&self, args: &[String], cwd: &Path) -> Result<CliOutput> {
        let mut path_env = Vec::new();
        if let Some(dir) = self.binary.parent() {
            path_env.push(dir.to_string_lossy().into_owned());
        }
        for d in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ] {
            path_env.push(d.to_string());
        }
        if let Ok(existing) = std::env::var("PATH") {
            path_env.push(existing);
        }
        let mut cmd = Command::new(&self.binary);
        cmd.args(args)
            .current_dir(cwd)
            .env("PATH", path_env.join(":"))
            .env("NO_COLOR", "1");
        run_with_timeout(&mut cmd, self.timeout)
    }
}

fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<CliOutput> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("无法启动 claude 命令")?;
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = so.as_mut() {
            o.read_to_string(&mut s).ok();
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = se.as_mut() {
            e.read_to_string(&mut s).ok();
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() > timeout {
            child.kill().ok();
            child.wait().ok();
            return Err(anyhow!(
                "claude 命令超过 {} 秒没有结束，已中止",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    Ok(CliOutput {
        success: status.success(),
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

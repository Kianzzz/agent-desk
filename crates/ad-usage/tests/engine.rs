use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ad_usage::{Tool, UsageEngine, UsageSnapshot};
use chrono::{Local, TimeZone};
use tempfile::TempDir;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for ent in fs::read_dir(src).unwrap() {
        let ent = ent.unwrap();
        let to = dst.join(ent.file_name());
        if ent.file_type().unwrap().is_dir() {
            copy_dir(&ent.path(), &to);
        } else {
            fs::copy(ent.path(), &to).unwrap();
        }
    }
}

struct Env {
    home: TempDir,
    state: TempDir,
}

impl Env {
    fn new() -> Env {
        let home = TempDir::new().unwrap();
        copy_dir(&fixtures().join("home"), home.path());
        let state = TempDir::new().unwrap();
        Self::install_pricing(state.path());
        Env { home, state }
    }

    fn empty() -> Env {
        let state = TempDir::new().unwrap();
        Self::install_pricing(state.path());
        Env {
            home: TempDir::new().unwrap(),
            state,
        }
    }

    fn install_pricing(state: &Path) {
        fs::create_dir_all(state.join("usage")).unwrap();
        fs::copy(
            fixtures().join("pricing.json"),
            state.join("usage/pricing.json"),
        )
        .unwrap();
    }

    fn engine(&self) -> UsageEngine {
        UsageEngine::new(
            self.home.path().to_path_buf(),
            self.state.path().to_path_buf(),
        )
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.path().join(rel)
    }
}

const CLAUDE_A: &str = ".claude/projects/-Users-test-proj/sess-a.jsonl";
const CLAUDE_B: &str = ".claude/projects/-Users-test-other/sess-b.jsonl";
const CODEX_1: &str = ".codex/sessions/2026/10/01/rollout-2026-10-01T12-00-00-cdx1.jsonl";
const CODEX_2: &str = ".codex/archived_sessions/rollout-2026-10-02T12-00-00-cdx2.jsonl";

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

struct Totals {
    cost: f64,
    requests: u64,
    unpriced: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

fn totals(s: &UsageSnapshot, tool: Tool) -> Totals {
    let mut t = Totals {
        cost: 0.0,
        requests: 0,
        unpriced: 0,
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };
    for d in s.days.iter().filter(|d| d.tool == tool) {
        t.cost += d.cost_usd;
        t.requests += d.requests;
        t.unpriced += d.unpriced_requests;
        t.input += d.tokens.input;
        t.output += d.tokens.output;
        t.cache_read += d.tokens.cache_read;
        t.cache_write += d.tokens.cache_write;
        t.reasoning += d.tokens.reasoning;
    }
    t
}

fn local_date(rfc: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc)
        .unwrap()
        .with_timezone(&Local)
        .format("%Y-%m-%d")
        .to_string()
}

// msg_1：10 输入 + 500 输出 + 1000 个 5 分钟缓存写 + 2000 个 1 小时缓存写
const MSG1: f64 = 10.0 * 4e-6 + 500.0 * 2e-5 + 1000.0 * 5e-6 + 2000.0 * 8e-6;
// msg_2：没有 cache_creation 细分，400 个缓存写全按 5 分钟价
const MSG2: f64 = 20.0 * 4e-6 + 100.0 * 2e-5 + 3000.0 * 2e-7 + 400.0 * 5e-6;
// msg_3：sonnet 没有 1 小时缓存价，按 2 倍输入价
const MSG3: f64 = 1000.0 * 3e-6 + 200.0 * 1.5e-5 + 100.0 * 2.0 * 3e-6;
// msg_4：快速模式 ×2
const MSG4: f64 = (100.0 * 4e-6 + 100.0 * 2e-5) * 2.0;

#[test]
fn claude_dedup_synthetic_and_cache_prices() {
    let env = Env::new();
    let s = env.engine().refresh().unwrap();
    let t = totals(&s, Tool::Claude);
    // msg_1 在三行里出现（两次流式 + 子代理文件里的副本）只算一次；<synthetic> 跳过
    assert_eq!(t.requests, 5);
    assert_eq!(t.unpriced, 1);
    approx(t.cost, MSG1 + MSG2 + MSG3 + MSG4);
    assert_eq!(t.output, 500 + 100 + 200 + 100 + 3);
    assert_eq!(t.input, 10 + 20 + 1000 + 100 + 7);
    assert_eq!(t.cache_write, 3000 + 400 + 100);
    assert_eq!(t.cache_read, 3000);
    assert_eq!(t.reasoning, 100);
    assert!(s.unpriced_models.contains(&"claude-mystery-1".to_string()));

    // 按天：本地日期
    let day1 = local_date("2026-10-01T04:00:10Z");
    let opus = s
        .days
        .iter()
        .find(|d| d.tool == Tool::Claude && d.model == "claude-opus-5-5" && d.date == day1)
        .unwrap();
    assert_eq!(opus.requests, 2);
    approx(opus.cost_usd, MSG1 + MSG2);
    assert!(s.days.windows(2).all(|w| w[0].date <= w[1].date));

    // 会话：主对话标题优先于子代理；跳过命令、meta、system-reminder、Caveat
    let a = s
        .sessions
        .iter()
        .find(|x| x.session_id == "sess-a")
        .unwrap();
    assert_eq!(a.title.as_deref(), Some("帮我 修一下 登录页的 bug"));
    assert_eq!(a.project.as_deref(), Some("/Users/test/proj"));
    assert_eq!(
        a.models,
        vec!["claude-opus-5-5", "claude-sonnet-4-5-20250929"]
    );
    assert_eq!(a.requests, 3);
    approx(a.cost_usd, MSG1 + MSG2 + MSG3);
    let b = s
        .sessions
        .iter()
        .find(|x| x.session_id == "sess-b")
        .unwrap();
    assert_eq!(b.title.as_deref(), Some("第二个会话"));
    assert!(s.sessions[0].last_active >= s.sessions[1].last_active);

    // 项目按 (project, tool) 聚合
    let p = s
        .projects
        .iter()
        .find(|p| p.project == "/Users/test/proj" && p.tool == Tool::Claude)
        .unwrap();
    assert_eq!(p.sessions, 1);
    approx(p.cost_usd, MSG1 + MSG2 + MSG3);
    assert!(s
        .projects
        .windows(2)
        .all(|w| w[0].cost_usd >= w[1].cost_usd));

    let src = s.sources.iter().find(|x| x.tool == Tool::Claude).unwrap();
    assert_eq!(src.files, 3);
    assert_eq!(src.records, 5);
    assert_eq!(src.archived_records, 0);
    assert!(src.errors.is_empty());
    assert_eq!(
        s.pricing_updated_at.as_deref(),
        Some("2999-01-01T00:00:00Z")
    );
}

// 第一次：800 未缓存 + 200 缓存 + 50 输出
const CDX_R1: f64 = 800.0 * 2e-6 + 200.0 * 2e-7 + 50.0 * 1e-5;
// 差值：2000-1500=500 未缓存 + 1500 缓存 + 100 输出
const CDX_R2: f64 = 500.0 * 2e-6 + 1500.0 * 2e-7 + 100.0 * 1e-5;
// 累计值变小 → 用 last：400 + 100 缓存 + 20 输出，priority 价
const CDX_R3: f64 = 400.0 * 4e-6 + 100.0 * 4e-7 + 20.0 * 2e-5;
// 归档目录里的会话
const CDX_R5: f64 = 100.0 * 2e-6 + 10.0 * 1e-5;

#[test]
fn codex_deltas_quota_and_titles() {
    let env = Env::new();
    let s = env.engine().refresh().unwrap();
    let t = totals(&s, Tool::Codex);
    // 重复上报跳过；info 为 null 跳过；第 4 次换成未定价模型
    assert_eq!(t.requests, 5);
    assert_eq!(t.unpriced, 1);
    approx(t.cost, CDX_R1 + CDX_R2 + CDX_R3 + CDX_R5);
    assert_eq!(t.input, 800 + 500 + 400 + 300 + 100);
    assert_eq!(t.cache_read, 200 + 1500 + 100);
    assert_eq!(t.output, 50 + 100 + 20 + 20 + 10);
    assert_eq!(t.reasoning, 10 + 20);
    assert!(s.unpriced_models.contains(&"gpt-imaginary-9".to_string()));

    // 额度取所有文件里最新的那次
    assert_eq!(s.quotas.len(), 2);
    let five = s.quotas.iter().find(|q| q.window_minutes == 300).unwrap();
    assert_eq!(five.label, "5 小时额度");
    approx(five.used_percent, 33.0);
    assert_eq!(
        five.resets_at
            .as_deref()
            .map(|r| chrono::DateTime::parse_from_rfc3339(r).unwrap().timestamp()),
        Some(1790100000)
    );
    let week = s.quotas.iter().find(|q| q.window_minutes == 10080).unwrap();
    assert_eq!(week.label, "每周额度");
    approx(week.used_percent, 44.0);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&week.observed_at)
            .unwrap()
            .timestamp(),
        chrono::DateTime::parse_from_rfc3339("2026-10-02T04:00:03Z")
            .unwrap()
            .timestamp()
    );

    // 标题跳过 AGENTS.md 和 <environment_context>
    let c1 = s.sessions.iter().find(|x| x.session_id == "cdx1").unwrap();
    assert_eq!(c1.title.as_deref(), Some("把 README 翻译成英文"));
    assert_eq!(c1.project.as_deref(), Some("/Users/test/proj"));
    assert_eq!(c1.models, vec!["gpt-6-sol", "gpt-imaginary-9"]);
    let c2 = s.sessions.iter().find(|x| x.session_id == "cdx2").unwrap();
    assert_eq!(c2.title.as_deref(), Some("写一个 Python 脚本"));
    assert_eq!(c2.project.as_deref(), Some("/Users/test/other"));

    let src = s.sources.iter().find(|x| x.tool == Tool::Codex).unwrap();
    assert_eq!(src.files, 2);
    assert_eq!(src.records, 5);
}

const GEM_1: f64 = 6050.0 * 5e-7 + 4000.0 * 5e-8 + 800.0 * 3e-6;
const GEM_2: f64 = 1000.0 * 5e-7 + 100.0 * 3e-6;

#[test]
fn gemini_tokens_and_projects() {
    let env = Env::new();
    let s = env.engine().refresh().unwrap();
    let t = totals(&s, Tool::Gemini);
    // jsonl 里同一条消息写了两次只算一次
    assert_eq!(t.requests, 2);
    // input 含 cached：10000-4000+50(tool)；thoughts 加进输出
    assert_eq!(t.input, 6050 + 1000);
    assert_eq!(t.cache_read, 4000);
    assert_eq!(t.output, 800 + 100);
    assert_eq!(t.reasoning, 300);
    approx(t.cost, GEM_1 + GEM_2);

    let g1 = s.sessions.iter().find(|x| x.session_id == "g1").unwrap();
    assert_eq!(g1.project.as_deref(), Some("/Users/test/gem"));
    assert_eq!(g1.title.as_deref(), Some("解释一下这个仓库"));
    let g2 = s.sessions.iter().find(|x| x.session_id == "g2").unwrap();
    assert_eq!(g2.project.as_deref(), Some("/Users/test/slug"));
    assert_eq!(g2.title.as_deref(), Some("总结一下"));
    assert!(s
        .projects
        .iter()
        .any(|p| p.project == "/Users/test/slug" && p.tool == Tool::Gemini));
}

#[test]
fn deleted_files_keep_records() {
    let env = Env::new();
    let mut eng = env.engine();
    let before = eng.refresh().unwrap();
    fs::remove_file(env.path(CLAUDE_B)).unwrap();
    fs::remove_file(env.path(CODEX_2)).unwrap();
    let after = eng.refresh().unwrap();
    for tool in [Tool::Claude, Tool::Codex, Tool::Gemini] {
        let (a, b) = (totals(&before, tool), totals(&after, tool));
        approx(a.cost, b.cost);
        assert_eq!(a.requests, b.requests);
    }
    let c = after
        .sources
        .iter()
        .find(|x| x.tool == Tool::Claude)
        .unwrap();
    assert_eq!((c.files, c.records, c.archived_records), (2, 3, 2));
    let x = after
        .sources
        .iter()
        .find(|x| x.tool == Tool::Codex)
        .unwrap();
    assert_eq!((x.files, x.records, x.archived_records), (1, 4, 1));
    // 删掉的会话还在会话列表里，额度也还是最新那次
    assert!(after.sessions.iter().any(|s| s.session_id == "sess-b"));
    approx(
        after.quotas[0]
            .used_percent
            .max(after.quotas[1].used_percent),
        44.0,
    );

    // 换一个进程（从磁盘缓存读）也一样
    let again = env.engine().refresh().unwrap();
    approx(
        totals(&again, Tool::Claude).cost,
        totals(&before, Tool::Claude).cost,
    );
    let c = again
        .sources
        .iter()
        .find(|x| x.tool == Tool::Claude)
        .unwrap();
    assert_eq!(c.archived_records, 2);

    // 文件恢复后不再算作已删除
    copy_dir(&fixtures().join("home"), env.home.path());
    let restored = eng.refresh().unwrap();
    let c = restored
        .sources
        .iter()
        .find(|x| x.tool == Tool::Claude)
        .unwrap();
    assert_eq!((c.files, c.records, c.archived_records), (3, 5, 0));
    approx(
        totals(&restored, Tool::Claude).cost,
        totals(&before, Tool::Claude).cost,
    );
}

fn set_mtime(path: &Path, t: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

#[test]
fn unchanged_files_are_not_reread() {
    let env = Env::new();
    let mut eng = env.engine();
    let before = eng.refresh().unwrap();

    // 内容换成等长的垃圾、修改时间改回原值：大小和 mtime 都没变，不应重读
    let p = env.path(CLAUDE_A);
    let md = fs::metadata(&p).unwrap();
    let mtime = md.modified().unwrap();
    fs::write(&p, vec![b' '; md.len() as usize]).unwrap();
    set_mtime(&p, mtime);
    let same = eng.refresh().unwrap();
    approx(
        totals(&same, Tool::Claude).cost,
        totals(&before, Tool::Claude).cost,
    );
    // 从磁盘缓存启动的新实例也不重读
    let cold = env.engine().refresh().unwrap();
    approx(
        totals(&cold, Tool::Claude).cost,
        totals(&before, Tool::Claude).cost,
    );

    // mtime 一变就重读：sess-a 里只剩子代理文件中的 msg_1 副本和 msg_3
    set_mtime(&p, mtime + Duration::from_secs(5));
    let changed = eng.refresh().unwrap();
    let t = totals(&changed, Tool::Claude);
    assert_eq!(t.requests, 4);
    approx(t.cost, MSG1 + MSG3 + MSG4);
}

#[test]
fn appended_lines_are_read_incrementally() {
    let env = Env::new();
    let mut eng = env.engine();
    let before = eng.refresh().unwrap();

    // 续写：换回 gpt-6-sol、关掉 priority，累计值继续增长
    let mut extra = String::new();
    extra.push_str(r#"{"timestamp":"2026-10-01T04:00:11.000Z","type":"turn_context","payload":{"cwd":"/Users/test/proj","model":"gpt-6-sol"}}"#);
    extra.push('\n');
    extra.push_str(r#"{"timestamp":"2026-10-01T04:00:11.500Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-6-sol","service_tier":"default"}}}"#);
    extra.push('\n');
    extra.push_str(r#"{"timestamp":"2026-10-01T04:00:12.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1800,"cached_input_tokens":600,"cache_write_input_tokens":0,"output_tokens":90,"reasoning_output_tokens":5,"total_tokens":1890},"last_token_usage":null},"rate_limits":null}}"#);
    extra.push('\n');
    // 半行（还在写）：这次不算
    extra.push_str(r#"{"timestamp":"2026-10-01T04:00:13.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2800"#);
    let p = env.path(CODEX_1);
    let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
    std::io::Write::write_all(&mut f, extra.as_bytes()).unwrap();
    drop(f);

    let after = eng.refresh().unwrap();
    let (a, b) = (totals(&before, Tool::Codex), totals(&after, Tool::Codex));
    assert_eq!(b.requests, a.requests + 1);
    // 差值 (1800-800, 600-100, 90-40) → 未缓存 500、缓存 500、输出 50
    approx(b.cost - a.cost, 500.0 * 2e-6 + 500.0 * 2e-7 + 50.0 * 1e-5);

    // 把半行写完
    let tail = r#","cached_input_tokens":600,"cache_write_input_tokens":0,"output_tokens":100,"reasoning_output_tokens":5,"total_tokens":2900},"last_token_usage":null},"rate_limits":null}}"#;
    let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
    std::io::Write::write_all(&mut f, format!("{tail}\n").as_bytes()).unwrap();
    drop(f);
    let done = eng.refresh().unwrap();
    let c = totals(&done, Tool::Codex);
    assert_eq!(c.requests, b.requests + 1);
    approx(c.cost - b.cost, 1000.0 * 2e-6 + 10.0 * 1e-5);

    // 和全新全量解析的结果一致
    let fresh_state = TempDir::new().unwrap();
    Env::install_pricing(fresh_state.path());
    let full = UsageEngine::new(
        env.home.path().to_path_buf(),
        fresh_state.path().to_path_buf(),
    )
    .refresh()
    .unwrap();
    let f = totals(&full, Tool::Codex);
    assert_eq!(f.requests, c.requests);
    approx(f.cost, c.cost);
}

#[test]
fn active_block_follows_ccusage_rules() {
    let env = Env::empty();
    let now = Local::now().timestamp_millis();
    let iso = |ms: i64| {
        Local
            .timestamp_millis_opt(ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let line = |ms: i64, id: &str| {
        format!(
            r#"{{"type":"assistant","timestamp":"{}","requestId":"r-{id}","sessionId":"s1","cwd":"/p","message":{{"model":"claude-opus-5-5","id":"{id}","usage":{{"input_tokens":1000,"output_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            iso(ms)
        )
    };
    let h = 3_600_000i64;
    // 9 小时前的一条自成一个旧窗口；最近 2 小时内三条是当前窗口
    let text = [
        line(now - 9 * h, "old"),
        line(now - 2 * h, "a"),
        line(now - h, "b"),
        line(now - 10 * 60_000, "c"),
    ]
    .join("\n")
        + "\n";
    let dir = env.home.path().join(".claude/projects/p");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("s1.jsonl"), text).unwrap();

    let s = env.engine().refresh().unwrap();
    assert_eq!(s.active_blocks.len(), 1);
    let b = &s.active_blocks[0];
    assert_eq!(b.requests, 3);
    let per = 1000.0 * 4e-6 + 100.0 * 2e-5;
    approx(b.cost_usd, 3.0 * per);
    let start = chrono::DateTime::parse_from_rfc3339(&b.start)
        .unwrap()
        .timestamp_millis();
    let first = now - 2 * h;
    assert_eq!(start, first - first.rem_euclid(h));
    let end = chrono::DateTime::parse_from_rfc3339(&b.end)
        .unwrap()
        .timestamp_millis();
    assert_eq!(end - start, 5 * h);
    let hours = (Local::now().timestamp_millis() - start) as f64 / h as f64;
    assert!((b.burn_rate_usd_per_hour - 3.0 * per / hours).abs() < 1e-6);

    // 最后一条在 5 小时以前：没有活跃窗口
    let env2 = Env::empty();
    let dir = env2.home.path().join(".claude/projects/p");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("s1.jsonl"), line(now - 6 * h, "x") + "\n").unwrap();
    assert!(env2.engine().refresh().unwrap().active_blocks.is_empty());
}

#[test]
fn empty_home_is_fine() {
    let env = Env::empty();
    let s = env.engine().refresh().unwrap();
    assert!(s.days.is_empty() && s.sessions.is_empty() && s.quotas.is_empty());
    assert_eq!(s.sources.len(), 3);
    assert!(s
        .sources
        .iter()
        .all(|x| x.files == 0 && x.errors.is_empty()));
    assert!(!s.timezone.is_empty());
}

#[test]
fn snapshot_serializes_camel_case() {
    let env = Env::new();
    let s = env.engine().refresh().unwrap();
    let v = serde_json::to_value(&s).unwrap();
    assert!(v.get("generatedAt").is_some());
    assert!(v.get("unpricedModels").is_some());
    assert!(v["days"][0].get("costUsd").is_some());
    assert!(v["days"][0]["tokens"].get("cacheRead").is_some());
    assert_eq!(v["days"][0]["tool"], "claude");
}

#[test]
fn bundled_pricing_used_without_state_file() {
    let home = TempDir::new().unwrap();
    copy_dir(&fixtures().join("home"), home.path());
    let state = TempDir::new().unwrap();
    let s = UsageEngine::new(home.path().to_path_buf(), state.path().to_path_buf())
        .refresh()
        .unwrap();
    assert!(s.pricing_updated_at.is_some());
    assert_ne!(
        s.pricing_updated_at.as_deref(),
        Some("2999-01-01T00:00:00Z")
    );
    // 打包快照里没有这两个虚构模型
    assert!(s.unpriced_models.contains(&"claude-mystery-1".to_string()));
    assert!(totals(&s, Tool::Claude).cost > 0.0);
    // 坏掉的价格文件退回打包快照
    fs::write(state.path().join("usage/pricing.json"), "{oops").unwrap();
    let s2 = UsageEngine::new(home.path().to_path_buf(), state.path().to_path_buf())
        .refresh()
        .unwrap();
    assert_eq!(s2.pricing_updated_at, s.pricing_updated_at);
}

#[test]
fn codex_fork_replay_and_carried_totals() {
    let env = Env::empty();
    let tc = |ts: &str, total: (u64, u64, u64), last: (u64, u64, u64)| {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{},"cached_input_tokens":{},"output_tokens":{}}},"last_token_usage":{{"input_tokens":{},"cached_input_tokens":{},"output_tokens":{}}}}},"rate_limits":{{"primary":{{"used_percent":99.0,"window_minutes":300,"resets_at":1}}}}}}}}"#,
            total.0, total.1, total.2, last.0, last.1, last.2
        )
    };
    let lines = [
        r#"{"timestamp":"2026-10-03T02:00:00.000Z","type":"session_meta","payload":{"id":"fork1","forked_from_id":"parent0","cwd":"/p"}}"#.to_string(),
        r#"{"timestamp":"2026-10-03T02:00:00.001Z","type":"turn_context","payload":{"model":"gpt-6-sol"}}"#.to_string(),
        // 父会话历史的重放：时间戳都挤在 fork 那一刻
        tc("2026-10-03T02:00:00.002Z", (1000, 0, 10), (1000, 0, 10)),
        tc("2026-10-03T02:00:00.003Z", (3000, 500, 30), (2000, 500, 20)),
        r#"{"timestamp":"2026-10-03T02:00:00.004Z","type":"compacted","payload":{"message":""}}"#.to_string(),
        // 真正的新请求：累计值从父会话的真实总量接着算（跳变），last 才是这次的用量
        r#"{"timestamp":"2026-10-03T02:05:00.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"\n# Files mentioned by the user:\n\n## a.png: /tmp/a.png\n\n## My request:\n[$cover](/Users/x/skills/cover/SKILL.md) 做个封面"}]}}"#.to_string(),
        tc("2026-10-03T02:05:10.000Z", (90000, 80000, 500), (1200, 1000, 40)),
        tc("2026-10-03T02:05:20.000Z", (91500, 81300, 560), (1500, 1300, 60)),
    ];
    let dir = env.home.path().join(".codex/sessions/2026/10/03");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("rollout-2026-10-03T10-00-00-fork1.jsonl"),
        lines.join("\n") + "\n",
    )
    .unwrap();

    let s = env.engine().refresh().unwrap();
    let t = totals(&s, Tool::Codex);
    assert_eq!(t.requests, 2);
    assert_eq!(t.input, 200 + 200);
    assert_eq!(t.cache_read, 1000 + 1300);
    assert_eq!(t.output, 40 + 60);
    let x = s.sessions.iter().find(|x| x.session_id == "fork1").unwrap();
    assert_eq!(x.title.as_deref(), Some("$cover 做个封面"));
    // 重放里的额度是父会话的旧数据，不采用；之后两次的额度正常读取
    assert_eq!(s.quotas.len(), 1);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&s.quotas[0].observed_at)
            .unwrap()
            .timestamp(),
        chrono::DateTime::parse_from_rfc3339("2026-10-03T02:05:20Z")
            .unwrap()
            .timestamp()
    );
}

#[test]
fn claude_title_skips_task_notifications() {
    let env = Env::empty();
    let dir = env.home.path().join(".claude/projects/p");
    fs::create_dir_all(&dir).unwrap();
    let text = [
        r#"{"type":"user","timestamp":"2026-10-01T04:00:00.000Z","sessionId":"s9","cwd":"/p","message":{"role":"user","content":"<task-notification>\n<task-id>x</task-id>\n</task-notification>"}}"#,
        r#"{"type":"user","timestamp":"2026-10-01T04:00:01.000Z","sessionId":"s9","cwd":"/p","message":{"role":"user","content":"<local-command-stdout>ok</local-command-stdout>"}}"#,
        r#"{"type":"user","timestamp":"2026-10-01T04:00:02.000Z","sessionId":"s9","cwd":"/p","message":{"role":"user","content":"[Request interrupted by user]"}}"#,
        r#"{"type":"user","timestamp":"2026-10-01T04:00:03.000Z","sessionId":"s9","cwd":"/p","message":{"role":"user","content":"看看 [README](./README.md) 里写了什么"}}"#,
        r#"{"type":"assistant","timestamp":"2026-10-01T04:00:05.000Z","requestId":"r9","sessionId":"s9","cwd":"/p","message":{"model":"claude-opus-5-5","id":"m9","usage":{"input_tokens":1,"output_tokens":1}}}"#,
    ]
    .join("\n")
        + "\n";
    fs::write(dir.join("s9.jsonl"), text).unwrap();
    let s = env.engine().refresh().unwrap();
    assert_eq!(
        s.sessions[0].title.as_deref(),
        Some("看看 README 里写了什么")
    );
}

/// 联网更新价格表（只写临时目录）：`cargo test -p ad-usage -- --ignored network_pricing_update`
#[test]
#[ignore]
fn network_pricing_update() {
    let env = Env::new();
    fs::remove_file(env.state.path().join("usage/pricing.json")).unwrap();
    let mut eng = env.engine();
    let n = eng.update_pricing_from_network().unwrap();
    assert!(n > 50, "只拿到 {n} 个模型");
    let saved = fs::read_to_string(env.state.path().join("usage/pricing.json")).unwrap();
    assert!(saved.contains("claude-opus-5-5"));
    let s = eng.refresh().unwrap();
    assert!(s.pricing_updated_at.is_some());
    assert!(totals(&s, Tool::Claude).cost > 0.0);
}

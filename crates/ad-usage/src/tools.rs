//! Claude / Codex / Gemini 以外各工具的登记表：怎么找数据文件、按格式交给哪个解析函数。
//! 每个工具只产生一条 `SourceStatus`；目录不存在就安静地是 0 条，`root` 照常给出默认路径。

use std::io;

use crate::discover::{Ctx, Found, Kind, Source};
use crate::record::FileEntry;
use crate::Tool;
use crate::{amp, cline, codebuddy, copilot, crush, droid, goose, grok, kimi, opencode, pi, qwen};

pub(crate) type DiscoverFn = fn(&Ctx, &mut Vec<Found>) -> Source;

/// 顺序和 `Tool` 枚举一致
pub(crate) const DISCOVERERS: &[DiscoverFn] = &[
    grok::discover,
    opencode::discover,
    discover_kilo,
    qwen::discover,
    copilot::discover,
    cline::discover_cline,
    cline::discover_roo,
    kimi::discover,
    droid::discover,
    amp::discover,
    pi::discover_pi,
    pi::discover_openclaw,
    codebuddy::discover,
    crush::discover,
    goose::discover,
];

/// Kilo：Kilo CLI（OpenCode 同款存储）和 Kilo Code 扩展（Cline 同款任务目录）算同一个工具
fn discover_kilo(ctx: &Ctx, out: &mut Vec<Found>) -> Source {
    let mut src = Source::new(Tool::Kilo, opencode::data_dir(ctx, Tool::Kilo));
    opencode::discover_into(ctx, Tool::Kilo, out, &mut src);
    cline::discover_tasks(ctx, Tool::Kilo, out, &mut src);
    src
}

/// 解析一个数据文件（整份重读）。
pub(crate) fn parse(f: &Found, entry: &mut FileEntry) -> io::Result<()> {
    let p = f.path.as_path();
    match f.kind {
        Kind::GrokUpdates => grok::parse_updates(p, entry),
        Kind::GrokDevDb => grok::parse_dev_db(p, entry),
        Kind::OpencodeDb => opencode::parse_db(p, f.tool, entry),
        Kind::OpencodeJson => opencode::parse_json_dir(p, f.tool, entry),
        Kind::Qwen => qwen::parse(p, entry),
        Kind::CopilotDb => copilot::parse_store(p, entry),
        Kind::CopilotEvents => copilot::parse_events(p, entry),
        Kind::ClineTask => cline::parse_task(p, f.tool, f.project.clone(), entry),
        Kind::ClineCli => cline::parse_sdk(p, entry),
        Kind::KimiWire => kimi::parse_wire(p, f.project.clone(), entry),
        Kind::KimiCode => kimi::parse_code(p, f.project.clone(), entry),
        Kind::Droid => droid::parse(p, entry),
        Kind::Amp => amp::parse(p, entry),
        Kind::Pi => pi::parse_jsonl(p, f.tool, entry),
        Kind::OpenclawDb => pi::parse_openclaw_db(p, entry),
        Kind::Codebuddy => codebuddy::parse(p, entry),
        Kind::Crush => crush::parse(p, f.project.clone(), entry),
        Kind::Goose => goose::parse(p, entry),
        // 这三种由 engine 自己处理
        Kind::Claude | Kind::Codex | Kind::Gemini => Ok(()),
    }
}

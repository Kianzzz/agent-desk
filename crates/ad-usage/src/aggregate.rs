//! 把缓存里所有文件的记录汇总成快照：全局去重、定价、按天/模型/项目/会话聚合、5 小时窗口、额度。

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Local, SecondsFormat, TimeZone};

use crate::cache::Files;
use crate::pricing::{ModelPrice, Pricing};
use crate::record::{FileEntry, QuotaObs, Rec};
use crate::{
    ActiveBlock, DailyModelRow, ProjectRow, QuotaWindow, SessionRow, SourceStatus, TokenCounts,
    Tool, UsageSnapshot,
};

const HOUR_MS: i64 = 3_600_000;
const BLOCK_MS: i64 = 5 * HOUR_MS;
const MAX_SESSIONS: usize = 300;
const UNKNOWN_PROJECT: &str = "未知项目";

pub(crate) struct SourceInput {
    pub tool: Tool,
    pub root: String,
    pub files: u32,
    pub errors: Vec<String>,
}

#[derive(Default, Clone)]
struct Acc {
    tokens: TokenCounts,
    cost: f64,
    requests: u64,
    unpriced: u64,
}

impl Acc {
    fn add(&mut self, r: &Rec, cost: Option<f64>) {
        add_tokens(&mut self.tokens, r);
        self.requests += 1;
        match cost {
            Some(c) => self.cost += c,
            None => self.unpriced += 1,
        }
    }

    fn merge(&mut self, o: &Acc) {
        self.tokens.input += o.tokens.input;
        self.tokens.output += o.tokens.output;
        self.tokens.cache_read += o.tokens.cache_read;
        self.tokens.cache_write += o.tokens.cache_write;
        self.tokens.reasoning += o.tokens.reasoning;
        self.cost += o.cost;
        self.requests += o.requests;
        self.unpriced += o.unpriced;
    }
}

fn add_tokens(t: &mut TokenCounts, r: &Rec) {
    t.input += r.input;
    t.output += r.output;
    t.cache_read += r.cache_read;
    t.cache_write += r.cache_write();
    t.reasoning += r.reasoning;
}

pub(crate) fn rfc3339(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(d) => d.to_rfc3339_opts(SecondsFormat::Secs, false),
        None => String::new(),
    }
}

pub(crate) fn quota_label(minutes: u64) -> String {
    match minutes {
        300 => "5 小时额度".to_string(),
        10080 => "每周额度".to_string(),
        m if m % 60 == 0 => format!("{} 小时额度", m / 60),
        m => format!("{m} 分钟额度"),
    }
}

fn timezone_name(now: &DateTime<Local>) -> String {
    iana_time_zone::get_timezone()
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| now.format("%:z").to_string())
}

struct SessAcc<'a> {
    tool: Tool,
    id: &'a str,
    project: Option<&'a str>,
    /// (标题, 是否来自旁支, 时间)
    title: Option<(&'a str, bool, i64)>,
    acc: Acc,
    first: i64,
    last: i64,
    models: HashMap<usize, u64>,
}

impl<'a> SessAcc<'a> {
    fn new(tool: Tool, id: &'a str) -> Self {
        SessAcc {
            tool,
            id,
            project: None,
            title: None,
            acc: Acc::default(),
            first: i64::MAX,
            last: i64::MIN,
            models: HashMap::new(),
        }
    }
}

#[derive(Default)]
struct SrcAcc {
    records: u64,
    archived: u64,
    last: Option<i64>,
}

/// 本地日期缓存：同一个 15 分钟内的时间戳日期相同（时区偏移最小粒度是 15 分钟）。
struct Dates {
    by_bucket: HashMap<i64, usize>,
    names: Vec<String>,
}

impl Dates {
    fn index(&mut self, ts: i64) -> usize {
        let bucket = ts.div_euclid(900_000);
        if let Some(&i) = self.by_bucket.get(&bucket) {
            return i;
        }
        let name = Local
            .timestamp_millis_opt(ts)
            .single()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let i = match self.names.iter().position(|n| *n == name) {
            Some(i) => i,
            None => {
                self.names.push(name);
                self.names.len() - 1
            }
        };
        self.by_bucket.insert(bucket, i);
        i
    }
}

fn pick_quota<'a>(entries: &[&'a FileEntry]) -> Option<&'a QuotaObs> {
    let all = || {
        entries
            .iter()
            .filter(|e| e.tool == Tool::Codex)
            .filter_map(|e| e.quota.as_ref())
    };
    let preferred = all()
        .filter(|q| q.limit_id.as_deref().is_none_or(|id| id == "codex"))
        .max_by_key(|q| q.ts);
    preferred.or_else(|| all().max_by_key(|q| q.ts))
}

/// ccusage 同款 5 小时窗口，只返回当前活跃的那个。
fn active_block(recs: &mut [(i64, Option<f64>, &Rec)], now_ms: i64) -> Option<ActiveBlock> {
    recs.sort_by_key(|r| r.0);
    let mut start: Option<i64> = None;
    let mut start_idx = 0;
    let mut last_ts = 0;
    for (i, (ts, _, _)) in recs.iter().enumerate() {
        let ts = *ts;
        let new_block = match start {
            None => true,
            Some(s) => ts - s > BLOCK_MS || ts - last_ts > BLOCK_MS,
        };
        if new_block {
            start = Some(ts - ts.rem_euclid(HOUR_MS));
            start_idx = i;
        }
        last_ts = ts;
    }
    let start = start?;
    let end = start + BLOCK_MS;
    if end <= now_ms || now_ms - last_ts >= BLOCK_MS {
        return None;
    }
    let mut acc = Acc::default();
    for (_, cost, r) in &recs[start_idx..] {
        acc.add(r, *cost);
    }
    let hours = ((now_ms - start) as f64 / HOUR_MS as f64).max(0.1);
    Some(ActiveBlock {
        tool: Tool::Claude,
        start: rfc3339(start),
        end: rfc3339(end),
        cost_usd: acc.cost,
        tokens: acc.tokens,
        requests: acc.requests,
        burn_rate_usd_per_hour: acc.cost / hours,
    })
}

pub(crate) fn build(
    files: &Files,
    pricing: &Pricing,
    sources_in: Vec<SourceInput>,
    now: DateTime<Local>,
) -> UsageSnapshot {
    let now_ms = now.timestamp_millis();
    let mut entries: Vec<&FileEntry> = files.values().collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    // 全局模型编号
    let mut model_gid: HashMap<&str, usize> = HashMap::new();
    let mut model_names: Vec<&str> = Vec::new();
    let mut local_models: Vec<Vec<usize>> = Vec::with_capacity(entries.len());
    for e in &entries {
        let mut ids = Vec::with_capacity(e.models.len());
        for m in &e.models {
            let id = match model_gid.get(m.as_str()) {
                Some(&id) => id,
                None => {
                    model_names.push(m.as_str());
                    model_gid.insert(m.as_str(), model_names.len() - 1);
                    model_names.len() - 1
                }
            };
            ids.push(id);
        }
        local_models.push(ids);
    }
    let prices: Vec<Option<&ModelPrice>> = model_names.iter().map(|m| pricing.lookup(m)).collect();

    // 全局去重：同一个键保留 output 最大的；一样大时优先保留原文件还在的
    let mut best: HashMap<u64, (u32, u32)> = HashMap::new();
    for (ei, e) in entries.iter().enumerate() {
        for (ri, r) in e.recs.iter().enumerate() {
            if r.key == 0 {
                continue;
            }
            match best.entry(r.key) {
                Entry::Vacant(v) => {
                    v.insert((ei as u32, ri as u32));
                }
                Entry::Occupied(mut o) => {
                    let (bei, bri) = *o.get();
                    let be = entries[bei as usize];
                    let b = &be.recs[bri as usize];
                    if r.output > b.output || (r.output == b.output && be.missing && !e.missing) {
                        o.insert((ei as u32, ri as u32));
                    }
                }
            }
        }
    }

    // 会话信息（项目、标题）先从所有文件里收集
    let mut sessions: HashMap<(Tool, &str), SessAcc> = HashMap::new();
    for e in &entries {
        for s in &e.sessions {
            let sa = sessions
                .entry((e.tool, s.id.as_str()))
                .or_insert_with(|| SessAcc::new(e.tool, &s.id));
            if sa.project.is_none() {
                sa.project = s.project.as_deref();
            }
            if let Some(t) = &s.title {
                let better = match sa.title {
                    None => true,
                    Some((_, side, ts)) => {
                        (side && !s.title_side) || (side == s.title_side && s.title_ts < ts)
                    }
                };
                if better {
                    sa.title = Some((t.as_str(), s.title_side, s.title_ts));
                }
            }
        }
    }

    let mut dates = Dates {
        by_bucket: HashMap::new(),
        names: Vec::new(),
    };
    let mut days: HashMap<(usize, Tool, usize), Acc> = HashMap::new();
    let mut src: HashMap<Tool, SrcAcc> = HashMap::new();
    let mut unpriced: BTreeSet<&str> = BTreeSet::new();
    let mut claude_recs: Vec<(i64, Option<f64>, &Rec)> = Vec::new();

    for (ei, e) in entries.iter().enumerate() {
        for (ri, r) in e.recs.iter().enumerate() {
            if r.key != 0 && best.get(&r.key) != Some(&(ei as u32, ri as u32)) {
                continue;
            }
            let Some(&gid) = local_models[ei].get(r.model as usize) else {
                continue;
            };
            let cost = prices[gid].map(|p| p.cost(r));
            if cost.is_none() {
                unpriced.insert(model_names[gid]);
            }

            let di = dates.index(r.ts);
            days.entry((di, e.tool, gid)).or_default().add(r, cost);

            let sid = e
                .sessions
                .get(r.session as usize)
                .map(|s| s.id.as_str())
                .unwrap_or("");
            let sa = sessions
                .entry((e.tool, sid))
                .or_insert_with(|| SessAcc::new(e.tool, sid));
            sa.acc.add(r, cost);
            sa.first = sa.first.min(r.ts);
            sa.last = sa.last.max(r.ts);
            *sa.models.entry(gid).or_default() += 1;

            let s = src.entry(e.tool).or_default();
            if e.missing {
                s.archived += 1;
            } else {
                s.records += 1;
            }
            s.last = Some(s.last.map_or(r.ts, |l| l.max(r.ts)));

            if e.tool == Tool::Claude {
                claude_recs.push((r.ts, cost, r));
            }
        }
    }

    // 按天
    let mut day_rows: BTreeMap<(&str, Tool, &str), Acc> = BTreeMap::new();
    for ((di, tool, gid), acc) in &days {
        day_rows
            .entry((dates.names[*di].as_str(), *tool, model_names[*gid]))
            .or_default()
            .merge(acc);
    }
    let days: Vec<DailyModelRow> = day_rows
        .into_iter()
        .map(|((date, tool, model), a)| DailyModelRow {
            date: date.to_string(),
            tool,
            model: model.to_string(),
            tokens: a.tokens,
            cost_usd: a.cost,
            requests: a.requests,
            unpriced_requests: a.unpriced,
        })
        .collect();

    // 项目
    struct ProjAcc {
        acc: Acc,
        sessions: u32,
        first: i64,
        last: i64,
    }
    let mut projects: HashMap<(&str, Tool), ProjAcc> = HashMap::new();
    for s in sessions.values().filter(|s| s.acc.requests > 0) {
        let p = projects
            .entry((s.project.unwrap_or(UNKNOWN_PROJECT), s.tool))
            .or_insert(ProjAcc {
                acc: Acc::default(),
                sessions: 0,
                first: i64::MAX,
                last: i64::MIN,
            });
        p.acc.merge(&s.acc);
        p.sessions += 1;
        p.first = p.first.min(s.first);
        p.last = p.last.max(s.last);
    }
    let mut projects: Vec<(i64, ProjectRow)> = projects
        .into_iter()
        .map(|((project, tool), p)| {
            (
                p.last,
                ProjectRow {
                    project: project.to_string(),
                    tool,
                    tokens: p.acc.tokens,
                    cost_usd: p.acc.cost,
                    requests: p.acc.requests,
                    sessions: p.sessions,
                    first_active: rfc3339(p.first),
                    last_active: rfc3339(p.last),
                },
            )
        })
        .collect();
    projects.sort_by(|a, b| {
        b.1.cost_usd
            .total_cmp(&a.1.cost_usd)
            .then(b.0.cmp(&a.0))
            .then_with(|| a.1.project.cmp(&b.1.project))
    });

    // 会话
    let mut sess: Vec<(i64, &SessAcc)> = sessions
        .values()
        .filter(|s| s.acc.requests > 0)
        .map(|s| (s.last, s))
        .collect();
    sess.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(b.1.id)));
    sess.truncate(MAX_SESSIONS);
    let sessions_out: Vec<SessionRow> = sess
        .into_iter()
        .map(|(_, s)| {
            let mut models: Vec<(usize, u64)> = s.models.iter().map(|(k, v)| (*k, *v)).collect();
            models.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| model_names[a.0].cmp(model_names[b.0]))
            });
            SessionRow {
                session_id: s.id.to_string(),
                tool: s.tool,
                project: s.project.map(str::to_string),
                title: s.title.map(|t| t.0.to_string()),
                models: models
                    .into_iter()
                    .map(|(g, _)| model_names[g].to_string())
                    .collect(),
                started_at: rfc3339(s.first),
                last_active: rfc3339(s.last),
                tokens: s.acc.tokens.clone(),
                cost_usd: s.acc.cost,
                requests: s.acc.requests,
            }
        })
        .collect();

    // 额度
    let quotas: Vec<QuotaWindow> = pick_quota(&entries)
        .map(|q| {
            q.windows
                .iter()
                .map(|w| QuotaWindow {
                    tool: Tool::Codex,
                    label: quota_label(w.window_minutes),
                    used_percent: w.used_percent,
                    window_minutes: w.window_minutes,
                    resets_at: w.resets_at.map(|s| rfc3339(s * 1000)),
                    observed_at: rfc3339(q.ts),
                })
                .collect()
        })
        .unwrap_or_default();

    let active_blocks: Vec<ActiveBlock> =
        active_block(&mut claude_recs, now_ms).into_iter().collect();

    let sources: Vec<SourceStatus> = sources_in
        .into_iter()
        .map(|s| {
            let a = src.remove(&s.tool).unwrap_or_default();
            SourceStatus {
                tool: s.tool,
                root: s.root,
                files: s.files,
                records: a.records,
                archived_records: a.archived,
                last_record_at: a.last.map(rfc3339),
                errors: s.errors,
            }
        })
        .collect();

    UsageSnapshot {
        generated_at: now.to_rfc3339_opts(SecondsFormat::Secs, false),
        timezone: timezone_name(&now),
        days,
        projects: projects.into_iter().map(|(_, p)| p).collect(),
        sessions: sessions_out,
        quotas,
        active_blocks,
        sources,
        pricing_updated_at: pricing.fetched_at.clone(),
        unpriced_models: unpriced.into_iter().map(str::to_string).collect(),
    }
}

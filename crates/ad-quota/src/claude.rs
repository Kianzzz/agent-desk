use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use std::path::Path;

use crate::{bridge, is_past, Account, Tool, Window};

/// `costs`：最近几天每次 Claude 请求的（Unix 毫秒，折算美元），升序。
pub fn account(home: &Path, state_dir: &Path, costs: &[(i64, f64)], now_ms: i64) -> Account {
    let profile = read_profile(home);
    let logged_in = profile.is_some();
    let plan = profile.as_ref().and_then(plan_label);
    let org = profile.as_ref().and_then(|p| p.get("organizationUuid")).and_then(Value::as_str);
    let bridge = read_bridge(state_dir);
    let desktop = read_desktop(home, org);
    let (windows, source) = build_windows(bridge.as_ref(), &desktop, costs, now_ms);
    let hint = if !logged_in || !windows.is_empty() {
        None
    } else {
        Some(match bridge::status(home, state_dir) {
            bridge::BridgeStatus::Installed => {
                "打开一次 Claude 桌面版，或在终端里用一次 Claude Code，额度就会显示".into()
            }
            _ => "打开一次 Claude 桌面版就会显示额度；只用终端的话，在设置里打开「Claude 额度」".into(),
        })
    };
    Account {
        tool: Tool::Claude,
        logged_in,
        login_kind: logged_in.then(|| "Claude 订阅".into()),
        plan,
        quota_source: source.map(Into::into),
        windows,
        hint,
    }
}

/// `~/.claude.json` 的 `oauthAccount`：只有账号资料，没有凭据。
fn read_profile(home: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(home.join(".claude.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let acct = v.get("oauthAccount")?;
    acct.is_object().then(|| acct.clone())
}

fn plan_label(acct: &Value) -> Option<String> {
    let org = acct.get("organizationType").and_then(Value::as_str).unwrap_or("");
    let tier = acct
        .get("userRateLimitTier")
        .and_then(Value::as_str)
        .or_else(|| acct.get("organizationRateLimitTier").and_then(Value::as_str))
        .unwrap_or("");
    let base = match org {
        "claude_max" => "Max",
        "claude_pro" => "Pro",
        "claude_team" => "Team",
        "claude_enterprise" => "Enterprise",
        "" => return None,
        other => other.trim_start_matches("claude_"),
    };
    let mult = ["5x", "20x"].into_iter().find(|m| tier.ends_with(&format!("_{m}")));
    Some(match mult {
        Some(m) => format!("{base} {m}"),
        None => capitalize(base),
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// 一份官方额度读数：百分比，以及记下它的时刻。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Reading {
    pct: f64,
    observed_ms: i64,
}

/// 状态栏脚本记下的最后一份输入（只在终端里用 Claude Code 时更新）。
struct BridgeRecord {
    five_hour: Option<(Reading, Option<i64>)>,
    seven_day: Option<(Reading, Option<i64>)>,
}

fn read_bridge(state_dir: &Path) -> Option<BridgeRecord> {
    let path = bridge::last_path(state_dir);
    let text = std::fs::read_to_string(&path).ok()?;
    let observed_ms = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(|t| DateTime::<Utc>::from(t).timestamp_millis())
        .unwrap_or_else(|_| Utc::now().timestamp_millis());
    let v: Value = serde_json::from_str(&text).ok()?;
    let rl = v.get("rate_limits")?;
    let one = |key: &str| {
        let w = rl.get(key)?;
        let pct = w.get("used_percentage").and_then(Value::as_f64)?;
        let resets_ms = w.get("resets_at").and_then(Value::as_i64).map(|s| s * 1000);
        Some((Reading { pct, observed_ms }, resets_ms))
    };
    Some(BridgeRecord { five_hour: one("five_hour"), seven_day: one("seven_day") })
}

/// Claude 桌面版定时记下的官方额度：`fh` 是 5 小时、`sd` 是每周，单位 %。
/// 只在桌面版开着时更新，没有重置时间。
#[derive(Debug, Clone, Copy)]
struct Sample {
    t: i64,
    fh: Option<f64>,
    sd: Option<f64>,
}

fn desktop_history_path(home: &Path) -> std::path::PathBuf {
    home.join("Library/Application Support/Claude/plan-usage-history.json")
}

/// 读桌面版的额度历史，只取当前账号所在组织的，按时间升序。
fn read_desktop(home: &Path, org: Option<&str>) -> Vec<Sample> {
    let Ok(text) = std::fs::read_to_string(desktop_history_path(home)) else {
        return vec![];
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return vec![];
    };
    let Some(list) = v.get("samples").and_then(Value::as_array) else {
        return vec![];
    };
    let mut out: Vec<Sample> = list
        .iter()
        .filter(|x| match (org, x.get("org").and_then(Value::as_str)) {
            (Some(want), Some(got)) => want == got,
            _ => true,
        })
        .filter_map(|x| {
            let t = x.get("t").and_then(Value::as_i64)?;
            let u = x.get("u")?;
            Some(Sample { t, fh: u.get("fh").and_then(Value::as_f64), sd: u.get("sd").and_then(Value::as_f64) })
        })
        .collect();
    out.sort_by_key(|x| x.t);
    out
}

/// 某一种额度在桌面版历史里的读数（按时间升序，跳过没有这一项的样本）。
fn series(samples: &[Sample], pick: fn(&Sample) -> Option<f64>) -> Vec<Reading> {
    samples.iter().filter_map(|x| pick(x).map(|pct| Reading { pct, observed_ms: x.t })).collect()
}

fn floor_hour(ms: i64) -> i64 {
    ms - ms.rem_euclid(3_600_000)
}

/// `boundary` 是某一次每周重置的时刻；返回 `t` 所在窗口的结束时刻。
fn weekly_end(boundary: i64, t: i64) -> i64 {
    boundary + ((t - boundary).div_euclid(SEVEN_DAYS) + 1) * SEVEN_DAYS
}

/// 5 小时额度从窗口结束后的第一次请求开始计时（取整到整点）。
/// 从 `from`（已知不在任何窗口中间的时刻）往后推，返回包含 `t` 的窗口；`t` 不在任何窗口里时返回 None。
fn five_hour_window(costs: &[(i64, f64)], mut from: i64, t: i64) -> Option<(i64, i64)> {
    loop {
        let &(first, _) = costs.iter().find(|(ts, _)| *ts >= from)?;
        let s = floor_hour(first);
        if t < s {
            return None;
        }
        if t < s + FIVE_HOURS {
            return Some((s, s + FIVE_HOURS));
        }
        from = s + FIVE_HOURS;
    }
}

/// 拼出要显示的额度窗口：每种额度取桌面版和状态栏里较新的那份读数，再用本地用量估算现在的值。
fn build_windows(
    bridge: Option<&BridgeRecord>,
    desktop: &[Sample],
    costs: &[(i64, f64)],
    now_ms: i64,
) -> (Vec<Window>, Option<&'static str>) {
    let history_from = now_ms - ad_usage::CLAUDE_COST_HISTORY_MS;
    let mut out = Vec::new();
    let mut from_desktop = false;
    let mut from_bridge = false;

    // 5 小时
    let fh = series(desktop, |x| x.fh);
    let bridge_fh = bridge.and_then(|b| b.five_hour);
    if let Some((rec, is_desktop)) = pick_newer(fh.last().copied(), bridge_fh.map(|(r, _)| r)) {
        // 推算窗口的起点：状态栏给的上次重置时刻，或桌面版里降到 0 的那一刻（那时没有窗口在计时）
        let bridge_end = bridge_fh.and_then(|(_, r)| r).filter(|&r| r <= rec.observed_ms);
        let desktop_idle = fh
            .windows(2)
            .filter(|w| w[1].pct == 0.0 && w[0].pct > 0.0 && w[1].observed_ms <= rec.observed_ms)
            .map(|w| w[1].observed_ms)
            .next_back();
        let resets_ms = if is_desktop || bridge_fh.and_then(|(_, r)| r).is_none() {
            [bridge_end, desktop_idle]
                .into_iter()
                .flatten()
                .filter(|&a| a >= history_from)
                .max()
                .and_then(|from| five_hour_window(costs, from, rec.observed_ms))
                .map(|(_, end)| end)
        } else {
            bridge_fh.and_then(|(_, r)| r)
        };
        out.push(window("fiveHour", "5 小时", FIVE_HOURS, rec, resets_ms, costs, history_from, now_ms));
        if is_desktop {
            from_desktop = true
        } else {
            from_bridge = true
        }
    }

    // 每周
    let sd = series(desktop, |x| x.sd);
    let bridge_sd = bridge.and_then(|b| b.seven_day);
    if let Some((rec, is_desktop)) = pick_newer(sd.last().copied(), bridge_sd.map(|(r, _)| r)) {
        // 每周额度按固定节奏重置。重置时刻从最近的证据推：桌面版里百分比回落的那一刻（取整到整点），
        // 或状态栏给的重置时刻
        let desktop_reset = sd
            .windows(2)
            .filter(|w| w[1].pct < w[0].pct)
            .map(|w| {
                let h = floor_hour(w[1].observed_ms);
                (if h > w[0].observed_ms { h } else { w[1].observed_ms }, w[1].observed_ms)
            })
            .next_back();
        let bridge_reset = bridge_sd.and_then(|(r, resets)| resets.map(|x| (x, r.observed_ms)));
        let boundary =
            [desktop_reset, bridge_reset].into_iter().flatten().max_by_key(|&(_, seen)| seen).map(|(b, _)| b);
        let resets_ms = match bridge_sd {
            // 读数就来自状态栏：它自带的重置时刻最准
            Some((_, Some(r))) if !is_desktop => Some(r),
            _ => boundary.map(|b| weekly_end(b, rec.observed_ms)),
        };
        out.push(window("weekly", "本周", SEVEN_DAYS, rec, resets_ms, costs, history_from, now_ms));
        if is_desktop {
            from_desktop = true
        } else {
            from_bridge = true
        }
    }

    let source = match (from_desktop, from_bridge) {
        (true, false) => Some("Claude 桌面版"),
        (false, true) => Some("Claude Code 状态栏"),
        (true, true) => Some("Claude 桌面版和状态栏"),
        (false, false) => None,
    };
    (out, source)
}

/// 两份读数取较新的；第二个值表示是否来自桌面版。
fn pick_newer(desktop: Option<Reading>, bridge: Option<Reading>) -> Option<(Reading, bool)> {
    match (desktop, bridge) {
        (Some(d), Some(b)) => Some(if d.observed_ms >= b.observed_ms { (d, true) } else { (b, false) }),
        (Some(d), None) => Some((d, true)),
        (None, Some(b)) => Some((b, false)),
        (None, None) => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn window(
    kind: &str,
    label: &str,
    dur_ms: i64,
    rec: Reading,
    resets_ms: Option<i64>,
    costs: &[(i64, f64)],
    history_from: i64,
    now_ms: i64,
) -> Window {
    let recorded = Recorded { pct: rec.pct, resets_ms, observed_ms: rec.observed_ms, dur_ms };
    let (pct, resets, estimated) = match estimate(&recorded, costs, history_from, now_ms) {
        Some(e) => (e.pct, e.resets_ms, true),
        None => (rec.pct, resets_ms, false),
    };
    let resets_at = resets.and_then(|ms| Utc.timestamp_millis_opt(ms).single()).map(|t| t.to_rfc3339());
    let observed_at = Utc.timestamp_millis_opt(rec.observed_ms).single().unwrap_or_else(Utc::now).to_rfc3339();
    Window {
        kind: kind.into(),
        label: label.into(),
        used_percent: pct,
        expired: !estimated && is_past(resets_at.as_deref()),
        resets_at,
        observed_at,
        estimated,
        recorded_percent: rec.pct,
    }
}

const FIVE_HOURS: i64 = 5 * 3_600_000;
const SEVEN_DAYS: i64 = 7 * 86_400_000;
/// 记录在这么久以内算新的，直接用
const FRESH_MS: i64 = 5 * 60_000;

struct Recorded {
    pct: f64,
    resets_ms: Option<i64>,
    observed_ms: i64,
    dur_ms: i64,
}

#[derive(Debug, PartialEq)]
struct Estimate {
    pct: f64,
    /// 新窗口还没开始时为 None
    resets_ms: Option<i64>,
}

fn cost_between(costs: &[(i64, f64)], from: i64, to: i64) -> f64 {
    costs.iter().filter(|(t, _)| *t >= from && *t < to).map(|(_, c)| c).sum()
}

/// 估算现在的额度。额度大致按算力扣，这里用折算费用代替算力：
/// - 还在记录的那个窗口里：记录的百分比 × 窗口开始到现在的费用 ÷ 窗口开始到记录时的费用
/// - 窗口已经重置：用记录算出「每 1% 额度对应多少费用」，再看新窗口里花了多少
///
/// 记录太新、记录里的用量太少（比例不可靠）、或本地用量没覆盖到记录所在窗口的开头时返回 None，
/// 界面照原样显示记录值。
fn estimate(rec: &Recorded, costs: &[(i64, f64)], history_from: i64, now_ms: i64) -> Option<Estimate> {
    if now_ms - rec.observed_ms < FRESH_MS {
        return None;
    }
    let resets = rec.resets_ms?;
    let start = resets - rec.dur_ms;
    // 本地用量记录没覆盖到窗口开头时，算出来的比例会偏（少算了窗口前段的费用），不估
    if start < history_from {
        return None;
    }
    let c_obs = cost_between(costs, start, rec.observed_ms);
    if now_ms < resets {
        if rec.pct < 1.0 || c_obs < 0.5 {
            return None;
        }
        let c_now = cost_between(costs, start, now_ms + 1);
        return Some(Estimate { pct: (rec.pct * c_now / c_obs).min(100.0), resets_ms: Some(resets) });
    }
    // 窗口已经重置
    if rec.pct < 2.0 || c_obs < 0.5 {
        return None;
    }
    let per_pct = c_obs / rec.pct;
    let (new_start, new_end) = if rec.dur_ms == SEVEN_DAYS {
        // 每周额度按固定节奏重置
        let k = (now_ms - resets) / SEVEN_DAYS;
        let s = resets + k * SEVEN_DAYS;
        (s, s + SEVEN_DAYS)
    } else {
        match five_hour_window(costs, resets, now_ms) {
            Some(w) => w,
            // 重置后还没用过，新窗口没开始
            None => return Some(Estimate { pct: 0.0, resets_ms: None }),
        }
    };
    let spent = cost_between(costs, new_start, now_ms + 1);
    Some(Estimate { pct: (spent / per_pct).min(100.0), resets_ms: Some(new_end) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_from_profile() {
        let p = json!({"organizationType":"claude_max","organizationRateLimitTier":"default_claude_max_5x","userRateLimitTier":null});
        assert_eq!(plan_label(&p).as_deref(), Some("Max 5x"));
        let p = json!({"organizationType":"claude_max","organizationRateLimitTier":"default_claude_max_20x"});
        assert_eq!(plan_label(&p).as_deref(), Some("Max 20x"));
        let p = json!({"organizationType":"claude_pro","organizationRateLimitTier":"default_claude_ai"});
        assert_eq!(plan_label(&p).as_deref(), Some("Pro"));
        assert_eq!(plan_label(&json!({})), None);
    }

    #[test]
    fn logged_out_without_profile() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".claude.json"), r#"{"numStartups": 3}"#).unwrap();
        let a = account(home.path(), state.path(), &[], Utc::now().timestamp_millis());
        assert!(!a.logged_in);
        assert!(a.hint.is_none());
    }

    #[test]
    fn windows_from_statusline_record() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"oauthAccount":{"organizationType":"claude_max","organizationRateLimitTier":"default_claude_max_5x"}}"#,
        )
        .unwrap();
        let future = Utc::now().timestamp() + 3600;
        let past = Utc::now().timestamp() - 60;
        std::fs::create_dir_all(state.path().join("claude")).unwrap();
        std::fs::write(
            bridge::last_path(state.path()),
            json!({"model":{"display_name":"Opus"},"rate_limits":{
                "five_hour":{"used_percentage":42.5,"resets_at":future},
                "seven_day":{"used_percentage":18,"resets_at":past}}})
            .to_string(),
        )
        .unwrap();
        let a = account(home.path(), state.path(), &[], Utc::now().timestamp_millis());
        assert!(a.logged_in);
        assert_eq!(a.plan.as_deref(), Some("Max 5x"));
        assert_eq!(a.windows.len(), 2);
        assert_eq!(a.windows[0].kind, "fiveHour");
        assert_eq!(a.windows[0].used_percent, 42.5);
        assert!(!a.windows[0].expired);
        assert_eq!(a.windows[1].kind, "weekly");
        assert!(a.windows[1].expired);
        assert_eq!(a.quota_source.as_deref(), Some("Claude Code 状态栏"));
    }

    #[test]
    fn hint_when_not_connected() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".claude.json"), r#"{"oauthAccount":{"organizationType":"claude_pro"}}"#)
            .unwrap();
        let a = account(home.path(), state.path(), &[], Utc::now().timestamp_millis());
        assert!(a.windows.is_empty());
        assert!(a.hint.unwrap().contains("桌面版"));
    }

    const H: i64 = 3_600_000;
    /// 本地用量记录覆盖了所有时间
    const ALL: i64 = i64::MIN;

    fn rec(pct: f64, resets_ms: i64, observed_ms: i64, dur_ms: i64) -> Recorded {
        Recorded { pct, resets_ms: Some(resets_ms), observed_ms, dur_ms }
    }

    #[test]
    fn fresh_record_is_used_as_is() {
        let costs = [(0, 10.0)];
        assert_eq!(estimate(&rec(50.0, 100 * H, 10 * H, SEVEN_DAYS), &costs, ALL, 10 * H + 60_000), None);
    }

    #[test]
    fn same_window_scales_by_cost() {
        // 记录时窗口内花了 $100、记 50%；之后又花了 $20 → 约 60%
        let start = 200 * H - SEVEN_DAYS;
        let costs = [(start + H, 60.0), (start + 2 * H, 40.0), (start + 30 * H, 20.0)];
        let e = estimate(&rec(50.0, 200 * H, start + 10 * H, SEVEN_DAYS), &costs, ALL, start + 40 * H).unwrap();
        assert!((e.pct - 60.0).abs() < 1e-9);
        assert_eq!(e.resets_ms, Some(200 * H));
    }

    #[test]
    fn weekly_after_reset_uses_cost_per_percent() {
        // 上个窗口 $100 = 50%，即 $2/1%；新窗口里花了 $30 → 15%
        let reset = 200 * H;
        let start = reset - SEVEN_DAYS;
        let costs = [(start + H, 100.0), (reset + H, 30.0)];
        let e = estimate(&rec(50.0, reset, start + 5 * H, SEVEN_DAYS), &costs, ALL, reset + 3 * H).unwrap();
        assert!((e.pct - 15.0).abs() < 1e-9);
        assert_eq!(e.resets_ms, Some(reset + SEVEN_DAYS));
    }

    #[test]
    fn five_hour_after_reset_starts_at_first_request() {
        let reset = 100 * H;
        let costs = [(reset - 2 * H, 10.0), (reset + 3 * H + 1234, 4.0)];
        // 记录：上个 5 小时窗口里 $10 = 20%（$0.5/1%）
        let e = estimate(&rec(20.0, reset, reset - H, FIVE_HOURS), &costs, ALL, reset + 4 * H).unwrap();
        assert!((e.pct - 8.0).abs() < 1e-9);
        // 新窗口从 reset+3h 的整点开始，5 小时后重置
        assert_eq!(e.resets_ms, Some(reset + 3 * H + FIVE_HOURS));
        // 重置后还没用过：0%，新窗口没开始
        let e = estimate(&rec(20.0, reset, reset - H, FIVE_HOURS), &costs[..1], ALL, reset + 4 * H).unwrap();
        assert_eq!(e, Estimate { pct: 0.0, resets_ms: None });
    }

    #[test]
    fn too_little_data_gives_up() {
        let start = 200 * H - SEVEN_DAYS;
        let costs = [(start + H, 0.1)];
        assert_eq!(estimate(&rec(50.0, 200 * H, start + 10 * H, SEVEN_DAYS), &costs, ALL, start + 40 * H), None);
    }

    #[test]
    fn history_gap_gives_up() {
        // 本地用量只从窗口开始一天后才有：窗口前段的费用缺了，不能按比例估
        let reset = 200 * H;
        let start = reset - SEVEN_DAYS;
        let costs = [(start + 30 * H, 100.0), (reset + H, 30.0)];
        let r = rec(50.0, reset, start + 40 * H, SEVEN_DAYS);
        assert_eq!(estimate(&r, &costs, start + 24 * H, reset + 3 * H), None);
        assert!(estimate(&r, &costs, start, reset + 3 * H).is_some());
    }

    fn sample(t: i64, fh: f64, sd: f64) -> Sample {
        Sample { t, fh: Some(fh), sd: Some(sd) }
    }

    #[test]
    fn desktop_reading_wins_when_newer() {
        // 状态栏记录很旧（上个每周窗口 51%），桌面版刚记下 19%：显示 19%，重置时刻从桌面版回落那一刻推
        let reset = 1000 * H;
        let bridge = BridgeRecord {
            five_hour: None,
            seven_day: Some((Reading { pct: 51.0, observed_ms: reset - 50 * H }, Some(reset))),
        };
        let desktop = [
            sample(reset - 15 * 60_000, 2.0, 64.0),
            sample(reset + 60_000, 0.0, 0.0),
            sample(reset + 60 * H, 7.0, 19.0),
        ];
        let now = reset + 60 * H + 60_000;
        let (w, source) = build_windows(Some(&bridge), &desktop, &[], now);
        let weekly = w.iter().find(|x| x.kind == "weekly").unwrap();
        assert_eq!(weekly.used_percent, 19.0);
        assert!(!weekly.estimated);
        let expect = Utc.timestamp_millis_opt(reset + SEVEN_DAYS).unwrap().to_rfc3339();
        assert_eq!(weekly.resets_at.as_deref(), Some(expect.as_str()));
        assert_eq!(source, Some("Claude 桌面版"));
    }

    #[test]
    fn desktop_weekly_reset_rounds_to_the_hour() {
        // 回落发生在 21:47 和 22:00:58 之间：重置时刻取 22:00
        let reset = 1000 * H;
        let desktop =
            [sample(reset - 13 * 60_000, 2.0, 64.0), sample(reset + 58_000, 0.0, 0.0), sample(reset + 2 * H, 1.0, 1.0)];
        let (w, _) = build_windows(None, &desktop, &[], reset + 2 * H + 1);
        let expect = Utc.timestamp_millis_opt(reset + SEVEN_DAYS).unwrap().to_rfc3339();
        assert_eq!(w[1].resets_at.as_deref(), Some(expect.as_str()));
    }

    #[test]
    fn desktop_five_hour_window_from_requests() {
        // 桌面版在 idle 时记到 0；之后 10:20 第一次请求 → 窗口 10:00–15:00
        let base = 1000 * H;
        let desktop = [sample(base, 30.0, 10.0), sample(base + H, 0.0, 10.0), sample(base + 3 * H, 12.0, 11.0)];
        let costs = [(base - 2 * H, 5.0), (base + 2 * H + 20 * 60_000, 3.0)];
        let (w, _) = build_windows(None, &desktop, &costs, base + 3 * H + 1);
        let expect = Utc.timestamp_millis_opt(base + 2 * H + FIVE_HOURS).unwrap().to_rfc3339();
        assert_eq!(w[0].kind, "fiveHour");
        assert_eq!(w[0].used_percent, 12.0);
        assert_eq!(w[0].resets_at.as_deref(), Some(expect.as_str()));
    }
}

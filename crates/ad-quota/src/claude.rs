use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use std::path::Path;

use crate::{bridge, is_past, Account, Tool, Window};

/// `costs`：最近几天每次 Claude 请求的（Unix 毫秒，折算美元），升序。
pub fn account(home: &Path, state_dir: &Path, costs: &[(i64, f64)], now_ms: i64) -> Account {
    let profile = read_profile(home);
    let logged_in = profile.is_some();
    let plan = profile.as_ref().and_then(plan_label);
    let (windows, observed) = read_windows(state_dir, costs, now_ms);
    let hint = if !logged_in || !windows.is_empty() {
        None
    } else {
        Some(match bridge::status(home, state_dir) {
            bridge::BridgeStatus::Installed => {
                if observed.is_some() {
                    "最近一次记录里没有额度数据，在终端里用一次 Claude Code 后会更新".into()
                } else {
                    "已接入，在终端里用一次 Claude Code 后会显示额度".into()
                }
            }
            _ => "还没接入 Claude Code 状态栏，在设置里打开「Claude 额度」".into(),
        })
    };
    Account {
        tool: Tool::Claude,
        logged_in,
        login_kind: logged_in.then(|| "Claude 订阅".into()),
        plan,
        quota_source: (!windows.is_empty()).then(|| "Claude Code 状态栏".into()),
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

/// 读状态栏脚本记下的最后一份输入。返回额度窗口和记录时间。
/// 记录不新时（超过 5 分钟），按记录之后的本地用量估算现在的值，见 [`estimate`]。
fn read_windows(state_dir: &Path, costs: &[(i64, f64)], now_ms: i64) -> (Vec<Window>, Option<String>) {
    let path = bridge::last_path(state_dir);
    let Ok(text) = std::fs::read_to_string(&path) else { return (vec![], None) };
    let observed: DateTime<Utc> = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());
    let observed_at = observed.to_rfc3339();
    let observed_ms = observed.timestamp_millis();
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return (vec![], Some(observed_at)) };
    let Some(rl) = v.get("rate_limits") else { return (vec![], Some(observed_at)) };
    let mut out = Vec::new();
    for (key, kind, label, dur_ms) in [
        ("five_hour", "fiveHour", "5 小时", FIVE_HOURS),
        ("seven_day", "weekly", "本周", SEVEN_DAYS),
    ] {
        let Some(w) = rl.get(key) else { continue };
        let Some(used) = w.get("used_percentage").and_then(Value::as_f64) else { continue };
        let resets_ms = w.get("resets_at").and_then(Value::as_i64).map(|s| s * 1000);
        let rec = Recorded { pct: used, resets_ms, observed_ms, dur_ms };
        let (pct, resets, estimated) = match estimate(&rec, costs, now_ms) {
            Some(e) => (e.pct, e.resets_ms, true),
            None => (used, resets_ms, false),
        };
        let resets_at = resets.and_then(|ms| Utc.timestamp_millis_opt(ms).single()).map(|t| t.to_rfc3339());
        out.push(Window {
            kind: kind.into(),
            label: label.into(),
            used_percent: pct,
            expired: !estimated && is_past(resets_at.as_deref()),
            resets_at,
            observed_at: observed_at.clone(),
            estimated,
            recorded_percent: used,
        });
    }
    (out, Some(observed_at))
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
/// 记录太新、或者记录里的用量太少（比例不可靠）时返回 None，界面照原样显示记录值。
fn estimate(rec: &Recorded, costs: &[(i64, f64)], now_ms: i64) -> Option<Estimate> {
    if now_ms - rec.observed_ms < FRESH_MS {
        return None;
    }
    let resets = rec.resets_ms?;
    let start = resets - rec.dur_ms;
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
        // 5 小时额度从重置后的第一次请求开始计时（取整到整点）
        let mut from = resets;
        loop {
            let Some(&(first, _)) = costs.iter().find(|(t, _)| *t >= from) else {
                return Some(Estimate { pct: 0.0, resets_ms: None });
            };
            let s = first - first.rem_euclid(3_600_000);
            if now_ms < s + rec.dur_ms {
                break (s, s + rec.dur_ms);
            }
            from = s + rec.dur_ms;
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
        std::fs::write(home.path().join(".claude.json"), r#"{"oauthAccount":{"organizationType":"claude_pro"}}"#).unwrap();
        let a = account(home.path(), state.path(), &[], Utc::now().timestamp_millis());
        assert!(a.windows.is_empty());
        assert!(a.hint.unwrap().contains("还没接入"));
    }

    const H: i64 = 3_600_000;

    fn rec(pct: f64, resets_ms: i64, observed_ms: i64, dur_ms: i64) -> Recorded {
        Recorded { pct, resets_ms: Some(resets_ms), observed_ms, dur_ms }
    }

    #[test]
    fn fresh_record_is_used_as_is() {
        let costs = [(0, 10.0)];
        assert_eq!(estimate(&rec(50.0, 100 * H, 10 * H, SEVEN_DAYS), &costs, 10 * H + 60_000), None);
    }

    #[test]
    fn same_window_scales_by_cost() {
        // 记录时窗口内花了 $100、记 50%；之后又花了 $20 → 约 60%
        let start = 200 * H - SEVEN_DAYS;
        let costs = [(start + H, 60.0), (start + 2 * H, 40.0), (start + 30 * H, 20.0)];
        let e = estimate(&rec(50.0, 200 * H, start + 10 * H, SEVEN_DAYS), &costs, start + 40 * H).unwrap();
        assert!((e.pct - 60.0).abs() < 1e-9);
        assert_eq!(e.resets_ms, Some(200 * H));
    }

    #[test]
    fn weekly_after_reset_uses_cost_per_percent() {
        // 上个窗口 $100 = 50%，即 $2/1%；新窗口里花了 $30 → 15%
        let reset = 200 * H;
        let start = reset - SEVEN_DAYS;
        let costs = [(start + H, 100.0), (reset + H, 30.0)];
        let e = estimate(&rec(50.0, reset, start + 5 * H, SEVEN_DAYS), &costs, reset + 3 * H).unwrap();
        assert!((e.pct - 15.0).abs() < 1e-9);
        assert_eq!(e.resets_ms, Some(reset + SEVEN_DAYS));
    }

    #[test]
    fn five_hour_after_reset_starts_at_first_request() {
        let reset = 100 * H;
        let costs = [(reset - 2 * H, 10.0), (reset + 3 * H + 1234, 4.0)];
        // 记录：上个 5 小时窗口里 $10 = 20%（$0.5/1%）
        let e = estimate(&rec(20.0, reset, reset - H, FIVE_HOURS), &costs, reset + 4 * H).unwrap();
        assert!((e.pct - 8.0).abs() < 1e-9);
        // 新窗口从 reset+3h 的整点开始，5 小时后重置
        assert_eq!(e.resets_ms, Some(reset + 3 * H + FIVE_HOURS));
        // 重置后还没用过：0%，新窗口没开始
        let e = estimate(&rec(20.0, reset, reset - H, FIVE_HOURS), &costs[..1], reset + 4 * H).unwrap();
        assert_eq!(e, Estimate { pct: 0.0, resets_ms: None });
    }

    #[test]
    fn too_little_data_gives_up() {
        let start = 200 * H - SEVEN_DAYS;
        let costs = [(start + H, 0.1)];
        assert_eq!(estimate(&rec(50.0, 200 * H, start + 10 * H, SEVEN_DAYS), &costs, start + 40 * H), None);
    }
}

//! 价格表：来自 LiteLLM 的 `model_prices_and_context_window.json`，过滤成精简快照。
//! 打包一份在 `data/pricing.json`，联网更新后存在 `<state_dir>/usage/pricing.json`。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context};
use serde_json::{Map, Value};

use crate::record::{Rec, FLAG_FAST, FLAG_PRIORITY};

pub(crate) const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

const BUNDLED: &str = include_str!("../data/pricing.json");

const BASE_FIELDS: [&str; 4] = [
    "input_cost_per_token",
    "output_cost_per_token",
    "cache_read_input_token_cost",
    "cache_creation_input_token_cost",
];

/// 单价（美元/token），已经按分档、优先级处理好回退。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub cache_write_1h: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelPrice {
    /// 超过这个输入上下文（input + cache_read + cache_write）用分档价
    pub threshold: Option<u64>,
    /// 下标：bit0 = 分档，bit1 = 优先级
    pub rates: [Rates; 4],
    pub has_priority: bool,
    /// LiteLLM `provider_specific_entry.fast`，Claude 快速模式的倍率
    pub fast_multiplier: Option<f64>,
}

/// `_above_200k_tokens` → 200
fn tier_k(field: &str) -> Option<u64> {
    let mut rest = field;
    while let Some(i) = rest.find("_above_") {
        let after = &rest[i + "_above_".len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after[digits..].starts_with("k_tokens") {
            return after[..digits].parse().ok();
        }
        rest = after;
    }
    None
}

/// 快照里保留的价格字段：基础价、1 小时缓存写、`_above_<N>k_tokens` 分档、`_priority`。
fn keep_field(k: &str) -> bool {
    let Some(mut rest) = BASE_FIELDS.iter().find_map(|b| k.strip_prefix(b)) else {
        return false;
    };
    if let Some(r) = rest.strip_prefix("_above_1hr") {
        rest = r;
    }
    if let Some(r) = rest.strip_prefix("_above_") {
        let digits = r.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        match r[digits..].strip_prefix("k_tokens") {
            Some(r2) => rest = r2,
            None => return false,
        }
    }
    if let Some(r) = rest.strip_prefix("_priority") {
        rest = r;
    }
    rest.is_empty()
}

impl ModelPrice {
    pub fn from_fields(obj: &Map<String, Value>) -> Option<ModelPrice> {
        let get = |k: &str| obj.get(k).and_then(Value::as_f64);
        let base_in = get("input_cost_per_token")?;
        let base_out = get("output_cost_per_token")?;
        let tier = obj.keys().find_map(|k| tier_k(k));
        let tier_suffix = tier.map(|n| format!("_above_{n}k_tokens"));
        let has_priority = obj.keys().any(|k| k.ends_with("_priority"));

        let resolve = |use_tier: bool, prio: bool| -> Rates {
            let pick = |base: &str| -> Option<f64> {
                let t = tier_suffix.as_deref().filter(|_| use_tier);
                let mut keys = Vec::with_capacity(4);
                if let (Some(t), true) = (t, prio) {
                    keys.push(format!("{base}{t}_priority"));
                }
                if prio {
                    keys.push(format!("{base}_priority"));
                }
                if let Some(t) = t {
                    keys.push(format!("{base}{t}"));
                }
                keys.push(base.to_string());
                keys.iter().find_map(|k| get(k))
            };
            let input = pick("input_cost_per_token").unwrap_or(base_in);
            Rates {
                input,
                output: pick("output_cost_per_token").unwrap_or(base_out),
                cache_read: pick("cache_read_input_token_cost").unwrap_or(input),
                cache_write: pick("cache_creation_input_token_cost").unwrap_or(input),
                cache_write_1h: pick("cache_creation_input_token_cost_above_1hr")
                    .unwrap_or(2.0 * input),
            }
        };

        Some(ModelPrice {
            threshold: tier.map(|n| n * 1000),
            rates: [
                resolve(false, false),
                resolve(true, false),
                resolve(false, true),
                resolve(true, true),
            ],
            has_priority,
            fast_multiplier: get("fast_multiplier"),
        })
    }

    pub fn cost(&self, r: &Rec) -> f64 {
        let ctx = r.input + r.cache_read + r.cache_write_5m + r.cache_write_1h;
        let tier = self.threshold.is_some_and(|t| ctx > t);
        let prio = self.has_priority && r.flags & FLAG_PRIORITY != 0;
        let rt = &self.rates[(tier as usize) | ((prio as usize) << 1)];
        let mut c = r.input as f64 * rt.input
            + r.output as f64 * rt.output
            + r.cache_read as f64 * rt.cache_read
            + r.cache_write_5m as f64 * rt.cache_write
            + r.cache_write_1h as f64 * rt.cache_write_1h;
        if r.flags & FLAG_FAST != 0 {
            if let Some(m) = self.fast_multiplier {
                c *= m;
            }
        }
        c
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Pricing {
    pub fetched_at: Option<String>,
    models: HashMap<String, ModelPrice>,
}

impl Pricing {
    pub fn parse(json: &str) -> anyhow::Result<Pricing> {
        let v: Value = serde_json::from_str(json)?;
        let models_obj = v
            .get("models")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("价格表缺少 models"))?;
        let models: HashMap<String, ModelPrice> = models_obj
            .iter()
            .filter_map(|(k, m)| Some((k.clone(), ModelPrice::from_fields(m.as_object()?)?)))
            .collect();
        if models.is_empty() {
            return Err(anyhow!("价格表是空的"));
        }
        Ok(Pricing {
            fetched_at: v
                .get("fetched_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
        })
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// 先精确匹配；再依次去掉 `[1m]` 之类的后缀、provider 前缀、`@版本`、日期后缀。
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        let mut name = model.trim();
        if let Some(p) = self.models.get(name) {
            return Some(p);
        }
        if let Some(i) = name.find('[') {
            name = &name[..i];
            if let Some(p) = self.models.get(name) {
                return Some(p);
            }
        }
        if let Some(i) = name.rfind('/') {
            name = &name[i + 1..];
            if let Some(p) = self.models.get(name) {
                return Some(p);
            }
        }
        if let Some(i) = name.find('@') {
            name = &name[..i];
            if let Some(p) = self.models.get(name) {
                return Some(p);
            }
        }
        if let Some(stripped) = strip_date_suffix(name) {
            if let Some(p) = self.models.get(stripped) {
                return Some(p);
            }
        }
        None
    }
}

/// `-20251001` 或 `-2025-10-01`
fn strip_date_suffix(name: &str) -> Option<&str> {
    let b = name.as_bytes();
    let n = b.len();
    if n > 9 && b[n - 9] == b'-' && b[n - 8..].iter().all(u8::is_ascii_digit) {
        return Some(&name[..n - 9]);
    }
    if n > 11
        && b[n - 11] == b'-'
        && b[n - 6] == b'-'
        && b[n - 3] == b'-'
        && b[n - 10..n - 6].iter().all(u8::is_ascii_digit)
        && b[n - 5..n - 3].iter().all(u8::is_ascii_digit)
        && b[n - 2..].iter().all(u8::is_ascii_digit)
    {
        return Some(&name[..n - 11]);
    }
    None
}

pub(crate) fn bundled() -> Pricing {
    Pricing::parse(BUNDLED).expect("打包的价格表格式错误")
}

pub(crate) fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("usage").join("pricing.json")
}

/// 优先用联网更新过的价格表；读不了或比打包的旧，就用打包快照。
pub(crate) fn load(state_dir: &Path) -> Pricing {
    let bundled = bundled();
    let Ok(text) = std::fs::read_to_string(state_path(state_dir)) else {
        return bundled;
    };
    match Pricing::parse(&text) {
        Ok(p) => {
            let newer_bundled = match (&p.fetched_at, &bundled.fetched_at) {
                (Some(a), Some(b)) => match (
                    chrono::DateTime::parse_from_rfc3339(a),
                    chrono::DateTime::parse_from_rfc3339(b),
                ) {
                    (Ok(a), Ok(b)) => b > a,
                    _ => false,
                },
                _ => false,
            };
            if newer_bundled {
                bundled
            } else {
                p
            }
        }
        Err(_) => bundled,
    }
}

fn slim(v: &Map<String, Value>) -> Option<Map<String, Value>> {
    v.get("input_cost_per_token")?.as_f64()?;
    v.get("output_cost_per_token")?.as_f64()?;
    let mut fields: BTreeMap<&str, Value> = BTreeMap::new();
    for (k, val) in v {
        if keep_field(k) && val.is_number() {
            fields.insert(k, val.clone());
        }
    }
    if let Some(fast) = v
        .get("provider_specific_entry")
        .and_then(|p| p.get("fast"))
        .and_then(Value::as_f64)
    {
        fields.insert("fast_multiplier", Value::from(fast));
    }
    let mut out = Map::new();
    if let Some(p) = v.get("litellm_provider").and_then(Value::as_str) {
        out.insert("litellm_provider".into(), Value::from(p));
    }
    for (k, val) in fields {
        out.insert(k.to_string(), val);
    }
    Some(out)
}

fn mode_of(v: &Map<String, Value>) -> &str {
    v.get("mode").and_then(Value::as_str).unwrap_or("")
}

fn provider_of(v: &Map<String, Value>) -> &str {
    v.get("litellm_provider")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// 从 LiteLLM 原始价格表里挑出 Anthropic / OpenAI / Gemini 的对话模型，只留计费需要的字段。
pub(crate) fn filter_litellm(raw: &Value, fetched_at: &str) -> anyhow::Result<Value> {
    let obj = raw
        .as_object()
        .ok_or_else(|| anyhow!("价格表不是 JSON 对象"))?;

    let mut out: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for (k, v) in obj {
        if k.contains('/') {
            continue;
        }
        let Some(v) = v.as_object() else { continue };
        let mode = mode_of(v);
        let ok = match provider_of(v) {
            "anthropic" | "openai" => matches!(mode, "chat" | "responses"),
            "gemini" | "vertex_ai-language-models" => mode == "chat" && k.starts_with("gemini"),
            _ => false,
        };
        if ok {
            if let Some(m) = slim(v) {
                out.insert(k.clone(), m);
            }
        }
    }
    // 只以 `gemini/…`、`vertex_ai/…` 形式出现的 Gemini 模型，补成不带前缀的名字
    for prefix in ["gemini/", "vertex_ai/"] {
        for (k, v) in obj {
            let Some(name) = k.strip_prefix(prefix) else {
                continue;
            };
            if !name.starts_with("gemini") || name.contains('/') || out.contains_key(name) {
                continue;
            }
            let Some(v) = v.as_object() else { continue };
            if mode_of(v) != "chat" {
                continue;
            }
            if let Some(m) = slim(v) {
                out.insert(name.to_string(), m);
            }
        }
    }
    if out.is_empty() {
        return Err(anyhow!("价格表里没有找到可用的模型"));
    }
    let mut root = Map::new();
    root.insert("source".into(), Value::from(LITELLM_URL));
    root.insert("fetched_at".into(), Value::from(fetched_at));
    root.insert(
        "models".into(),
        Value::Object(
            out.into_iter()
                .map(|(k, v)| (k, Value::Object(v)))
                .collect(),
        ),
    );
    Ok(Value::Object(root))
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn download(use_env_proxy: bool) -> anyhow::Result<String> {
    let mut cfg = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30)));
    if !use_env_proxy {
        cfg = cfg.proxy(None);
    }
    let agent: ureq::Agent = cfg.build().into();
    let body = agent
        .get(LITELLM_URL)
        .call()
        .context("下载价格表失败")?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .context("读取价格表失败")?;
    Ok(body)
}

/// 下载、过滤、保存。返回新价格表。
pub(crate) fn fetch_and_store(state_dir: &Path) -> anyhow::Result<Pricing> {
    // 环境变量里配置的代理有时和 rustls 握手不兼容，失败时直连再试一次
    let body = match download(true) {
        Ok(b) => b,
        Err(first) if ureq::Proxy::try_from_env().is_some() => {
            download(false).map_err(|_| first)?
        }
        Err(e) => return Err(e),
    };
    let raw: Value = serde_json::from_str(&body).context("价格表不是有效的 JSON")?;
    let fetched_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let slimmed = filter_litellm(&raw, &fetched_at)?;
    let text = serde_json::to_string_pretty(&slimmed)?;
    let pricing = Pricing::parse(&text).context("价格表格式不对")?;
    write_atomic(&state_path(state_dir), text.as_bytes()).context("保存价格表失败")?;
    Ok(pricing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn field_filter() {
        assert!(keep_field("input_cost_per_token"));
        assert!(keep_field("cache_creation_input_token_cost_above_1hr"));
        assert!(keep_field(
            "cache_creation_input_token_cost_above_1hr_above_200k_tokens"
        ));
        assert!(keep_field(
            "input_cost_per_token_above_272k_tokens_priority"
        ));
        assert!(keep_field("output_cost_per_token_priority"));
        assert!(!keep_field("input_cost_per_token_batches"));
        assert!(!keep_field("input_cost_per_token_flex"));
        assert!(!keep_field("input_cost_per_token_cache_hit"));
        assert!(!keep_field("input_cost_per_audio_token"));
        assert_eq!(tier_k("input_cost_per_token_above_200k_tokens"), Some(200));
        assert_eq!(tier_k("cache_creation_input_token_cost_above_1hr"), None);
    }

    #[test]
    fn filter_and_lookup() {
        let raw = json!({
            "sample_spec": {"input_cost_per_token": 0.0},
            "claude-x": {"litellm_provider": "anthropic", "mode": "chat",
                "input_cost_per_token": 1e-6, "output_cost_per_token": 5e-6,
                "input_cost_per_token_batches": 5e-7, "provider_specific_entry": {"fast": 2.0}},
            "gpt-x": {"litellm_provider": "openai", "mode": "responses",
                "input_cost_per_token": 2e-6, "output_cost_per_token": 8e-6},
            "gpt-image": {"litellm_provider": "openai", "mode": "image_generation",
                "input_cost_per_token": 2e-6, "output_cost_per_token": 8e-6},
            "vertex_ai/gemini-y": {"litellm_provider": "vertex_ai", "mode": "chat",
                "input_cost_per_token": 3e-6, "output_cost_per_token": 9e-6},
            "azure/gpt-x": {"litellm_provider": "azure", "mode": "chat",
                "input_cost_per_token": 9.0, "output_cost_per_token": 9.0},
            "mistral-z": {"litellm_provider": "mistral", "mode": "chat",
                "input_cost_per_token": 1.0, "output_cost_per_token": 1.0}
        });
        let v = filter_litellm(&raw, "2026-10-04T00:00:00Z").unwrap();
        let models = v["models"].as_object().unwrap();
        let mut names: Vec<_> = models.keys().cloned().collect();
        names.sort();
        assert_eq!(names, vec!["claude-x", "gemini-y", "gpt-x"]);
        assert!(models["claude-x"]
            .get("input_cost_per_token_batches")
            .is_none());
        assert_eq!(models["claude-x"]["fast_multiplier"], 2.0);

        let p = Pricing::parse(&v.to_string()).unwrap();
        assert_eq!(p.fetched_at.as_deref(), Some("2026-10-04T00:00:00Z"));
        assert!(p.lookup("claude-x").is_some());
        assert!(p.lookup("claude-x-20251001").is_some());
        assert!(p.lookup("anthropic/claude-x").is_some());
        assert!(p.lookup("claude-x[1m]").is_some());
        assert!(p.lookup("gpt-x-2025-08-07").is_some());
        assert!(p.lookup("claude-x-2").is_none());
        assert!(p.lookup("claude").is_none());
        assert!(p.lookup("gpt-x-mini").is_none());
    }

    #[test]
    fn bundled_covers_local_models() {
        let p = bundled();
        for m in [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-sonnet-4-5",
            "gpt-5.6-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6.1-sol",
            "gpt-5.5",
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
            "gemini-3.1-pro-preview",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
        ] {
            assert!(p.lookup(m).is_some(), "{m} 没有价格");
        }
        assert!(p.fetched_at.is_some());
    }

    /// 重新生成打包的价格快照：
    /// `LITELLM_JSON=/path/to/model_prices_and_context_window.json cargo test -p ad-usage -- --ignored regenerate_bundled_pricing`
    /// fetched_at 取这份 JSON 文件的修改时间。
    #[test]
    #[ignore]
    fn regenerate_bundled_pricing() {
        let src = std::env::var("LITELLM_JSON").expect("需要 LITELLM_JSON");
        let text = std::fs::read_to_string(&src).unwrap();
        let mtime: chrono::DateTime<chrono::Utc> =
            std::fs::metadata(&src).unwrap().modified().unwrap().into();
        let fetched_at = mtime.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let raw: Value = serde_json::from_str(&text).unwrap();
        let slimmed = filter_litellm(&raw, &fetched_at).unwrap();
        let out = serde_json::to_string_pretty(&slimmed).unwrap() + "\n";
        Pricing::parse(&out).unwrap();
        let dst = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/pricing.json");
        std::fs::write(dst, out).unwrap();
    }

    #[test]
    fn tiered_and_priority_rates() {
        let obj = json!({
            "input_cost_per_token": 3e-6, "output_cost_per_token": 1.5e-5,
            "cache_read_input_token_cost": 3e-7, "cache_creation_input_token_cost": 3.75e-6,
            "cache_creation_input_token_cost_above_1hr": 6e-6,
            "input_cost_per_token_above_200k_tokens": 6e-6,
            "output_cost_per_token_above_200k_tokens": 2.25e-5,
            "input_cost_per_token_priority": 6e-6
        });
        let mp = ModelPrice::from_fields(obj.as_object().unwrap()).unwrap();
        assert_eq!(mp.threshold, Some(200_000));
        let small = Rec {
            input: 1000,
            output: 100,
            ..Default::default()
        };
        assert!((mp.cost(&small) - (1000.0 * 3e-6 + 100.0 * 1.5e-5)).abs() < 1e-12);
        let big = Rec {
            input: 150_000,
            cache_read: 60_000,
            output: 100,
            ..Default::default()
        };
        let want = 150_000.0 * 6e-6 + 60_000.0 * 3e-7 + 100.0 * 2.25e-5;
        assert!((mp.cost(&big) - want).abs() < 1e-9);
        let prio = Rec {
            input: 1000,
            flags: FLAG_PRIORITY,
            ..Default::default()
        };
        assert!((mp.cost(&prio) - 1000.0 * 6e-6).abs() < 1e-12);
    }
}

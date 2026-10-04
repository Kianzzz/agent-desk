use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

use ad_disk::DiskReport;
use ad_security::ScanReport;
use ad_usage::{UsageEngine, UsageSnapshot};

use crate::settings::Settings;

pub struct AppState {
    pub home: PathBuf,
    /// `~/.agent-desk`
    pub state_dir: PathBuf,
    pub settings: RwLock<Settings>,
    pub engine: Mutex<UsageEngine>,
    pub usage: RwLock<Option<UsageSnapshot>>,
    pub usage_error: RwLock<Option<String>>,
    pub security: RwLock<Option<ScanReport>>,
    pub disk: RwLock<Option<DiskReport>>,
    /// 磁盘扫描同一时间只跑一个
    pub disk_busy: Mutex<()>,
}

impl AppState {
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let state_dir = home.join(".agent-desk");
        let _ = std::fs::create_dir_all(&state_dir);
        let settings = Settings::load(&state_dir);
        Self {
            engine: Mutex::new(UsageEngine::new(home.clone(), state_dir.clone())),
            home,
            state_dir,
            settings: RwLock::new(settings),
            usage: RwLock::new(None),
            usage_error: RwLock::new(None),
            security: RwLock::new(None),
            disk: RwLock::new(None),
            disk_busy: Mutex::new(()),
        }
    }

    /// 今天（本地时区）各工具合计：(tokens, 美元)。
    pub fn today_totals(&self) -> Option<(u64, f64)> {
        let usage = self.usage.read().unwrap();
        let snap = usage.as_ref()?;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let rows = snap.days.iter().filter(|d| d.date == today);
        Some(rows.fold((0, 0.0), |(t, c), d| {
            let k = &d.tokens;
            (t + k.input + k.output + k.cache_read + k.cache_write, c + d.cost_usd)
        }))
    }
}

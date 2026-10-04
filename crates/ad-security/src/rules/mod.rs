//! 各类检测规则。这里只负责「找到」，分级和措辞在 engine / config 里按上下文决定。

pub(crate) mod commands;
pub(crate) mod hidden;
pub(crate) mod injection;
pub(crate) mod secrets;

use regex::Regex;

/// 一条正则，加上它要求文本里至少出现其中一个的关键词（小写）。关键词一个都没有的文件
/// 直接跳过这条正则，省掉大文件上的无用扫描。
pub(crate) struct Pat {
    pub re: Regex,
    pub needles: &'static [&'static str],
}

impl Pat {
    /// `low` 是整段文本的 ASCII 小写版本。
    pub(crate) fn applies(&self, low: &str) -> bool {
        self.needles.is_empty() || self.needles.iter().any(|n| low.contains(n))
    }
}

pub(crate) fn pat(needles: &'static [&'static str], re: &str) -> Pat {
    Pat {
        re: Regex::new(re).unwrap(),
        needles,
    }
}

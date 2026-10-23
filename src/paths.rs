//! Where everything lives on disk. All data is plain text.
//!
//! Default root:
//!   Windows: %APPDATA%\DayRecap
//!   macOS:   ~/Library/Application Support/DayRecap
//!   Linux:   ~/.local/share/DayRecap
//! Portable mode: put an empty `portable.flag` file next to the executable and
//! data is stored in `DayRecapData` beside it (USB-stick friendly).

use chrono::NaiveDate;
use std::{fs, path::PathBuf, sync::OnceLock};

static ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn root() -> &'static PathBuf {
    ROOT.get_or_init(|| {
        if cfg!(test) {
            let test_root = std::env::temp_dir().join("DayRecapTestEnv");
            let _ = fs::create_dir_all(&test_root);
            return test_root;
        }

        let portable = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
            .filter(|dir| dir.join("portable.flag").exists())
            .map(|dir| dir.join("DayRecapData"));

        let root = portable
            .or_else(|| directories::BaseDirs::new().map(|b| b.data_dir().join("DayRecap")))
            .unwrap_or_else(|| PathBuf::from("DayRecapData"));
        let _ = fs::create_dir_all(&root);
        root
    })
}

fn sub(name: &str) -> PathBuf {
    let dir = root().join(name);
    let _ = fs::create_dir_all(&dir);
    dir
}

fn ymd(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

pub fn data_dir() -> PathBuf {
    sub("data")
}

pub fn reports_dir() -> PathBuf {
    sub("reports")
}

pub fn config_file() -> PathBuf {
    root().join("config.toml")
}

pub fn log_file() -> PathBuf {
    root().join("dayrecap.log")
}

/// Raw activity spans for one day.
pub fn day_file(date: NaiveDate) -> PathBuf {
    data_dir().join(format!("{}.tsv", ymd(date)))
}

/// 1-click daily report (overwritten on each generation).
pub fn daily_report_file(date: NaiveDate) -> PathBuf {
    reports_dir().join(format!("{}_report.txt", ymd(date)))
}

/// Hourly log, appended at the top of every hour.
pub fn hourly_report_file(date: NaiveDate) -> PathBuf {
    reports_dir().join(format!("{}_hourly.txt", ymd(date)))
}

/// Chat transcript for one day.
pub fn chat_file(date: NaiveDate) -> PathBuf {
    sub("chat").join(format!("{}.txt", ymd(date)))
}

/// Directory storing named rule profiles (.toml).
pub fn rules_dir() -> PathBuf {
    sub("rules")
}

/// Directory storing persistent learned memory.
pub fn memory_dir() -> PathBuf {
    sub("memory")
}

/// Learned user facts and persistent instructions.
pub fn learned_file() -> PathBuf {
    memory_dir().join("learned.txt")
}

/// Which rule profiles are active for a specific date.
pub fn active_rules_file(date: NaiveDate) -> PathBuf {
    rules_dir().join(format!("active_{}.txt", ymd(date)))
}

/// Captured clipboard/typed text & read-only git commit senses for one day.
pub fn senses_file(date: NaiveDate) -> PathBuf {
    data_dir().join(format!("{}_senses.tsv", ymd(date)))
}

/// Compacted hourly micro-digests for one day.
pub fn digest_file(date: NaiveDate) -> PathBuf {
    data_dir().join(format!("{}_digests.tsv", ymd(date)))
}


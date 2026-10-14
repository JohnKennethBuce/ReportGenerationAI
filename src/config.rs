//! `config.toml` — created with comments on first run, hot-reloaded by the tracker.

use crate::{paths, util};
use serde::{Deserialize, Serialize};
use std::{fs, time::SystemTime};

/// Written on first run. Values here MUST match the `Default` impls below.
pub const DEFAULT_TOML: &str = r#"# DayRecap settings. Edit, save, done — changes apply automatically.

[tracking]
# How often (seconds) the active window is checked. 3 is accurate and uses ~0% CPU.
poll_seconds = 3
# No keyboard/mouse input for this many minutes = idle.
idle_minutes = 5
# Apps recorded only as "Private" (executable names, case-insensitive).
exclude_apps = ["KeePass.exe", "KeePassXC.exe", "1Password.exe", "Bitwarden.exe"]
# Window titles containing any of these words are stored as "[private]".
redact_keywords = ["incognito", "inprivate", "private browsing", "password"]
# Delete raw activity logs older than this many days (reports are kept). 0 = keep forever.
retention_days = 90
# Senses: capture copied/pasted text, typed text lines, and read-only Git commit history.
enable_clipboard_sense = true
enable_typing_sense = true
enable_git_sense = true
# Optional list of Git project directories to inspect (in addition to auto-detected folders).
project_dirs = []

[reports]
# Append an hourly section to reports/<date>_hourly.txt every hour.
hourly = true
# Also ask the AI for a short summary of each hour (one AI call per hour).
hourly_use_ai = false
# Language the AI writes in.
language = "English"
# Open the report automatically after generating it.
open_after_generate = true
# Automatically pop up the Chatbox at work_start_time (even if PC is left on 24/7) to choose today's rules.
auto_morning_popup = true
work_start_time = "08:00"
# Automatically predict & generate active rule reports (Word/Excel) at this time.
report_trigger_time = "16:40"
# Default schedule window (8 AM to 5 PM, excluding 12-1 PM lunch).
work_start_hour = 8
work_end_hour = 17
exclude_hours = [12]

[ai]
# auto       = use local Ollama if it is running, otherwise the built-in report (no AI)
# gemini     = Google Gemini free tier  (free key: https://aistudio.google.com/apikey)
# groq       = Groq free tier           (free key: https://console.groq.com/keys)
# openrouter = OpenRouter free models   (free key: https://openrouter.ai/keys)
# ollama     = local + offline          (https://ollama.com  then:  ollama pull qwen2.5:1.5b)
# openai_compatible = any OpenAI-compatible server (LM Studio, llama.cpp, ...) - set base_url
# template   = never use AI; built-in statistical report (works everywhere, offline)
provider = "auto"
# Leave empty to use the provider's default model.
model = ""
api_key = ""
# Only needed for openai_compatible (e.g. http://localhost:1234/v1) or a non-default Ollama host.
base_url = ""
# Max seconds to wait for the AI (local models on slow CPUs need longer).
timeout_seconds = 180
"#;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tracking: Tracking,
    pub reports: Reports,
    pub ai: Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tracking {
    pub poll_seconds: u64,
    pub idle_minutes: u64,
    pub exclude_apps: Vec<String>,
    pub redact_keywords: Vec<String>,
    pub retention_days: u32,
    pub enable_clipboard_sense: bool,
    pub enable_typing_sense: bool,
    pub enable_git_sense: bool,
    pub project_dirs: Vec<String>,
}

impl Default for Tracking {
    fn default() -> Self {
        Self {
            poll_seconds: 3,
            idle_minutes: 5,
            exclude_apps: ["KeePass.exe", "KeePassXC.exe", "1Password.exe", "Bitwarden.exe"]
                .map(String::from)
                .to_vec(),
            redact_keywords: ["incognito", "inprivate", "private browsing", "password"]
                .map(String::from)
                .to_vec(),
            retention_days: 90,
            enable_clipboard_sense: true,
            enable_typing_sense: true,
            enable_git_sense: true,
            project_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Reports {
    pub hourly: bool,
    pub hourly_use_ai: bool,
    pub language: String,
    pub open_after_generate: bool,
    pub auto_morning_popup: bool,
    pub work_start_time: String,
    pub report_trigger_time: String,
    pub work_start_hour: u32,
    pub work_end_hour: u32,
    pub exclude_hours: Vec<u32>,
}

impl Default for Reports {
    fn default() -> Self {
        Self {
            hourly: true,
            hourly_use_ai: false,
            language: "English".into(),
            open_after_generate: true,
            auto_morning_popup: true,
            work_start_time: "08:00".into(),
            report_trigger_time: "16:40".into(),
            work_start_hour: 8,
            work_end_hour: 17,
            exclude_hours: vec![12],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ai {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub timeout_seconds: u64,
}

impl Default for Ai {
    fn default() -> Self {
        Self {
            provider: "auto".into(),
            model: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            timeout_seconds: 180,
        }
    }
}

/// Load the config, creating a commented default file on first run.
/// Never fails: an invalid file falls back to defaults (and is logged).
pub fn load() -> Config {
    let path = paths::config_file();
    if !path.exists() {
        let _ = fs::write(&path, DEFAULT_TOML);
    }
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_else(|e| {
            util::log(format!("config.toml is invalid, using defaults: {e}"));
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

/// Last-modified time of config.toml, used for hot-reload.
pub fn modified() -> Option<SystemTime> {
    fs::metadata(paths::config_file()).and_then(|m| m.modified()).ok()
}

/// Save config to config.toml with automatic hot-reload triggering.
pub fn save(cfg: &Config) -> Result<(), String> {
    let path = paths::config_file();
    let toml_str = toml::to_string_pretty(cfg).map_err(|e| format!("Cannot serialize config: {e}"))?;
    fs::write(&path, toml_str).map_err(|e| format!("Cannot write config.toml: {e}"))?;
    util::log("config.toml updated and saved by DayRecap.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_matches_defaults() {
        let parsed: Config = toml::from_str(DEFAULT_TOML).unwrap();
        let d = Config::default();
        assert_eq!(parsed.tracking.poll_seconds, d.tracking.poll_seconds);
        assert_eq!(parsed.tracking.exclude_apps, d.tracking.exclude_apps);
        assert_eq!(parsed.tracking.redact_keywords, d.tracking.redact_keywords);
        assert_eq!(parsed.reports.language, d.reports.language);
        assert_eq!(parsed.ai.provider, d.ai.provider);
        assert_eq!(parsed.ai.timeout_seconds, d.ai.timeout_seconds);
    }
}

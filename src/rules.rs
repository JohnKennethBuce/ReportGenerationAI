//! Named Rule Profiles (`workReportFormatword`, `ExcelReportFormatExcel`, and custom rules)
//! + Persistent Learned Memory (`memory/learned.txt`).
//!
//! Each rule is saved both as `.toml` (structured config) and `.txt` (human-readable summary)
//! in `rules/`, and can be discussed, fetched, modified, or toggled in the Chatbox.

use crate::paths;
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

pub const RULE_WORD: &str = "workReportFormatword";
pub const RULE_EXCEL: &str = "ExcelReportFormatExcel";

pub const STYLE_EXEMPLAR: &str = r#"Follow this exact engineering report style for every hourly row:
- Column 1 (Time): Hourly range such as "8:00 AM – 9:00 AM" (skipping 12:00 PM – 1:00 PM lunch).
- Column 2 (Cause): Start with "Need ..." stating the architectural, security, operational, or business reason why this hour's work was needed (e.g. "Need single-instance enforcement to prevent race conditions, database file locking conflicts, and duplicate background processes." or "Need enterprise-grade password security and timing-attack mitigation to replace insecure hardcoded administrative credentials.").
- Column 3 (Findings): MUST start with the Work/Project Title prefix: "Araneta Mall Linking – " followed by concrete technical work completed or turnover conducted, naming specific classes, methods, forms, services, and outcomes on the user's active codebase (MallLinking: e.g. "Araneta Mall Linking – Implemented single-instance application control in Program.cs utilizing named mutex (AppMutexName)..." or "Araneta Mall Linking – Built a static PasswordService implementing PBKDF2..."). NEVER attribute work to background tools like DayRecap or Antigravity.
- Clean out any irrelevant searches or distractions and attribute the full hour to the main project worked on (Araneta Mall Linking).
- For the final 4:00 PM – 5:00 PM slot (when triggered at 4:40 PM), synthesize and predict the final integration and end-to-end verification completing the afternoon's work."#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScheduleSplit {
    pub start_hour: u32,
    pub end_hour: u32,
    pub project_title: String,
    pub repo_path: String,
}

impl Default for ScheduleSplit {
    fn default() -> Self {
        Self {
            start_hour: 8,
            end_hour: 12,
            project_title: String::new(),
            repo_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleProfile {
    pub name: String,
    /// "docx", "xlsx", or "txt"
    pub format: String,
    /// e.g. "Report Work Done_(Date today)" or "Report Pre-Sched_(Date today)"
    pub filename_pattern: String,
    pub columns: Vec<String>,
    pub start_hour: u32,
    pub end_hour: u32,
    pub exclude_hours: Vec<u32>,
    /// HH:MM (24h) auto-trigger time, e.g. "16:40"
    pub trigger_time: String,
    pub predict_tail: bool,
    pub use_git_commits: bool,
    pub use_typed_and_copied: bool,
    pub clean_noise: bool,
    pub title_prefix: String,
    /// Target column for the Work Title ("Findings" or "Cause")
    pub title_target_col: String,
    pub instructions: String,
    /// Optional project/repository schedule splits (e.g. 8-12 Project A, 1-5 Project B)
    pub schedule_splits: Vec<ScheduleSplit>,
}

impl Default for RuleProfile {
    fn default() -> Self {
        Self {
            name: RULE_WORD.into(),
            format: "docx".into(),
            filename_pattern: "Report Work Done_(Date today)".into(),
            columns: vec!["Time".into(), "Cause".into(), "Findings".into()],
            start_hour: 8,
            end_hour: 17,
            exclude_hours: vec![12],
            trigger_time: "16:40".into(),
            predict_tail: true,
            use_git_commits: true,
            use_typed_and_copied: true,
            clean_noise: true,
            title_prefix: "Araneta Mall Linking".into(),
            title_target_col: "Findings".into(),
            instructions: STYLE_EXEMPLAR.into(),
            schedule_splits: Vec::new(),
        }
    }
}

impl RuleProfile {
    pub fn word_default() -> Self {
        Self::default()
    }

    pub fn excel_default() -> Self {
        Self {
            name: RULE_EXCEL.into(),
            format: "xlsx".into(),
            filename_pattern: "Report Pre-Sched_(Date today)".into(),
            ..Self::default()
        }
    }

    /// Resolve the exact work title prefix for a given hour based on configured schedule splits or default title prefix.
    pub fn title_for_hour(&self, hour: u32) -> String {
        for split in &self.schedule_splits {
            if hour >= split.start_hour && hour < split.end_hour && !split.project_title.trim().is_empty() {
                return split.project_title.trim().to_string();
            }
        }
        if !self.title_prefix.trim().is_empty() {
            self.title_prefix.trim().to_string()
        } else {
            "Araneta Mall Linking".to_string()
        }
    }

    /// Resolve the specific repository path assigned to a given hour if configured in schedule splits.
    pub fn repo_for_hour(&self, hour: u32) -> Option<String> {
        for split in &self.schedule_splits {
            if hour >= split.start_hour && hour < split.end_hour && !split.repo_path.trim().is_empty() {
                return Some(split.repo_path.trim().to_string());
            }
        }
        None
    }

    /// Returns true if the rule uses a single unified project across the entire working day.
    pub fn is_single_project_day(&self) -> bool {
        if self.schedule_splits.len() <= 1 {
            return true;
        }
        let first_title = self.schedule_splits[0].project_title.trim();
        let first_repo = self.schedule_splits[0].repo_path.trim();
        self.schedule_splits.iter().all(|s| {
            s.project_title.trim().eq_ignore_ascii_case(first_title)
                && s.repo_path.trim().eq_ignore_ascii_case(first_repo)
        })
    }

    /// Configure this rule to focus 100% of the working day (8:00 AM – 5:00 PM) on a single project and repository,
    /// removing any multi-project splits or secondary titles.
    pub fn apply_single_project(&mut self, title: &str, repo_path: &str, target_col: &str) {
        let clean_title = title.trim();
        let clean_repo = repo_path.trim();
        let col = if target_col.trim().is_empty() {
            "Findings"
        } else {
            target_col.trim()
        };

        self.title_prefix = clean_title.to_string();
        self.title_target_col = col.to_string();
        self.schedule_splits = vec![ScheduleSplit {
            start_hour: self.start_hour,
            end_hour: self.end_hour,
            project_title: clean_title.to_string(),
            repo_path: clean_repo.to_string(),
        }];

        let repo_clause = if clean_repo.is_empty() {
            format!("the {clean_title} codebase")
        } else {
            format!("{clean_title} ({clean_repo})")
        };

        if col.eq_ignore_ascii_case("Findings") {
            self.instructions = format!(
                "Follow this exact engineering report style for every hourly row:\n\
                 - Column 1 (Time): Hourly range such as \"8:00 AM – 9:00 AM\" (skipping 12:00 PM – 1:00 PM lunch).\n\
                 - Column 2 (Cause): Start with \"Need ...\" stating the architectural, security, operational, or business reason why this hour's work was needed.\n\
                 - Column 3 (Findings): MUST start with the exact Work/Project Title prefix: \"{clean_title} – \" for EVERY SINGLE ROW across the entire day (8:00 AM – 5:00 PM), followed by concrete technical work completed from the latest Git commit history of {repo_clause}, naming specific classes, methods, services, and outcomes. Do NOT include any other project title.\n\
                 - Clean out any irrelevant searches or distractions and attribute the full 8-hour day strictly to {clean_title}.\n\
                 - For the final 4:00 PM – 5:00 PM slot (when triggered at 4:40 PM), synthesize and predict the final integration and end-to-end verification completing the afternoon's work on {clean_title}."
            );
        } else {
            self.instructions = format!(
                "Follow this exact engineering report style for every hourly row:\n\
                 - Column 1 (Time): Hourly range such as \"8:00 AM – 9:00 AM\" (skipping 12:00 PM – 1:00 PM lunch).\n\
                 - Column 2 (Cause): MUST start with the Work Title prefix: \"{clean_title} - Need ...\" for EVERY SINGLE ROW across the entire day (8:00 AM – 5:00 PM).\n\
                 - Column 3 (Findings): Concrete technical work completed from the latest Git commit history of {repo_clause}, naming specific classes, methods, services, and outcomes.\n\
                 - Clean out any irrelevant searches or distractions and attribute the full 8-hour day strictly to {clean_title}.\n\
                 - For the final 4:00 PM – 5:00 PM slot (when triggered at 4:40 PM), synthesize and predict the final integration and end-to-end verification completing the afternoon's work on {clean_title}."
            );
        }
    }

    /// Configure this rule with a morning (8:00–12:00) and afternoon (13:00–17:00) project split.
    pub fn apply_split_projects(
        &mut self,
        morning_title: &str,
        morning_repo: &str,
        afternoon_title: &str,
        afternoon_repo: &str,
    ) {
        self.title_target_col = "Findings".into();
        self.title_prefix = morning_title.trim().into();
        self.schedule_splits = vec![
            ScheduleSplit {
                start_hour: 8,
                end_hour: 12,
                project_title: morning_title.trim().to_string(),
                repo_path: morning_repo.trim().to_string(),
            },
            ScheduleSplit {
                start_hour: 13,
                end_hour: 17,
                project_title: afternoon_title.trim().to_string(),
                repo_path: afternoon_repo.trim().to_string(),
            },
        ];
        self.instructions = format!(
            "Follow this exact engineering report style for every hourly row:\n\
             - Column 1 (Time): Hourly range such as \"8:00 AM – 9:00 AM\" (skipping 12:00 PM – 1:00 PM lunch).\n\
             - Column 2 (Cause): Start with \"Need ...\" stating the architectural, security, operational, or business reason why this hour's work was needed.\n\
             - Column 3 (Findings): MUST start with the assigned Work/Project Title prefix:\n\
               • Hours 8:00 AM – 12:00 PM: Prefix with \"{} – \" naming specific classes, methods, and outcomes completed on {}.\n\
               • Hours 1:00 PM – 5:00 PM: Prefix with \"{} – \" naming specific classes, methods, and outcomes completed on {}.\n\
             - Clean out any irrelevant searches or distractions and attribute the full hour to the main project worked on.\n\
             - For the final 4:00 PM – 5:00 PM slot (when triggered at 4:40 PM), synthesize and predict the final integration and end-to-end verification completing the afternoon's work.",
            morning_title.trim(),
            morning_repo.trim(),
            afternoon_title.trim(),
            afternoon_repo.trim()
        );
    }

    pub fn to_readable_txt(&self) -> String {
        let excl: Vec<String> = self.exclude_hours.iter().map(|h| format!("{h}:00")).collect();
        let mut splits_txt = String::new();
        if !self.schedule_splits.is_empty() {
            splits_txt.push_str("\nSchedule Project Splits:\n");
            for s in &self.schedule_splits {
                splits_txt.push_str(&format!(
                    "• {:02}:00–{:02}:00: Title \"{}\" (Repo: {})\n",
                    s.start_hour,
                    s.end_hour,
                    s.project_title,
                    if s.repo_path.is_empty() { "Auto-detect" } else { &s.repo_path }
                ));
            }
        }
        format!(
            "RULE PROFILE: {}\n\
             Format: .{} ({})\n\
             Output Filename Pattern: {}\n\
             Table Columns: {}\n\
             Title Placement: {} (\"{}\")\n\
             Schedule Hours: {:02}:00 to {:02}:00 (Excluding: {})\n\
             Auto-Generate Trigger: {} (Predict 4:40-5:00 PM tail: {}){}\n\
             Senses Enabled: Git Commits={}, Typed/Copied Text={}, Smart Noise Scrubber={}\n\n\
             AI & Formatting Rules:\n{}\n",
            self.name,
            self.format,
            if self.format == "xlsx" { "Excel Spreadsheet" } else { "Word Document" },
            self.filename_pattern,
            self.columns.join(" | "),
            self.title_target_col,
            self.title_prefix,
            self.start_hour,
            self.end_hour,
            if excl.is_empty() { "None".into() } else { excl.join(", ") },
            self.trigger_time,
            self.predict_tail,
            splits_txt,
            self.use_git_commits,
            self.use_typed_and_copied,
            self.clean_noise,
            self.instructions
        )
    }
}

/// Ensure the two default rules (`workReportFormatword` and `ExcelReportFormatExcel`) exist on disk
/// and are updated with modern title placement (Findings).
pub fn ensure_defaults() {
    let dir = paths::rules_dir();
    let word_path = dir.join(format!("{RULE_WORD}.toml"));
    let excel_path = dir.join(format!("{RULE_EXCEL}.toml"));

    if !word_path.exists() {
        let _ = save_rule(&RuleProfile::word_default());
    } else if let Ok(text) = fs::read_to_string(&word_path) {
        if let Ok(mut r) = toml::from_str::<RuleProfile>(text.trim_start_matches('\u{feff}')) {
            if r.title_target_col.is_empty()
                || r.instructions.contains("Column 2 (Cause): MUST start with the Work")
            {
                r.title_target_col = "Findings".into();
                r.title_prefix = "Araneta Mall Linking".into();
                r.instructions = STYLE_EXEMPLAR.into();
                let _ = save_rule(&r);
            }
        }
    }

    if !excel_path.exists() {
        let _ = save_rule(&RuleProfile::excel_default());
    } else if let Ok(text) = fs::read_to_string(&excel_path) {
        if let Ok(mut r) = toml::from_str::<RuleProfile>(text.trim_start_matches('\u{feff}')) {
            if r.title_target_col.is_empty()
                || r.instructions.contains("Column 2 (Cause): MUST start with the Work")
            {
                r.title_target_col = "Findings".into();
                r.title_prefix = "Araneta Mall Linking".into();
                r.instructions = STYLE_EXEMPLAR.into();
                let _ = save_rule(&r);
            }
        }
    }
}

/// Save a rule profile as both `rules/<name>.toml` and `rules/<name>.txt`.
pub fn save_rule(rule: &RuleProfile) -> Result<(PathBuf, PathBuf), String> {
    let safe_name: String = rule
        .name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let safe_name = if safe_name.is_empty() { "CustomRule".into() } else { safe_name };

    let toml_path = paths::rules_dir().join(format!("{safe_name}.toml"));
    let txt_path = paths::rules_dir().join(format!("{safe_name}.txt"));

    let toml_str = toml::to_string_pretty(rule).map_err(|e| e.to_string())?;
    fs::write(&toml_path, toml_str).map_err(|e| e.to_string())?;
    fs::write(&txt_path, rule.to_readable_txt()).map_err(|e| e.to_string())?;
    Ok((toml_path, txt_path))
}

/// Load all saved rule profiles.
pub fn list_rules() -> Vec<RuleProfile> {
    ensure_defaults();
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(paths::rules_dir()) else {
        return vec![RuleProfile::word_default(), RuleProfile::excel_default()];
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(r) = toml::from_str::<RuleProfile>(text.trim_start_matches('\u{feff}')) {
                    out.push(r);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn load_rule(name: &str) -> Option<RuleProfile> {
    ensure_defaults();
    let lower = name.trim().to_lowercase();
    list_rules()
        .into_iter()
        .find(|r| r.name.to_lowercase() == lower || r.name.to_lowercase().contains(&lower))
}

// ---------------------------------------------------------------- Active Rules per Day

/// Return the names of rules activated for `date`. Defaults to both starter rules.
pub fn active_rule_names(date: NaiveDate) -> Vec<String> {
    ensure_defaults();
    let path = paths::active_rules_file(date);
    if let Ok(text) = fs::read_to_string(&path) {
        let names: Vec<String> = text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if !names.is_empty() {
            return names;
        }
    }
    vec![RULE_WORD.into(), RULE_EXCEL.into()]
}

pub fn set_active_rules(date: NaiveDate, names: &[String]) {
    let path = paths::active_rules_file(date);
    let content = names.join("\n");
    let _ = fs::write(path, content);
}

#[allow(dead_code)]
pub fn toggle_active_rule(date: NaiveDate, rule_name: &str) -> Vec<String> {
    let mut current = active_rule_names(date);
    if let Some(pos) = current.iter().position(|x| x.eq_ignore_ascii_case(rule_name)) {
        if current.len() > 1 {
            current.remove(pos);
        }
    } else {
        current.push(rule_name.to_string());
    }
    set_active_rules(date, &current);
    current
}

pub fn active_profiles(date: NaiveDate) -> Vec<RuleProfile> {
    let active = active_rule_names(date);
    let all = list_rules();
    let matched: Vec<RuleProfile> = all
        .into_iter()
        .filter(|r| active.iter().any(|a| a.eq_ignore_ascii_case(&r.name)))
        .collect();
    if matched.is_empty() {
        vec![RuleProfile::word_default(), RuleProfile::excel_default()]
    } else {
        matched
    }
}

// ---------------------------------------------------------------- Learned Memory

pub fn load_learned_notes() -> String {
    fs::read_to_string(paths::learned_file()).unwrap_or_default()
}

pub fn add_learned_note(note: &str) -> Result<(), String> {
    let cleaned = note.trim();
    if cleaned.is_empty() {
        return Ok(());
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::learned_file())
        .map_err(|e| e.to_string())?;
    writeln!(f, "[{}] {}", Local::now().format("%Y-%m-%d %H:%M"), cleaned)
        .map_err(|e| e.to_string())
}

/// Replace any outdated project/schedule/title rules in `memory/learned.txt` with the latest directive
/// so old split or title configurations never conflict with the user's current instruction.
pub fn set_project_schedule_memory(note: &str) -> Result<(), String> {
    let cleaned = note.trim();
    if cleaned.is_empty() {
        return Ok(());
    }
    let path = paths::learned_file();
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let filtered: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let l = line.trim();
            !l.is_empty()
                && !l.contains("Multi-Project Schedule Configured")
                && !l.contains("Single-Project Schedule Configured")
                && !l.contains("Mandatory Work Title:")
                && !l.starts_with("• 8:00 AM – 12:00 PM:")
                && !l.starts_with("• 1:00 PM – 5:00 PM:")
                && !l.starts_with("• 8:00 AM – 5:00 PM")
                && !l.starts_with("• All reports saved in centralized")
                && !l.starts_with("• Findings start with")
        })
        .collect();

    let mut out = if filtered.is_empty() {
        String::new()
    } else {
        format!("{}\n", filtered.join("\n"))
    };
    out.push_str(&format!("[{}] {}\n", Local::now().format("%Y-%m-%d %H:%M"), cleaned));
    fs::write(&path, out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profiles_match_user_spec() {
        let w = RuleProfile::word_default();
        assert_eq!(w.name, "workReportFormatword");
        assert_eq!(w.format, "docx");
        assert_eq!(w.filename_pattern, "Report Work Done_(Date today)");
        assert_eq!(w.columns, vec!["Time", "Cause", "Findings"]);
        assert_eq!(w.exclude_hours, vec![12]);
        assert_eq!(w.trigger_time, "16:40");
        assert_eq!(w.title_target_col, "Findings");
        assert_eq!(w.title_prefix, "Araneta Mall Linking");

        let e = RuleProfile::excel_default();
        assert_eq!(e.name, "ExcelReportFormatExcel");
        assert_eq!(e.format, "xlsx");
        assert_eq!(e.filename_pattern, "Report Pre-Sched_(Date today)");
        assert_eq!(e.title_target_col, "Findings");
    }
}

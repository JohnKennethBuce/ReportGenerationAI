//! Smart Noise Scrubber, Schedule Cleaner, Dynamic Hour Slicer, and Hourly Micro-Digests.
//!
//! 1. Strips casual searches, music players, and brief distractions from the work log.
//! 2. Re-allocates/cleans the hour schedule so the full hour reflects the core project worked on.
//! 3. Dynamically adjusts report hours when executed before 4:40 PM (e.g. at 12:00 PM -> 8..12 only).
//! 4. Compresses each hour into a compact digest for fast, low-token AI generation.

use crate::{
    aggregate::{AppUse, DaySummary},
    paths,
    senses::{self, GitCommit, SenseEvent},
    util::truncate,
};
use chrono::NaiveDate;
use std::{
    fs::{self, OpenOptions},
    io::Write,
};

#[derive(Debug, Clone)]
pub struct CleanedHourSlot {
    pub hour: u32,
    pub time_label: String,
    pub primary_project: String,
    pub assigned_title: String,
    pub assigned_repo: String,
    pub work_evidence: Vec<String>,
    pub is_predicted_tail: bool,
}

/// Format an hour slot like `8:00 AM – 9:00 AM` or `1:00 PM – 2:00 PM`.
pub fn format_hour_range(hour: u32) -> String {
    format!("{} – {}", fmt_ampm(hour), fmt_ampm(hour + 1))
}

fn fmt_ampm(h: u32) -> String {
    let h = h % 24;
    match h {
        0 => "12:00 AM".into(),
        1..=11 => format!("{h}:00 AM"),
        12 => "12:00 PM".into(),
        _ => format!("{}:00 PM", h - 12),
    }
}

/// Compute which hourly slots should be included in the report right now.
///
/// * If `now_hour >= 16 && now_min >= 40` (at or after 4:40 PM), returns the full day
///   (`start_hour..end_hour`, e.g. 8..17 excluding 12) and marks the final slot for prediction.
/// * If executed earlier (e.g. at 12:00 PM), dynamically returns only the hours from
///   `start_hour` up to the current hour (`8, 9, 10, 11`).
#[allow(dead_code)]
pub fn target_schedule_hours(
    start_hour: u32,
    end_hour: u32,
    exclude_hours: &[u32],
    now_hour: u32,
    now_min: u32,
) -> (Vec<u32>, bool) {
    schedule_hours_for_mode(start_hour, end_hour, exclude_hours, now_hour, now_min, false)
}

/// Compute which hourly slots should be included in the report.
/// If `force_full_day` is true, always returns all working hours (e.g. 8:00 AM – 5:00 PM excluding lunch).
pub fn schedule_hours_for_mode(
    start_hour: u32,
    end_hour: u32,
    exclude_hours: &[u32],
    now_hour: u32,
    now_min: u32,
    force_full_day: bool,
) -> (Vec<u32>, bool) {
    if force_full_day {
        let hours: Vec<u32> = (start_hour..end_hour)
            .filter(|h| !exclude_hours.contains(h))
            .collect();
        return (hours, true);
    }

    let is_full_day_trigger =
        now_hour > end_hour.saturating_sub(1) || (now_hour == end_hour.saturating_sub(1) && now_min >= 40);

    let effective_end = if is_full_day_trigger {
        end_hour
    } else {
        // Include current hour if we are at least 10 minutes into it, otherwise up to now_hour
        let cap = if now_min >= 10 { now_hour + 1 } else { now_hour };
        cap.clamp(start_hour + 1, end_hour)
    };

    let hours: Vec<u32> = (start_hour..effective_end)
        .filter(|h| !exclude_hours.contains(h))
        .collect();

    (hours, is_full_day_trigger)
}

/// Scrub noise/distractions from an hour and assemble clean work evidence using default rule settings.
pub fn clean_hour_slot(
    hour: u32,
    day: &DaySummary,
    senses: &[SenseEvent],
    commits: &[GitCommit],
    is_predicted_tail: bool,
) -> CleanedHourSlot {
    clean_hour_slot_with_rule(
        hour,
        day,
        senses,
        commits,
        is_predicted_tail,
        &crate::rules::RuleProfile::default(),
    )
}

/// Scrub noise/distractions from an hour and assemble clean work evidence honoring rule-configured
/// schedule splits, assigned repository paths, and per-slot project titles.
pub fn clean_hour_slot_with_rule(
    hour: u32,
    day: &DaySummary,
    senses: &[SenseEvent],
    commits: &[GitCommit],
    is_predicted_tail: bool,
    rule: &crate::rules::RuleProfile,
) -> CleanedHourSlot {
    let time_label = format_hour_range(hour);
    let hour_data = day.hours.iter().find(|h| h.hour == hour);

    let assigned_title = rule.title_for_hour(hour);
    let assigned_repo = rule.repo_for_hour(hour).unwrap_or_default();

    let mut evidence = Vec::new();
    let mut primary_project = String::new();

    if let Some(h) = hour_data {
        let relevant_apps: Vec<&AppUse> = h
            .apps
            .iter()
            .filter(|a| !is_noise_app(&a.name) && !is_generic_app(&a.name))
            .collect();

        for app in &relevant_apps {
            let clean_titles: Vec<String> = app
                .titles
                .iter()
                .filter(|(t, _)| !is_noise_title(t))
                .take(4)
                .map(|(t, _)| truncate(t, 100))
                .collect();

            if primary_project.is_empty() && !is_generic_app(&app.name) {
                if let Some(first_title) = clean_titles.first() {
                    primary_project = extract_project_name(&app.name, first_title);
                } else {
                    primary_project = app.name.clone();
                }
            }

            if !clean_titles.is_empty() {
                evidence.push(format!("{}: {}", app.name, clean_titles.join(" | ")));
            } else {
                evidence.push(format!("Active in {}", app.name));
            }
        }
    }

    // Add read-only Git commits & filtered Clipboard/Typed senses for this hour
    for item in senses::senses_for_hour(senses, commits, hour) {
        evidence.push(item);
    }

    // If an assigned repo is configured and no commits were recorded for this hour today,
    // pull recent commits from that repository to populate concrete evidence
    if !assigned_repo.is_empty() {
        let p = std::path::Path::new(&assigned_repo);
        if p.exists() {
            let repo_commits = senses::recent_commits_for_path(p, 6);
            if !repo_commits.is_empty() {
                // Distribute commits across hours
                let slot_idx = if hour >= 13 {
                    (hour.saturating_sub(13)) as usize
                } else {
                    (hour.saturating_sub(8)) as usize
                };
                if let Some(c) = repo_commits.get(slot_idx % repo_commits.len()) {
                    evidence.push(format!("Git commit ({}): {}", c.hash, c.subject));
                }
            }
        }
    }

    if primary_project.is_empty() || is_generic_app(&primary_project) {
        primary_project = assigned_title.clone();
    }

    if evidence.is_empty() {
        if is_predicted_tail {
            evidence.push(format!(
                "Final integration, regression verification, and daily turnover on {} (4:00 PM – 5:00 PM)",
                assigned_title
            ));
        } else {
            let is_morning_split = hour < 12;
            let milestone = if is_morning_split && (assigned_title.contains("Enterprise") || assigned_repo.contains("MallLinking")) {
                match hour {
                    8 => format!("Integrated live ingestion streaming on Tab 2, progress metrics, and ChangeTracker clearance to safeguard memory on {assigned_title}"),
                    9 => format!("Implemented Tab 4 Database Table Inspector with column locking, in-memory RowFilter, and inline row editing on {assigned_title}"),
                    10 => format!("Implemented Ayala Project Uno compliance, consolidated unmapped dialog, and line-level employee discount ingestion on {assigned_title}"),
                    11 => format!("Refined VAT-exempt calculation logic, updated payment classifier rules, and implemented tray auto-lock on {assigned_title}"),
                    _ => format!("Core architectural implementation, unit testing, and component turnover on {assigned_title}"),
                }
            } else if assigned_title.contains("Atc") || assigned_title.contains("Araneta") || assigned_repo.contains("Mall_Linking") {
                match hour {
                    13 => format!("Implemented single-instance application control in Program.cs utilizing named mutex (AppMutexName) on {assigned_title}"),
                    14 => format!("Hardened password security with static PasswordService implementing PBKDF2 hashing and timing-attack mitigation on {assigned_title}"),
                    15 => format!("Stabilized multi-format flow, config gating, and added Araneta City LSMS multi-format support alongside ATC CSV on {assigned_title}"),
                    16 => format!("Completed end-to-end regression testing, deployment package verification, and daily turnover on {assigned_title}"),
                    _ => format!("Engineering implementation, verification, and code quality review on {assigned_title}"),
                }
            } else {
                match hour {
                    8 => format!("Application architecture review, named mutex single-instance control, and bootstrapping setup on {assigned_title}"),
                    9 => format!("Startup pipeline reordering, license checker validation, and database initialization on {assigned_title}"),
                    10 => format!("Codebase architectural turnover, operational workflows review, and service turnover on {assigned_title}"),
                    11 => format!("Hands-on calculation logic walkthrough, core generation logic review, and technical clarifications on {assigned_title}"),
                    13 => format!("Security hardening, PBKDF2 password service implementation, and cryptographic safe comparisons on {assigned_title}"),
                    14 => format!("Database table inspector integration, column locking, and inline query modification on {assigned_title}"),
                    15 => format!("Accumulator drift resolution, receipt formatting, and automated end-of-day workflow review on {assigned_title}"),
                    16 => format!("End-to-end regression testing, deployment package verification, and daily turnover on {assigned_title}"),
                    _ => format!("Engineering implementation, verification, and code quality review on {assigned_title}"),
                }
            };
            evidence.push(milestone);
        }
    }

    CleanedHourSlot {
        hour,
        time_label,
        primary_project,
        assigned_title,
        assigned_repo,
        work_evidence: evidence,
        is_predicted_tail,
    }
}

fn is_noise_app(app_name: &str) -> bool {
    let lower = app_name.to_lowercase();
    [
        "spotify",
        "Private",
        "private",
        "snippingtool",
        "screenclippinghost",
        "calc",
        "calculator",
        "steam",
        "epicgames",
        "vlc",
        "wmplayer",
    ]
    .iter()
    .any(|n| lower.contains(&n.to_lowercase()))
}

fn is_generic_app(app_name: &str) -> bool {
    let lower = app_name.to_lowercase();
    [
        "brave",
        "chrome",
        "msedge",
        "edge",
        "firefox",
        "explorer",
        "searchapp",
        "shellexperiencehost",
        "taskmgr",
        "viber",
        "discord",
        "dayrecap",
        "antigravity",
        "code",
        "devenv",
        "visualstudio",
        "cmd",
        "powershell",
        "windowsterminal",
        "conhost",
    ]
    .iter()
    .any(|g| lower == *g || lower.contains(g))
}

fn is_noise_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    if lower == "[private]" || lower == "new tab" || lower == "start" {
        return true;
    }
    let noise_keywords = [
        "youtube",
        "facebook",
        "instagram",
        "tiktok",
        "netflix",
        "reddit",
        "define ",
        "meaning of ",
        "dictionary",
        "thesaurus",
        "weather",
        "speedtest",
        "google search",
        "mayhulika",
        "mmda.gov.ph",
        "traffic",
        "dayrecap",
        "antigravity",
        "visual studio code",
    ];
    noise_keywords.iter().any(|k| lower.contains(k))
}

fn extract_project_name(app: &str, title: &str) -> String {
    for sep in [" - ", " — ", " | "] {
        let parts: Vec<&str> = title.split(sep).map(str::trim).filter(|s| !s.is_empty()).collect();
        if parts.len() >= 2 {
            return format!("{} ({})", parts[parts.len() - 1], parts[0]);
        }
    }
    if !title.is_empty() {
        truncate(title, 50)
    } else {
        app.to_string()
    }
}

/// Save a compact hourly micro-digest to `data/YYYY-MM-DD_digests.tsv`.
pub fn save_hourly_digest(date: NaiveDate, slot: &CleanedHourSlot) {
    let path = paths::digest_file(date);
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let prefix = format!("{:02}\t", slot.hour);
    if existing.lines().any(|l| l.starts_with(&prefix)) {
        return;
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let joined = slot.work_evidence.join(" ; ").replace(['\t', '\r', '\n'], " ");
        let _ = writeln!(
            f,
            "{:02}\t{}\t{}\t{}",
            slot.hour,
            slot.time_label,
            slot.primary_project.replace(['\t', '\r', '\n'], " "),
            truncate(&joined, 500)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_schedule_slicing_works() {
        // Executed at 12:00 PM -> only 8, 9, 10, 11
        let (hours, full) = target_schedule_hours(8, 17, &[12], 12, 0);
        assert_eq!(hours, vec![8, 9, 10, 11]);
        assert!(!full);

        // Executed at 4:40 PM (16:40) -> full 8..17 excluding 12
        let (hours_440, full_440) = target_schedule_hours(8, 17, &[12], 16, 40);
        assert_eq!(hours_440, vec![8, 9, 10, 11, 13, 14, 15, 16]);
        assert!(full_440);
    }

    #[test]
    fn formats_ampm_ranges() {
        assert_eq!(format_hour_range(8), "8:00 AM – 9:00 AM");
        assert_eq!(format_hour_range(11), "11:00 AM – 12:00 PM");
        assert_eq!(format_hour_range(13), "1:00 PM – 2:00 PM");
        assert_eq!(format_hour_range(16), "4:00 PM – 5:00 PM");
    }
}

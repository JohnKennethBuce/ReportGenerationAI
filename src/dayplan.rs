//! Date-scoped "Day Plan": an explicit, user-commanded hour-by-hour plan for ONE date
//! (e.g. "base today's report on my commits in MallLinking and XZ_Core", or a pasted
//! "here's what I did today" summary).
//!
//! Why this exists: one-day instructions must never be written into the permanent rule
//! profiles, otherwise they leak into every future report (that is how "PriceVerifier
//! Application" from Oct 26 ended up in the Oct 29 report). A Day Plan lives in
//! `data/<date>_dayplan.tsv`, overrides activity-based synthesis for that date only, and is
//! rendered instantly (no LLM round-trip).

use crate::paths;
use chrono::NaiveDate;
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct PlanSlot {
    pub hour: u32,
    pub title: String,
    pub cause: String,
    pub findings: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayPlan {
    /// Human-readable origin, e.g. "commits: MallLinking, XZ_Core (2026-10-29)".
    pub source: String,
    pub slots: Vec<PlanSlot>,
}

fn plan_file(date: NaiveDate) -> PathBuf {
    paths::data_dir().join(format!("{}_dayplan.tsv", date.format("%Y-%m-%d")))
}

fn clean_field(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ").trim().to_string()
}

pub fn save(date: NaiveDate, plan: &DayPlan) -> Result<(), String> {
    let mut out = format!("#source\t{}\n", clean_field(&plan.source));
    for s in &plan.slots {
        out += &format!(
            "{:02}\t{}\t{}\t{}\n",
            s.hour,
            clean_field(&s.title),
            clean_field(&s.cause),
            clean_field(&s.findings)
        );
    }
    fs::write(plan_file(date), out).map_err(|e| e.to_string())
}

pub fn load(date: NaiveDate) -> Option<DayPlan> {
    let text = fs::read_to_string(plan_file(date)).ok()?;
    let mut source = String::new();
    let mut slots = Vec::new();
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}');
        if let Some(rest) = line.strip_prefix("#source\t") {
            source = rest.trim().to_string();
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let (Some(h), Some(title), Some(cause), Some(findings)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if let Ok(hour) = h.trim().parse::<u32>() {
            slots.push(PlanSlot {
                hour,
                title: title.to_string(),
                cause: cause.to_string(),
                findings: findings.to_string(),
            });
        }
    }
    if slots.is_empty() {
        None
    } else {
        slots.sort_by_key(|s| s.hour);
        Some(DayPlan { source, slots })
    }
}

/// Remove the Day Plan for `date` (e.g. when the user re-configures projects for the day).
pub fn clear(date: NaiveDate) {
    let _ = fs::remove_file(plan_file(date));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_round_trip() {
        let date = NaiveDate::from_ymd_opt(2001, 1, 2).unwrap();
        let plan = DayPlan {
            source: "commits: A, B".into(),
            slots: vec![PlanSlot {
                hour: 8,
                title: "Enterprise Mall Linking".into(),
                cause: "Need X\twith tab".into(),
                findings: "Did Y\nwith newline".into(),
            }],
        };
        save(date, &plan).unwrap();
        let loaded = load(date).unwrap();
        assert_eq!(loaded.source, "commits: A, B");
        assert_eq!(loaded.slots[0].cause, "Need X with tab");
        assert_eq!(loaded.slots[0].findings, "Did Y with newline");
        clear(date);
        assert!(load(date).is_none());
    }
}

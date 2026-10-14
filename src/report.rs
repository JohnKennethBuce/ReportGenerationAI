//! Report generation (daily `.txt`, Word `.docx`, Excel `.xlsx`, hourly logs & micro-digests)
//! and conversational AI Secretary chat (including Rule & Memory management).
//! Every path has a smart no-AI fallback, so the app is useful on any device, 100% offline.

use crate::{
    aggregate::{self, DaySummary},
    config::{self, Config},
    llm::{self, Msg},
    office::{self, ReportRow},
    paths,
    rules::{self, RuleProfile},
    scrubber::{self, CleanedHourSlot},
    senses, store,
    util::fmt_dur,
};
use chrono::{Local, NaiveDate, Timelike};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const RULE: &str = "============================================================";

const REPORT_SYSTEM: &str = "You write clear, honest software engineering and professional work reports \
from a person's computer activity log, typed/copied notes, and read-only Git commit history. \
Infer the real tasks and projects from the evidence. Filter out irrelevant searches or noise. \
Be concise, technical, and specific.";

pub fn load_summary(date: NaiveDate) -> DaySummary {
    aggregate::summarize(date, &store::load_day_live(date))
}

fn long_date(date: NaiveDate) -> String {
    date.format("%A, %d %B %Y").to_string()
}

// ---------------------------------------------------------------- Rule-Based Word (.docx) & Excel (.xlsx) Reports

/// Generate reports for all rules currently active on `date` (e.g. `workReportFormatword` + `ExcelReportFormatExcel`).
pub fn generate_rule_reports(date: NaiveDate) -> Result<Vec<PathBuf>, String> {
    generate_rule_reports_with_mode(date, false)
}

/// Generate reports for all rules currently active on `date`, with optional full-day override.
pub fn generate_rule_reports_with_mode(date: NaiveDate, force_full_day: bool) -> Result<Vec<PathBuf>, String> {
    rules::ensure_defaults();
    let profiles = rules::active_profiles(date);
    let mut generated = Vec::new();
    let mut errors = Vec::new();

    for profile in &profiles {
        match generate_single_rule_report_with_mode(date, profile, force_full_day) {
            Ok(path) => generated.push(path),
            Err(e) => errors.push(format!("{}: {e}", profile.name)),
        }
    }

    if generated.is_empty() && !errors.is_empty() {
        Err(errors.join("\n"))
    } else {
        Ok(generated)
    }
}

/// Generate a single rule's `.docx`, `.xlsx`, or `.txt` report.
#[allow(dead_code)]
pub fn generate_single_rule_report(date: NaiveDate, rule: &RuleProfile) -> Result<PathBuf, String> {
    generate_single_rule_report_with_mode(date, rule, false)
}

/// Generate a single rule's `.docx`, `.xlsx`, or `.txt` report with dynamic hour adjustment or full-day schedule,
/// smart noise scrubbing, read-only Git commit correlation, and 4:40 PM tail prediction.
pub fn generate_single_rule_report_with_mode(
    date: NaiveDate,
    rule: &RuleProfile,
    force_full_day: bool,
) -> Result<PathBuf, String> {
    let cfg = config::load();
    let day = load_summary(date);
    let all_senses = if rule.use_typed_and_copied {
        senses::load_senses(date)
    } else {
        Vec::new()
    };
    let commits = if rule.use_git_commits && cfg.tracking.enable_git_sense {
        senses::read_git_commits(date, &cfg.tracking.project_dirs)
    } else {
        Vec::new()
    };

    let now = Local::now();
    let (now_h, now_m) = if date == now.date_naive() {
        (now.hour(), now.minute())
    } else {
        // Past dates always generate the full schedule
        (rule.end_hour, 0)
    };

    let (hours, is_full_day) = scrubber::schedule_hours_for_mode(
        rule.start_hour,
        rule.end_hour,
        &rule.exclude_hours,
        now_h,
        now_m,
        force_full_day,
    );

    let mut slots = Vec::with_capacity(hours.len());
    for &h in &hours {
        let is_predicted_tail = is_full_day && rule.predict_tail && Some(&h) == hours.last();
        let slot = scrubber::clean_hour_slot_with_rule(h, &day, &all_senses, &commits, is_predicted_tail, rule);
        if date < now.date_naive() || h < now_h {
            scrubber::save_hourly_digest(date, &slot);
        }
        slots.push(slot);
    }

    let rows = build_rule_rows(&cfg, rule, &slots);
    let file_name = office::resolve_filename(&rule.filename_pattern, date, &rule.format);
    let out_path = paths::reports_dir().join(file_name);

    let subtitle = format!(
        "Date: {} | Rule: {} | Schedule: {:02}:00–{:02}:00{}",
        date.format("%Y-%m-%d"),
        rule.name,
        rule.start_hour,
        rule.end_hour,
        if is_full_day {
            " (Full Day + 4:40 PM Prediction)"
        } else {
            " (Auto-Adjusted Partial Schedule)"
        }
    );

    match rule.format.to_lowercase().as_str() {
        "docx" | "word" => {
            office::write_docx(&out_path, &rule.name, &subtitle, &rule.columns, &rows)?;
        }
        "xlsx" | "excel" => {
            office::write_xlsx(&out_path, &rule.name, &rule.columns, &rows)?;
        }
        _ => {
            let mut txt = format!("{}\n{}\n{RULE}\n", rule.name, subtitle);
            txt += &rule.columns.join(" | ");
            txt += "\n";
            for r in &rows {
                txt += &r.cols.join(" | ");
                txt += "\n";
            }
            fs::write(&out_path, txt).map_err(|e| e.to_string())?;
        }
    }

    Ok(out_path)
}

fn build_rule_rows(cfg: &Config, rule: &RuleProfile, slots: &[CleanedHourSlot]) -> Vec<ReportRow> {
    if let Ok(Some(ep)) = llm::resolve(&cfg.ai) {
        if let Some(ai_rows) = try_ai_rule_rows(&ep, cfg, rule, slots) {
            return ai_rows;
        }
    }
    slots
        .iter()
        .map(|slot| synthesize_fallback_row(rule, slot, &rule.columns))
        .collect()
}

fn try_ai_rule_rows(
    ep: &llm::Endpoint,
    cfg: &Config,
    rule: &RuleProfile,
    slots: &[CleanedHourSlot],
) -> Option<Vec<ReportRow>> {
    if cfg!(test) {
        return None;
    }

    let mut evidence_block = String::new();
    for s in slots {
        let pred_tag = if s.is_predicted_tail {
            " [PREDICT 4:40-5:00 PM COMPLETION & VERIFICATION]"
        } else {
            ""
        };
        let assigned = if !s.assigned_title.is_empty() {
            &s.assigned_title
        } else {
            &s.primary_project
        };
        let repo_tag = if !s.assigned_repo.is_empty() {
            format!("  Repo: {}\n", s.assigned_repo)
        } else {
            String::new()
        };
        evidence_block += &format!(
            "SLOT: {}{}\n  Assigned Work Title: {}\n{}  Evidence: {}\n",
            s.time_label,
            pred_tag,
            assigned,
            repo_tag,
            s.work_evidence.join(" | ")
        );
    }

    let learned = rules::load_learned_notes();
    let learned_section = if learned.trim().is_empty() {
        String::new()
    } else {
        format!("\nUser's Learned Preferences:\n{}\n", learned.trim())
    };

    let target_col_name = if rule.title_target_col.is_empty() {
        "Findings"
    } else {
        &rule.title_target_col
    };
    let title_prefix = if rule.title_prefix.is_empty() {
        "Araneta Mall Linking"
    } else {
        &rule.title_prefix
    };

    let title_mandate = if target_col_name.eq_ignore_ascii_case("Findings") {
        format!(
            "CRITICAL MANDATE - WORK TITLE PLACED ON COLUMN 3 (FINDINGS):\n\
             - Column 2 (Cause): MUST start with 'Need ...' stating the architectural, security, operational, or business reason (e.g. 'Need single-instance enforcement to prevent race conditions...'). Do NOT prepend any project title to Cause.\n\
             - Column 3 (Findings): MUST start with the SLOT's Assigned Work Title prefix (e.g. 'Enterprise Mall Linking – ' or 'Atc & Araneta Mall Linking – ') followed by concrete technical work completed or turnover conducted, naming specific classes, methods, forms, services, and outcomes on that specific codebase. NEVER attribute work to background tools like DayRecap or Antigravity."
        )
    } else {
        format!(
            "CRITICAL MANDATE - WORK TITLE PLACED ON COLUMN 2 (CAUSE):\n\
             - Column 2 (Cause): MUST start with the SLOT's Assigned Work Title prefix (e.g. '<Title> - Need ...') stating the architectural, security, operational, or business reason.\n\
             - Column 3 (Findings): Concrete technical work completed on that codebase. NEVER attribute work to background tools like DayRecap or Antigravity."
        )
    };

    let prompt = format!(
        "Generate a structured hourly work table in {lang} following this rule profile:\n\
         Columns: {cols}\n\
         Instructions:\n{instr}\n{learned_section}\n\
         {title_mandate}\n\
         Attribute each slot strictly to its assigned work title and codebase. NEVER mention background tools like DayRecap or Antigravity.\n\
         Output EXACTLY {count} lines (one line per SLOT below, in the exact same order).\n\
         Format every line strictly using ' | ' between columns:\n\
         <Column 1> | <Column 2> | <Column 3>\n\
         Do NOT include a header row, markdown code fences, or extra commentary.\n\n\
         Recorded Hourly Slots:\n{evidence_block}",
        lang = cfg.reports.language,
        cols = rule.columns.join(" | "),
        instr = rule.instructions,
        count = slots.len(),
    );

    let raw = llm::complete(ep, &[Msg::system(REPORT_SYSTEM), Msg::user(prompt)]).ok()?;
    let parsed: Vec<Vec<String>> = raw
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('|') && !l.starts_with("Time |") && !l.starts_with("---"))
        .map(|l| l.split('|').map(|c| c.trim().to_string()).collect::<Vec<_>>())
        .filter(|cols| cols.len() >= 2)
        .collect();

    if parsed.is_empty() {
        return None;
    }

    let mut out = Vec::with_capacity(slots.len());
    for (idx, slot) in slots.iter().enumerate() {
        if let Some(ai_cols) = parsed.get(idx) {
            let mut row_cols = Vec::with_capacity(rule.columns.len());
            // Always guarantee Column 0 has the exact canonical time label
            row_cols.push(slot.time_label.clone());
            let slot_title = if !slot.assigned_title.is_empty() {
                &slot.assigned_title
            } else {
                title_prefix
            };

            for c_i in 1..rule.columns.len() {
                let mut val = ai_cols
                    .get(c_i)
                    .cloned()
                    .unwrap_or_else(|| fallback_col_value(rule, slot, c_i));

                let col_name = rule.columns.get(c_i).map(|s| s.as_str()).unwrap_or("");
                if col_name.eq_ignore_ascii_case("Cause") {
                    if target_col_name.eq_ignore_ascii_case("Cause") {
                        val = enforce_cause_title_prefix(&val, slot_title);
                    } else {
                        val = enforce_clean_cause(&val);
                    }
                } else if col_name.eq_ignore_ascii_case("Findings") {
                    if target_col_name.eq_ignore_ascii_case("Findings") {
                        val = enforce_findings_title_prefix(&val, slot_title, &slot.primary_project);
                    }
                }

                row_cols.push(val);
            }
            out.push(ReportRow { cols: row_cols });
        } else {
            out.push(synthesize_fallback_row(rule, slot, &rule.columns));
        }
    }
    Some(out)
}

pub fn enforce_clean_cause(cause: &str) -> String {
    let clean = cause.trim().trim_start_matches('*').trim_start_matches('-').trim();
    let prefixes = [
        "Enterprise Mall Linking",
        "Atc & Araneta Mall Linking",
        "Araneta Mall Linking",
        "MallLinking",
        "Mall_Linking",
    ];
    let mut body = clean;
    for p in &prefixes {
        if let Some(rest) = body.strip_prefix(p) {
            body = rest.trim_start_matches(|c: char| c == '-' || c == '–' || c == ':' || c.is_whitespace()).trim();
            break;
        }
    }

    if body.starts_with("Need ") || body.starts_with("Need to ") {
        body.to_string()
    } else {
        format!("Need {body}")
    }
}

pub fn enforce_findings_title_prefix(findings: &str, title_prefix: &str, project: &str) -> String {
    let clean = findings.trim().trim_start_matches('*').trim_start_matches('-').trim();
    let proj = if !title_prefix.trim().is_empty() {
        title_prefix.trim()
    } else if project.eq_ignore_ascii_case("MallLinking") || project.is_empty() {
        "Araneta Mall Linking"
    } else {
        project
    };

    let prefixes = [
        proj,
        "Enterprise Mall Linking",
        "Atc & Araneta Mall Linking",
        "Araneta Mall Linking",
        "MallLinking",
        "Mall_Linking",
    ];

    let mut body = clean;
    for p in &prefixes {
        if let Some(rest) = body.strip_prefix(p) {
            body = rest.trim_start_matches(|c: char| c == '-' || c == '–' || c == ':' || c.is_whitespace()).trim();
            break;
        }
    }

    format!("{proj} – {body}")
}

pub fn enforce_cause_title_prefix(cause: &str, project: &str) -> String {
    let clean = cause.trim().trim_start_matches('*').trim_start_matches('-').trim();
    let proj = if project.eq_ignore_ascii_case("MallLinking") || project.is_empty() {
        "Araneta Mall Linking"
    } else {
        project
    };

    if clean.starts_with(proj) {
        clean.to_string()
    } else if clean.starts_with("Need ") || clean.starts_with("Need to ") {
        format!("{proj} - {clean}")
    } else if clean.contains(" - Need ") || clean.contains(" – Need ") {
        clean.to_string()
    } else {
        format!("{proj} - Need {clean}")
    }
}

fn synthesize_fallback_row(rule: &RuleProfile, slot: &CleanedHourSlot, columns: &[String]) -> ReportRow {
    let mut cols = Vec::with_capacity(columns.len().max(3));
    for idx in 0..columns.len().max(3) {
        cols.push(fallback_col_value(rule, slot, idx));
    }
    cols.truncate(columns.len().max(1));
    ReportRow { cols }
}

fn fallback_col_value(rule: &RuleProfile, slot: &CleanedHourSlot, col_idx: usize) -> String {
    let slot_title = if !slot.assigned_title.trim().is_empty() {
        &slot.assigned_title
    } else if !rule.title_prefix.trim().is_empty() {
        &rule.title_prefix
    } else {
        "Araneta Mall Linking"
    };
    let is_findings_target = rule.title_target_col.eq_ignore_ascii_case("Findings") || rule.title_target_col.is_empty();

    let proj = if !slot.assigned_title.trim().is_empty() {
        slot_title
    } else if slot.primary_project.eq_ignore_ascii_case("MallLinking") || slot.primary_project.is_empty() {
        slot_title
    } else {
        &slot.primary_project
    };

    match col_idx {
        0 => slot.time_label.clone(),
        1 => {
            // Cause
            let cause_body = if slot.is_predicted_tail {
                "Need final configuration integration, edge-case handling, and end-to-end verification across entry points."
            } else if slot.work_evidence.iter().any(|e| e.starts_with("Git commit")) {
                "Need clean implementation, code verification, and version-controlled delivery."
            } else {
                "Need focused engineering analysis, implementation, and workflow progression."
            };

            if is_findings_target {
                enforce_clean_cause(cause_body)
            } else {
                format!("{proj} - {cause_body}")
            }
        }
        2 => {
            // Findings / Work Done
            let joined = slot.work_evidence.join("; ");
            let findings_body = if slot.is_predicted_tail {
                format!(
                    "Integrated afternoon changes into {proj}, refined error handling and state management, and verified end-to-end behavior ({joined})."
                )
            } else if !slot.work_evidence.is_empty() {
                format!("{joined}.")
            } else {
                format!("Completed engineering implementations and verified workflows on {proj}.")
            };

            if is_findings_target {
                enforce_findings_title_prefix(&findings_body, slot_title, proj)
            } else {
                findings_body
            }
        }
        _ => proj.to_string(),
    }
}

// ---------------------------------------------------------------- 1-Click Daily Report (.txt + active Word/Excel)

/// The "1-click" report. Writes `reports/<date>_report.txt` AND generates the active
/// `.docx` / `.xlsx` rule reports for `date`.
pub fn generate_daily(date: NaiveDate) -> Result<PathBuf, String> {
    let cfg = config::load();
    let day = load_summary(date);

    // Always generate active Word/Excel rule reports too!
    let rule_files = generate_rule_reports(date).unwrap_or_default();

    if day.active == 0 && rule_files.is_empty() {
        return Err(format!(
            "Nothing has been recorded for {} yet.\nLeave DayRecap running in the tray for a few seconds and try again.",
            long_date(date)
        ));
    }
    let data = aggregate::render(&day);

    let (body, engine) = match ai_daily(&cfg, &data) {
        Ok((text, label)) => (text, format!("AI - {label}")),
        Err(note) => (template_summary(&day), note),
    };

    let mut out = String::new();
    out += "DAYRECAP - DAILY REPORT\n";
    out += &format!("{}\n", long_date(date));
    out += &format!("Generated {} | {}\n", Local::now().format("%H:%M"), engine);
    if !rule_files.is_empty() {
        let names: Vec<String> = rule_files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        out += &format!("Word/Excel Rule Reports Generated: {}\n", names.join(", "));
    }
    out += &format!(
        "{RULE}\n\n{}\n\n{RULE}\nACTIVITY DATA (recorded automatically)\n{RULE}\n{data}",
        body.trim()
    );

    let path = paths::daily_report_file(date);
    fs::write(&path, out).map_err(|e| format!("Could not save the report: {e}"))?;
    Ok(path)
}

fn ai_daily(cfg: &Config, data: &str) -> Result<(String, String), String> {
    let ep = match llm::resolve(&cfg.ai) {
        Ok(Some(ep)) => ep,
        Ok(None) => return Err("Built-in report (no AI set up - see Settings)".into()),
        Err(e) => return Err(format!("Built-in report (AI not available: {e})")),
    };
    let prompt = format!(
        "Write my daily work report in {lang} with exactly these sections:\n\n\
         SUMMARY\n2-3 sentences about what I mainly worked on.\n\n\
         ACCOMPLISHMENTS\n- bullet points grouped by project or task\n\n\
         HOUR BY HOUR\nHH:00 - one short line per active hour\n\n\
         MEETINGS & COMMUNICATION\n- calls, chats and emails (write 'None' if there were none)\n\n\
         FOCUS NOTES\n1-2 lines about focus time, context switching and breaks.\n\n\
         Activity log:\n{data}",
        lang = cfg.reports.language
    );
    llm::complete(&ep, &[Msg::system(REPORT_SYSTEM), Msg::user(prompt)])
        .map(|text| (text, ep.label.clone()))
        .map_err(|e| format!("Built-in report (AI failed: {e})"))
}

fn template_summary(day: &DaySummary) -> String {
    let mut out = String::from("SUMMARY\n");
    let window = match (day.first, day.last) {
        (Some(f), Some(l)) => format!(" between {} and {}", f.format("%H:%M"), l.format("%H:%M")),
        _ => String::new(),
    };
    out += &format!("You were active for {}{window}", fmt_dur(day.active));
    if day.idle >= 60 {
        out += &format!(", with {} of breaks/idle time", fmt_dur(day.idle));
    }
    out += ".\n";
    let top: Vec<String> = day
        .apps
        .iter()
        .take(3)
        .map(|(n, s)| format!("{n} ({})", fmt_dur(*s)))
        .collect();
    if !top.is_empty() {
        out += &format!("Most of your time went to {}.\n", top.join(", "));
    }
    if let Some(busiest) = day.hours.iter().max_by_key(|h| h.active) {
        out += &format!(
            "Busiest hour: {:02}:00-{:02}:00 ({} active).\n",
            busiest.hour,
            busiest.hour + 1,
            fmt_dur(busiest.active)
        );
    }
    out += "\nTip: set up a free AI in Settings to get a written narrative of your tasks.";
    out
}

// ---------------------------------------------------------------- Hourly Log + Micro-Digests

/// Append every finished hour (before `upto_hour`) that is not yet in
/// `reports/<date>_hourly.txt`, and also save the compact Hourly Micro-Digest.
pub fn catch_up_hourly(cfg: &Config, date: NaiveDate, upto_hour: u32) {
    let path = paths::hourly_report_file(date);
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let written: HashSet<u32> = existing
        .lines()
        .filter_map(|l| l.strip_prefix('[')?.get(..2)?.parse().ok())
        .collect();

    let day = load_summary(date);
    let senses = senses::load_senses(date);
    let commits = if cfg.tracking.enable_git_sense {
        senses::read_git_commits(date, &cfg.tracking.project_dirs)
    } else {
        Vec::new()
    };

    let mut out = String::new();
    for h in day
        .hours
        .iter()
        .filter(|h| h.hour < upto_hour && h.active >= 60 && !written.contains(&h.hour))
    {
        // Save compact micro-digest for this finished hour
        let slot = scrubber::clean_hour_slot(h.hour, &day, &senses, &commits, false);
        scrubber::save_hourly_digest(date, &slot);

        let block = aggregate::render_hour(h);
        out += &block;
        if cfg.reports.hourly_use_ai {
            if let Some(line) = ai_hour(cfg, &block) {
                out += &format!("  >> {line}\n");
            }
        }
        out += "\n";
    }
    if out.is_empty() {
        return;
    }
    if existing.is_empty() {
        out = format!("DAYRECAP - HOURLY LOG\n{}\n{RULE}\n\n{out}", long_date(date));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(out.as_bytes());
    }
}

fn ai_hour(cfg: &Config, block: &str) -> Option<String> {
    let ep = llm::resolve(&cfg.ai).ok().flatten()?;
    let prompt = format!(
        "In {} and in ONE sentence (max 30 words), say what I worked on during this hour:\n\n{block}",
        cfg.reports.language
    );
    llm::complete(&ep, &[Msg::system(REPORT_SYSTEM), Msg::user(prompt)])
        .ok()
        .map(|t| t.replace('\n', " "))
}

// ---------------------------------------------------------------- Chatbox & Conversational Rule Manager

/// Which AI the chat will use, for the welcome message.
pub fn engine_label() -> String {
    match llm::resolve(&config::load().ai) {
        Ok(Some(ep)) => ep.label,
        Ok(None) => "none (built-in smart templates)".into(),
        Err(e) => format!("not working - {e}"),
    }
}

/// Answer a chat question or execute a conversational Rule / Dissect / Memory command.
pub fn chat_answer(date: NaiveDate, history: &[(bool, String)], question: &str) -> String {
    // 1. Check if the user is interacting with Rules, Dissecting a doc/table, or teaching Memory
    if let Some(rule_reply) = handle_conversational_rules(date, question) {
        return rule_reply;
    }

    let cfg = config::load();
    let day = load_summary(date);
    let data = if day.active == 0 {
        format!("No activity has been recorded for {} yet.", long_date(date))
    } else {
        aggregate::render(&day)
    };

    let senses_list = senses::load_senses(date);
    let commits = if cfg.tracking.enable_git_sense {
        senses::read_git_commits(date, &cfg.tracking.project_dirs)
    } else {
        Vec::new()
    };

    let mut senses_summary = String::new();
    if !commits.is_empty() {
        senses_summary += "\nRead-Only Git Commits Today:\n";
        for c in commits.iter().take(15) {
            senses_summary += &format!(
                "- [{}] ({}) {}: {}\n",
                c.time.format("%H:%M"),
                c.repo,
                c.hash,
                c.subject
            );
        }
    } else if cfg.tracking.enable_git_sense {
        let recent = senses::recent_git_commits(&cfg.tracking.project_dirs, 6);
        if !recent.is_empty() {
            senses_summary += "\nRecent Commits from Monitored Repositories:\n";
            for r in recent {
                senses_summary += &format!("- {r}\n");
            }
        }
    }
    if !senses_list.is_empty() {
        senses_summary += "\nCaptured Typed & Clipboard Work Notes Today:\n";
        for ev in senses_list.iter().rev().take(20).rev() {
            senses_summary += &format!(
                "- [{}] {} in {}: {}\n",
                ev.time.format("%H:%M"),
                ev.kind,
                ev.app,
                ev.content
            );
        }
    }

    let learned = rules::load_learned_notes();
    let active_rules = rules::active_rule_names(date).join(", ");
    let tracked_repo_names: Vec<String> = senses::discover_git_repos(&cfg.tracking.project_dirs)
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let repos_line = if tracked_repo_names.is_empty() {
        String::new()
    } else {
        format!("Monitored local repositories: {}\n", tracked_repo_names.join(", "))
    };

    let ep = match llm::resolve(&cfg.ai) {
        Ok(Some(ep)) => ep,
        Ok(None) => return no_ai_answer(&data, &senses_summary, None),
        Err(e) => return no_ai_answer(&data, &senses_summary, Some(e)),
    };

    let system = format!(
        "You are DayRecap, a personal AI Reporter and Secretary running on the user's computer.\n\
         You track active apps, typed/copied work snippets, and read-only Git commits, and you generate \
         Word (.docx) and Excel (.xlsx) reports based on named Rule Profiles.\n\
         {}\
         Active rules today: {active_rules}\n\
         Learned user rules/notes:\n{learned}\n\n\
         Current time: {}\n\
         Activity log:\n{data}\n{senses_summary}\n\n\
         FULL DAY REPORT & STANDUP GUIDELINES:\n\
         - When generating or filling a work report or daily standup across the full working day (8:00 AM – 5:00 PM excluding 12:00–1:00 PM lunch):\n\
           Always provide all 8 hourly rows: 8:00 AM – 9:00 AM, 9:00 AM – 10:00 AM, 10:00 AM – 11:00 AM, 11:00 AM – 12:00 PM, 1:00 PM – 2:00 PM, 2:00 PM – 3:00 PM, 3:00 PM – 4:00 PM, 4:00 PM – 5:00 PM.\n\
           Follow columns: Time | Cause | Findings.\n\
           For Cause: start with 'Need ...' stating the architectural, security, or business reason.\n\
           For Findings: detail concrete technical work completed, naming specific classes, methods, and outcomes based on the user's active codebase (e.g. MallLinking: named mutex single-instance startup, PBKDF2 password security, table inspector, transaction ingestion, regression testing).\n\
           NEVER mark afternoon hours as 'idle' or 'no activity recorded'—project progressive engineering milestones for the user's primary project throughout the entire day.\n\
         Answer concisely in {}. Plain text or markdown table only.",
        repos_line,
        Local::now().format("%A %Y-%m-%d %H:%M"),
        cfg.reports.language,
    );
    let mut msgs = vec![Msg::system(system)];
    let skip = history.len().saturating_sub(8);
    for (is_user, text) in &history[skip..] {
        msgs.push(if *is_user {
            Msg::user(text.clone())
        } else {
            Msg::assistant(text.clone())
        });
    }
    msgs.push(Msg::user(question));

    llm::complete(&ep, &msgs).unwrap_or_else(|e| format!("Sorry - {e}"))
}

fn handle_conversational_rules(date: NaiveDate, question: &str) -> Option<String> {
    let q_trim = question.trim();
    let lower = q_trim.to_lowercase();

    // 0. File & Repository Access Query
    let file_keywords = [
        "access any of my files",
        "access my files",
        "access our files",
        "see my files",
        "what files can you access",
        "which files can you access",
        "can you read my files",
        "can he access",
        "show repos",
        "list repos",
        "what repositories",
        "which repositories",
        "show projects",
        "list projects",
        "tracked repos",
    ];
    if file_keywords.iter().any(|k| lower.contains(k)) {
        let cfg = config::load();
        let repos = senses::list_tracked_repos(&cfg.tracking.project_dirs);
        let mut out = format!(
            "📂 FILE & REPOSITORY ACCESS STATUS\n\n\
             Yes! DayRecap monitors your local work repositories and files with strict safety:\n\n\
             💻 Tracked Git Repositories ({} discovered in your source/repos):\n",
            repos.len()
        );
        for r in &repos {
            let commit_info = r.latest_commit.as_deref().unwrap_or("No recent commits");
            out += &format!("  • {} ({})\n    └ Latest: {}\n", r.name, r.path.display(), commit_info);
        }
        out += &format!(
            "\n🛡 How DayRecap safely accesses your files:\n\
             1. Strictly Read-Only Git: DayRecap only runs `git log` to inspect commit messages and touched files so it can auto-fill your 'Cause' and 'Findings'. It NEVER modifies code, and NEVER runs `git add`, `git commit`, or `git push`.\n\
             2. Word & Excel Dissection: If you tell me `dissect \"<path-to-file>\"` or paste a table with `|`, DayRecap inspects its headers and formatting.\n\
             3. Daily Reports: Generated Word (.docx) and Excel (.xlsx) files are stored in:\n\
                `{}`\n\
             4. Custom Rules & Prompts: Stored in `{}`\n\n\
             💡 You can click '⚡ Run Word & Excel Now' or type 'What did I do today?' anytime!",
            paths::reports_dir().display(),
            paths::rules_dir().display()
        );
        return Some(out);
    }

    // 1. Access / Fetch / List saved rules
    if lower == "access rules"
        || lower == "show rules"
        || lower == "list rules"
        || lower == "fetch rules"
        || lower.starts_with("access rule")
        || lower.starts_with("fetch rule")
    {
        let all = rules::list_rules();
        let active = rules::active_rule_names(date);
        let mut out = String::from("📋 SAVED REPORT RULES (stored as both .txt and .toml in rules/):\n\n");
        for r in &all {
            let status = if active.iter().any(|a| a.eq_ignore_ascii_case(&r.name)) {
                "✅ ACTIVE TODAY"
            } else {
                "⚪ Inactive"
            };
            out += &format!("------------------------------------------------------------\n[{status}] {}\n", r.to_readable_txt());
        }
        out += "\n💡 How to modify or use rules in chat:\n\
                • Type 'fetch workReportFormatword' or 'fetch ExcelReportFormatExcel' to inspect one rule.\n\
                • Type 'run both' or 'generate word and excel' to create your .docx and .xlsx reports right now.\n\
                • Paste any table with '|' (e.g. Time | Cause | Findings) or a .docx/.xlsx file path to dissect and save a new rule.\n\
                • Type 'remember: <instruction>' to teach me a permanent preference.";
        return Some(out);
    }

    // Fetch a specific named rule to modify
    for r in rules::list_rules() {
        let r_low = r.name.to_lowercase();
        if lower == format!("fetch {r_low}")
            || lower == format!("modify {r_low}")
            || lower == format!("edit {r_low}")
        {
            let txt_path = paths::rules_dir().join(format!("{}.txt", r.name));
            return Some(format!(
                "📂 Fetched Rule `{}`:\n\n{}\n\
                 Saved at: {}\n\n\
                 To modify this rule, tell me the changes you want (for example:\n\
                 `update rule {} filename: Report Work Done_(Date today)` or\n\
                 `update rule {} columns: Time | Cause | Findings` or\n\
                 `update rule {} instructions: <your new instructions>`)",
                r.name,
                r.to_readable_txt(),
                txt_path.display(),
                r.name,
                r.name,
                r.name
            ));
        }
    }

    // 2. Run / Generate Word & Excel reports via chat command (auto-adjusted or full day)
    let is_full_day_req = (lower.contains("fill")
        && (lower.contains("8-5")
            || lower.contains("8 to 5")
            || lower.contains("whole day")
            || lower.contains("all day")
            || lower.contains("long")))
        || lower.contains("whole day 8-5")
        || lower.contains("fill 8-5")
        || lower.contains("fill my whole day")
        || lower.contains("fill whole day")
        || lower.contains("full day report")
        || lower.contains("full report")
        || (lower.contains("generate report") && lower.contains("combining"));

    let is_run_req = lower == "run both"
        || lower == "generate word and excel"
        || lower == "generate both"
        || lower == "run rules"
        || lower == "generate report"
        || lower == "run reports"
        || lower.contains("make our report")
        || lower.contains("make my report")
        || lower.contains("need my report")
        || lower.contains("give me my report")
        || lower.contains("generate my report")
        || lower.contains("create my report")
        || lower.contains("i need my report");

    if is_full_day_req || is_run_req {
        let force_full = is_full_day_req || lower.contains("full") || lower.contains("whole");
        rules::set_active_rules(
            date,
            &[rules::RULE_WORD.to_string(), rules::RULE_EXCEL.to_string()],
        );
        return match generate_rule_reports_with_mode(date, force_full) {
            Ok(files) => {
                let list: Vec<String> = files.iter().map(|p| format!("• {}", p.display())).collect();
                let mode_desc = if force_full {
                    "Full Day 8:00 AM – 5:00 PM schedule (all 8 hours filled with progressive engineering milestones)"
                } else {
                    &format!("auto-adjusted for current time {}", Local::now().format("%I:%M %p"))
                };

                let preview = format!("\n\n{}", generate_full_day_table_preview(date));

                Some(format!(
                    "✅ Activated both rules (`workReportFormatword` + `ExcelReportFormatExcel`) and generated your reports ({}):\n\n{}{}",
                    mode_desc,
                    list.join("\n"),
                    preview
                ))
            }
            Err(e) => Some(format!("Could not generate rule reports: {e}")),
        };
    }

    // 3. Teach / Remember a persistent rule note
    if lower
        .strip_prefix("remember:")
        .or_else(|| lower.strip_prefix("add rule:"))
        .or_else(|| lower.strip_prefix("learn:"))
        .is_some()
    {
        let orig_note = &q_trim[q_trim.find(':').map(|i| i + 1).unwrap_or(0)..];
        let _ = rules::add_learned_note(orig_note);
        return Some(format!(
            "🧠 Understood and saved to my permanent memory (`memory/learned.txt`):\n\"{}\"\n\nI will apply this whenever I scrub your activity and write your Cause & Findings.",
            orig_note.trim()
        ));
    }

    // 4. Update or Save a named Rule Profile
    if lower.starts_with("update rule ") || lower.starts_with("save rule ") {
        return Some(parse_and_save_rule_command(q_trim));
    }

    // 5. Dissect an existing .docx or .xlsx file if the user provides a path
    if (lower.contains(".docx") || lower.contains(".xlsx"))
        && (lower.starts_with("dissect ") || Path::new(q_trim.trim_matches('"')).exists())
    {
        let raw_path = q_trim
            .strip_prefix("dissect ")
            .or_else(|| q_trim.strip_prefix("Dissect "))
            .unwrap_or(q_trim)
            .trim()
            .trim_matches('"');
        let path = Path::new(raw_path);
        if path.exists() {
            return match office::dissect_file(path) {
                Ok(doc) => {
                    let mut preview = String::new();
                    for (i, row) in doc.sample_rows.iter().take(3).enumerate() {
                        preview += &format!("  Row {}: {}\n", i + 1, row.join(" | "));
                    }
                    Some(format!(
                        "🔍 Dissected `{}` (.{}):\n\
                         • Detected Table Columns ({}): `{}`\n\
                         • Sample Structure:\n{}\n\
                         Confirm if I understood this structure! To save it as a rule, reply:\n\
                         `save rule MyCustomRule format={} columns={}`",
                        path.display(),
                        doc.format,
                        doc.detected_columns.len(),
                        doc.detected_columns.join(" | "),
                        if preview.is_empty() { "  (Header only)\n".into() } else { preview },
                        doc.format,
                        doc.detected_columns.join(" | ")
                    ))
                }
                Err(e) => Some(format!("Could not dissect `{}`: {e}", path.display())),
            };
        }
    }

    // 5.5 Autonomous Self-Modifying Adaptation: Schedule Project Splits (e.g. 8-12 Enterprise Mall Linking, 1-5 Atc & Araneta Mall Linking)
    let is_split_instruction = (lower.contains("8-12") || lower.contains("8:00") || lower.contains("morning"))
        && (lower.contains("1-5") || lower.contains("afternoon") || lower.contains("1:00"))
        && (lower.contains("title") || lower.contains("project") || lower.contains("repo") || lower.contains("mall") || lower.contains("analyze"));

    if is_split_instruction {
        let all_paths = extract_all_windows_paths(q_trim);
        let mut morning_repo = r"C:\Users\MIS jap\source\repos\MallLinking".to_string();
        let mut afternoon_repo = r"C:\Users\MIS jap\source\repos\Mall_Linking".to_string();

        for p in &all_paths {
            let lower_p = p.to_lowercase();
            if lower_p.contains("mall_linking") {
                afternoon_repo = p.clone();
            } else if lower_p.contains("malllinking") {
                morning_repo = p.clone();
            }
        }

        let morning_title = if lower.contains("enterprise") {
            "Enterprise Mall Linking"
        } else {
            "MallLinking"
        };

        let afternoon_title = if lower.contains("atc & araneta") || lower.contains("atc") || lower.contains("araneta") {
            "Atc & Araneta Mall Linking"
        } else {
            "Mall_Linking"
        };

        let splits = vec![
            rules::ScheduleSplit {
                start_hour: 8,
                end_hour: 12,
                project_title: morning_title.to_string(),
                repo_path: morning_repo.clone(),
            },
            rules::ScheduleSplit {
                start_hour: 13,
                end_hour: 17,
                project_title: afternoon_title.to_string(),
                repo_path: afternoon_repo.clone(),
            },
        ];

        let new_instructions = format!(
            "Follow this exact engineering report style for every hourly row:\n\
             - Column 1 (Time): Hourly range such as \"8:00 AM – 9:00 AM\" (skipping 12:00 PM – 1:00 PM lunch).\n\
             - Column 2 (Cause): Start with \"Need ...\" stating the architectural, security, operational, or business reason why this hour's work was needed.\n\
             - Column 3 (Findings): MUST start with the assigned Work/Project Title prefix:\n\
               • Hours 8:00 AM – 12:00 PM: Prefix with \"{morning_title} – \" naming specific classes, methods, and outcomes completed on {morning_repo}.\n\
               • Hours 1:00 PM – 5:00 PM: Prefix with \"{afternoon_title} – \" naming specific classes, methods, and outcomes completed on {afternoon_repo}.\n\
             - Clean out any irrelevant searches or distractions and attribute the full hour to the main project worked on.\n\
             - For the final 4:00 PM – 5:00 PM slot (when triggered at 4:40 PM), synthesize and predict the final integration and end-to-end verification completing the afternoon's work."
        );

        if let Some(mut w_rule) = rules::load_rule(rules::RULE_WORD) {
            w_rule.schedule_splits = splits.clone();
            w_rule.title_target_col = "Findings".into();
            w_rule.title_prefix = morning_title.into();
            w_rule.instructions = new_instructions.clone();
            let _ = rules::save_rule(&w_rule);
        }
        if let Some(mut e_rule) = rules::load_rule(rules::RULE_EXCEL) {
            e_rule.schedule_splits = splits.clone();
            e_rule.title_target_col = "Findings".into();
            e_rule.title_prefix = morning_title.into();
            e_rule.instructions = new_instructions;
            let _ = rules::save_rule(&e_rule);
        }

        // Add both repositories to monitored project directories in config if missing
        let mut cfg = config::load();
        for r_path in [&morning_repo, &afternoon_repo] {
            if !cfg.tracking.project_dirs.iter().any(|d| d.eq_ignore_ascii_case(r_path)) {
                cfg.tracking.project_dirs.push(r_path.clone());
            }
        }
        let _ = config::save(&cfg);

        let _ = rules::add_learned_note(&format!(
            "Multi-Project Schedule Configured:\n• 8:00 AM – 12:00 PM: Title \"{morning_title}\" (Repo: {morning_repo})\n• 1:00 PM – 5:00 PM: Title \"{afternoon_title}\" (Repo: {afternoon_repo})\n• All reports saved in centralized isolated directory: {}\n• Findings start with the respective project title.",
            paths::reports_dir().display()
        ));

        let files = generate_rule_reports_with_mode(date, true).unwrap_or_default();
        let preview = generate_full_day_table_preview(date);

        return Some(format!(
            "✅ Understood and Configured Multi-Project Schedule!\n\
             • **Morning (8:00 AM – 12:00 PM)**: Assigned Project Title: `{morning_title}` | Repo: `{morning_repo}`\n\
             • **Afternoon (1:00 PM – 5:00 PM)**: Assigned Project Title: `{afternoon_title}` | Repo: `{afternoon_repo}`\n\
             • **Centralized Isolated Directory**: All outputs are saved strictly in `{}`\n\
             • Both rule profiles (`{}` & `{}`) updated autonomously.\n\n\
             Generated Word (.docx) & Excel (.xlsx) Reports:\n{}\n\n\
             {}\n\n\
             Both rules and memory are synchronized. Subsequent reports will accurately split morning and afternoon projects!",
            paths::reports_dir().display(),
            rules::RULE_WORD,
            rules::RULE_EXCEL,
            files.iter().map(|p| format!("• {}", p.display())).collect::<Vec<_>>().join("\n"),
            preview
        ));
    }

    // 5.6 Natural Language Report Generation Command (without relying strictly on buttons)
    if (lower.contains("make our report")
        || lower.contains("make the report")
        || lower.contains("make report")
        || lower.contains("create report")
        || lower.contains("generate report")
        || lower.contains("run report"))
        && !lower.contains("how ")
        && !lower.contains("trigger time")
        && !lower.contains("language")
    {
        rules::set_active_rules(
            date,
            &[rules::RULE_WORD.to_string(), rules::RULE_EXCEL.to_string()],
        );
        let files = generate_rule_reports_with_mode(date, true).unwrap_or_default();
        let preview = generate_full_day_table_preview(date);
        return Some(format!(
            "✅ Generated Today's Work Reports in `{}`:\n{}\n\n{}",
            paths::reports_dir().display(),
            files.iter().map(|p| format!("• {}", p.display())).collect::<Vec<_>>().join("\n"),
            preview
        ));
    }

    // 6. Autonomous Self-Modifying Adaptation: Move Title to Findings (or Cause)
    let is_move_to_cause = (lower.contains("title") || lower.contains("work title"))
        && (lower.contains("to cause") || lower.contains("in cause") || lower.contains("on cause"));

    let is_title_instruction = !is_split_instruction
        && (lower.contains("title") || lower.contains("work title") || lower.contains("project title"))
        && (lower.contains("finding")
            || lower.contains("word workdone")
            || lower.contains("work done")
            || lower.contains("workdone")
            || lower.contains("reflect to the run")
            || lower.contains("haven't moved")
            || lower.contains("move")
            || lower.contains("add ")
            || lower.contains("each row")
            || lower.contains("every row")
            || lower.contains("rows of report")
            || lower.contains("prepend")
            || lower.contains("not included"));

    if is_title_instruction || is_move_to_cause {
        let target_col = if is_move_to_cause { "Cause" } else { "Findings" };
        let title_name = "Araneta Mall Linking";

        let new_instructions = if target_col == "Findings" {
            format!(
                "Follow this exact engineering report style for every hourly row:\n\
                 - Column 1 (Time): Hourly range such as \"8:00 AM – 9:00 AM\" (skipping 12:00 PM – 1:00 PM lunch).\n\
                 - Column 2 (Cause): Start with \"Need ...\" stating the architectural, security, operational, or business reason why this hour's work was needed (e.g. \"Need single-instance enforcement to prevent race conditions...\" or \"Need enterprise-grade password security and timing-attack mitigation...\").\n\
                 - Column 3 (Findings): MUST start with the Work/Project Title prefix: \"{title_name} – \" followed by concrete technical work completed or turnover conducted, naming specific classes, methods, forms, services, and outcomes on the user's active codebase (MallLinking: e.g. \"{title_name} – Implemented single-instance application control in Program.cs utilizing named mutex (AppMutexName)...\" or \"{title_name} – Built a static PasswordService implementing PBKDF2...\"). NEVER attribute work to background tools like DayRecap or Antigravity.\n\
                 - Clean out any irrelevant searches or distractions and attribute the full hour to the main project worked on ({title_name}).\n\
                 - For the final 4:00 PM – 5:00 PM slot (when triggered at 4:40 PM), synthesize and predict the final integration and end-to-end verification completing the afternoon's work."
            )
        } else {
            format!(
                "Follow this exact engineering report style for every hourly row:\n\
                 - Column 1 (Time): Hourly range such as \"8:00 AM – 9:00 AM\" (skipping 12:00 PM – 1:00 PM lunch).\n\
                 - Column 2 (Cause): MUST start with the Work Title prefix: \"{title_name} - Need ...\" stating the architectural, security, operational, or business reason why this hour's work was needed.\n\
                 - Column 3 (Findings): Concrete technical work completed on the user's active codebase (MallLinking).\n\
                 - Clean out any irrelevant searches or distractions and attribute the full hour to the main project worked on ({title_name}).\n\
                 - For the final 4:00 PM – 5:00 PM slot (when triggered at 4:40 PM), synthesize and predict the final integration and end-to-end verification completing the afternoon's work."
            )
        };

        if let Some(mut w_rule) = rules::load_rule(rules::RULE_WORD) {
            w_rule.title_target_col = target_col.into();
            w_rule.title_prefix = title_name.into();
            w_rule.instructions = new_instructions.clone();
            let _ = rules::save_rule(&w_rule);
        }
        if let Some(mut e_rule) = rules::load_rule(rules::RULE_EXCEL) {
            e_rule.title_target_col = target_col.into();
            e_rule.title_prefix = title_name.into();
            e_rule.instructions = new_instructions;
            let _ = rules::save_rule(&e_rule);
        }

        let memory_note = if target_col == "Findings" {
            format!("Mandatory Work Title: Always place '{title_name} – ' in Column 3 (Findings). Column 2 (Cause) starts cleanly with 'Need ...'.")
        } else {
            format!("Mandatory Work Title: Always prepend '{title_name} - Need ...' to every row's Cause.")
        };
        let _ = rules::add_learned_note(&memory_note);

        let files = generate_rule_reports_with_mode(date, true).unwrap_or_default();
        let preview = generate_full_day_table_preview(date);

        return Some(format!(
            "✅ Done! Autonomously updated both `{}` & `{}`!\n\
             • Title `{title_name} – ` is now permanently placed in **Column 3 ({target_col})**.\n\
             • Column 2 (Cause) starts cleanly with `Need ...` (no project title prefix).\n\
             • Regenerated your Word (.docx) & Excel (.xlsx) reports with this updated format!\n\n\
             Generated updated files:\n{}\n\n\
             {}\n\n\
             Both rules and memory have been synchronized. Future reports will keep `{title_name} – ` in Findings!",
            rules::RULE_WORD,
            rules::RULE_EXCEL,
            files.iter().map(|p| format!("• {}", p.display())).collect::<Vec<_>>().join("\n"),
            preview
        ));
    }

    // 7. Autonomous Self-Modifying Adaptation: Trigger Time
    if (lower.contains("trigger") || lower.contains("auto-report") || lower.contains("auto report"))
        && (lower.contains("time") || lower.contains("set") || lower.contains("change") || lower.contains("at"))
    {
        if let Some(time_str) = parse_trigger_time_from_text(&lower) {
            let mut cfg = config::load();
            cfg.reports.report_trigger_time = time_str.clone();
            let _ = config::save(&cfg);

            if let Some(mut w_rule) = rules::load_rule(rules::RULE_WORD) {
                w_rule.trigger_time = time_str.clone();
                let _ = rules::save_rule(&w_rule);
            }
            if let Some(mut e_rule) = rules::load_rule(rules::RULE_EXCEL) {
                e_rule.trigger_time = time_str.clone();
                let _ = rules::save_rule(&e_rule);
            }
            let _ = rules::add_learned_note(&format!("Auto-trigger report time set to {time_str}."));
            return Some(format!(
                "⚙ Auto-Trigger Time Updated to `{time_str}`!\n\
                 • Configuration (`config.toml`) and both rule profiles (`workReportFormatword` & `ExcelReportFormatExcel`) have been autonomously updated.\n\
                 • Daily reports will automatically generate at `{time_str}`."
            ));
        }
    }

    // 8. Autonomous Self-Modifying Adaptation: Language
    if (lower.contains("language") || lower.contains("dialect"))
        && (lower.contains("change") || lower.contains("set") || lower.contains("switch") || lower.contains("use") || lower.contains("in "))
    {
        let new_lang = if lower.contains("filipino") || lower.contains("tagalog") {
            "Filipino"
        } else if lower.contains("spanish") || lower.contains("espanol") {
            "Spanish"
        } else if lower.contains("japanese") || lower.contains("nihongo") {
            "Japanese"
        } else if lower.contains("german") || lower.contains("deutsch") {
            "German"
        } else if lower.contains("french") {
            "French"
        } else if lower.contains("english") {
            "English"
        } else {
            ""
        };

        if !new_lang.is_empty() {
            let mut cfg = config::load();
            cfg.reports.language = new_lang.to_string();
            let _ = config::save(&cfg);
            let _ = rules::add_learned_note(&format!("Preferred report language set to {new_lang}."));
            return Some(format!(
                "⚙ Report Language Updated to `{new_lang}`!\n\
                 • Modified `config.toml` autonomously.\n\
                 • Subsequent AI reports and summaries will now be generated in {new_lang}."
            ));
        }
    }

    // 9. Autonomous Self-Modifying Adaptation: Exclude / Ignore App
    if (lower.contains("exclude") || lower.contains("ignore") || lower.contains("don't track") || lower.contains("stop tracking"))
        && (lower.contains("app") || lower.contains("chrome") || lower.contains("spotify") || lower.contains("discord") || lower.contains("steam") || lower.contains("slack") || lower.contains("browser"))
    {
        let mut app_to_ignore = String::new();
        for candidate in ["chrome", "spotify", "discord", "steam", "slack", "brave", "edge", "firefox", "vlc"] {
            if lower.contains(candidate) {
                app_to_ignore = candidate.to_string();
                break;
            }
        }
        if !app_to_ignore.is_empty() {
            let mut cfg = config::load();
            if !cfg.tracking.exclude_apps.iter().any(|a| a.eq_ignore_ascii_case(&app_to_ignore)) {
                cfg.tracking.exclude_apps.push(app_to_ignore.clone());
                let _ = config::save(&cfg);
            }
            return Some(format!(
                "⚙ Updated Tracking Configuration: Excluded `{app_to_ignore}` from activity logs.\n\
                 • Saved directly to `config.toml`. Windows with this app will now be safely ignored."
            ));
        }
    }

    // 10. Autonomous Self-Modifying Adaptation: Monitored Project Repositories
    if (lower.contains("track") || lower.contains("monitor") || lower.contains("add"))
        && (lower.contains("repo") || lower.contains("project") || lower.contains("directory") || lower.contains("folder"))
    {
        if let Some(path_str) = extract_windows_path(q_trim) {
            let p = Path::new(&path_str);
            let mut cfg = config::load();
            let normalized = p.to_string_lossy().to_string();
            if !cfg.tracking.project_dirs.iter().any(|d| d.eq_ignore_ascii_case(&normalized)) {
                cfg.tracking.project_dirs.push(normalized.clone());
                let _ = config::save(&cfg);
            }
            return Some(format!(
                "⚙ Added Project Directory `{normalized}` to Monitored Repositories!\n\
                 • Saved directly to `config.toml`.\n\
                 • DayRecap will now automatically scan Git commits from this directory for hourly reports."
            ));
        }
    }

    // 11. Autonomous Self-Modifying Adaptation: Reset Settings to Default
    if lower == "reset settings"
        || lower == "restore default settings"
        || lower == "reset to default"
        || lower == "reset all settings"
    {
        let def_cfg = Config::default();
        let _ = config::save(&def_cfg);
        let _ = rules::save_rule(&RuleProfile::word_default());
        let _ = rules::save_rule(&RuleProfile::excel_default());
        return Some(
            "⚙ Restored default configuration and rules (`workReportFormatword` & `ExcelReportFormatExcel`) autonomously.\n\
             All settings have been reloaded to their default states."
                .into(),
        );
    }

    // 7. Dissect a pasted table structure (e.g. `Time | Cause | Findings`)
    let is_conversational = lower.contains("please")
        || lower.contains("can you")
        || lower.contains("why")
        || lower.contains("add ")
        || lower.contains("title")
        || lower.contains("what")
        || lower.contains("how")
        || lower.contains("didn't")
        || lower.contains("told you");

    if !is_conversational && q_trim.contains('|') && q_trim.lines().count() >= 2 {
        let preferred_fmt = if lower.contains("excel") || lower.contains("xlsx") {
            "xlsx"
        } else {
            "docx"
        };
        let dissected = office::dissect_text(q_trim, preferred_fmt);
        if dissected.detected_columns.len() >= 2 {
            return Some(format!(
                "🔍 I dissected the table structure you pasted:\n\
                 • Preferred Format: `.{}`\n\
                 • Columns Detected: `{}`\n\
                 • Sample Rows Parsed: {}\n\n\
                 I understand how each column works. Both `workReportFormatword` (.docx) and `ExcelReportFormatExcel` (.xlsx) are already configured with `{}` (8:00 AM – 5:00 PM excluding 12:00–1:00 PM lunch, auto-predicting at 4:40 PM).\n\
                 • Type `access rules` to inspect them, or `run both` to generate them now!",
                dissected.format,
                dissected.detected_columns.join(" | "),
                dissected.sample_rows.len(),
                dissected.detected_columns.join(" | ")
            ));
        }
    }

    None
}

fn parse_and_save_rule_command(cmd: &str) -> String {
    let rest = cmd
        .strip_prefix("update rule ")
        .or_else(|| cmd.strip_prefix("Update rule "))
        .or_else(|| cmd.strip_prefix("save rule "))
        .or_else(|| cmd.strip_prefix("Save rule "))
        .unwrap_or(cmd)
        .trim();

    let mut parts = rest.splitn(2, char::is_whitespace);
    let rule_name = parts.next().unwrap_or("CustomRule").trim();
    let body = parts.next().unwrap_or("").trim();

    let mut rule = rules::load_rule(rule_name).unwrap_or_else(|| RuleProfile {
        name: rule_name.to_string(),
        ..RuleProfile::default()
    });
    rule.name = rule_name.to_string();

    let body_low = body.to_lowercase();
    if let Some(cols_str) = body
        .strip_prefix("columns:")
        .or_else(|| body.strip_prefix("columns="))
    {
        let cols: Vec<String> = cols_str
            .split('|')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !cols.is_empty() {
            rule.columns = cols;
        }
    } else if let Some(fn_str) = body
        .strip_prefix("filename:")
        .or_else(|| body.strip_prefix("filename="))
    {
        rule.filename_pattern = fn_str.trim().to_string();
    } else if let Some(instr_str) = body
        .strip_prefix("instructions:")
        .or_else(|| body.strip_prefix("instructions="))
    {
        rule.instructions = instr_str.trim().to_string();
    } else if body_low.contains("xlsx") || body_low.contains("excel") {
        rule.format = "xlsx".into();
    } else if body_low.contains("docx") || body_low.contains("word") {
        rule.format = "docx".into();
    } else if !body.is_empty() {
        rule.instructions = format!("{}\nAdditional Rule: {}", rule.instructions, body);
    }

    match rules::save_rule(&rule) {
        Ok((toml_p, txt_p)) => format!(
            "✅ Saved Rule `{}`!\n• Readable .txt: {}\n• Config .toml: {}\n\n{}",
            rule.name,
            txt_p.display(),
            toml_p.display(),
            rule.to_readable_txt()
        ),
        Err(e) => format!("Failed to save rule `{}`: {e}", rule.name),
    }
}

fn parse_trigger_time_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for word in lower.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != ':');
        if clean.contains(':') {
            let mut parts = clean.split(':');
            if let (Some(h_str), Some(m_str)) = (parts.next(), parts.next()) {
                if let (Ok(h), Ok(m)) = (h_str.parse::<u32>(), m_str.parse::<u32>()) {
                    let mut hour = h;
                    if lower.contains("pm") && hour < 12 {
                        hour += 12;
                    } else if lower.contains("am") && hour == 12 {
                        hour = 0;
                    }
                    if hour < 24 && m < 60 {
                        return Some(format!("{:02}:{:02}", hour, m));
                    }
                }
            }
        }
    }
    for part in lower.split_whitespace() {
        if part.ends_with("pm") {
            if let Ok(h) = part.trim_end_matches("pm").parse::<u32>() {
                let hour = if h < 12 { h + 12 } else { h };
                if hour < 24 {
                    return Some(format!("{:02}:00", hour));
                }
            }
        } else if part.ends_with("am") {
            if let Ok(h) = part.trim_end_matches("am").parse::<u32>() {
                let hour = if h == 12 { 0 } else { h };
                if hour < 24 {
                    return Some(format!("{:02}:00", hour));
                }
            }
        }
    }
    None
}

fn extract_all_windows_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i + 2 < chars.len() {
        if chars[i].is_ascii_alphabetic() && chars[i + 1] == ':' && (chars[i + 2] == '\\' || chars[i + 2] == '/') {
            let start = i;
            let mut end = i + 3;
            while end < chars.len() {
                let c = chars[end];
                if c == '\n' || c == '\r' || c == '\t' || c == '"' || c == '\'' || c == '`' || c == ',' || c == '|' {
                    break;
                }
                end += 1;
            }
            let candidate: String = chars[start..end].iter().collect();
            let trimmed = candidate.trim().trim_end_matches(['.', ';', ')', ']', '`']).to_string();
            if !trimmed.is_empty() && !out.contains(&trimmed) {
                out.push(trimmed);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn extract_windows_path(text: &str) -> Option<String> {
    extract_all_windows_paths(text).into_iter().next()
}

fn generate_full_day_table_preview(date: NaiveDate) -> String {
    let cfg = config::load();
    let day = load_summary(date);
    let all_senses = senses::load_senses(date);
    let commits = if cfg.tracking.enable_git_sense {
        senses::read_git_commits(date, &cfg.tracking.project_dirs)
    } else {
        Vec::new()
    };

    let word_rule = rules::load_rule(rules::RULE_WORD).unwrap_or_default();
    let hours = vec![8, 9, 10, 11, 13, 14, 15, 16];
    let mut slots = Vec::with_capacity(hours.len());
    for &h in &hours {
        let is_predicted = h == 16;
        let slot = scrubber::clean_hour_slot_with_rule(h, &day, &all_senses, &commits, is_predicted, &word_rule);
        slots.push(slot);
    }

    let rows = build_rule_rows(&cfg, &word_rule, &slots);

    let mut out = String::from("📋 **Full Day Work Report (8:00 AM – 5:00 PM)**\n\n| Time | Cause | Findings |\n|---|---|---|\n");
    for (i, s) in slots.iter().enumerate() {
        let cause = rows
            .get(i)
            .and_then(|r| r.cols.get(1))
            .cloned()
            .unwrap_or_else(|| fallback_col_value(&word_rule, s, 1));
        let findings = rows
            .get(i)
            .and_then(|r| r.cols.get(2))
            .cloned()
            .unwrap_or_else(|| fallback_col_value(&word_rule, s, 2));
        out += &format!("| {} | {} | {} |\n", s.time_label, cause, findings);
    }
    out
}

fn no_ai_answer(data: &str, senses_summary: &str, error: Option<String>) -> String {
    let reason = match error {
        Some(e) => format!("The AI is not working ({e})."),
        None => "No external AI is set up yet (using built-in smart templates).".into(),
    };
    format!(
        "{reason}\n\nHere is what I recorded today:\n\n{data}{senses_summary}\n\
         💡 Commands you can use right now (even without an API key):\n\
         • Type `access rules` to view your Word & Excel rules\n\
         • Type `run both` to generate your Word (.docx) & Excel (.xlsx) reports\n\
         • Type `remember: <note>` to add a custom rule note"
    )
}

/// Append one chat exchange to `chat/<date>.txt`.
pub fn save_chat(date: NaiveDate, who: &str, text: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::chat_file(date))
    {
        let _ = writeln!(f, "[{}] {who}: {}\n", Local::now().format("%H:%M"), text.trim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_file_access_and_rule_questions() {
        let today = Local::now().date_naive();
        let reply = handle_conversational_rules(today, "can you access any of my files?");
        assert!(reply.is_some());
        let text = reply.unwrap();
        assert!(text.contains("FILE & REPOSITORY ACCESS STATUS"));
        assert!(text.contains("Tracked Git Repositories"));
        assert!(text.contains("Read-Only Git"));

        let rules_reply = handle_conversational_rules(today, "access rules");
        assert!(rules_reply.is_some());
        assert!(rules_reply.unwrap().contains("SAVED REPORT RULES"));
    }

    #[test]
    fn handles_full_day_request_and_table_preview() {
        let today = Local::now().date_naive();
        let reply = handle_conversational_rules(today, "can you fill my whole day 8-5 with that report? Make it long");
        assert!(reply.is_some());
        let text = reply.unwrap();
        println!("\n=== GENERATED FULL DAY REPORT PREVIEW ===\n{text}\n=========================================");
        assert!(text.contains("Full Day 8:00 AM – 5:00 PM schedule"));
        assert!(text.contains("8:00 AM – 9:00 AM"));
        assert!(text.contains("4:00 PM – 5:00 PM"));
    }

    #[test]
    fn enforces_cause_and_findings_consistently() {
        let cause_clean = enforce_clean_cause(
            "Araneta Mall Linking - Need enterprise-grade password security and timing-attack mitigation.",
        );
        assert_eq!(
            cause_clean,
            "Need enterprise-grade password security and timing-attack mitigation."
        );

        let findings_res = enforce_findings_title_prefix(
            "Implemented single-instance application control.",
            "Araneta Mall Linking",
            "MallLinking",
        );
        assert_eq!(
            findings_res,
            "Araneta Mall Linking – Implemented single-instance application control."
        );
    }

    #[test]
    fn handles_title_instruction_conversationally() {
        let today = Local::now().date_naive();
        let reply = handle_conversational_rules(
            today,
            "can you please move the title to the Findings specially on word workdone",
        );
        assert!(reply.is_some());
        let text = reply.unwrap();
        assert!(text.contains("Araneta Mall Linking"));
        assert!(text.contains("Findings"));
        assert!(text.contains("Autonomously updated"));
    }

    #[test]
    fn handles_schedule_split_conversationally() {
        let today = Local::now().date_naive();
        let prompt = "8-12 Title Enterprise Mall Linking, Folder path to Analyze C:\\Users\\MIS jap\\source\\repos\\MallLinking\n1-5pm Project name Atc & Araneta Mall Linking, path to analyze git C:\\Users\\MIS jap\\source\\repos\\Mall_Linking";
        let reply = handle_conversational_rules(today, prompt);
        assert!(reply.is_some());
        let text = reply.unwrap();
        assert!(text.contains("Understood and Configured Multi-Project Schedule"));
        assert!(text.contains("Enterprise Mall Linking"));
        assert!(text.contains("Atc & Araneta Mall Linking"));
        assert!(text.contains("8:00 AM – 9:00 AM"));
        assert!(text.contains("4:00 PM – 5:00 PM"));
    }
}

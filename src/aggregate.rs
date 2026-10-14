//! Turns raw spans into compact per-hour summaries.
//!
//! This is the key to staying inside free AI quotas and tiny local models:
//! a whole day of raw activity (~1,000 lines) is compressed to ~1-2K tokens
//! of "hour -> app -> top window titles" before it is ever sent to an AI.

use crate::{
    store::Span,
    util::{fmt_dur, truncate},
};
use chrono::{NaiveDate, NaiveTime, Timelike};
use std::collections::HashMap;

pub const IDLE: &str = "IDLE";

const MIN_APP_SECS: i64 = 60;
const MIN_TITLE_SECS: i64 = 30;
const MAX_TITLES: usize = 5;
const TITLE_CHARS: usize = 90;

pub struct AppUse {
    pub name: String,
    pub secs: i64,
    pub titles: Vec<(String, i64)>,
}

pub struct HourSummary {
    pub hour: u32,
    pub active: i64,
    pub idle: i64,
    pub apps: Vec<AppUse>,
}

pub struct DaySummary {
    pub date: NaiveDate,
    pub active: i64,
    pub idle: i64,
    pub first: Option<NaiveTime>,
    pub last: Option<NaiveTime>,
    pub apps: Vec<(String, i64)>,
    pub hours: Vec<HourSummary>,
}

type AppMap = HashMap<String, (i64, HashMap<String, i64>)>;

pub fn summarize(date: NaiveDate, spans: &[Span]) -> DaySummary {
    let mut per_hour: [AppMap; 24] = Default::default();
    let mut idle = [0i64; 24];
    let (mut first, mut last): (Option<NaiveTime>, Option<NaiveTime>) = (None, None);

    for s in spans {
        let is_idle = s.app == IDLE;
        if !is_idle && s.secs() > 0 {
            first = Some(first.map_or(s.start, |f| f.min(s.start)));
            last = Some(last.map_or(s.end, |l| l.max(s.end)));
        }
        let (name, title) = if is_idle {
            (String::new(), String::new())
        } else {
            display(&s.app, &s.title)
        };

        // Split spans that cross hour boundaries.
        let mut cur = s.start;
        while cur < s.end {
            let h = cur.hour();
            let boundary = if h == 23 {
                s.end
            } else {
                NaiveTime::from_hms_opt(h + 1, 0, 0).unwrap().min(s.end)
            };
            let secs = (boundary - cur).num_seconds();
            if is_idle {
                idle[h as usize] += secs;
            } else {
                let entry = per_hour[h as usize].entry(name.clone()).or_default();
                entry.0 += secs;
                if !title.is_empty() {
                    *entry.1.entry(title.clone()).or_default() += secs;
                }
            }
            cur = boundary;
        }
    }

    let mut totals: HashMap<String, i64> = HashMap::new();
    let mut hours = Vec::new();
    for (h, apps) in per_hour.into_iter().enumerate() {
        let active: i64 = apps.values().map(|(s, _)| s).sum();
        if active == 0 && idle[h] == 0 {
            continue;
        }
        let mut list: Vec<AppUse> = apps
            .into_iter()
            .map(|(name, (secs, titles))| {
                *totals.entry(name.clone()).or_default() += secs;
                let mut titles: Vec<(String, i64)> = titles.into_iter().collect();
                titles.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                AppUse { name, secs, titles }
            })
            .collect();
        list.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.name.cmp(&b.name)));
        hours.push(HourSummary { hour: h as u32, active, idle: idle[h], apps: list });
    }

    let mut apps: Vec<(String, i64)> = totals.into_iter().collect();
    apps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    DaySummary {
        date,
        active: hours.iter().map(|h| h.active).sum(),
        idle: hours.iter().map(|h| h.idle).sum(),
        first,
        last,
        apps,
        hours,
    }
}

/// Overview + every hour. Used for reports, the AI prompt and the chat context.
pub fn render(day: &DaySummary) -> String {
    let mut out = render_overview(day);
    out.push('\n');
    let min_hour = if day.active < 120 { 1 } else { MIN_APP_SECS };
    for h in day.hours.iter().filter(|h| h.active >= min_hour) {
        out.push_str(&render_hour(h));
        out.push('\n');
    }
    out
}

pub fn render_overview(day: &DaySummary) -> String {
    let mut out = format!("Date: {}\n", day.date.format("%A, %d %B %Y"));
    let span = match (day.first, day.last) {
        (Some(f), Some(l)) => format!(" ({} - {})", f.format("%H:%M"), l.format("%H:%M")),
        _ => String::new(),
    };
    out += &format!(
        "Active time: {}{span}\nIdle / breaks: {}\n",
        fmt_dur(day.active),
        fmt_dur(day.idle)
    );
    let top: Vec<String> = day
        .apps
        .iter()
        .take(6)
        .map(|(name, secs)| format!("{name} {} ({}%)", fmt_dur(*secs), pct(*secs, day.active)))
        .collect();
    if !top.is_empty() {
        out += &format!("Top apps: {}\n", top.join(", "));
    }
    out
}

pub fn render_hour(h: &HourSummary) -> String {
    let mut out = format!(
        "[{:02}:00-{:02}:00] active {}{}\n",
        h.hour,
        h.hour + 1,
        fmt_dur(h.active),
        if h.idle >= 60 { format!(", idle {}", fmt_dur(h.idle)) } else { String::new() }
    );
    let min_app = if h.active < 120 { 1 } else { MIN_APP_SECS };
    let min_title = if h.active < 120 { 1 } else { MIN_TITLE_SECS };
    let mut other = 0;
    for app in &h.apps {
        if app.secs < min_app {
            other += app.secs;
            continue;
        }
        let titles: Vec<String> = app
            .titles
            .iter()
            .filter(|(_, s)| *s >= min_title)
            .take(MAX_TITLES)
            .map(|(t, s)| format!("{} ({})", truncate(t, TITLE_CHARS), fmt_dur(*s)))
            .collect();
        if titles.is_empty() {
            out += &format!("  - {} {}\n", app.name, fmt_dur(app.secs));
        } else {
            out += &format!("  - {} {}: {}\n", app.name, fmt_dur(app.secs), titles.join("; "));
        }
    }
    if other >= 60 {
        out += &format!("  - Other quick switches {}\n", fmt_dur(other));
    }
    out
}

fn pct(part: i64, total: i64) -> i64 {
    if total == 0 { 0 } else { part * 100 / total }
}

/// (executable, friendly name, keyword that marks the app's own title suffix)
const KNOWN_APPS: &[(&str, &str, &str)] = &[
    ("code.exe", "VS Code", "visual studio code"),
    ("cursor.exe", "Cursor", "cursor"),
    ("antigravity.exe", "Antigravity", "antigravity"),
    ("devenv.exe", "Visual Studio", "visual studio"),
    ("idea64.exe", "IntelliJ IDEA", "intellij"),
    ("pycharm64.exe", "PyCharm", "pycharm"),
    ("chrome.exe", "Chrome", "google chrome"),
    ("msedge.exe", "Edge", "edge"),
    ("firefox.exe", "Firefox", "firefox"),
    ("brave.exe", "Brave", "brave"),
    ("opera.exe", "Opera", "opera"),
    ("explorer.exe", "File Explorer", "file explorer"),
    ("winword.exe", "Word", "word"),
    ("excel.exe", "Excel", "excel"),
    ("powerpnt.exe", "PowerPoint", "powerpoint"),
    ("outlook.exe", "Outlook", "outlook"),
    ("olk.exe", "Outlook", "outlook"),
    ("onenote.exe", "OneNote", "onenote"),
    ("ms-teams.exe", "Teams", "microsoft teams"),
    ("teams.exe", "Teams", "microsoft teams"),
    ("slack.exe", "Slack", "slack"),
    ("discord.exe", "Discord", "discord"),
    ("zoom.exe", "Zoom", "zoom"),
    ("telegram.exe", "Telegram", "telegram"),
    ("whatsapp.exe", "WhatsApp", "whatsapp"),
    ("notepad.exe", "Notepad", "notepad"),
    ("notepad++.exe", "Notepad++", "notepad++"),
    ("windowsterminal.exe", "Terminal", "terminal"),
    ("powershell.exe", "PowerShell", "powershell"),
    ("cmd.exe", "Command Prompt", "command prompt"),
    ("acrobat.exe", "Acrobat", "adobe acrobat"),
    ("acrord32.exe", "Acrobat Reader", "adobe acrobat"),
    ("photoshop.exe", "Photoshop", "photoshop"),
    ("figma.exe", "Figma", "figma"),
    ("spotify.exe", "Spotify", "spotify"),
    ("obsidian.exe", "Obsidian", "obsidian"),
    ("notion.exe", "Notion", "notion"),
];

/// Friendly app name + window title without the app's own suffix
/// ("main.rs - Proj - Visual Studio Code" -> "main.rs - Proj").
fn display(app: &str, title: &str) -> (String, String) {
    let lower = app.to_lowercase();
    let (name, keyword) = KNOWN_APPS
        .iter()
        .find(|k| k.0 == lower)
        .map(|k| (k.1.to_string(), k.2))
        .unwrap_or_else(|| (pretty(app), ""));
    let title = strip_app_suffix(title, keyword, &name);
    (name, title)
}

fn pretty(app: &str) -> String {
    let base = if app.to_lowercase().ends_with(".exe") { &app[..app.len() - 4] } else { app };
    let mut chars = base.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => "Unknown".into(),
    }
}

fn strip_app_suffix(title: &str, keyword: &str, name: &str) -> String {
    let title = title.trim();
    for sep in [" - ", " — ", " – ", " | "] {
        if let Some(i) = title.rfind(sep) {
            let tail = title[i + sep.len()..].replace('\u{200b}', "").to_lowercase();
            if (!keyword.is_empty() && tail.contains(keyword)) || tail == name.to_lowercase() {
                return title[..i].trim().to_string();
            }
        }
    }
    title.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M:%S").unwrap()
    }

    fn span(a: &str, b: &str, app: &str, title: &str) -> Span {
        Span { start: t(a), end: t(b), app: app.into(), title: title.into() }
    }

    #[test]
    fn splits_hours_and_strips_suffixes() {
        let spans = vec![
            span("09:30:00", "10:15:00", "Code.exe", "main.rs - Proj - Visual Studio Code"),
            span("10:15:00", "10:20:00", IDLE, ""),
            span("10:20:00", "10:30:00", "chrome.exe", "Docs - Google Chrome"),
        ];
        let day = summarize(NaiveDate::from_ymd_opt(2026, 10, 8).unwrap(), &spans);
        assert_eq!(day.active, 55 * 60);
        assert_eq!(day.idle, 5 * 60);
        assert_eq!(day.hours.len(), 2);
        assert_eq!(day.hours[0].apps[0].name, "VS Code");
        assert_eq!(day.hours[0].apps[0].titles[0].0, "main.rs - Proj");
        assert_eq!(day.hours[1].apps[0].secs, 15 * 60);
        assert_eq!(day.apps[0], ("VS Code".to_string(), 45 * 60));
        let text = render(&day);
        assert!(text.contains("[09:00-10:00] active 30m"));
        assert!(text.contains("Chrome 10m: Docs (10m)"));
    }
}

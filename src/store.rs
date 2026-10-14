//! Append-only TSV activity log, one file per day:
//!
//! ```text
//! 16:02:10<TAB>16:14:55<TAB>Code.exe<TAB>main.rs - ReportGenerationAI - Visual Studio Code
//! ```
//!
//! Each line is written (and the file closed) immediately, so a crash loses at most
//! the current in-progress span (<= 1 minute, see tracker checkpointing).

use crate::{paths, util};
use chrono::{Local, NaiveDate, NaiveTime};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
};

const TIME_FMT: &str = "%H:%M:%S";

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: NaiveTime,
    pub end: NaiveTime,
    pub app: String,
    pub title: String,
}

impl Span {
    pub fn secs(&self) -> i64 {
        (self.end - self.start).num_seconds().max(0)
    }
}

fn sanitize(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

pub fn append(date: NaiveDate, span: &Span) -> io::Result<()> {
    if span.end <= span.start {
        return Ok(());
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::day_file(date))?;
    writeln!(
        f,
        "{}\t{}\t{}\t{}",
        span.start.format(TIME_FMT),
        span.end.format(TIME_FMT),
        sanitize(&span.app),
        sanitize(&span.title)
    )
}

pub fn checkpoint_file() -> std::path::PathBuf {
    paths::data_dir().join("checkpoint.tsv")
}

pub fn write_checkpoint(date: NaiveDate, span: &Span) {
    let path = checkpoint_file();
    let content = format!(
        "{}\t{}\t{}\t{}\t{}\n",
        date.format("%Y-%m-%d"),
        span.start.format(TIME_FMT),
        span.end.format(TIME_FMT),
        sanitize(&span.app),
        sanitize(&span.title)
    );
    let _ = fs::write(path, content);
}

pub fn read_checkpoint() -> Option<(NaiveDate, Span)> {
    let path = checkpoint_file();
    let text = fs::read_to_string(path).ok()?;
    let line = text.lines().next()?.trim_start_matches('\u{feff}');
    let mut parts = line.splitn(5, '\t');
    let date = NaiveDate::parse_from_str(parts.next()?, "%Y-%m-%d").ok()?;
    let start = NaiveTime::parse_from_str(parts.next()?, TIME_FMT).ok()?;
    let end = NaiveTime::parse_from_str(parts.next()?, TIME_FMT).ok()?;
    let app = parts.next()?.to_string();
    let title = parts.next().unwrap_or("").to_string();
    Some((date, Span { start, end, app, title }))
}

pub fn clear_checkpoint() {
    let _ = fs::remove_file(checkpoint_file());
}

pub fn load_day(date: NaiveDate) -> Vec<Span> {
    fs::read_to_string(paths::day_file(date))
        .map(|text| text.lines().filter_map(parse_line).collect())
        .unwrap_or_default()
}

pub fn load_day_live(date: NaiveDate) -> Vec<Span> {
    let mut spans = load_day(date);
    if let Some((cp_date, cp_span)) = read_checkpoint() {
        if cp_date == date && cp_span.secs() > 0 {
            spans.push(cp_span);
        }
    }
    spans
}

fn parse_line(line: &str) -> Option<Span> {
    let mut parts = line.trim_start_matches('\u{feff}').splitn(4, '\t');
    let start = NaiveTime::parse_from_str(parts.next()?, TIME_FMT).ok()?;
    let end = NaiveTime::parse_from_str(parts.next()?, TIME_FMT).ok()?;
    let app = parts.next()?.to_string();
    let title = parts.next().unwrap_or("").to_string();
    Some(Span { start, end, app, title })
}

/// Delete raw logs older than `retention_days` (0 = keep forever). Reports are kept.
pub fn cleanup(retention_days: u32) {
    if retention_days == 0 {
        return;
    }
    let cutoff = Local::now().date_naive() - chrono::Duration::days(retention_days as i64);
    let Ok(entries) = fs::read_dir(paths::data_dir()) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let old = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .is_some_and(|d| d < cutoff);
        if old {
            match fs::remove_file(&path) {
                Ok(()) => util::log(format!("retention: deleted {}", path.display())),
                Err(e) => util::log(format!("retention: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines() {
        let s = parse_line("09:00:00\t09:10:30\tCode.exe\tmain.rs - X").unwrap();
        assert_eq!(s.secs(), 630);
        assert_eq!(s.app, "Code.exe");
        let idle = parse_line("09:10:30\t09:20:00\tIDLE\t").unwrap();
        assert_eq!(idle.title, "");
        assert!(parse_line("garbage").is_none());
    }
}

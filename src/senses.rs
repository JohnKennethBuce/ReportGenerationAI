//! Multi-Sense Collector:
//! 1. Clipboard Sense (copied/pasted text snippets, filtered for secrets & noise)
//! 2. Typed Text Sense (reconstructed phrases in work apps, skipping password fields)
//! 3. Strict Read-Only Git Commit Sense (`git log` only — never modifies or commits anything)

use crate::{config::Tracking, paths, privacy, util::truncate};
use chrono::{NaiveDate, NaiveTime, Timelike};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const TIME_FMT: &str = "%H:%M:%S";

#[derive(Debug, Clone, PartialEq)]
pub struct SenseEvent {
    pub time: NaiveTime,
    /// "CLIP" (copied/pasted), "TYPE" (typed line), or "GIT" (committed change)
    pub kind: String,
    pub app: String,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct GitCommit {
    pub time: NaiveTime,
    pub repo: String,
    pub hash: String,
    pub subject: String,
    pub files_summary: String,
}

/// Stateful sense collector owned by the tracker thread.
pub struct SenseCollector {
    last_clip_seq: u32,
    last_clip_text: String,
    type_buf: String,
    last_key_state: [bool; 256],
}

impl Default for SenseCollector {
    fn default() -> Self {
        Self {
            last_clip_seq: clipboard_seq(),
            last_clip_text: String::new(),
            type_buf: String::new(),
            last_key_state: [false; 256],
        }
    }
}

impl SenseCollector {
    /// Poll clipboard and typed text during a tracker tick.
    pub fn poll(&mut self, cfg: &Tracking, date: NaiveDate, now: NaiveTime, app: &str, title: &str) {
        let (clean_app, clean_title) = privacy::apply(cfg, app, title);
        if clean_app == privacy::PRIVATE_APP || clean_title == privacy::PRIVATE_TITLE {
            self.type_buf.clear();
            return;
        }
        if is_sensitive_window(&clean_app, &clean_title) {
            self.type_buf.clear();
            return;
        }

        // 1. Clipboard Sense
        if cfg.enable_clipboard_sense {
            let seq = clipboard_seq();
            if seq != 0 && seq != self.last_clip_seq {
                self.last_clip_seq = seq;
                if let Some(raw) = read_clipboard_text() {
                    let cleaned = clean_snippet(&raw);
                    if !cleaned.is_empty()
                        && cleaned != self.last_clip_text
                        && !is_secret_or_noise(&cleaned, &clean_app, &clean_title)
                    {
                        self.last_clip_text = cleaned.clone();
                        let _ = append_sense(
                            date,
                            &SenseEvent {
                                time: now,
                                kind: "CLIP".into(),
                                app: clean_app.clone(),
                                title: clean_title.clone(),
                                content: truncate(&cleaned, 240),
                            },
                        );
                    }
                }
            }
        }

        // 2. Typed Text Sense (flushes when Enter is pressed or buffer reaches 80 chars)
        if cfg.enable_typing_sense {
            self.sample_keys(date, now, &clean_app, &clean_title);
        }
    }

    fn sample_keys(&mut self, date: NaiveDate, now: NaiveTime, app: &str, title: &str) {
        for vk in 0x20u8..=0x5Au8 {
            let pressed = is_key_down(vk as i32);
            let was_pressed = self.last_key_state[vk as usize];
            self.last_key_state[vk as usize] = pressed;
            if pressed && !was_pressed {
                if let Some(ch) = vk_to_char(vk) {
                    self.type_buf.push(ch);
                }
            }
        }
        let enter = is_key_down(0x0D);
        let was_enter = self.last_key_state[0x0D];
        self.last_key_state[0x0D] = enter;

        if (enter && !was_enter && self.type_buf.trim().len() >= 12) || self.type_buf.len() >= 120 {
            let line = clean_snippet(&self.type_buf);
            self.type_buf.clear();
            if !is_secret_or_noise(&line, app, title) {
                let _ = append_sense(
                    date,
                    &SenseEvent {
                        time: now,
                        kind: "TYPE".into(),
                        app: app.into(),
                        title: title.into(),
                        content: truncate(&line, 200),
                    },
                );
            }
        }
    }
}

/// Filter out passwords, tokens, keys, connection strings, and trivial 1-word lookups.
pub fn is_secret_or_noise(text: &str, app: &str, title: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.len() < 8 {
        return true;
    }
    // Single-token string with no spaces is often a password, hash, or single-word lookup
    if !trimmed.contains(' ') && trimmed.len() < 40 {
        return true;
    }
    let lower = trimmed.to_lowercase();
    let secret_markers = [
        "password",
        "passwd",
        "pwd=",
        "secret",
        "api_key",
        "apikey",
        "bearer ",
        "authorization:",
        "begin rsa",
        "begin openssh",
        "connectionstring",
        "aiZa",
        "sk-",
        "ghp_",
    ];
    if secret_markers.iter().any(|m| lower.contains(&m.to_lowercase())) {
        return true;
    }
    // Filter out casual dictionary/word searches in browsers
    let title_lower = title.to_lowercase();
    let app_lower = app.to_lowercase();
    let is_browser = ["chrome", "msedge", "firefox", "brave", "opera"]
        .iter()
        .any(|b| app_lower.contains(b));
    if is_browser {
        let casual_queries = [
            "define ",
            "meaning of ",
            "synonym",
            "translate ",
            "weather",
            "youtube",
            "facebook",
            "reddit",
            "twitter",
            "netflix",
        ];
        if casual_queries
            .iter()
            .any(|q| lower.contains(q) || title_lower.contains(q))
        {
            return true;
        }
    }
    false
}

fn is_sensitive_window(app: &str, title: &str) -> bool {
    let t = title.to_lowercase();
    let a = app.to_lowercase();
    if a.contains("dayrecap")
        || a.contains("antigravity")
        || t.contains("dayrecap —")
        || t.contains("personal ai reporter")
    {
        return true;
    }
    let blocked = [
        "login",
        "sign in",
        "signin",
        "log in",
        "authentication",
        "credential",
        "password",
        "changepassword",
        "vault",
        "keepass",
        "bitwarden",
        "1password",
    ];
    blocked.iter().any(|k| t.contains(k) || a.contains(k))
}

fn clean_snippet(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn append_sense(date: NaiveDate, ev: &SenseEvent) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::senses_file(date))?;
    writeln!(
        f,
        "{}\t{}\t{}\t{}\t{}",
        ev.time.format(TIME_FMT),
        ev.kind,
        ev.app.replace(['\t', '\r', '\n'], " "),
        ev.title.replace(['\t', '\r', '\n'], " "),
        ev.content.replace(['\t', '\r', '\n'], " ")
    )
}

pub fn load_senses(date: NaiveDate) -> Vec<SenseEvent> {
    fs::read_to_string(paths::senses_file(date))
        .map(|text| text.lines().filter_map(parse_sense_line).collect())
        .unwrap_or_default()
}

fn parse_sense_line(line: &str) -> Option<SenseEvent> {
    let mut parts = line.trim_start_matches('\u{feff}').splitn(5, '\t');
    let time = NaiveTime::parse_from_str(parts.next()?, TIME_FMT).ok()?;
    let kind = parts.next()?.to_string();
    let app = parts.next()?.to_string();
    let title = parts.next()?.to_string();
    let content = parts.next().unwrap_or("").to_string();
    Some(SenseEvent {
        time,
        kind,
        app,
        title,
        content,
    })
}

#[derive(Debug, Clone)]
pub struct RepoSummary {
    pub name: String,
    pub path: PathBuf,
    pub latest_commit: Option<String>,
}

/// Helper that spawns git with CREATE_NO_WINDOW (0x08000000) on Windows so no black console
/// window ever pops up or steals focus from the user.
fn silent_git_cmd() -> Command {
    let mut cmd = Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

static GIT_CACHE: OnceLock<Mutex<GitCacheState>> = OnceLock::new();

#[derive(Default)]
struct GitCacheState {
    last_scan: Option<Instant>,
    cached_repos: Vec<PathBuf>,
    cached_recent: Vec<String>,
    cached_day_commits: Option<(NaiveDate, Instant, Vec<GitCommit>)>,
}

fn cache_lock() -> &'static Mutex<GitCacheState> {
    GIT_CACHE.get_or_init(|| Mutex::new(GitCacheState::default()))
}

/// Discover all Git repository directories across configured folders, USERPROFILE/source/repos,
/// and working directories. Caches result for 120 seconds to prevent disk thrashing.
pub fn discover_git_repos(configured_dirs: &[String]) -> Vec<PathBuf> {
    let now = Instant::now();
    if let Ok(guard) = cache_lock().lock() {
        if let Some(t) = guard.last_scan {
            if now.duration_since(t) < Duration::from_secs(120) && !guard.cached_repos.is_empty() {
                return guard.cached_repos.clone();
            }
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut seen = HashSet::new();

    let mut candidate_parents = Vec::new();
    for d in configured_dirs {
        let p = PathBuf::from(d);
        if !candidate_parents.contains(&p) {
            candidate_parents.push(p);
        }
    }
    // Auto-include standard developer repos folders if present
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let vs_repos = PathBuf::from(&userprofile).join("source").join("repos");
        if vs_repos.exists() && !candidate_parents.contains(&vs_repos) {
            candidate_parents.push(vs_repos);
        }
    }

    for p in candidate_parents {
        if p.join(".git").exists() {
            if seen.insert(p.clone()) {
                dirs.push(p);
            }
        } else if p.is_dir() {
            if let Ok(entries) = fs::read_dir(&p) {
                for entry in entries.flatten() {
                    let sub = entry.path();
                    if sub.is_dir() && sub.join(".git").exists() && seen.insert(sub.clone()) {
                        dirs.push(sub);
                    }
                }
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".git").exists() && seen.insert(cwd.clone()) {
            dirs.push(cwd);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            let p = parent.to_path_buf();
            if p.join(".git").exists() && seen.insert(p.clone()) {
                dirs.push(p);
            }
        }
    }

    dirs.sort();

    if let Ok(mut guard) = cache_lock().lock() {
        guard.last_scan = Some(now);
        guard.cached_repos = dirs.clone();
    }

    dirs
}

/// Returns a human-readable list of all tracked Git repositories and their latest commit.
pub fn list_tracked_repos(configured_dirs: &[String]) -> Vec<RepoSummary> {
    let dirs = discover_git_repos(configured_dirs);
    let mut result = Vec::new();
    for d in dirs {
        let name = d
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let latest = latest_commit_for_repo(&d);
        result.push(RepoSummary {
            name,
            path: d,
            latest_commit: latest,
        });
    }
    result
}

fn latest_commit_for_repo(dir: &Path) -> Option<String> {
    let out = silent_git_cmd()
        .arg("-C")
        .arg(dir)
        .arg("log")
        .arg("-n")
        .arg("1")
        .arg("--no-merges")
        .arg("--date=format:%Y-%m-%d %H:%M")
        .arg("--pretty=format:%h|%ad|%s")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.splitn(3, '|');
    let hash = parts.next()?;
    let date_str = parts.next().unwrap_or("");
    let subject = parts.next().unwrap_or("");
    Some(format!("{hash} ({date_str}): {subject}"))
}

/// Retrieve the most recent commits across all tracked repositories (newest first).
/// Cached for 120 seconds to prevent unnecessary process spawning.
pub fn recent_git_commits(configured_dirs: &[String], limit: usize) -> Vec<String> {
    let now = Instant::now();
    if let Ok(guard) = cache_lock().lock() {
        if let Some(t) = guard.last_scan {
            if now.duration_since(t) < Duration::from_secs(120) && !guard.cached_recent.is_empty() {
                return guard.cached_recent.iter().take(limit).cloned().collect();
            }
        }
    }

    let dirs = discover_git_repos(configured_dirs);
    let mut all = Vec::new();
    for d in &dirs {
        let repo_name = d
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        if let Ok(out) = silent_git_cmd()
            .arg("-C")
            .arg(d)
            .arg("log")
            .arg("-n")
            .arg("2")
            .arg("--no-merges")
            .arg("--date=format:%Y-%m-%d %H:%M")
            .arg("--pretty=format:COMMIT|%h|%ad|%s")
            .output()
        {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines() {
                    let trimmed = line.trim();
                    if let Some(rest) = trimmed.strip_prefix("COMMIT|") {
                        let mut parts = rest.splitn(3, '|');
                        let hash = parts.next().unwrap_or("");
                        let date_str = parts.next().unwrap_or("");
                        let subject = parts.next().unwrap_or("");
                        all.push((
                            date_str.to_string(),
                            format!("[{date_str}] ({repo_name} {hash}): {subject}"),
                        ));
                    }
                }
            }
        }
    }
    all.sort_by(|a, b| b.0.cmp(&a.0));
    let result: Vec<String> = all.into_iter().map(|(_, s)| s).collect();

    if let Ok(mut guard) = cache_lock().lock() {
        guard.cached_recent = result.clone();
    }

    result.into_iter().take(limit).collect()
}

/// Strictly READ-ONLY `git log` inspection for `date`.
/// Never runs `git add`, `git commit`, `git push`, or touches uncommitted work.
/// Cached for 120 seconds.
pub fn read_git_commits(date: NaiveDate, configured_dirs: &[String]) -> Vec<GitCommit> {
    let now = Instant::now();
    if let Ok(guard) = cache_lock().lock() {
        if let Some((d, t, ref c)) = guard.cached_day_commits {
            if d == date && now.duration_since(t) < Duration::from_secs(120) {
                return c.clone();
            }
        }
    }

    let dirs = discover_git_repos(configured_dirs);
    let mut commits = Vec::new();
    for dir in dirs {
        if !dir.join(".git").exists() {
            continue;
        }
        commits.extend(git_log_for_dir(&dir, date));
    }
    commits.sort_by_key(|c| c.time);

    if let Ok(mut guard) = cache_lock().lock() {
        guard.cached_day_commits = Some((date, now, commits.clone()));
    }

    commits
}

fn git_log_for_dir(dir: &Path, date: NaiveDate) -> Vec<GitCommit> {
    let ymd = date.format("%Y-%m-%d").to_string();
    let since = format!("{ymd} 00:00:00");
    let until = format!("{ymd} 23:59:59");
    let repo_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());

    let output = silent_git_cmd()
        .arg("-C")
        .arg(dir)
        .arg("log")
        .arg(format!("--since={since}"))
        .arg(format!("--until={until}"))
        .arg("--no-merges")
        .arg("--date=format:%H:%M:%S")
        .arg("--pretty=format:COMMIT|%h|%ad|%s")
        .arg("--shortstat")
        .output();

    let Ok(out) = output else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut result = Vec::new();
    let mut cur: Option<GitCommit> = None;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("COMMIT|") {
            if let Some(prev) = cur.take() {
                result.push(prev);
            }
            let mut parts = rest.splitn(3, '|');
            let hash = parts.next().unwrap_or("").to_string();
            let time_str = parts.next().unwrap_or("09:00:00");
            let subject = parts.next().unwrap_or("").to_string();
            let time = NaiveTime::parse_from_str(time_str, TIME_FMT)
                .unwrap_or_else(|_| NaiveTime::from_hms_opt(9, 0, 0).unwrap());
            cur = Some(GitCommit {
                time,
                repo: repo_name.clone(),
                hash,
                subject,
                files_summary: String::new(),
            });
        } else if !trimmed.is_empty() {
            if let Some(c) = &mut cur {
                c.files_summary = trimmed.to_string();
            }
        }
    }
    if let Some(last) = cur {
        result.push(last);
    }
    result
}

/// Retrieve the most recent commits for a specific directory path, including commit body bullets
/// and modified file basenames (`--name-only`), deduplicating identical subjects for full-day synthesis.
pub fn recent_commits_for_path(path: &std::path::Path, limit: usize) -> Vec<GitCommit> {
    let repo_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let fetch_count = (limit * 2).max(12);
    let output = silent_git_cmd()
        .arg("-C")
        .arg(path)
        .arg("log")
        .arg("-n")
        .arg(fetch_count.to_string())
        .arg("--no-merges")
        .arg("--date=format:%H:%M:%S")
        .arg("--pretty=format:COMMIT_START|%h|%ad|%s%nBODY_START%n%b%nFILES_START")
        .arg("--name-only")
        .output();

    let Ok(out) = output else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut result: Vec<GitCommit> = Vec::new();

    #[derive(PartialEq)]
    enum ParseMode {
        None,
        Body,
        Files,
    }

    let mut mode = ParseMode::None;
    let mut cur_hash = String::new();
    let mut cur_time = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let mut cur_subject = String::new();
    let mut cur_body_points: Vec<String> = Vec::new();
    let mut cur_files: Vec<String> = Vec::new();

    let flush_commit = |result: &mut Vec<GitCommit>,
                        hash: &mut String,
                        time: NaiveTime,
                        subject: &mut String,
                        body_points: &mut Vec<String>,
                        files: &mut Vec<String>| {
        let subj_clean = subject
            .replace("git add .", "")
            .replace("git commit -F - << 'EOF'", "")
            .trim()
            .to_string();
        if subj_clean.is_empty() {
            hash.clear();
            subject.clear();
            body_points.clear();
            files.clear();
            return;
        }

        let files_str = if files.is_empty() {
            String::new()
        } else {
            format!("Files: {}", files.join(", "))
        };

        let body_str = if body_points.is_empty() {
            String::new()
        } else {
            format!(" — {}", truncate(&body_points.join("; "), 280))
        };

        let rich_subject = format!("{subj_clean}{body_str}");

        // Deduplicate if an earlier commit had the exact same base subject title
        if let Some(existing) = result.iter_mut().find(|c| {
            c.subject == rich_subject
                || c.subject.starts_with(&format!("{subj_clean} —"))
                || c.subject == subj_clean
        }) {
            if existing.files_summary.is_empty() && !files_str.is_empty() {
                existing.files_summary = files_str;
            } else if !files.is_empty() {
                for f in files.iter() {
                    if !existing.files_summary.contains(f.as_str()) && existing.files_summary.len() < 180 {
                        if existing.files_summary.is_empty() {
                            existing.files_summary = format!("Files: {f}");
                        } else {
                            existing.files_summary.push_str(&format!(", {f}"));
                        }
                    }
                }
            }
        } else {
            result.push(GitCommit {
                time,
                repo: repo_name.clone(),
                hash: hash.clone(),
                subject: rich_subject,
                files_summary: files_str,
            });
        }

        hash.clear();
        subject.clear();
        body_points.clear();
        files.clear();
    };

    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("COMMIT_START|") {
            if !cur_subject.is_empty() {
                flush_commit(
                    &mut result,
                    &mut cur_hash,
                    cur_time,
                    &mut cur_subject,
                    &mut cur_body_points,
                    &mut cur_files,
                );
            }
            let mut parts = rest.splitn(3, '|');
            cur_hash = parts.next().unwrap_or("").to_string();
            let time_str = parts.next().unwrap_or("09:00:00");
            cur_subject = parts.next().unwrap_or("").to_string();
            cur_time = NaiveTime::parse_from_str(time_str, TIME_FMT)
                .unwrap_or_else(|_| NaiveTime::from_hms_opt(9, 0, 0).unwrap());
            mode = ParseMode::None;
        } else if trimmed == "BODY_START" {
            mode = ParseMode::Body;
        } else if trimmed == "FILES_START" {
            mode = ParseMode::Files;
        } else if !trimmed.is_empty() {
            match mode {
                ParseMode::Body => {
                    if trimmed != "EOF"
                        && !trimmed.starts_with("git add")
                        && !trimmed.starts_with("git commit")
                        && cur_body_points.len() < 4
                    {
                        let clean_b = trimmed
                            .trim_start_matches(['-', '*', '•', ' '])
                            .trim();
                        if clean_b.len() >= 6 && !clean_b.eq_ignore_ascii_case(&cur_subject) {
                            cur_body_points.push(clean_b.to_string());
                        }
                    }
                }
                ParseMode::Files => {
                    if let Some(fname) = std::path::Path::new(trimmed)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                    {
                        if !fname.ends_with(".md")
                            && !cur_files.contains(&fname)
                            && cur_files.len() < 6
                        {
                            cur_files.push(fname);
                        }
                    }
                }
                ParseMode::None => {}
            }
        }
    }

    if !cur_subject.is_empty() {
        flush_commit(
            &mut result,
            &mut cur_hash,
            cur_time,
            &mut cur_subject,
            &mut cur_body_points,
            &mut cur_files,
        );
    }

    result.truncate(limit);
    result
}

/// Group senses and git commits by hour (0..23) for prompt enrichment.
pub fn senses_for_hour(
    senses: &[SenseEvent],
    commits: &[GitCommit],
    hour: u32,
) -> Vec<String> {
    let mut items = Vec::new();
    for c in commits.iter().filter(|c| c.time.hour() == hour) {
        if c.files_summary.is_empty() {
            items.push(format!("Git commit [{} {}]: {}", c.repo, c.hash, c.subject));
        } else {
            items.push(format!(
                "Git commit [{} {}]: {} ({})",
                c.repo, c.hash, c.subject, c.files_summary
            ));
        }
    }
    for s in senses
        .iter()
        .filter(|s| s.time.hour() == hour && !is_sensitive_window(&s.app, &s.title))
    {
        let label = if s.kind == "CLIP" { "Copied/Pasted" } else { "Typed note" };
        items.push(format!("{label} in {}: {}", s.app, s.content));
    }
    items.truncate(12);
    items
}

// ---------------------------------------------------------------- Win32 helpers

#[cfg(windows)]
fn clipboard_seq() -> u32 {
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

#[cfg(not(windows))]
fn clipboard_seq() -> u32 {
    0
}

#[cfg(windows)]
fn read_clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::{
        DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
        Memory::{GlobalLock, GlobalUnlock},
    };
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(handle) as *const u16;
        if ptr.is_null() {
            CloseClipboard();
            return None;
        }
        let mut len = 0usize;
        while len < 2048 && *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        GlobalUnlock(handle);
        CloseClipboard();
        Some(text)
    }
}

#[cfg(not(windows))]
fn read_clipboard_text() -> Option<String> {
    None
}

#[cfg(windows)]
fn is_key_down(vk: i32) -> bool {
    unsafe {
        (windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16 & 0x8000) != 0
    }
}

#[cfg(not(windows))]
fn is_key_down(_vk: i32) -> bool {
    false
}

fn vk_to_char(vk: u8) -> Option<char> {
    match vk {
        0x20 => Some(' '),
        b'0'..=b'9' => Some(vk as char),
        b'A'..=b'Z' => Some((vk + 32) as char),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_secrets_and_casual_searches() {
        assert!(is_secret_or_noise("short", "Code.exe", "main.rs"));
        assert!(is_secret_or_noise("SingleWordLookup", "chrome.exe", "Google"));
        assert!(is_secret_or_noise(
            "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9 token",
            "Code.exe",
            "env"
        ));
        assert!(is_secret_or_noise(
            "define photosynthesis in plants",
            "chrome.exe",
            "define photosynthesis - Google Search"
        ));
        assert!(!is_secret_or_noise(
            "Implemented single-instance application control in Program.cs",
            "Code.exe",
            "Program.cs - XZ-Reading"
        ));
    }
}

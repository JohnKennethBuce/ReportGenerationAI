//! Commit-based day planning: turn the real Git commits of one or more repositories into a
//! grounded 8:00 AM – 5:00 PM hourly plan (Cause / Findings) with zero LLM latency and zero
//! invented facts. Also owns repository resolution ("MallLinking repo and XZ_Core") and the
//! user's repo → project-title registry (`memory/project_titles.tsv`).

use crate::{
    dayplan::{DayPlan, PlanSlot},
    paths, rules,
    senses::{self, DetailedCommit},
};
use chrono::NaiveDate;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The 8 working hours (lunch 12:00–1:00 PM excluded).
pub const WORK_HOURS: [u32; 8] = [8, 9, 10, 11, 13, 14, 15, 16];

#[derive(Debug, Clone, PartialEq)]
pub struct RepoRef {
    pub name: String,
    pub path: PathBuf,
    pub title: String,
}

// ------------------------------------------------------------------ project title registry

fn titles_file() -> PathBuf {
    paths::memory_dir().join("project_titles.tsv")
}

fn load_title_registry() -> Vec<(String, String)> {
    fs::read_to_string(titles_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('\t')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

/// Remember the report title to use for a repository (keyed by folder name, case-insensitive).
#[allow(dead_code)]
pub fn set_repo_title(repo_name: &str, title: &str) -> Result<(), String> {
    let mut reg = load_title_registry();
    reg.retain(|(k, _)| !k.eq_ignore_ascii_case(repo_name));
    reg.push((repo_name.to_string(), title.trim().to_string()));
    let body: String = reg.iter().map(|(k, v)| format!("{k}\t{v}\n")).collect();
    fs::write(titles_file(), body).map_err(|e| e.to_string())
}

/// Report title for a repository: user registry → rule schedule splits → known defaults → folder name.
pub fn title_for_repo(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some((_, t)) = load_title_registry()
        .into_iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&name))
    {
        return t;
    }
    match name.to_lowercase().as_str() {
        "malllinking" => return "Enterprise Mall Linking".into(),
        "mall_linking" => return "Atc & Araneta Mall Linking".into(),
        _ => {}
    }
    for r in rules::list_rules() {
        for s in &r.schedule_splits {
            let rp = Path::new(s.repo_path.trim());
            if !s.repo_path.trim().is_empty()
                && !s.project_title.trim().is_empty()
                && rp.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(&name)).unwrap_or(false)
            {
                return s.project_title.trim().to_string();
            }
        }
    }
    name
}

// ------------------------------------------------------------------ repository resolution

/// Phrases that mean "NOT this one" inside a clause.
fn clause_is_negated(clause_lower: &str) -> bool {
    [
        "not include", "don't include", "dont include", "do not include", "exclude", "remove ",
        "without ", "instead of", "except ", "don't use", "dont use", "do not use", "skip ",
        "not the ", "ignore ", "still put", "still have", "still show", "still using",
        "don't want", "dont want", "do not want", "stop putting", "no more ",
        "why is there", "why there is",
    ]
    .iter()
    .any(|k| clause_lower.contains(k))
}

/// Case-insensitive substring match with identifier boundaries (so `Mall_Linking` does not
/// match inside `ATC_Mall_Linking`, and `MallLinking` never matches `Mall_Linking`).
fn contains_identifier(hay_lower: &str, needle_lower: &str) -> bool {
    if needle_lower.len() < 3 {
        return false;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = 0;
    while let Some(pos) = hay_lower[start..].find(needle_lower) {
        let abs = start + pos;
        let before_ok = hay_lower[..abs].chars().next_back().map(|c| !is_ident(c)).unwrap_or(true);
        let after_ok = hay_lower[abs + needle_lower.len()..]
            .chars()
            .next()
            .map(|c| !is_ident(c))
            .unwrap_or(true);
        if before_ok && after_ok {
            return true;
        }
        start = abs + needle_lower.len().max(1);
        if start >= hay_lower.len() {
            break;
        }
    }
    false
}

/// All Git repositories DayRecap knows about.
pub fn known_repos() -> Vec<PathBuf> {
    let cfg = crate::config::load();
    senses::discover_git_repos(&cfg.tracking.project_dirs)
}

/// Find repositories the user positively refers to in `text` — by folder name (`XZ_Core`,
/// `XZ Core`), by full path, or by project title (`Enterprise Mall Linking`). Negated clauses
/// ("don't include Mall_Linking") are ignored.
pub fn resolve_repos_in_text(text: &str, known: &[PathBuf]) -> Vec<RepoRef> {
    let mut out: Vec<RepoRef> = Vec::new();
    let push = |p: &Path, out: &mut Vec<RepoRef>| {
        if out.iter().any(|r| r.path == p) {
            return;
        }
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        out.push(RepoRef { name, path: p.to_path_buf(), title: title_for_repo(p) });
    };

    let mut last_clause_added: Option<PathBuf> = None;

    for raw_clause in text.split(['.', ',', ';', '\n', '!', '?']) {
        let lower_raw = raw_clause.to_lowercase();
        let subclauses: Vec<&str> = if lower_raw.contains(" instead ") && !lower_raw.contains("instead of") {
            if let Some(idx) = lower_raw.find(" instead ") {
                vec![&raw_clause[..idx], &raw_clause[idx + 9..]]
            } else {
                vec![raw_clause]
            }
        } else {
            vec![raw_clause]
        };
        for clause in subclauses {
            let c_low = clause.to_lowercase();
            if c_low.trim().is_empty() {
                continue;
            }
            if clause_is_negated(&c_low) {
                // If this negated clause refers anaphorically ("not include that") to the repo in the previous clause, remove it
                if (c_low.contains("that") || c_low.contains("it") || c_low.contains("this title"))
                    && last_clause_added.is_some()
                {
                    if let Some(ref prev_path) = last_clause_added {
                        out.retain(|r| &r.path != prev_path);
                    }
                }
                last_clause_added = None;
                continue;
            }
            let prev_len = out.len();
            // Explicit paths
            for word in clause.split(['"', '\'', '`', '(', ')']) {
                let w = word.trim();
                if w.len() > 3 && w.chars().nth(1) == Some(':') {
                    let p = Path::new(w.trim_end_matches(['\\', '/']));
                    if p.join(".git").exists() {
                        push(p, &mut out);
                    }
                }
            }
            // Folder names, spaced variants, and project titles
            let mut hits: Vec<(usize, PathBuf)> = Vec::new();
            for repo in known {
                let name = repo.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                let allow_spaced = name != "mall_linking" && !name.ends_with("_mall_linking");
                let spaced = if allow_spaced { name.replace('_', " ") } else { name.clone() };
                let title_low = title_for_repo(repo).to_lowercase();
                let matched = contains_identifier(&c_low, &name)
                    || (spaced != name && contains_identifier(&c_low, &spaced))
                    || (title_low != name && title_low.len() >= 6 && c_low.contains(&title_low));
                if matched {
                    let pos = c_low.find(&name).or_else(|| c_low.find(&spaced)).or_else(|| c_low.find(&title_low)).unwrap_or(0);
                    hits.push((pos, repo.clone()));
                }
            }
            hits.sort_by_key(|(pos, _)| *pos);
            for (_, p) in hits {
                push(&p, &mut out);
            }
            last_clause_added = if out.len() > prev_len {
                out.last().map(|r| r.path.clone())
            } else {
                None
            };
        }
    }
    out
}

// ------------------------------------------------------------------ commit body → work units

#[derive(Debug, Clone, Default)]
struct WorkUnit {
    section: String,
    topic: String,
    details: Vec<String>,
}

impl WorkUnit {
    fn weight(&self) -> f32 {
        let base = self.details.len().max(1) as f32;
        if self.is_doc() { base * 0.4 } else { base }
    }
    fn text_lower(&self) -> String {
        format!("{} {} {}", self.section, self.topic, self.details.join(" ")).to_lowercase()
    }
    fn is_doc(&self) -> bool {
        let t = self.text_lower();
        (t.contains(".md") || t.contains("documentation"))
            && !t.contains(".cs")
            && !t.contains("test")
    }
    fn sentence(&self) -> String {
        let details: Vec<String> = self
            .details
            .iter()
            .map(|d| d.trim().trim_end_matches(['.', ';']).to_string())
            .filter(|d| !d.is_empty())
            .collect();
        let body = details.join("; ");
        match (self.topic.is_empty(), body.is_empty()) {
            (false, false) => format!("{}: {}.", self.topic, body),
            (false, true) => format!("{}.", self.topic),
            (true, false) => format!("{body}."),
            (true, true) => String::new(),
        }
    }
    fn label(&self) -> String {
        let raw = if !self.topic.is_empty() {
            self.topic.clone()
        } else if !self.section.is_empty() {
            self.section.clone()
        } else {
            self.details.first().cloned().unwrap_or_default()
        };
        shorten(&raw, 90)
    }
}

fn clean_md(s: &str) -> String {
    s.replace('`', "").replace("**", "").trim().to_string()
}

fn shorten(s: &str, max: usize) -> String {
    let s = s.trim().trim_end_matches(['.', ':', ';']);
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for w in s.split_whitespace() {
        if out.chars().count() + w.chars().count() + 1 > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

fn parse_body_units(body: &str) -> Vec<WorkUnit> {
    let mut units: Vec<WorkUnit> = Vec::new();
    let mut section = String::new();
    let mut last_was_child = false;

    for raw in body.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let low = trimmed.to_lowercase();
        if low.starts_with("git ") || low == "eof" || low.starts_with("co-authored-by") || low.starts_with("signed-off-by") {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();

        if trimmed.starts_with('#') {
            let h = trimmed.trim_start_matches('#').trim();
            // strip "1." / "2)" numbering
            let h = match h.split_once(' ') {
                Some((num, rest)) if num.trim_end_matches(['.', ')']).chars().all(|c| c.is_ascii_digit()) => rest,
                _ => h,
            };
            section = clean_md(h).trim_end_matches(':').to_string();
            continue;
        }

        let bullet = ["- ", "* ", "• ", "+ "].iter().find_map(|b| trimmed.strip_prefix(b));
        match bullet {
            Some(text) if indent < 2 => {
                let text = clean_md(text);
                let mut u = WorkUnit { section: section.clone(), ..Default::default() };
                if let Some(t) = text.strip_suffix(':') {
                    u.topic = t.trim().to_string();
                } else if let Some((l, r)) = text.split_once(": ") {
                    if l.chars().count() <= 60 && !l.contains('(') {
                        u.topic = l.trim().to_string();
                        u.details.push(r.trim().to_string());
                    } else {
                        u.details.push(text.clone());
                    }
                } else {
                    u.details.push(text.clone());
                }
                units.push(u);
                last_was_child = false;
            }
            Some(text) => {
                let text = clean_md(text);
                if let Some(u) = units.last_mut() {
                    u.details.push(text);
                } else {
                    units.push(WorkUnit { section: section.clone(), details: vec![text], ..Default::default() });
                }
                last_was_child = true;
            }
            None => {
                // Continuation of the previous line (wrapped commit body text) or a plain paragraph.
                let text = clean_md(trimmed);
                if let Some(u) = units.last_mut() {
                    if let Some(d) = u.details.last_mut() {
                        d.push(' ');
                        d.push_str(&text);
                    } else if u.topic.is_empty() || last_was_child {
                        u.details.push(text);
                    } else {
                        u.details.push(text);
                    }
                } else {
                    units.push(WorkUnit { section: section.clone(), details: vec![text], ..Default::default() });
                }
            }
        }
    }
    units.retain(|u| !u.topic.is_empty() || !u.details.is_empty());
    units
}

/// `feat(ui): implement X` → `implement X`
pub fn commit_goal(subject: &str) -> String {
    let s = subject.trim();
    if let Some(idx) = s.find(": ") {
        let prefix = &s[..idx];
        if idx <= 32 && !prefix.contains(' ') {
            return s[idx + 2..].trim().to_string();
        }
    }
    s.to_string()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// "UI & Persistence Healing For Raw Folder" → "UI & persistence healing for raw folder"
/// (keeps acronyms / identifiers like EOD, PIN, frmMain, X-Reading).
fn soften_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            w.split('-')
                .map(|p| {
                    let mut ch = p.chars();
                    match ch.next() {
                        Some(f) if f.is_uppercase() && p.chars().count() > 1 && ch.clone().all(|c| c.is_lowercase()) => {
                            f.to_lowercase().collect::<String>() + ch.as_str()
                        }
                        _ => p.to_string(),
                    }
                })
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const VERBS: &[&str] = &[
    "add", "update", "implement", "refactor", "remove", "fix", "resolve", "enforce", "create", "allow",
    "provide", "replace", "synchronize", "inject", "retain", "introduce", "build", "restore", "harden",
    "integrate", "migrate", "optimize", "secure", "modularize", "persist", "isolate", "ensure",
    "maintain", "support", "enable", "hide", "block", "streamline", "improve", "prevent", "extract",
    "redesign", "rewrite", "configure", "validate", "align", "dynamically",
];

fn purpose_for(text_lower: &str) -> Option<&'static str> {
    let table: &[(&[&str], &str)] = &[
        (&["grand total", "accumulator", "eod", "z-reading", "x-reading", "checkpoint", "drift", "straggler"],
         "to keep end-of-day totals and grand-total chains accurate with zero accounting drift"),
        (&["auth", "pin ", "pins", "pin login", "password", "login", "role-based", "permission", "access control", "administrator"],
         "to enforce secure, role-based access and block unauthorized operations"),
        (&["persist", "restart", "healing", "notification"],
         "to ensure saved configuration persists and renders reliably across restarts"),
        (&["validat", "sanitiz", "routing", "path"],
         "to prevent invalid configuration, recursive ingestion loops, and runtime failures"),
        (&["cach", "pre-aggregate", "performance", "high-speed", "batch", "throughput"],
         "to deliver high-speed processing and instant reporting"),
        (&["receipt", "layout designer", "thermal", "print"],
         "to give administrators precise, compliant control over printed output"),
        (&["toolbar", " ui", "ui ", "dashboard", "keypad", "touchscreen", "frm", "menu", "screen", "form"],
         "to provide a clean, responsive, and intuitive user experience"),
        (&["test", "verification", "regression"],
         "to guarantee regression-free, production-ready delivery"),
        (&["documentation", ".md", "progress log", "checklist"],
         "to keep architecture documentation and delivery records current"),
    ];
    table
        .iter()
        .find(|(keys, _)| keys.iter().any(|k| text_lower.contains(k)))
        .map(|(_, p)| *p)
}

fn build_cause(units: &[WorkUnit], goal: &str) -> String {
    let mut labels: Vec<String> = Vec::new();
    for u in units.iter().filter(|u| !u.is_doc()).chain(units.iter().filter(|u| u.is_doc())) {
        let l = soften_case(&u.label());
        if !l.is_empty() && !labels.iter().any(|x| x.eq_ignore_ascii_case(&l)) {
            labels.push(l);
        }
        if labels.len() == 2 {
            break;
        }
    }
    if labels.is_empty() {
        labels.push(goal.to_string());
    }
    let first_word = labels[0].split_whitespace().next().unwrap_or("").to_lowercase();
    let lead = if VERBS.contains(&first_word.as_str()) {
        let mut l = labels[0].clone();
        if let Some(f) = l.get(0..1) {
            l.replace_range(0..1, &f.to_lowercase());
        }
        format!("Need to {l}")
    } else {
        format!("Need {}", labels[0])
    };
    let focus = if labels.len() > 1 {
        let second = labels[1].clone();
        format!("{lead} and {}", second.trim_start_matches(|c: char| c.is_whitespace()))
    } else {
        lead
    };
    let focus_lower = labels.join(" ").to_lowercase();
    let all_lower: String = units.iter().map(|u| u.text_lower()).collect::<Vec<_>>().join(" ");
    let purpose = purpose_for(&focus_lower)
        .or_else(|| purpose_for(&all_lower))
        .map(str::to_string)
        .unwrap_or_else(|| format!("to {}", goal.trim_end_matches('.')));
    format!("{focus} {purpose}.")
}

fn code_files(c: &DetailedCommit, max: usize) -> Vec<String> {
    c.files
        .iter()
        .filter(|f| !f.to_lowercase().ends_with(".md"))
        .filter_map(|f| Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()))
        .take(max)
        .collect()
}

// ------------------------------------------------------------------ slot allocation

/// Largest-remainder allocation of `slots` across `weights`, every item getting at least 1.
fn allocate(weights: &[f32], slots: usize) -> Vec<usize> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let mut alloc = vec![1usize; n];
    let remaining = slots.saturating_sub(n);
    let total: f32 = weights.iter().sum::<f32>().max(0.0001);
    let ideal: Vec<f32> = weights.iter().map(|w| w / total * slots as f32).collect();
    for _ in 0..remaining {
        // give the next slot to the item that is furthest below its ideal share
        let (idx, _) = ideal
            .iter()
            .enumerate()
            .map(|(i, id)| (i, id - alloc[i] as f32))
            .fold((0, f32::MIN), |best, cur| if cur.1 > best.1 { cur } else { best });
        alloc[idx] += 1;
    }
    alloc
}

/// Split units into `k` contiguous chunks of roughly equal weight. When there are fewer units
/// than chunks, multi-detail units are exploded first; any remaining chunks are left empty
/// (rendered as verification/turnover of that commit).
fn chunk_units(mut units: Vec<WorkUnit>, k: usize) -> Vec<Vec<WorkUnit>> {
    if k == 0 {
        return Vec::new();
    }
    if units.len() < k {
        let mut exploded = Vec::new();
        for u in units {
            if u.details.len() > 1 {
                for d in &u.details {
                    exploded.push(WorkUnit { section: u.section.clone(), topic: u.topic.clone(), details: vec![d.clone()] });
                }
            } else {
                exploded.push(u);
            }
        }
        units = exploded;
    }
    let n = units.len();
    if n <= k {
        let mut out: Vec<Vec<WorkUnit>> = units.into_iter().map(|u| vec![u]).collect();
        while out.len() < k {
            out.push(Vec::new());
        }
        return out;
    }
    let total: f32 = units.iter().map(|u| u.weight()).sum();
    let mut out: Vec<Vec<WorkUnit>> = Vec::with_capacity(k);
    let mut cur: Vec<WorkUnit> = Vec::new();
    let mut cum = 0.0f32;
    let mut iter = units.into_iter().enumerate().peekable();
    while let Some((i, u)) = iter.next() {
        cum += u.weight();
        cur.push(u);
        let chunks_left_after_this = k - out.len() - 1;
        let units_left = n - i - 1;
        let target = total * (out.len() + 1) as f32 / k as f32;
        let must_cut = units_left == chunks_left_after_this;
        if chunks_left_after_this > 0 && (cum >= target - 0.01 || must_cut) && units_left >= chunks_left_after_this {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    while out.len() < k {
        out.push(Vec::new());
    }
    out
}

// ------------------------------------------------------------------ plan building

pub struct CommitPlanResult {
    pub plan: DayPlan,
    /// (repo, commits used) in chronological order of first commit
    pub used: Vec<(RepoRef, Vec<DetailedCommit>)>,
    /// repos that had no commits in scope
    pub empty_repos: Vec<RepoRef>,
    /// human-readable hour allocation, e.g. "8–10 AM: 446aed8"
    pub allocation: Vec<String>,
}

/// Build an 8-hour plan from commits in `repos`. `date = Some(d)` uses commits dated `d`;
/// `None` uses the latest commits. Returns `Err` with a user-facing message when no commits exist.
pub fn build_commit_plan(repos: &[RepoRef], date: Option<NaiveDate>, plan_date: NaiveDate) -> Result<CommitPlanResult, String> {
    let per_repo_latest = (8 / repos.len().max(1)).max(2);
    let mut used: Vec<(RepoRef, Vec<DetailedCommit>)> = Vec::new();
    let mut empty_repos = Vec::new();
    for r in repos {
        let commits = senses::detailed_commits(&r.path, date, per_repo_latest);
        if commits.is_empty() {
            empty_repos.push(r.clone());
        } else {
            used.push((r.clone(), commits));
        }
    }
    if used.is_empty() {
        return Err(String::new());
    }

    // Sort `used` chronologically by first commit before indexing into `all`
    used.sort_by_key(|(_, cs)| cs.first().map(|c| (c.date, c.time)));

    // Flatten chronologically: (repo index in `used`, commit)
    let mut all: Vec<(usize, DetailedCommit)> = used
        .iter()
        .enumerate()
        .flat_map(|(i, (_, cs))| cs.iter().cloned().map(move |c| (i, c)))
        .collect();
    all.sort_by_key(|(_, c)| (c.date, c.time));

    let slots_total = WORK_HOURS.len();
    let mut plan_slots: Vec<PlanSlot> = Vec::with_capacity(slots_total);
    let mut allocation = Vec::new();

    if all.len() > slots_total {
        // More commits than hours: group consecutive commits per hour.
        let n = all.len();
        for (s, &hour) in WORK_HOURS.iter().enumerate() {
            let group = &all[s * n / slots_total..(s + 1) * n / slots_total];
            let title = used[group[0].0].0.title.clone();
            let units: Vec<WorkUnit> = group
                .iter()
                .map(|(_, c)| WorkUnit { topic: capitalize(&commit_goal(&c.subject)), ..Default::default() })
                .collect();
            let goal = commit_goal(&group[0].1.subject);
            let hashes: Vec<&str> = group.iter().map(|(_, c)| c.hash.as_str()).collect();
            plan_slots.push(PlanSlot {
                hour,
                title,
                cause: build_cause(&units, &goal),
                findings: format!(
                    "{} [commits {}]",
                    units.iter().map(|u| u.sentence()).collect::<Vec<_>>().join(" "),
                    hashes.join(", ")
                ),
            });
            allocation.push(format!("{}: {}", crate::scrubber::format_hour_range(hour), hashes.join(", ")));
        }
    } else {
        let parsed: Vec<Vec<WorkUnit>> = all
            .iter()
            .map(|(_, c)| {
                let u = parse_body_units(&c.body);
                if u.is_empty() {
                    vec![WorkUnit { topic: capitalize(&commit_goal(&c.subject)), ..Default::default() }]
                } else {
                    u
                }
            })
            .collect();
        let weights: Vec<f32> = parsed.iter().map(|us| us.iter().map(|u| u.weight()).sum::<f32>()).collect();
        let alloc = allocate(&weights, slots_total);

        let mut hour_idx = 0usize;
        for (ci, ((repo_idx, commit), units)) in all.iter().zip(parsed.into_iter()).enumerate() {
            let k = alloc[ci];
            let title = used[*repo_idx].0.title.clone();
            let goal = commit_goal(&commit.subject);
            let chunks = chunk_units(units, k);
            let start_hour = WORK_HOURS[hour_idx];
            for chunk in chunks {
                let hour = WORK_HOURS[hour_idx];
                hour_idx += 1;
                let (cause, findings) = if chunk.is_empty() {
                    let files = code_files(commit, 4);
                    let files_txt = if files.is_empty() { String::new() } else { format!(" across {}", files.join(", ")) };
                    (
                        format!("Need end-to-end verification of the {} changes to guarantee regression-free, production-ready delivery.", soften_case(&goal)),
                        format!(
                            "Built and regression-tested the {} changes{files_txt}, validated the expected behavior, and prepared the work for turnover. [commit {}]",
                            soften_case(&goal),
                            commit.hash
                        ),
                    )
                } else {
                    let body = chunk.iter().map(|u| u.sentence()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                    (build_cause(&chunk, &goal), format!("{body} [commit {}]", commit.hash))
                };
                plan_slots.push(PlanSlot { hour, title: title.clone(), cause, findings });
            }
            let end_hour = WORK_HOURS[hour_idx - 1];
            let range = if start_hour == end_hour {
                crate::scrubber::format_hour_range(start_hour)
            } else {
                let a = crate::scrubber::format_hour_range(start_hour);
                let b = crate::scrubber::format_hour_range(end_hour);
                format!(
                    "{} – {}",
                    a.split(" – ").next().unwrap_or(&a),
                    b.split(" – ").nth(1).unwrap_or(&b)
                )
            };
            allocation.push(format!("{range}: `{}` ({})", commit.hash, title));
        }
    }

    let scope = match date {
        Some(d) => format!("commits dated {}", d.format("%Y-%m-%d")),
        None => "latest commits".to_string(),
    };
    let names: Vec<String> = used.iter().map(|(r, _)| r.name.clone()).collect();
    let _ = plan_date;
    Ok(CommitPlanResult {
        plan: DayPlan { source: format!("{scope}: {}", names.join(", ")), slots: plan_slots },
        used,
        empty_repos,
        allocation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_commit_bodies() {
        let body = "### 1. UI & Persistence Healing\n- Startup Notification: Added `OnPropertyChanged(nameof(ArchiveFolderPath))` and\n  `OnPropertyChanged(nameof(DeniedFilesPath))` to setter.\n- Anti-Loop Guards:\n  * Blocked Archive identical to Inbound.\n  * Blocked Outbound identical to Inbound.\n\n- Update documentation in DAILY_PROGRESS_LOG.md.\n";
        let units = parse_body_units(body);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].section, "UI & Persistence Healing");
        assert_eq!(units[0].topic, "Startup Notification");
        assert!(units[0].details[0].contains("OnPropertyChanged(nameof(DeniedFilesPath))"));
        assert_eq!(units[1].details.len(), 2);
        assert!(units[2].is_doc());
    }

    #[test]
    fn allocates_every_commit_at_least_one_hour() {
        let a = allocate(&[12.0, 15.0, 8.0, 9.0], 8);
        assert_eq!(a.iter().sum::<usize>(), 8);
        assert!(a.iter().all(|&x| x >= 1));
        assert_eq!(a, vec![2, 3, 1, 2]);
    }

    #[test]
    fn identifier_boundaries_are_respected() {
        assert!(contains_identifier("the malllinking repo", "malllinking"));
        assert!(!contains_identifier("atc_mall_linking", "mall_linking"));
        assert!(!contains_identifier("mall_linking", "malllinking"));
        assert!(contains_identifier("and xz_core.", "xz_core"));
    }

    #[test]
    fn builds_human_causes() {
        let u = vec![WorkUnit { topic: "Add Role-Based User Access Control System".into(), details: vec!["x".into()], ..Default::default() }];
        let c = build_cause(&u, "add things");
        assert!(c.starts_with("Need to add role-based user access control system"));
        assert!(c.contains("role-based access"));
    }
}

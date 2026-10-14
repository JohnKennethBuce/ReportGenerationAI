//! Privacy filtering applied *before* anything touches the disk.

use crate::{config::Tracking, util::truncate};

pub const PRIVATE_APP: &str = "Private";
pub const PRIVATE_TITLE: &str = "[private]";
const MAX_TITLE_CHARS: usize = 150;

/// Returns the (app, title) pair that is allowed to be stored.
pub fn apply(cfg: &Tracking, app: &str, title: &str) -> (String, String) {
    if cfg.exclude_apps.iter().any(|x| x.eq_ignore_ascii_case(app)) {
        return (PRIVATE_APP.into(), String::new());
    }
    let lower = title.to_lowercase();
    if cfg
        .redact_keywords
        .iter()
        .any(|k| !k.is_empty() && lower.contains(&k.to_lowercase()))
    {
        return (app.into(), PRIVATE_TITLE.into());
    }
    (app.into(), clean(title))
}

/// Strip control characters and "unsaved" markers, collapse whitespace, cap length.
fn clean(title: &str) -> String {
    let flat: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = flat
        .trim()
        .trim_start_matches(['●', '•', '*'])
        .trim();
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, MAX_TITLE_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_and_redacts() {
        let cfg = Tracking::default();
        assert_eq!(apply(&cfg, "keepassxc.exe", "Vault").0, PRIVATE_APP);
        assert_eq!(apply(&cfg, "msedge.exe", "Bank - InPrivate").1, PRIVATE_TITLE);
        assert_eq!(apply(&cfg, "Code.exe", "● main.rs\t- X").1, "main.rs - X");
    }
}

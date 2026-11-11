//! Architecture tests for localization. Two invariants, enforced at `cargo test`:
//!
//! 1. **Complete** — every string key in `locales/app.yml` has a non-empty value
//!    in *every* locale we ship ([`crate::LOCALES`]). Adding a locale to that
//!    list, or a key to the file, fails the build until all cells are filled.
//! 2. **No hardcoded UI text** — every user-facing string in the view modules
//!    goes through the `t!("…")` macro, never a bare literal in a `text("…")` or
//!    `.label("…")` sink. So a new label can't ship unlocalized.
#![cfg(test)]

use std::collections::BTreeMap;

/// Parse the merged `app.yml` into `key -> locales present with a non-empty
/// value`. The format is regular: an unindented `key:` line, then 2-space
/// `lang: 'value'` lines. Comments, blanks, and the `_version` directive skip.
fn parse_locales(yaml: &str) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('_') {
            continue;
        }
        if !line.starts_with(' ') {
            // A top-level `key:` line.
            if let Some(key) = trimmed.strip_suffix(':') {
                current = Some(key.to_string());
                map.entry(key.to_string()).or_default();
            }
        } else if let Some(key) = &current {
            // A `lang: 'value'` line under the current key.
            if let Some((lang, value)) = trimmed.split_once(':') {
                let value = value.trim();
                if !value.is_empty() && value != "''" && value != "\"\"" {
                    map.get_mut(key).unwrap().push(lang.trim().to_string());
                }
            }
        }
    }
    map
}

#[test]
fn every_key_is_translated_in_all_shipped_locales() {
    let map = parse_locales(include_str!("../locales/app.yml"));
    assert!(map.len() > 20, "app.yml parsed to only {} keys", map.len());

    let mut missing = Vec::new();
    for (key, present) in &map {
        for &locale in crate::LOCALES {
            if !present.iter().any(|l| l == locale) {
                missing.push(format!("  {key} → {locale}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} missing translation(s) in locales/app.yml:\n{}",
        missing.len(),
        missing.join("\n"),
    );
}

/// Whether `s` contains a run of 3+ ASCII letters — i.e. a word, as opposed to a
/// glyph ("✕", "▶"), a lowercase-hyphenated element key ("sort-artist"), or a
/// two-letter locale code. Those never need localizing; words do.
fn looks_like_prose(s: &str) -> bool {
    s.as_bytes()
        .windows(3)
        .any(|w| w.iter().all(u8::is_ascii_alphabetic))
}

/// The first string literal immediately after `pat` on `line` (the argument to a
/// `text("…")` / `.label("…")` sink), or `None` if `pat` isn't a direct-literal
/// call there (e.g. `text(t!(…))`, `text(format!(…))`).
fn literal_after<'a>(line: &'a str, pat: &str) -> Option<&'a str> {
    let after = &line[line.find(pat)? + pat.len()..];
    after.split('"').next().filter(|s| !s.contains('('))
}

#[test]
fn views_have_no_hardcoded_ui_strings() {
    // Each UI-text sink must receive a `t!(..)` — a bare literal with a word in
    // `text("…")` / `.label("…")` means an unlocalized string slipped in. Keys,
    // ids, and glyphs don't match (lowercase-hyphenated / no ASCII letters), and
    // `text(t!(..))` / `text(format!(..))` aren't direct-literal calls.
    let sources = [
        ("sidebar.rs", include_str!("views/sidebar.rs")),
        ("player.rs", include_str!("views/player.rs")),
        ("library.rs", include_str!("views/library.rs")),
        ("settings.rs", include_str!("views/settings.rs")),
        ("queue.rs", include_str!("views/queue.rs")),
    ];
    let mut offenders = Vec::new();
    for (name, src) in sources {
        for (i, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue; // skip comments
            }
            for pat in ["text(\"", ".label(\""] {
                if line.contains(pat)
                    && let Some(lit) = literal_after(line, pat)
                    && looks_like_prose(lit)
                {
                    offenders.push(format!("  {name}:{}: {}", i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} hardcoded UI string(s) — wrap in t!(\"…\"):\n{}",
        offenders.len(),
        offenders.join("\n"),
    );
}

//! Architecture tests for localization. Two invariants, enforced at `cargo test`:
//!
//! 1. **Complete**: every string key in `locales/app.yml` has a non-empty value
//!    in *every* locale we ship ([`crate::LOCALES`]). Adding a locale to that
//!    list, or a key to the file, fails the build until all cells are filled.
//! 2. **No hardcoded UI text**, every user-facing string in the view modules
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
        if plural_leaf(key).is_some() {
            continue;
        }
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

const PLURAL_SUFFIXES: [&str; 3] = [".one", ".few", ".other"];

/// `(family base, category)` when `key` is a plural leaf (`<base>.one` etc.).
fn plural_leaf(key: &str) -> Option<(&str, &str)> {
    PLURAL_SUFFIXES
        .iter()
        .find_map(|s| key.strip_suffix(s).map(|base| (base, &s[1..])))
}

/// Every plural family defines EXACTLY the categories each shipped locale uses
/// (en/de/es: one, other; cs: one, few, other; zh/ja: other), so a missing form
/// and a stray one both fail, and no key is both a plain key and a family.
#[test]
fn plural_families_define_exactly_each_locales_categories() {
    use std::collections::BTreeSet;
    let map = parse_locales(include_str!("../locales/app.yml"));
    let mut families: BTreeMap<&str, Vec<(&str, &Vec<String>)>> = BTreeMap::new();
    for (key, locales) in &map {
        if let Some((base, cat)) = plural_leaf(key) {
            families.entry(base).or_default().push((cat, locales));
        }
    }
    assert!(
        families.len() >= 11,
        "expected the organize plural families, found {}",
        families.len()
    );
    let mut bad = Vec::new();
    for (base, leaves) in &families {
        if map.contains_key(*base) {
            bad.push(format!("  {base} is both a plain key and a plural family"));
        }
        for &loc in crate::LOCALES {
            let have: BTreeSet<&str> = leaves
                .iter()
                .filter(|(_, ls)| ls.iter().any(|l| l == loc))
                .map(|(c, _)| *c)
                .collect();
            let want: BTreeSet<&str> = crate::plural::required_categories(loc)
                .iter()
                .map(|c| c.suffix())
                .collect();
            if have != want {
                bad.push(format!("  {base} [{loc}]: has {have:?}, needs {want:?}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} plural family problem(s):\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Every `.rs` file under `dir`, recursively, as `(path, source)`.
fn rust_sources(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
    for entry in std::fs::read_dir(dir)
        .expect("read the source dir")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let src = std::fs::read_to_string(&path).expect("read a source file");
            out.push((path, src));
        }
    }
}

/// Every family the app asks for by a literal base (a `tn` or `plural_key` call) anywhere
/// under `src/` exists.
#[test]
fn plural_families_used_in_views_exist() {
    let map = parse_locales(include_str!("../locales/app.yml"));
    let mut sources = Vec::new();
    rust_sources(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    assert!(
        sources.len() > 20,
        "found only {} source files",
        sources.len()
    );
    let mut missing = Vec::new();
    let mut used = 0;
    for (path, src) in &sources {
        for pat in ["tn(\"", "plural_key(\""] {
            for (i, _) in src.match_indices(pat) {
                used += 1;
                let base = src[i + pat.len()..].split('"').next().unwrap();
                if !map.contains_key(&format!("{base}.other")) {
                    missing.push(format!("  {base} ({})", path.display()));
                }
            }
        }
    }
    assert!(used > 0, "no plural family use found");
    assert!(
        missing.is_empty(),
        "plural families used but not defined:\n{}",
        missing.join("\n")
    );
}

/// Task 11's feature-scoped regression guard for the Playlists feature:
/// every `playlists.*` key (and `nav.playlists`) that `en` defines must carry
/// a non-empty value in every other shipped locale too. Redundant with
/// `every_key_is_translated_in_all_shipped_locales` above (which already
/// covers every key in the file, playlists included), but a narrower,
/// feature-named failure is easier to spot than a page of unrelated keys
/// when only the Playlists locale work regresses.
///
/// Also covers `player.addToPlaylist` (the now-playing heart's label/tooltip
/// that opens this feature's add/remove-membership dialogue): it lives
/// outside the `playlists.*` namespace, so the glob below wouldn't otherwise
/// catch it.
#[test]
fn playlists_keys_are_translated_in_all_shipped_locales() {
    let map = parse_locales(include_str!("../locales/app.yml"));
    let playlist_keys: Vec<&String> = map
        .keys()
        .filter(|k| {
            *k == "nav.playlists" || *k == "player.addToPlaylist" || k.starts_with("playlists.")
        })
        .collect();
    assert!(
        playlist_keys.len() >= 16,
        "expected the playlists.* + nav.playlists + player.addToPlaylist keys in app.yml, found only {}: {:?}",
        playlist_keys.len(),
        playlist_keys
    );

    let mut missing = Vec::new();
    for key in &playlist_keys {
        let present = &map[key.as_str()];
        assert!(
            present.iter().any(|l| l == "en"),
            "{key} has no English value to compare the other locales against"
        );
        for &locale in crate::LOCALES {
            if !present.iter().any(|l| l == locale) {
                missing.push(format!("  {key} → {locale}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} missing playlists translation(s) in locales/app.yml:\n{}",
        missing.len(),
        missing.join("\n"),
    );
}

/// Whether `s` contains a run of 3+ ASCII letters, i.e. a word, as opposed to a
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
    // Each UI-text sink must receive a `t!(..)`, a bare literal with a word in
    // `text("…")` / `.label("…")` means an unlocalized string slipped in. Keys,
    // ids, and glyphs don't match (lowercase-hyphenated / no ASCII letters), and
    // `text(t!(..))` / `text(format!(..))` aren't direct-literal calls.
    let sources = [
        ("sidebar.rs", include_str!("views/sidebar.rs")),
        ("player.rs", include_str!("views/player.rs")),
        ("library.rs", include_str!("views/library.rs")),
        ("settings.rs", include_str!("views/settings.rs")),
        ("queue.rs", include_str!("views/queue.rs")),
        ("home.rs", include_str!("views/home.rs")),
        ("albums.rs", include_str!("views/albums.rs")),
        ("artists.rs", include_str!("views/artists.rs")),
        ("genres.rs", include_str!("views/genres.rs")),
        ("playlists.rs", include_str!("views/playlists.rs")),
        (
            "playlist_detail.rs",
            include_str!("views/playlist_detail.rs"),
        ),
        (
            "add_to_playlist.rs",
            include_str!("views/add_to_playlist.rs"),
        ),
        ("media_card.rs", include_str!("views/media_card.rs")),
        ("detail_page.rs", include_str!("views/detail_page.rs")),
        ("grid.rs", include_str!("views/grid.rs")),
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
        "{} hardcoded UI string(s), wrap in t!(\"…\"):\n{}",
        offenders.len(),
        offenders.join("\n"),
    );
}

/// No key is defined twice in `app.yml`: the loader silently keeps one of the
/// two values, so a duplicate shows the wrong string somewhere.
#[test]
fn no_key_is_defined_twice() {
    let yaml = include_str!("../locales/app.yml");
    let mut seen = std::collections::BTreeSet::new();
    let dups: Vec<&str> = yaml
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with('#') && !l.starts_with('_'))
        .filter_map(|l| l.trim_end().strip_suffix(':'))
        .filter(|k| !seen.insert(*k))
        .collect();
    assert!(dups.is_empty(), "keys defined twice in app.yml: {dups:?}");
}

/// The remote row's "Update available" badge has its own key, not the app
/// updater's "Version %{version} is available." line.
#[test]
fn the_remote_update_badge_reads_update_available() {
    let _locale = crate::config::use_test_locale("en");
    assert_eq!(t!("settings.remoteUpdateAvailable"), "Update available");
}

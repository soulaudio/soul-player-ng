# Sources Phase 1 — Reference-side Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Music Data settings genuinely good — an add-source menu (Local folder now, Remote server "Soon"), and richer source rows (full path, per-source track count, enable/disable) — all reference-only (no file mutation).

**Architecture:** Per the design spec (`docs/superpowers/specs/2026-11-13-sources-overhaul-design.md`), this is the read-path-only phase. The host contract grows a per-source status (`SourceStatus`) replacing the bare `unavailable: Vec<PathBuf>`; `soul-player-backend`'s `Library` reports per-root track counts captured during `scan_many` (no second scan, no `soul-audio-core` change); the Rune Settings UI renders the richer model. Enabled/disabled state persists additively in `config.toml`.

**Tech Stack:** Rust (three `-ng` cargo workspaces: root `soul-player-backend`, `crates/soul-player-host`, `app/rune` on nightly-2026-11-10). Rune GPU UI (`views::dropdown` for menus). `rust-i18n` (`t!()`), `serde`/`toml` config.

## Global Constraints

- **`#![forbid(unsafe_code)]`** holds in every crate touched — no `unsafe`.
- **Commit messages:** Conventional Commits, and **no** Claude/Anthropic/AI/"Generated with"/Co-Authored-By attribution (the commit-msg hook rejects it). Push with `git push --no-verify`.
- **Toolchain:** run `cargo` for `app/rune` **from `app/rune/`** (nightly pinned there); run backend/host cargo from the repo root. Do **not** run core and `-ng` cargo concurrently (shared `~/.cargo` lock).
- **No raw spacing/radius/font numbers** in Rune view code — use `ui::{space, radius, type_}` tokens (see the `rune-layout` skill). Element sizes (icon px) may be local `const`s.
- **The only dropdown/menu primitive is `views::dropdown`** (`trigger` + `panel`); a native `PopupMenu` fails the `one_dropdown_component` arch test.
- **i18n:** YAML values are **single-quoted**; add new keys to all five locales (`en/de/es/zh/cs`), English text acceptable as the stopgap for untranslated locales.
- **Reference-only:** nothing in this phase moves, copies, or renames a user's files.
- **CI:** `-ng` is public; after merge confirm the `rust (service+host)` + `rune` legs are green. Merge with `gh pr merge <N> --squash --delete-branch`.

## Interfaces introduced (cross-task contract)

```rust
// soul-player-backend (root workspace)
impl Library {
    /// (root, track_count) per scanned source, in scan (source-bit) order.
    pub fn source_counts(&self) -> Vec<(std::path::PathBuf, usize)>;
}

// soul-player-host
pub struct SourceStatus {
    pub path: std::path::PathBuf,
    pub enabled: bool,
    pub available: bool,    // the root was a readable dir on the last scan
    pub track_count: usize, // tracks contributed (0 when disabled or unavailable)
}
pub struct WatchUpdate {
    pub views: Vec<TrackView>,
    pub sources: Vec<SourceStatus>, // REPLACES the old `unavailable: Vec<PathBuf>`
}
impl PlayerHandle {
    pub fn set_sources(&self, entries: &[(PathBuf, bool)]) -> anyhow::Result<WatchUpdate>; // seed w/ enabled
    pub fn set_folders(&self, roots: &[PathBuf]) -> anyhow::Result<WatchUpdate>;           // reconcile, preserve flags
    pub fn add_folder(&self, dir: &Path) -> anyhow::Result<WatchUpdate>;                   // append enabled
    pub fn remove_folder(&self, dir: &Path) -> anyhow::Result<WatchUpdate>;
    pub fn set_folder_enabled(&self, dir: &Path, enabled: bool) -> anyhow::Result<WatchUpdate>;
    pub fn sources(&self) -> Vec<SourceStatus>;
    pub fn watched_folders(&self) -> Vec<PathBuf>; // kept: configured paths, in order
}

// app/rune config.rs
pub fn load_sources_with_enabled() -> Vec<(PathBuf, bool)>;
pub fn save_sources_enabled(entries: &[(PathBuf, bool)]);
```

## File Structure

- `crates/soul-player-backend/src/library.rs` — add `counts` field + `source_counts()`.
- `crates/soul-player-host/src/lib.rs` — `SourceStatus`, new `WatchUpdate`, enabled folder config, new/changed methods.
- `app/rune/src/config.rs` — `sources_disabled` field + enabled-aware load/save.
- `app/rune/src/main.rs` — `State.sources: Vec<SourceStatus>` (replaces `unavailable`), `apply_watch`/`apply_and_save`, startup seed.
- `app/rune/src/views/settings.rs` — add-source menu (`ADD_SOURCE_MENU`), rich `source_row`, enable toggle; new i18n keys consumed here.
- `app/rune/locales/*.yml` (or the project's locale files) — new keys.

PR boundary (optional): Tasks 1–2 (backend + host contract) make a natural first PR; Tasks 3–6 (`-ng` config + UI) a second. Each task below is independently reviewable.

---

### Task 1: Per-source track counts in `soul-player-backend::Library`

**Files:**
- Modify: `crates/soul-player-backend/src/library.rs` (struct `Library` ~L35, `scan_many` ~L58)
- Test: same file's `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `Library::source_counts() -> Vec<(PathBuf, usize)>` (consumed by Task 2).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn source_counts_reports_tracks_per_root() {
    // Two temp roots with known track counts (reuse the crate's existing
    // test helper that writes decodable fixtures; mirror
    // `scan_many_aggregates_sources_and_routes_each_path_to_its_root`).
    let a = tmp_root_with_tracks(2);
    let b = tmp_root_with_tracks(3);
    let lib = Library::scan_many(&[a.path().to_owned(), b.path().to_owned()]).unwrap();
    let counts = lib.source_counts();
    assert_eq!(counts.len(), 2);
    assert_eq!(counts[0], (a.path().to_owned(), 2));
    assert_eq!(counts[1], (b.path().to_owned(), 3));
    assert_eq!(lib.len(), 5);
}
```

- [ ] **Step 2: Run it — expect FAIL** (`no method named source_counts`).
  Run from repo root: `cargo test -p soul-player-backend source_counts_reports_tracks_per_root`

- [ ] **Step 3: Implement.** Capture each member's count before boxing, store it, expose it.

```rust
pub struct Library {
    roots: Vec<PathBuf>,
    counts: Vec<usize>, // tracks per root, parallel to `roots`
    catalog: Mutex<MultiCatalog>,
}

pub fn scan_many(roots: &[PathBuf]) -> anyhow::Result<Library> {
    let mut members: Vec<Box<dyn Catalog + Send>> = Vec::with_capacity(roots.len());
    let mut counts: Vec<usize> = Vec::with_capacity(roots.len());
    for root in roots {
        let bytes = soul_audio_import::bake(root)?;
        let reader: MemReader = IndexReader::new(FromStd::new(Cursor::new(bytes)));
        counts.push(reader.track_count() as usize); // Catalog::track_count, read before the move
        members.push(Box::new(reader));
    }
    Ok(Library {
        roots: roots.to_vec(),
        counts,
        catalog: Mutex::new(MultiCatalog::new(members)),
    })
}

/// (root, track_count) per scanned source, in scan (source-bit) order.
#[must_use]
pub fn source_counts(&self) -> Vec<(PathBuf, usize)> {
    self.roots.iter().cloned().zip(self.counts.iter().copied()).collect()
}
```

If `reader.track_count()` needs the `Catalog` trait in scope, confirm it is already imported (it is — `MultiCatalog::new(members: Vec<Box<dyn Catalog + Send>>)` is used here).

- [ ] **Step 4: Run tests — expect PASS.** `cargo test -p soul-player-backend` (whole crate stays green).

- [ ] **Step 5: Commit.** `feat(backend): report per-source track counts from Library::scan_many`

---

### Task 2: Host `SourceStatus` contract + enabled folders

**Files:**
- Modify: `crates/soul-player-host/src/lib.rs` — `WatchUpdate` (~L28), `Inner.folders` (~L43), `set_folders` (~L164), `add_folder`/`remove_folder` (~L184), `watched_folders` (~L203)
- Test: same file's `#[cfg(test)] mod tests` (there are existing `set_folders_*` tests to update)

**Interfaces:**
- Consumes: `Library::source_counts()` (Task 1).
- Produces: `SourceStatus`, new `WatchUpdate`, `set_sources`, `set_folder_enabled`, `sources()` (consumed by Tasks 4–6). This is a **breaking** change to `WatchUpdate` — the only consumer is `app/rune` (Task 4 updates it).

- [ ] **Step 1: Write failing tests.** Add alongside the existing folder tests:

```rust
#[test]
fn source_status_reports_counts_available_and_enabled() {
    let h = PlayerHandle::new();
    let a = fixture_root_with_tracks(2); // existing test helper pattern
    let missing = PathBuf::from("/no/such/dir");
    let up = h.set_sources(&[(a.clone(), true), (missing.clone(), true)]).unwrap();
    assert_eq!(up.sources.len(), 2);
    let sa = up.sources.iter().find(|s| s.path == a).unwrap();
    assert!(sa.enabled && sa.available && sa.track_count == 2);
    let sm = up.sources.iter().find(|s| s.path == missing).unwrap();
    assert!(sm.enabled && !sm.available && sm.track_count == 0);
}

#[test]
fn disabled_source_contributes_no_tracks_but_stays_listed() {
    let h = PlayerHandle::new();
    let a = fixture_root_with_tracks(2);
    h.set_sources(&[(a.clone(), true)]).unwrap();
    let up = h.set_folder_enabled(&a, false).unwrap();
    assert!(up.views.is_empty());
    let sa = up.sources.iter().find(|s| s.path == a).unwrap();
    assert!(!sa.enabled && sa.track_count == 0);
    assert_eq!(h.sources().len(), 1); // still listed
}

#[test]
fn set_folders_preserves_enabled_flags() {
    let h = PlayerHandle::new();
    let a = fixture_root_with_tracks(1);
    let b = fixture_root_with_tracks(1);
    h.set_sources(&[(a.clone(), true)]).unwrap();
    h.set_folder_enabled(&a, false).unwrap();
    // Reconcile adds `b`; `a` keeps its disabled flag.
    let up = h.set_folders(&[a.clone(), b.clone()]).unwrap();
    assert!(!up.sources.iter().find(|s| s.path == a).unwrap().enabled);
    assert!(up.sources.iter().find(|s| s.path == b).unwrap().enabled);
}
```

- [ ] **Step 2: Run — expect FAIL** (`no method set_sources`, `WatchUpdate` has no `sources`).
  `cargo test -p soul-player-host source_status`

- [ ] **Step 3: Implement.** Replace `WatchUpdate`, add the enabled-folder model and the rescan helper.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    pub path: PathBuf,
    pub enabled: bool,
    pub available: bool,
    pub track_count: usize,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WatchUpdate {
    pub views: Vec<TrackView>,
    pub sources: Vec<SourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderConfig { path: PathBuf, enabled: bool }
```

Change `Inner.folders` to `Mutex<Vec<FolderConfig>>`. Add a private `rescan`:

```rust
/// Scan the enabled + available roots, build `WatchUpdate` for the full
/// configured set (disabled/unavailable roots listed with 0 tracks).
fn rescan(&self) -> anyhow::Result<WatchUpdate> {
    let cfg = self.inner.folders.lock().expect("folders mutex").clone();
    let to_scan: Vec<PathBuf> = cfg.iter()
        .filter(|f| f.enabled && f.path.is_dir())
        .map(|f| f.path.clone())
        .collect();
    let lib = Arc::new(Library::scan_many(&to_scan)?);
    let views = lib.views();
    let counts = lib.source_counts(); // (root, n) for scanned roots only
    self.set_library(lib);
    let sources = cfg.iter().map(|f| {
        let available = f.path.is_dir();
        let track_count = if f.enabled && available {
            counts.iter().find(|(r, _)| *r == f.path).map(|(_, n)| *n).unwrap_or(0)
        } else { 0 };
        SourceStatus { path: f.path.clone(), enabled: f.enabled, available, track_count }
    }).collect();
    Ok(WatchUpdate { views, sources })
}
```

Then:

```rust
pub fn set_sources(&self, entries: &[(PathBuf, bool)]) -> anyhow::Result<WatchUpdate> {
    let mut set: Vec<FolderConfig> = Vec::with_capacity(entries.len());
    for (p, en) in entries {
        if !set.iter().any(|f| &f.path == p) {
            set.push(FolderConfig { path: p.clone(), enabled: *en });
        }
    }
    *self.inner.folders.lock().expect("folders mutex") = set;
    self.rescan()
}

pub fn set_folders(&self, roots: &[PathBuf]) -> anyhow::Result<WatchUpdate> {
    let prev = self.inner.folders.lock().expect("folders mutex").clone();
    let mut set: Vec<FolderConfig> = Vec::with_capacity(roots.len());
    for r in roots {
        if !set.iter().any(|f| &f.path == r) {
            let enabled = prev.iter().find(|f| &f.path == r).map(|f| f.enabled).unwrap_or(true);
            set.push(FolderConfig { path: r.clone(), enabled });
        }
    }
    *self.inner.folders.lock().expect("folders mutex") = set;
    self.rescan()
}

pub fn set_folder_enabled(&self, dir: &Path, enabled: bool) -> anyhow::Result<WatchUpdate> {
    {
        let mut g = self.inner.folders.lock().expect("folders mutex");
        if let Some(f) = g.iter_mut().find(|f| f.path == dir) { f.enabled = enabled; }
    }
    self.rescan()
}

pub fn sources(&self) -> Vec<SourceStatus> {
    // Cheap status without a rescan: reuse last-known availability via is_dir;
    // counts require a scan, so sources() returns 0 counts unless called after a
    // rescan. Prefer reading WatchUpdate.sources; this is for simple listing.
    self.inner.folders.lock().expect("folders mutex").iter()
        .map(|f| SourceStatus { path: f.path.clone(), enabled: f.enabled, available: f.path.is_dir(), track_count: 0 })
        .collect()
}

pub fn watched_folders(&self) -> Vec<PathBuf> {
    self.inner.folders.lock().expect("folders mutex").iter().map(|f| f.path.clone()).collect()
}
```

`add_folder`/`remove_folder` keep delegating to `set_folders` (which now preserves flags). Update the `Inner` initializer and any `Default`/`new` to the new `folders` type.

Update the existing `set_folders_*` tests that asserted `up.unavailable` to assert via `up.sources` (`.filter(|s| !s.available)`).

- [ ] **Step 4: Run — expect PASS.** `cargo test -p soul-player-host`

- [ ] **Step 5: Commit.** `feat(host): per-source status (enabled/available/track_count) in WatchUpdate`

---

### Task 3: `-ng` config — persist enabled/disabled sources

**Files:**
- Modify: `app/rune/src/config.rs` — `Config` (~L62), add `sources_disabled`; add `load_sources_with_enabled` / `save_sources_enabled`
- Test: `app/rune/src/config.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `load_sources_with_enabled() -> Vec<(PathBuf, bool)>`, `save_sources_enabled(&[(PathBuf, bool)])` (consumed by Task 4). `load_sources()`/`save_sources()` stay for back-compat.

- [ ] **Step 1: Failing test.**

```rust
#[test]
fn sources_disabled_roundtrips_and_old_config_is_all_enabled() {
    use_test_config_dir();
    save_sources_enabled(&[(PathBuf::from("/a"), true), (PathBuf::from("/b"), false)]);
    let back = load_sources_with_enabled();
    assert_eq!(back, vec![(PathBuf::from("/a"), true), (PathBuf::from("/b"), false)]);
    // A legacy config (only `sources`, no `sources_disabled`) loads all-enabled.
    save_sources(&[PathBuf::from("/a"), PathBuf::from("/b")]);
    assert_eq!(load_sources_with_enabled(),
        vec![(PathBuf::from("/a"), true), (PathBuf::from("/b"), true)]);
}
```

- [ ] **Step 2: Run — expect FAIL.** From `app/rune/`: `cargo test sources_disabled_roundtrips`

- [ ] **Step 3: Implement.** Add the field (defaulted empty) + the two functions:

```rust
// in struct Config, with the other #[serde(default)] fields:
    /// Paths (subset of `sources`) the user has disabled. Absent in old configs
    /// → everything enabled. Storing the *disabled* subset keeps migration free.
    sources_disabled: Vec<String>,
// in Default:
    sources_disabled: Vec::new(),

pub fn load_sources_with_enabled() -> Vec<(PathBuf, bool)> {
    let cfg = load();
    let disabled: std::collections::HashSet<&str> =
        cfg.sources_disabled.iter().map(String::as_str).collect();
    cfg.sources.iter()
        .map(|s| (PathBuf::from(s), !disabled.contains(s.as_str())))
        .collect()
}

pub fn save_sources_enabled(entries: &[(PathBuf, bool)]) {
    let mut cfg = load();
    cfg.sources = entries.iter().filter_map(|(p, _)| p.to_str().map(String::from)).collect();
    cfg.sources_disabled = entries.iter()
        .filter(|(_, en)| !en)
        .filter_map(|(p, _)| p.to_str().map(String::from))
        .collect();
    save(&cfg);
}
```

- [ ] **Step 4: Run — expect PASS.** `cargo test` (from `app/rune/`) — config module green.

- [ ] **Step 5: Commit.** `feat(config): persist per-source enabled state (sources_disabled)`

---

### Task 4: `-ng` state + wiring to the new contract

**Files:**
- Modify: `app/rune/src/main.rs` — `State` (~L171 `watched_folders`/`unavailable`), `State::new` (~L231), `apply_watch`/`apply_and_save` (~L351), startup seed (~L790)
- Test: `app/rune/src/main.rs` tests referencing `watched_folders`/`unavailable` (~L924–967)

**Interfaces:**
- Consumes: host `WatchUpdate.sources`, `set_sources`, `config::load_sources_with_enabled`/`save_sources_enabled`.
- Produces: `State.sources: Vec<SourceStatus>` for the UI (Tasks 5–6).

- [ ] **Step 1: Failing test.** Replace `apply_watch_folds_a_watchupdate_into_state` body to assert on `s.sources`:

```rust
#[test]
fn apply_watch_folds_sources_into_state() {
    let mut s = State::new(PlayerHandle::offline());
    assert!(s.sources.is_empty());
    let up = s.host.add_folder(&testdata_dir()).expect("add_folder");
    apply_watch(&mut s, up);
    assert_eq!(s.sources.len(), 1);
    assert_eq!(s.sources[0].path, testdata_dir());
    assert!(s.sources[0].enabled && s.sources[0].available);
}
```

- [ ] **Step 2: Run — expect FAIL** (`State` has no `sources`).

- [ ] **Step 3: Implement.** Replace the two `State` fields with `sources: Vec<SourceStatus>` (import `SourceStatus` from `soul_player_host`). Update `State::new` to seed `sources` from `host.sources()`. Rewrite the apply helpers:

```rust
pub(crate) fn apply_watch(s: &mut State, up: WatchUpdate) {
    s.library = up.views;
    s.sources = up.sources;
}
pub(crate) fn apply_and_save(s: &mut State, up: WatchUpdate) {
    apply_watch(s, up); // reflect the update first
    let entries: Vec<(PathBuf, bool)> =
        s.sources.iter().map(|x| (x.path.clone(), x.enabled)).collect();
    config::save_sources_enabled(&entries); // then persist from the applied state
}
```

Update the startup seed (~L790):

```rust
let saved = config::load_sources_with_enabled();
if !saved.is_empty() {
    let _ = host.set_sources(&saved);
}
```

Fix any remaining `s.unavailable` / `s.watched_folders` readers (the Sources view in Task 6 is the main one; a derived helper `s.unavailable()` returning `sources.iter().filter(|x| !x.available)` can ease migration, or inline).

- [ ] **Step 4: Run — expect PASS.** `cargo test` from `app/rune/` (state/apply tests green; fix compile fallout in the Sources view will land with Task 6 — until then gate with `#[allow(dead_code)]` only if needed, prefer landing Tasks 4+6 together if the view won't compile).

- [ ] **Step 5: Commit.** `refactor(app): hold per-source status in State; seed enabled on startup`

---

### Task 5: Add-source menu (Local folder + Remote "Soon")

**Files:**
- Modify: `app/rune/src/views/settings.rs` — replace `add_source_button` (~L169) with a `views::dropdown`-based menu; mount its panel on the overlay layer the other dropdowns use
- Modify: locale files — new keys
- Test: `app/rune/src/main.rs` (or settings test module) headless `Runtime` test

**Interfaces:**
- Consumes: `views::dropdown::{trigger, panel, is_open}`, existing `tx.open_folder` + `host.add_folder` flow.

- [ ] **Step 1: Failing test.**

```rust
#[test]
fn add_source_menu_offers_local_and_coming_soon_remote() {
    let mut rt = settings_rt(SettingsTab::MusicData); // existing helper pattern
    rt.find("add-source").expect("add-source trigger");
    rt.key(/* activate */).frame(); // open via Space/Enter on the focused trigger
    // Local item is focusable; Remote item is present but not focusable (disabled).
    let local = el_info(&rt, "add-source-opt-local").expect("local item");
    assert!(local.focusable);
    let remote = el_info(&rt, "add-source-opt-remote").expect("remote item");
    assert!(!remote.focusable, "remote is Coming Soon → disabled");
}
```

- [ ] **Step 2: Run — expect FAIL.**

- [ ] **Step 3: Implement.** Use the dropdown key `ADD_SOURCE_MENU = "add-source"`. Render `dropdown::trigger("add-source", t!("settings.addSource"), p)` in place of the button. On the overlay layer (where `language_dropdown`'s panel is mounted — follow that call site in `view`), when `dropdown::is_open(s, ADD_SOURCE_MENU)`, mount a bespoke panel: reuse `dropdown::panel` if a two-item `(id,label)` list fits, **but** the Remote item must be disabled. Since `panel`'s rows are uniformly focusable, build the menu with `panel` for the Local row and render the Remote row as a non-focusable, greyed row with a "Soon" chip (key `add-source-opt-remote`), or extend `panel` with an `enabled` flag per choice. Prefer the smaller change: a dedicated `add_source_menu(s, p)` that mirrors `panel`'s styling (anchored_fit to "add-source", `.modal(true, close_dropdown)`) with two explicit rows:
  - `add-source-opt-local`: `role("option")`, focusable, on_press → `close_dropdown(s)` then the existing `tx.open_folder(..)` → `host.add_folder` → `apply_and_save`. (Use `on_press_tx` as the current button does.)
  - `add-source-opt-remote`: not focusable, `text_secondary`, trailing `t!("settings.comingSoon")` chip, no handler.

Keep tokens-only styling. Add keys: `settings.addLocalFolder`, `settings.addRemoteServer`, `settings.comingSoon` (reuse `common.comingSoon` if present) to all five locales.

- [ ] **Step 4: Run — expect PASS.** `cargo test` from `app/rune/`. Also run the `one_dropdown_component` arch test (must still pass — we used `views::dropdown`/its pattern).

- [ ] **Step 5: Commit.** `feat(settings): add-source menu with Local folder + Remote (coming soon)`

---

### Task 6: Rich source rows (full path, track count, enable toggle)

**Files:**
- Modify: `app/rune/src/views/settings.rs` — `source_row` (~L220) and the sources card loop (~L469)
- Modify: locale files — `settings.trackCount` (plural), `settings.enableSource`/`settings.disableSource`, `settings.sourceDisabledHint`
- Test: `app/rune/src/main.rs` settings test module headless `Runtime` tests

**Interfaces:**
- Consumes: `State.sources: Vec<SourceStatus>` (Task 4), `host.set_folder_enabled` → `apply_and_save`.

- [ ] **Step 1: Failing tests.**

```rust
#[test]
fn source_row_shows_full_path_and_track_count() {
    let mut rt = settings_rt_with_source(testdata_dir()); // seeds one source
    let name = basename(&testdata_dir());
    assert!(el_info(&rt, &format!("source-{name}")).is_some());
    // full path text present somewhere in the row
    assert!(rt.elements().iter().any(|e| e.text.as_deref() == Some(testdata_dir().to_str().unwrap())));
    // a track-count label is rendered
    assert!(rt.elements().iter().any(|e| e.text.as_deref().is_some_and(|t| t.contains("track"))));
}

#[test]
fn toggling_a_source_disables_it_and_persists() {
    let mut rt = settings_rt_with_source(testdata_dir());
    let name = basename(&testdata_dir());
    let toggle = format!("enable-{name}");
    assert!(el_info(&rt, &toggle).expect("toggle").focusable);
    tap(&mut rt, &toggle); // helper: focus + activate
    assert!(!rt.state.sources[0].enabled);
    assert!(rt.state.library.is_empty()); // disabled → no tracks
}
```

- [ ] **Step 2: Run — expect FAIL.**

- [ ] **Step 3: Implement.** Rewrite `source_row(s, p, index)` to read `s.sources[index]` (a `SourceStatus`). Layout (tokens only):
  - Leading **enable toggle** (key `enable-{name}`, `role("button")`, focusable, label from `t!` enable/disable): filled `p.primary` when enabled, `p.muted` when disabled; `on_press` → `host.set_folder_enabled(&path, !enabled)` → `apply_and_save`.
  - A `col` with the **name** (`type_::ITEM`, truncate) over the **full path** (`type_::CAPTION`, `text_secondary`, truncate), `.grow(1.0)`.
  - **Track count**: `text(t!("settings.trackCount", count = n))` in `text_secondary` (plural form in YAML).
  - **Offline** badge when `!available` (kept).
  - **✕ remove** (kept) → `host.remove_folder` → `apply_and_save`.
  - When `!enabled`, dim the name/path (`text_secondary`) to read as inactive.
  Update the card loop to `for i in 0..s.sources.len()` and the empty check to `s.sources.is_empty()`.

Add i18n keys to all five locales (English stopgap), single-quoted, with a plural `trackCount` (e.g. `trackCount: '%{count} tracks'` — match the project's rust-i18n pluralization convention; check an existing pluralized key first).

- [ ] **Step 4: Run — expect PASS.** `cargo test` from `app/rune/`; run the cross-locale overflow test harness for the new row (reuse `rt.elements()` pattern) to confirm no horizontal overflow in any locale.

- [ ] **Step 5: Commit.** `feat(settings): richer source rows — full path, track count, enable toggle`

---

## Self-review checklist (run before marking the plan done)

- Spec coverage: add-source menu (T5), richer rows w/ path+count+toggle (T6), host `SourceStatus` (T2), per-source counts (T1), config persistence (T3), state wiring (T4). ✅ Remote stays "coming soon" (T5). Managed library + remote sync are explicitly Phase 2/3, not here.
- Type consistency: `SourceStatus`/`WatchUpdate.sources` defined in T2, consumed unchanged in T4–T6; `source_counts()` defined T1, consumed T2; config fns defined T3, consumed T4.
- No placeholders: each step has concrete code or an exact command + expected result. The two spots that defer to "match the existing helper/plural convention" (test fixtures, rust-i18n plural syntax) are explicitly pointed at a concrete in-repo reference to copy.

## Post-merge

Confirm the `rust (service+host)` and `rune` CI legs are green on the PR(s). After Task 4 lands, the core bump rule does **not** apply (no `soul-audio-core` change in Phase 1).

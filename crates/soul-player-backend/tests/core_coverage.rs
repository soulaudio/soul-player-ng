//! **Core-capability coverage ledger** — a deliberate reminder, not a gate.
//!
//! The desktop deliberately surfaces only a subset of `soul-audio-core`'s shared
//! data layer. As core grows, it's easy to forget that a capability exists but
//! isn't wired into the app. This test keeps an explicit ledger and **prints a
//! warning for every capability not yet in `-ng`**, so a gap is visible in
//! `cargo test` output and never silently lost.
//!
//! Discipline:
//! - When you wire a capability into the app, flip its row to `Wired`.
//! - When core gains a NEW capability, add a row here.
//! - This test only *warns* (it asserts the known-wired baseline, nothing more),
//!   so it never blocks CI — it's a checklist, not a blocker.
//!
//! The `BrowseAxis` guard below is **exhaustive on purpose**: if core adds a
//! browse axis, `axis_is_recorded` stops compiling until its desktop status is
//! written down here — a compile-time "don't forget".

use soul_audio_catalog::BrowseAxis;

#[derive(Clone, Copy, PartialEq)]
enum Status {
    /// Surfaced in the desktop app today.
    Wired,
    /// Available in core, NOT yet surfaced by `-ng`.
    Pending,
}
use Status::{Pending, Wired};

/// Every shared-core data-layer capability and its desktop status. Keep current.
const LEDGER: &[(&str, Status, &str)] = &[
    // --- surfaced today ---
    (
        "flat track list (title / artist / album)",
        Wired,
        "Library::views",
    ),
    (
        "playback transport + queue",
        Wired,
        "actor + driver + commands",
    ),
    (
        "cover art (embedded + sidecar)",
        Wired,
        "cover_bytes / track_art",
    ),
    ("tag-derived duration", Wired, "Library::duration_of"),
    (
        "track -> file location + decode",
        Wired,
        "Library::path_of -> open_track",
    ),
    (
        ".soulidx baked cache via IndexReader",
        Wired,
        "Library bakes each watched folder -> owning IndexReader (dogfood)",
    ),
    (
        "MultiCatalog (multiple sources)",
        Wired,
        "Library::scan_many aggregates N folders; path_of routes per source",
    ),
    // --- available in core, NOT yet in -ng ---
    (
        "album browse (order + tracks-of-album)",
        Pending,
        "core order tables ready; no album browse surface",
    ),
    (
        "artist browse (artist->album->track)",
        Pending,
        "flat list uses the baked ArtistAlbumTrack order; no artist drill-down UI",
    ),
    (
        "folder browse (folder tree + paths)",
        Pending,
        "core folder table ready; no folder tree UI",
    ),
    (
        "baked browse order / jump-to-letter",
        Pending,
        "flat list now uses baked ArtistAlbumTrack order; per-letter jump pending",
    ),
    (
        "in-index playlists",
        Pending,
        "core playlist table ready; no UI/commands",
    ),
    ("search / filter", Pending, "no baked term index wired"),
    // --- pending in core too (listed so the desktop seam isn't forgotten) ---
    (
        "extended (Tier-2/3) metadata display",
        Pending,
        "extended.blob deferred in core",
    ),
    (
        "1-bit e-ink art",
        Pending,
        "DAP-only; desktop keeps full-res originals (not a desktop UI need)",
    ),
    (
        "mutable state (favorites / play-counts / ratings)",
        Pending,
        "soul-audio-mutable not built",
    ),
];

/// Exhaustive match: a new `BrowseAxis` variant in core will fail to compile here
/// until its desktop status is recorded in `LEDGER` above.
#[allow(dead_code)]
fn axis_is_recorded(axis: BrowseAxis) {
    match axis {
        // Each arm = "this browse axis is accounted for in the ledger".
        BrowseAxis::Title
        | BrowseAxis::ArtistAlbumTrack
        | BrowseAxis::Album
        | BrowseAxis::Folder => {}
    }
}

#[test]
fn desktop_core_capability_coverage() {
    let pending: Vec<&(&str, Status, &str)> =
        LEDGER.iter().filter(|(_, s, _)| *s == Pending).collect();
    let wired = LEDGER.len() - pending.len();

    eprintln!("\n=== desktop coverage of soul-audio-core data-layer capabilities ===");
    eprintln!(
        "    {wired}/{} wired; {} pending:",
        LEDGER.len(),
        pending.len()
    );
    for (cap, _, note) in &pending {
        eprintln!("    [ ] NOT YET IN -ng: {cap}  ({note})");
    }
    eprintln!("    (reminder ledger — warnings only, never fails; keep it current)\n");

    // The only hard assertion: the known-wired baseline must stay recorded, so a
    // regression that drops the ledger is caught. Pending items never fail.
    assert!(
        wired >= 7,
        "the wired baseline (list / playback / art / duration / locate+decode / \
         IndexReader cache / MultiCatalog) must stay recorded"
    );
}

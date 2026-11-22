//! Architecture boundary tests.
//!
//! The project rule (docs/ARCHITECTURE.md): **the UI is just visualization; all
//! logic lives outside it.** `soul-player-backend` is the headless logic layer —
//! it must never pull in a GUI/windowing stack, directly or transitively, so it
//! stays reusable off the desktop (and, long-term, on firmware).
//!
//! This is the discoverable (`cargo test`) half of the enforcement; the
//! CI-blocking half is the `[bans]` list in the root `deny.toml`
//! (`cargo deny check bans`). Keep the two lists in sync.
//!
//! The test resolves the dependency graph with `cargo metadata`. If that can't
//! run (offline with un-fetched git deps, missing `cargo`), it skips rather than
//! failing spuriously — CI always has the graph available, so the gate still
//! holds there.

use std::process::Command;

/// GUI / windowing crates that must never appear in the headless service's
/// dependency closure. Mirrors `deny.toml`'s `[bans].deny`.
const FORBIDDEN: &[&str] = &[
    "tauri",
    "wry",
    "tao",
    "webkit2gtk",
    "gtk",
    "gtk3",
    "rune",
    "wgpu",
    "winit",
];

#[test]
fn headless_service_has_no_gui_dependencies() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        other => {
            eprintln!(
                "architecture test skipped: `cargo metadata` unavailable ({other:?}); \
                 the deny.toml [bans] gate still enforces this in CI"
            );
            return;
        }
    };

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata emits valid JSON");

    let offenders: Vec<String> = meta["packages"]
        .as_array()
        .expect("metadata has a packages array")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .filter(|name| FORBIDDEN.contains(name))
        .map(str::to_owned)
        .collect();

    assert!(
        offenders.is_empty(),
        "ARCHITECTURE VIOLATION: the headless `soul-player-backend` workspace pulled in \
         GUI/windowing crate(s) {offenders:?}. The UI head is just visualization — all \
         logic must stay outside it (in this service or soul-audio-core). Move the \
         offending code into the Rune app (`app/rune`) instead. See docs/ARCHITECTURE.md."
    );
}

/// License gate: the **AAC decoder must NEVER be compiled into the distributed
/// player.** AAC is a Via LA patent — **$15,000 one-time + $0.98 per unit** — so
/// each distributed copy bundling an AAC decoder owes a per-unit royalty (a
/// clean-room pure-Rust decoder is still patent-bearing). The decoder lives in
/// `soul-audio-core` behind an opt-in `aac` cargo feature (default **OFF**); this
/// asserts nothing in the resolved graph turns it on. If AAC playback is ever
/// wanted on the desktop, delegate to the OS codec (Media Foundation / CoreAudio),
/// never enable this feature. Rationale + cross-repo picture:
/// soul-listener/docs/research/2026-11-18-licensing-ip-and-workarounds.md §5.
#[test]
fn aac_decoder_feature_never_enabled() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        other => {
            eprintln!(
                "aac license gate skipped: `cargo metadata` unavailable ({other:?}); \
                 the CI gate still enforces this"
            );
            return;
        }
    };

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata emits valid JSON");

    // Resolve the decode core's package id (exact name — not its sibling crates).
    let core_id = meta["packages"]
        .as_array()
        .expect("metadata has a packages array")
        .iter()
        .find(|p| p["name"] == "soul-audio-core")
        .and_then(|p| p["id"].as_str())
        .map(str::to_owned);
    let core_id = match core_id {
        Some(id) => id,
        None => {
            eprintln!("aac license gate skipped: soul-audio-core not in the dependency graph");
            return;
        }
    };

    // Its ENABLED features in the current resolution must not include `aac`.
    let aac_enabled = meta["resolve"]["nodes"]
        .as_array()
        .expect("resolve has a nodes array")
        .iter()
        .find(|n| n["id"].as_str() == Some(core_id.as_str()))
        .and_then(|n| n["features"].as_array())
        .is_some_and(|feats| feats.iter().any(|f| f.as_str() == Some("aac")));

    assert!(
        !aac_enabled,
        "AAC LICENSE VIOLATION: `soul-audio-core` was resolved with the `aac` feature enabled, \
         pulling the patented AAC decoder into the distributed player. AAC is a Via LA patent \
         ($15,000 one-time + $0.98/unit) and is DROPPED from every shipped soul product — decode \
         AAC via the OS codec (Media Foundation / CoreAudio) instead, never by enabling this \
         feature. Remove `features = [\"aac\"]` from whatever dependency added it. See \
         soul-listener/docs/research/2026-11-18-licensing-ip-and-workarounds.md §5."
    );
}

//! Architecture boundary tests.
//!
//! The project rule (docs/ARCHITECTURE.md): **Tauri is just visualization; all
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
const FORBIDDEN: &[&str] = &["tauri", "wry", "tao", "webkit2gtk", "gtk", "gtk3"];

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

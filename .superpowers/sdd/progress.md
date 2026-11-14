# Sources Phase 1 — progress ledger

Plan: docs/superpowers/plans/2026-11-14-sources-phase1-reference-revamp.md
Branch: feat/sources-phase1-reference-revamp (one PR; breaking WatchUpdate change)

- [x] Task 1: backend Library::source_counts() — complete (commit 4fe96b9, review clean)
- [x] Task 2: host SourceStatus + WatchUpdate.sources + enabled folders — complete (commit 4ba37e6, review clean)
- [x] Task 3+4 (combined): config + State.sources + apply/seed + settings.rs migration — complete (commit 7596b35, app/rune green 106 passed, review clean)
- [x] Task 5: add-source menu (Local + Remote coming soon) — complete (commit 95c24f9, 107 passed, review clean). NOTE: trigger is full-width (dropdown::trigger forces w_full) → do a compact-button polish pass + --snap before PR.
- [ ] Task 6: -ng settings rich source rows (path, count, toggle)
- [ ] Polish: compact add-source trigger (not full-width) + --snap settings review

NOTE: Task 2 broke app/rune's consumer (WatchUpdate.unavailable removed), so 3+4
are combined to restore a compiling+green app/rune before 5/6 layer on it.
Host crate is its OWN workspace: test via --manifest-path crates/soul-player-host/Cargo.toml (NOT -p from root).

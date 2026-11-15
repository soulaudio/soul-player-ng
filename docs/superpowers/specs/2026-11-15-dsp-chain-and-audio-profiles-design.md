# DSP chain generalization + audio profiles — design

Date: 2026-11-15
Status: approved design, pre-implementation

## Goal

Two related capabilities, built in sequence:

1. **Generalized, expandable DSP chain** — the Settings → Audio "DSP/EQ" card
   becomes an ordered chain of **typed DSP units**, each a card you can toggle,
   reorder, remove, and **expand to edit its own controls**. Multiple cards can be
   expanded at once. EQ is the only type for now; the structure is built to grow.
2. **Audio profiles** — a profile is a **named snapshot of the DSP chain**. You can
   create many, switch between them, and import/export them in a portable format the
   future DAP reads. Built on top of Phase 1.

## Decisions (settled in brainstorming)

- A profile bundles the **DSP chain only**. ReplayGain + output device stay global.
- **Auto-save to the selected profile**: the live chain *is* the selected profile;
  edits persist immediately. No "unsaved changes" flow. Duplicate to experiment.
- Switching/managing profiles lives in the **Settings → Audio** card.
- The DSP chain holds **typed units** (`DspUnit` enum). "+ Add DSP" offers a type
  menu (EQ only today). Each unit is a card with a per-unit on/off toggle and
  type-specific controls; **multiple cards expand simultaneously**.
- **Phasing**: DSP chain generalization ships first (its own PR); profiles second
  (its own PR), so profiles bundle the final chain shape.

## Current state (what we build on)

- `soul-player-host::eq`: `DspChain { units: Vec<EqUnit>, generation }`,
  `EqUnit { bands: Vec<Band>, preamp_db, enabled }`. `DspSink` applies units in
  series on the pump thread and re-designs when `generation` moves. (`soul-audio-eq`
  designs coefficients, `soul-audio-core` applies them — no_std, DAP-shared.)
- App `State`: `dsp_units: Vec<EqUnit>`, `dsp_active: usize`, and a **single mirror**
  `eq_bands`/`eq_preamp`/`eq_enabled` reflecting `dsp_units[dsp_active]`. The EQ graph,
  band table (with the Ableton number boxes from #157), and all EQ mutators read/write
  that mirror; `apply_eq` syncs mirror → `dsp_units[active]` → save + live push.
- UI: `eq_card` = header toggle (active unit) + `dsp_unit_list` (reorderable rows) +
  one `body` (preamp + graph + band editor) for the **active** unit.
- `config.rs`: persists `eq_units: Vec<EqUnitCfg>` (+ legacy migration).

---

## Phase 1 — generalized, expandable DSP cards

### A. Data model (host + config)

- `DspChain.units: Vec<DspUnit>` where `DspUnit` is an enum:

  ```rust
  pub enum DspUnit { Eq(EqUnit) }
  ```

  with accessors `kind() -> DspKind`, `enabled() -> bool`, `set_enabled(bool)`.
  `DspKind { Eq }` is the tag the UI type-picker and serialization use.
- `DspSink` matches on the variant (only `Eq` → the existing `Equalizer<MAX_BANDS>`
  path). Everything else — per-unit preamp, disabled = passthrough, the
  `generation`-gated live redesign, the spectrum tap — is unchanged. This is a thin
  wrapper around the proven EQ apply code; **no change to `soul-audio-core` /
  `soul-audio-eq`**.
- `config.rs`: `EqUnitCfg` → a **tagged `DspUnitCfg`**:

  ```toml
  [[dsp_units]]
  kind = "eq"
  enabled = true
  preamp = 0.0
  # bands = [...]
  ```

  `load_dsp_chain()` reads tagged entries; a one-time migration maps old untagged
  (EQ-only) entries and the pre-#154 legacy fields to `kind = "eq"`. Unknown `kind`
  values are skipped with a logged warning (forward compatibility).

### B. UI model (app)

- `eq_card` becomes the **DSP chain card** (`sp-stage-dsp`): a vertical column of
  **unit cards** in processing order, then a **"+ Add DSP"** control.
- "+ Add DSP" opens the themed dropdown (`views::dropdown`, per the arch rule that
  bans native `PopupMenu::picker`) listing **EQ** only; selecting appends a default
  EQ unit and expands it.
- Each **unit card**:
  - **Header** (always visible): expand/collapse chevron, type label ("EQ"), a
    reorder grip (drag to reorder — reuse `dsp_drop_index`/`DSP_ROW_H` logic), a
    per-unit **on/off** toggle, and remove ✕.
  - **Body** (when expanded): the type's editor. For EQ: preamp slider + response
    graph + band table (the #157 number boxes), **bound to this unit** (`i`).
  - Keyed by **position** (`dsp-card-{i}`), consistent with the queue positional-key
    rule — never by a unit id.
- **Multiple cards expand at once.** Collapsed cards show just the header (a compact
  summary line is a possible later nicety, not in scope now).
- The **master stage toggle** stays on the card header (bypass the whole DSP stage —
  what the now-playing signal-path row reflects). Per-unit toggles are independent.

### C. State + mutator refactor (the substantive change)

- **Drop** `dsp_active` and the single mirror (`eq_bands`/`eq_preamp`/`eq_enabled`).
  Edits operate on an explicit **unit index** into `dsp_units[i]` directly.
- Add a per-unit **expanded** flag, kept in sync with `dsp_units` on add / remove /
  reorder (a `Vec<bool>` parallel to `dsp_units`, or an `expanded: bool` field on a
  UI-state struct per unit — writing-plans picks the representation; the requirement
  is that expansion survives reorder and is dropped with its unit).
- EQ mutators gain a unit index: `set_eq_band(i, band_idx, …)`,
  `eq_num_drag_*(i, …)`, `set_eq_preamp(i, …)`, `set_eq_unit_enabled(i, …)`, etc.
  (The band-index-level number-box/drag/scroll logic from #157 is unchanged; it just
  takes the unit index too.) `eq_selected: Option<(usize, usize)>` becomes `(unit,
  band)` — **one** node selected at a time across all cards (clicking a node in one
  card deselects the other's), which the readout bar reads.
- The EQ graph (`eq_graph`) and band editor (`eq_band_editor`) take a `unit: usize`
  and read/write `dsp_units[unit]`. Element keys include the unit index so two open
  EQ graphs don't collide (`eq-node-{unit}-{band}`, `eq-num-freq-{unit}-{band}`,
  etc.).
- `apply_dsp` replaces `apply_eq`: after any edit, bump `generation`, save the chain,
  push to the host. (No mirror to reconcile.)
- The live spectrum is one shared input signal; every expanded EQ graph draws the
  same spectrum behind its own curve.

### Scope boundary (YAGNI)

- Only the **EQ** `DspUnit` variant is implemented. The enum + type-picker +
  tagged serialization are the *only* generalization scaffolding; **no** compressor /
  limiter / reverb / crossfeed now.
- No per-card "summary" rendering of a collapsed unit beyond its header.
- No profiles yet (Phase 2).

### Testing (Phase 1)

- **host**: `DspChain` with a `Vec<DspUnit>` applies EQ units in series; a disabled
  unit is a passthrough; a `generation` bump reloads live (port existing `eq.rs`
  tests to the enum). Tagged `DspUnitCfg` round-trips; legacy/untagged migration maps
  to `Eq`.
- **app** (rune harness): add a 2nd DSP via the "+ Add DSP" → EQ menu; expand two EQ
  cards at once and confirm editing one card's band doesn't touch the other; per-unit
  toggle + remove + reorder; the #157 number-box tests still pass with a unit index
  threaded through. TDD per mutator.

### Migration / compatibility

- `.config` chains written by #154/#157 (untagged `eq_units`) load as `Eq` units.
  The on-disk key may stay `eq_units` with an added `kind` field, or move to
  `dsp_units`; either way old files load. Format is rewritten on next save.

---

## Phase 2 — audio profiles (concrete design, approved 2026-11-15)

Built on the merged Phase 1 (PR #162). Its own PR off `main`.

### A. Data model & persistence (`config.rs`)

- Runtime `Profile { name: String, chain: Vec<DspUnit> }` lives in `config.rs`
  (which already owns the `DspUnit` ⇄ `DspUnitCfg` conversion). Serde form
  `ProfileCfg { name: String, chain: Vec<DspUnitCfg> }` (reuses the Phase 1
  `DspUnitCfg`, so no new serialization code).
- `Config` gains `profiles: Vec<ProfileCfg>` and `active_profile: usize`
  (`#[serde(default)]`, clamped on load). The existing `eq_units` key is kept and
  **mirrored to the active profile's chain on every save**, so a rollback to a
  pre-profiles build still loads the right chain.
- `load_profiles() -> (Vec<Profile>, usize)`: if `profiles` is present, map it
  (filtering unknown unit kinds) and clamp the active index. Otherwise **migrate**:
  one `"Default"` profile from the existing `load_dsp_chain()` (which already handles
  `eq_units` + the pre-#154 legacy fields). Always returns ≥1 profile.
- `save_profiles(&[Profile], active: usize)`: writes `profiles` + `active_profile`
  and `eq_units = profiles[active].chain`.

### B. State & mutators (`main.rs`)

- `State` gains `profiles: Vec<config::Profile>`, `active_profile: usize`, and an
  inline-rename pair `profile_rename: Option<usize>` + `profile_rename_buf: TextEdit`
  (mirrors `dsp_rename`). Startup loads via `load_profiles()`; `dsp_units` is seeded
  from the active profile's chain.
- **Auto-save** is `apply_dsp`: it first writes the live `dsp_units` back into
  `profiles[active_profile].chain`, then `save_profiles(..)` + `host.set_dsp_chain(..)`.
  Every existing DSP mutator already funnels through `apply_dsp`, so they persist to
  the active profile with no per-mutator change.
- Mutators (each saves): `switch_profile(i)` (reloads `dsp_units` from profile `i`,
  resets the expand vectors, clears the EQ editor, pushes to host), `new_profile`
  (appends an empty-chain profile with a unique name, switches to it), `duplicate_profile`
  (copies the live chain under `"<name> copy"`, switches), `delete_profile` (removes the
  active one and switches to a neighbour — **no-op when only one profile remains**),
  `rename_profile(i, name)` + inline `start/commit_profile_rename`. A `unique_profile_name`
  helper disambiguates collisions.

### C. Import / export (JSON)

- A whole-chain profile needs its own container, so export is **JSON** (not the per-EQ
  AutoEQ `.txt`): `{ "name": ..., "chain": [ <tagged DspUnitCfg> ] }` via `serde_json`
  (new app dep). This is the portable format the future **DAP** (alloc + serde, reusing
  `soul-audio-eq`) reads.
- `export_profile(path)` writes the active profile; `import_profile(path)` parses a
  file into a new profile (filtering unknown kinds) and switches to it. Wired through
  the native dialogs (`tx.save_file`/`open_file`) with a `*.json` filter, exactly like
  `export_eq`/`import_eq`.

### D. UI (`settings.rs`) — **switcher + icon buttons** (chosen layout)

- A `profile_bar` inserted at the **top of the DSP card**, after `stage_header` and
  before the unit-card loop: the themed `dropdown` switcher (grows) showing the active
  profile; when renaming it becomes an inline text field. To its right, compact icon
  buttons: **New** (`plus`), **Duplicate** (`copy`), **Rename** (`pencil`), **Delete**
  (`trash`, disabled at one profile), a separator, **Import** (`download`), **Export**
  (`upload`). Five new Lucide icons added to `icons.rs`.
- The switcher panel mounts in `settings_overlays` (a new `PROFILE_PICKER` arm);
  picking an entry calls `switch_profile`. All labels/tooltips are new `settings.*`
  i18n keys across the six shipped locales (the arch tests enforce both).

### Non-goals (Phase 2)

- No cloud sync, no per-track/auto profile switching, no bundling ReplayGain/output
  device (profiles stay DSP-chain-only, per the brainstorm).

## Risks / notes

- The state refactor in C touches every EQ mutator + the graph + band editor. It is
  broad but mechanical; TDD each mutator and keep the #157 number-box tests green as
  the safety net.
- Element-key collisions between two open EQ graphs are the main new failure mode —
  namespacing all EQ element keys by unit index is the mitigation (and a test that
  renders two expanded EQ cards and asserts both graphs' nodes exist).
- Keep cards keyed by position (not unit id) per the queue dup-key lesson.

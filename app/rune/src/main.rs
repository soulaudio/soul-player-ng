//! Rune UI head for Soul Player — the one desktop UI. Wires
//! `soul_player_host::PlayerHandle` into a Rune `App`: builds the host, autoloads
//! a library from `SOUL_PLAYER_FOLDER`, and folds backend `Tick`/`Queue` events
//! into `State` via `AppHandle::update`. The view is a two-region shell: a
//! fixed-width `views::sidebar` (Library/Settings nav, `views::queue`,
//! `views::player` panel) on the left, and the routed main area — the
//! `views::library` track list or `views::settings` — filling the rest.
#![forbid(unsafe_code)]

#[macro_use]
extern crate rust_i18n;

// Compile-time-embed the translations in `locales/app.yml`; the crate-wide
// `t!("key")` macro then resolves against the current locale (set per the saved
// pref / OS language in `apply_locale`), falling back to English.
i18n!("locales", fallback = "en");

use std::path::PathBuf;

use rune::{App, AppSpec, Cx, El, Key, KeyEvent, Titlebar, Tx, Waker, col, el, row};
// Linked only to union parley_engine's `complex-scripts` feature on (see
// Cargo.toml) — enables CJK text segmentation. Not called directly.
use parley_engine as _;
use soul_player_host::{
    Band, BandKind, BrowseAxis, Command, DspUnit, EqUnit, Event, FREQ_MAX, FREQ_MIN, GAIN_LIMIT_DB,
    IndexingMode, LoudnessAnalysisMode, MAX_BANDS, PlayState, PlayerHandle, Q_MAX, Q_MIN,
    RepeatMode, ReplayGainMode, SPECTRUM_COLS, ScanProgress, SignalPath, SkippedGroup, SourceCodec,
    SourceFormat, SourceKind, SourceStatus, TrackLoudness, TrackView, WatchUpdate, default_bands,
    organize::{self, Strategy},
    remote::{KeyringStore, TokenStore},
};

mod config;
mod covers;
#[cfg(test)]
mod i18n_tests;
mod logging;
#[cfg(unix)]
mod signal_quit;
// macOS-only at runtime; still compiled for tests everywhere so its pure mapping is
// unit-tested on Linux/Windows CI too.
#[cfg(any(target_os = "macos", test))]
mod macos_menu;
mod media_session;
mod notify;
mod open_files;
mod organize_job;
mod playlists;
mod report;
// The tray (Rune `Tray`) has Linux/Windows backends only; macOS uses `macos_menu`.
#[cfg(any(not(target_os = "macos"), test))]
mod systray;
mod theme;
mod ui;
mod update;
mod views;

use views::player::ArtCache;

/// The UI locales shipped besides the English fallback (see `locales/app.yml`).
pub(crate) const LOCALES: &[&str] = &["en", "de", "es", "zh", "cs", "ja"];

/// Resolve a locale preference to a concrete locale code. A supported pref
/// ("de", "zh", …) is used as-is; "system" — or anything unsupported — matches
/// the OS language via `sys-locale`, falling back to English.
fn resolve_locale(pref: &str) -> &'static str {
    if let Some(&code) = LOCALES.iter().find(|&&c| c == pref) {
        return code;
    }
    let sys = sys_locale::get_locale().unwrap_or_default();
    let lang = sys.split(['-', '_']).next().unwrap_or("en").to_lowercase();
    LOCALES
        .iter()
        .copied()
        .find(|&c| c == lang.as_str())
        .unwrap_or("en")
}

/// Point the global `rust_i18n` locale at `pref`'s resolved code, so the next
/// render's `t!` calls translate against it.
fn apply_locale(pref: &str) {
    rust_i18n::set_locale(resolve_locale(pref));
}

/// Switch the UI language (Settings → Language): remember the pref, apply it, and
/// persist. The next frame re-renders in the new locale.
pub(crate) fn set_locale_pref(s: &mut State, pref: &str) {
    s.locale_pref = pref.to_string();
    apply_locale(pref);
    config::save_locale(pref);
}

/// Which page the main area shows. The sidebar switches it; `view` dispatches on it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Route {
    /// The landing page (library overview + quick actions).
    Home,
    Library,
    Albums,
    /// An album's detail page; which album is in [`State::open_album`] (a unit
    /// variant keeps `Route: Copy`).
    Album,
    Artists,
    /// An artist's detail page; which artist is in [`State::open_artist`].
    Artist,
    Genres,
    /// A genre's detail page; which genre is in [`State::open_genre`].
    Genre,
    Playlists,
    /// A playlist's detail page; which playlist is in [`State::open_playlist`].
    /// Not yet reachable outside tests and the stub dispatch below: a playlist
    /// row's click (the real `navigate_to(.., Route::Playlist, ..)` call site)
    /// lands in Task 8/9, once `views::playlists::page` has real rows to click.
    #[allow(
        dead_code,
        reason = "constructed by tests until Task 8/9 wires a playlist row's click"
    )]
    Playlist,
    Settings,
}

/// One entry in the navigation back-stack: a full location (route + which
/// album/artist/genre is open). Pushed by `navigate_to`, restored by `go_back`.
#[derive(Clone, PartialEq)]
pub(crate) struct NavEntry {
    pub route: Route,
    pub open_album: Option<String>,
    pub open_artist: Option<String>,
    pub open_genre: Option<String>,
    pub open_playlist: Option<playlists::PlaylistId>,
}

/// The current location as a `NavEntry`.
fn here(s: &State) -> NavEntry {
    NavEntry {
        route: s.route,
        open_album: s.open_album.clone(),
        open_artist: s.open_artist.clone(),
        open_genre: s.open_genre.clone(),
        open_playlist: s.open_playlist,
    }
}

/// Open a detail location, remembering where we were so `go_back` can return. The
/// five "open a detail" interactions (album/artist/genre cards, the Discography
/// card, and the now-playing → album link) route through this.
pub(crate) fn navigate_to(
    s: &mut State,
    route: Route,
    open_album: Option<String>,
    open_artist: Option<String>,
    open_genre: Option<String>,
    open_playlist: Option<playlists::PlaylistId>,
) {
    let target = NavEntry {
        route,
        open_album: open_album.clone(),
        open_artist: open_artist.clone(),
        open_genre: open_genre.clone(),
        open_playlist,
    };
    let current = here(s);
    if current != target {
        s.nav_history.push(current);
    }
    s.route = route;
    s.open_album = open_album;
    s.open_artist = open_artist;
    s.open_genre = open_genre;
    s.open_playlist = open_playlist;
    s.mobile_show_content = true;
    s.genre_search = rune::TextEdit::new("");
    s.album_search = rune::TextEdit::new("");
    s.artist_search = rune::TextEdit::new("");
    s.albums_filters_open = false;
    s.album_genre_filter.clear();
    // A playlist's inline new/rename/delete-confirm fields are page-scoped
    // ephemeral UI, same as `genre_search` above; reset rather than carried to
    // wherever we land next (including back onto the same detail page via a
    // fresh open).
    s.playlist_new_buf = rune::TextEdit::new("");
    s.playlist_search = rune::TextEdit::new("");
    s.playlists_adding = false;
    s.playlist_renaming = None;
    s.playlist_rename_buf = rune::TextEdit::new("");
    s.playlist_delete_confirm = None;
}

/// The detail back button: pop to the previous location, or — if the stack is
/// empty — fall back to the current page's browse parent so back is never a dead
/// end.
pub(crate) fn go_back(s: &mut State) {
    s.genre_search = rune::TextEdit::new("");
    s.album_search = rune::TextEdit::new("");
    s.artist_search = rune::TextEdit::new("");
    s.albums_filters_open = false;
    s.album_genre_filter.clear();
    s.playlist_new_buf = rune::TextEdit::new("");
    s.playlist_search = rune::TextEdit::new("");
    s.playlists_adding = false;
    s.playlist_renaming = None;
    s.playlist_rename_buf = rune::TextEdit::new("");
    s.playlist_delete_confirm = None;
    if let Some(prev) = s.nav_history.pop() {
        s.route = prev.route;
        s.open_album = prev.open_album;
        s.open_artist = prev.open_artist;
        s.open_genre = prev.open_genre;
        s.open_playlist = prev.open_playlist;
    } else {
        s.route = match s.route {
            Route::Album | Route::Albums => Route::Albums,
            Route::Artist | Route::Artists => Route::Artists,
            Route::Genre | Route::Genres => Route::Genres,
            Route::Playlist | Route::Playlists => Route::Playlists,
            _ => Route::Home,
        };
    }
}

/// Which Settings category the content pane shows — a sub-navigation inside the
/// Settings route, mirroring the original soul-player's settings rail
/// (Appearance / Music Data / Audio / Keyboard / About). Ephemeral UI state (not
/// persisted); defaults to `Appearance` each launch.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SettingsTab {
    Appearance,
    MusicData,
    Audio,
    Keyboard,
    About,
}

/// Light/dark preference: follow the OS, or force one. Persisted; the Appearance
/// settings section sets it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ThemePref {
    System,
    Light,
    Dark,
}

impl ThemePref {
    fn from_config(s: &str) -> Self {
        match s {
            "light" => ThemePref::Light,
            "dark" => ThemePref::Dark,
            _ => ThemePref::System,
        }
    }
    fn as_config(self) -> &'static str {
        match self {
            ThemePref::System => "system",
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
        }
    }
}

/// The managed-library "Add music → organize" flow as an explicit phase machine,
/// driven entirely by direct state transitions (the pure `organize_*` helpers
/// below) so it is fully headless-testable. `State::organize` holds the current
/// phase (`None` = the flow is idle, no modal). The thread wiring that fills
/// `Planning`/`Applying` with real `organize::plan`/`apply` work is a separate
/// task; this type and its transitions carry only the UI state machine.
pub(crate) enum OrganizePhase {
    /// Scanning the chosen files and computing the plan (Task 2 fills the work).
    Planning,
    /// The plan is ready; the user confirms or cancels before anything is written.
    Confirm(organize::OrganizePlan),
    /// The plan is being applied; progress ticks in per finished entry.
    Applying {
        done: usize,
        total: usize,
        current: String,
    },
    /// The run finished; the outcome tally is shown.
    Done(organize::Summary),
}

struct State {
    /// Read by the view's command handlers (click-to-play, transport).
    host: PlayerHandle,
    library: Vec<TrackView>,
    /// User-created playlists, loaded from the on-disk store at startup
    /// ([`playlists::PlaylistStore::load`]). Every mutation
    /// (create/rename/delete/reorder) persists through the store itself.
    playlists: playlists::PlaylistStore,
    /// User-uploaded custom covers for artists/albums (the old app's "Edit
    /// Artwork"), loaded from `<config>/covers` at startup. A set cover wins
    /// over the generated/embedded art everywhere that artist/album appears
    /// (grid cards + detail heroes). Mirrors the playlist custom-cover store.
    covers: covers::CoverStore,
    /// The text buffer the Playlists list page's inline "New playlist" field
    /// draws and the user types into. Confirming (Enter or the button)
    /// creates a playlist named by the trimmed text and resets this buffer;
    /// a blank/whitespace-only name is a no-op. Also reset when
    /// `navigate_to`/`go_back`/sidebar nav leaves the page, so stale text never
    /// resurfaces. One dedicated buffer per inline editor, like
    /// `dsp_rename_buf`/`remote_url_buf`.
    playlist_new_buf: rune::TextEdit,
    /// The Playlists grid's in-page search box (ephemeral): filters the grid by
    /// playlist name. Same convention as `album_search`/`genre_search`, reset
    /// whenever we navigate (see [`navigate_to`]/[`go_back`]).
    playlist_search: rune::TextEdit,
    /// Whether the Playlists page's inline "New playlist" name field is revealed
    /// (toggled by the top bar's New-playlist button). Page-scoped ephemeral UI,
    /// reset on nav like `playlist_new_buf`; confirming a new playlist navigates
    /// away, which clears it.
    playlists_adding: bool,
    /// Which playlist's inline rename field (on its detail page) is open, or
    /// `None`. Mirrors `dsp_rename`; its buffer is `playlist_rename_buf`. Reset
    /// (not committed) on cancel and whenever `navigate_to`/`go_back` leaves the
    /// current location, so a stale buffer never resurfaces on the next visit
    /// (the same page-scoped reset `playlist_new_buf` gets).
    playlist_renaming: Option<playlists::PlaylistId>,
    /// The text buffer the inline playlist-rename editor draws. Seeded from the
    /// playlist's current name when a rename opens; ignored while
    /// `playlist_renaming` is `None`.
    playlist_rename_buf: rune::TextEdit,
    /// Which playlist's delete action is pending the confirm step (its detail
    /// page shows "Delete this playlist?" plus Delete/Cancel in place of the
    /// Rename/Delete icons), or `None`. A single stray click never deletes.
    /// Reset the same way as `playlist_renaming`.
    playlist_delete_confirm: Option<playlists::PlaylistId>,
    /// The track id pending an "Add to playlist" pick, or `None`. `Some`
    /// opens `views::add_to_playlist::overlay`, mounted at the window root
    /// like `signal_path_open`/`remote_health_open`: it survives a route
    /// change (a global action, not a page-local widget) and is reset only
    /// by `open_add_to_playlist`/`close_add_to_playlist`.
    add_to_playlist_for: Option<u32>,
    /// The overlay's inline "New playlist..." field, bound the same way
    /// `playlist_new_buf` binds the Playlists page's own field.
    add_to_playlist_buf: rune::TextEdit,
    /// The overlay's "Search playlists..." filter box, narrowing the
    /// membership list by name. Reset alongside the pending track so the
    /// overlay always opens with an empty filter.
    add_to_playlist_search: rune::TextEdit,
    /// The in-progress "Edit Artwork" crop (the old app's cover-crop step), or
    /// `None` (closed). `Some` opens `views::artwork_crop::overlay`, mounted at
    /// the window root like `add_to_playlist_for`: it carries the picked source
    /// image plus the live pan/zoom, and confirming crops + stores it as the
    /// target album/artist's custom cover (non-destructively). Reset only by
    /// `edit_artwork_control`'s pick and `close_artwork_crop`.
    artwork_crop: Option<crate::views::detail_page::ArtworkCrop>,
    /// The "Edit Artwork" MANAGE dialogue (the old app's 'select' state), or
    /// `None` (closed). `Some(target)` is opened by the hero's pencil control and
    /// offers Change (pick an image -> the crop step) and, when a custom cover is
    /// already set, Delete. `artwork_edit` and `artwork_crop` are never both
    /// `Some`: Change clears this and sets `artwork_crop`.
    artwork_edit: Option<crate::views::detail_page::ArtworkTarget>,
    now_playing_id: Option<u32>,
    position_secs: f64,
    duration_secs: f64,
    playing: bool,
    /// Whether the window is maximized (or full screen), so the title bar can
    /// show the RESTORE glyph in place of maximize — the same swap Windows
    /// makes on its native caption button.
    ///
    /// Mirrored into `State` rather than read from the window inside the view:
    /// a view runs on every frame and must stay pure, and the authoritative
    /// answer is an OS query behind a mutex. `sync_window_maximized` refreshes
    /// it from the live window each frame instead.
    maximized: bool,
    /// The backend's full three-way playback state, as `Tick` reports it.
    ///
    /// `playing` is the bool the UI draws from (play icon vs pause icon), but it
    /// collapses Paused and Stopped into the same `false`. The OS media session
    /// has to tell them apart — MPRIS has a distinct `Stopped` status, and
    /// reporting `Paused` after a Stop is a spec violation a Linux desktop's
    /// lock screen shows — so the unreduced state is kept alongside it.
    play_state: PlayState,
    volume: f32,
    /// Whether the output is muted — mirrored from `Tick.muted`. The backend owns
    /// the mute state and the pre-mute level; the UI just sends `ToggleMute` and
    /// reflects this flag.
    muted: bool,
    shuffle: bool,
    repeat: RepeatMode,
    /// The manual queue ("Next in queue"): tracks the user queued via
    /// `PlayNext`/`Enqueue`, in play order. Reorderable and removable.
    up_next: Vec<u32>,
    /// The real upcoming CONTEXT tracks (shuffle-correct) after the manual queue,
    /// from the backend's `QueueState` — the "Next from <context>" band. This
    /// replaces the old synthesis from `library` order (wrong under shuffle).
    context_ids: Vec<u32>,
    /// Transient drag-reorder state for the Up-Next band: `Some((from, to))` while
    /// a row is being dragged, `None` at rest. Drives the drop indicator; the
    /// commit (`Command::MoveInQueue`) fires on release.
    queue_drag: Option<(usize, usize)>,
    context_pos: u32,
    context_len: u32,
    /// The current track's decode→output chain (source/rate/bits/ReplayGain/
    /// resample), from the backend's `Event::SignalPath`. `None` when nothing
    /// plays. Rendered as the now-playing signal-path line.
    signal_path: Option<SignalPath>,
    /// Whether the expanded signal-path panel (the stage-by-stage view) is open —
    /// toggled by pressing the now-playing signal-path line.
    signal_path_open: bool,
    /// Current page (Library vs Settings).
    route: Route,
    /// The album whose detail page is open (by album name, the `group_albums`
    /// key), when `route == Route::Album`. Set from an album card's body click.
    open_album: Option<String>,
    /// Which artist's detail page is showing, when `route == Route::Artist`.
    /// Set from an artist card's body click.
    open_artist: Option<String>,
    /// Which genre's detail page is showing, when `route == Route::Genre`.
    /// Set from a genre row's click.
    open_genre: Option<String>,
    /// Which playlist's detail page is showing, when `route == Route::Playlist`.
    /// Unlike `open_album`/`open_artist`/`open_genre` (keyed by display name),
    /// this is the playlist's stable [`playlists::PlaylistId`]: a
    /// playlist has no natural unique name, so the id is the only durable key.
    /// Set from a playlist row's click.
    open_playlist: Option<playlists::PlaylistId>,
    /// The navigation back-stack (ephemeral): locations behind the current one, so
    /// a detail's back button returns to where you came from rather than a
    /// hardcoded parent. Pushed by [`navigate_to`], popped by [`go_back`]; cleared
    /// by a top-level sidebar nav.
    nav_history: Vec<NavEntry>,
    /// Artist-page Discography view mode (ephemeral): `false` = album-card grid
    /// (default), `true` = compact row list. Toggled by the grid/list buttons.
    discography_list: bool,
    /// The Genre-detail in-page search box (ephemeral): filters the open genre's
    /// track list. Separate from the library search so the two never cross over;
    /// reset whenever we navigate (see [`navigate_to`]/[`go_back`]).
    genre_search: rune::TextEdit,
    /// The Albums grid's in-page search box (ephemeral): filters the grid by
    /// album name or artist. Same convention as `genre_search`, reset whenever
    /// we navigate (see [`navigate_to`]/[`go_back`]).
    album_search: rune::TextEdit,
    /// Whether the Albums page's genre-Filters chip row is expanded.
    albums_filters_open: bool,
    /// Genres selected in the Albums page's Filters (empty = no genre filter).
    album_genre_filter: std::collections::BTreeSet<String>,
    /// The Artists grid's in-page search box (ephemeral): filters the grid by
    /// artist name. Same convention as `genre_search`, reset whenever we
    /// navigate (see [`navigate_to`]/[`go_back`]).
    artist_search: rune::TextEdit,
    /// Whether the artist detail's "Top Songs" section is expanded past the first
    /// five (toggled by its Show more/less link). Reset when a new artist opens.
    artist_top_expanded: bool,
    /// Which Settings category the content pane shows (ephemeral; defaults to
    /// `Appearance`). Switched by the settings page's own category rail.
    settings_tab: SettingsTab,
    /// Light/dark preference (persisted); `System` follows the OS.
    theme_pref: ThemePref,
    /// Which date an album's "year" is read from (persisted): the original
    /// first-release date, or this particular release's own date. Consumed by
    /// the library/discography display and sort.
    album_year: config::AlbumYearSource,
    /// Whether the library track list shows the per-track loudness columns (DR + LUFS)
    /// and their sort pills. User preference (Settings -> Appearance), persisted.
    show_loudness_columns: bool,
    /// Which baked axis the library list is ordered by.
    sort_axis: BrowseAxis,
    /// An optional client-side loudness sort layered over `sort_axis` (DR or Integrated
    /// LUFS). `None` = the plain baked-axis order. Loudness is joined from the live cache,
    /// not a baked axis, so this sort runs in memory and is re-applied on each refresh.
    loudness_sort: Option<LoudnessKey>,
    /// Set when a `TrackLoudnessAnalyzed` event arrives; the next Tick re-fetches the library
    /// so the loudness/DR column fills in, coalescing a sweep's event burst.
    loudness_dirty: bool,
    /// The background loudness sweep's remaining work (from `Tick.analysis_pending`), shown as
    /// an "Analyzing N tracks" line in the Settings loudness section while > 0.
    analysis_pending: u32,
    /// The configured sources with their per-source status (from the host),
    /// rendered by Settings → Sources: each carries its path, enabled flag,
    /// current availability (an unreachable root shows "offline"), and track count.
    /// A `Remote` source's entry also carries its last-probed [`RemoteHealth`]
    /// (latency/version/online/last-checked), kept fresh by the periodic poller
    /// spawned in `main` plus an on-demand refresh (see [`refresh_remote_health`]).
    sources: Vec<SourceStatus>,
    /// Whether a remote-health refresh (the periodic poller or an on-demand one
    /// from opening Settings → Music Data) is in flight on a background thread.
    /// Guards against two overlapping probes — see [`refresh_remote_health`].
    remote_health_refreshing: bool,
    /// Whether the title-bar remote-health popover is open — toggled by
    /// pressing the title-bar indicator (`views::titlebar::remote_health_indicator`),
    /// closed by an outside press/Escape (`El::modal`) or the popover's own
    /// "Open sources settings" action. Ephemeral; mirrors `signal_path_open`.
    remote_health_open: bool,
    /// A transient "N tracks removed from <folder>" warning for the Sources page,
    /// set from the host's [`Event::TracksRemoved`] when a rescan finds deletions
    /// (`(root, count)`). Cleared on the next Sources refresh ([`apply_watch`]) so
    /// it reads as a notice about the *last* scan, not a permanent banner.
    removal_notice: Option<(PathBuf, usize)>,
    /// What the most recent scan passed over, for the Sources page: the source
    /// root and the per-extension groups with their reason, from the host's
    /// [`Event::TracksSkipped`]. `None` when the last scan imported everything it
    /// found. Cleared on the next Sources refresh ([`apply_watch`]) so it reads as
    /// a notice about the *last* scan rather than a permanent banner.
    skip_notice: Option<(PathBuf, Vec<SkippedGroup>)>,
    /// Whether the Remote server connect form is open (Settings → Add Source →
    /// Remote server). Opened by `open_remote_form` (which also closes the
    /// add-source menu), closed by Cancel/Escape/a successful connect.
    remote_form_open: bool,
    /// The connect form's server-URL buffer. Follows the `dsp_rename_buf`
    /// lifecycle: a fresh, empty, focused buffer when the form opens; read
    /// (trimmed) on Connect; ignored while the form is closed. There is no
    /// username/password buffer — the login itself happens in the system
    /// browser (OAuth), so the app never holds a password.
    remote_url_buf: rune::TextEdit,
    /// The most recent connect attempt's failure message (the host's
    /// `anyhow::Error`, rendered), shown inline in the form. `None` when the
    /// form is closed, just opened, or the last attempt succeeded.
    remote_error: Option<String>,
    /// Whether a connect attempt is in flight on the background thread
    /// `connect_remote_source` spawns (waiting on the browser round-trip).
    /// Disables the Connect button and switches the form to a "waiting for
    /// your browser" status line while `true`. Cleared when the thread's
    /// result lands back on the UI thread via `AppHandle::update`.
    remote_connecting: bool,
    /// Every remote URL persisted via `config::save_remote` (seeded from
    /// `config::load_remotes()` by [`reconnect_remotes_on_startup`], kept in
    /// step by a successful connect/[`disconnect_remote_source`]). Used by
    /// `views::settings::merged_source_rows` (Inc 6) to render an offline row
    /// for a configured remote that has no live entry in `sources` — one
    /// that failed to reconnect at launch, or is still mid-reconnect — so it
    /// never silently vanishes from the Sources list.
    remote_urls: Vec<String>,
    /// URLs currently mid a manual reconnect/re-sync attempt, started by
    /// [`reconnect_remote_now`] (an offline row's Reconnect action, or a
    /// connected row's "update available" Sync-now action). Drives that
    /// row's transient "reconnecting…" state; cleared whichever way the
    /// attempt ends. A url present here is guarded against a second
    /// concurrent attempt.
    remote_reconnecting: Vec<String>,
    /// The `index_version` each remote was last actually synced at — the
    /// Inc 6 "update available" cue's baseline. A health probe only fills a
    /// MISSING entry (the first reading after a connect establishes the
    /// baseline); only a real sync — a connect, or [`reconnect_remote_now`]
    /// succeeding — overwrites it (see `record_synced_index` /
    /// `seed_synced_baseline`). `views::settings::remote_update_available`
    /// compares this against the live health's `index_version` to decide
    /// whether a re-sync would pick up fresh tracks.
    remote_synced_index: std::collections::HashMap<String, u64>,
    /// The library indexing-speed mode (persisted), rendered by Settings → Sources.
    /// Drives how much parallelism the scanner uses; `Automatic` follows CPU load.
    indexing_mode: IndexingMode,
    /// How the background loudness sweep runs (persisted): Off / Throttled / Full. Drives the
    /// scan-time Tier-2 analysis that fills the library loudness/DR column; Throttled yields
    /// to active playback so it never causes dropouts.
    loudness_mode: LoudnessAnalysisMode,
    /// Live progress of a running library scan (from the host's `on_scan_progress`),
    /// rendered as the title-bar activity widget. `None` when no scan is in flight;
    /// set per tick while an off-UI-thread scan runs and cleared on the completing
    /// [`apply_watch`]. Only off-thread scans (the live watcher, a threaded add)
    /// animate it — a scan that blocks the UI thread fills it in one step at the end.
    scan_progress: Option<ScanProgress>,
    /// Free-running phase (seconds) advancing while `scan_progress` is `Some`, driven
    /// by the per-frame poll; the activity icon's pulse is a function of it.
    scan_anim: f32,
    /// The chosen output device (persisted), rendered by Settings → Output;
    /// `None` = follow the system default. Applies to the next playback session.
    output_device: Option<String>,
    /// The output devices enumerated from the host — the picker's choices. Enumerated
    /// OFF the UI thread (cpal's WASAPI enumeration can take seconds on a Bluetooth-heavy
    /// system) and refreshed on a loop, so this starts seeded with just the selected device
    /// and fills in once the first enumeration lands.
    output_devices: Vec<String>,
    /// True until the first off-thread device enumeration has landed — the picker shows a
    /// "scanning" row instead of an empty/"none" state while the (possibly multi-second)
    /// enumeration is still in flight, so the list never looks mistakenly empty.
    output_devices_loading: bool,
    /// How each output device is connected (AirPlay / Bluetooth / USB …), filled in
    /// off the UI thread after startup (macOS; empty elsewhere) — lets the picker flag
    /// devices that can never be bit-perfect before the user picks one.
    output_transports:
        std::collections::HashMap<String, soul_player_host::device_transport::Transport>,
    /// Files the OS asked us to open that weren't in the library yet: held while their
    /// folders are added and scanned, then played (`open_files`).
    pending_open: Vec<PathBuf>,
    /// The restored library's first scan hasn't landed yet. Files opened meanwhile wait in
    /// `pending_open` (their track ids would be resolved against a library that's about to
    /// be replaced) and are opened when it lands (`apply_startup_scan`).
    startup_scan_pending: bool,
    /// Whether **exclusive** (bit-perfect) output is requested (persisted), rendered
    /// as a toggle in Settings → Output. Applies to the next playback session; the
    /// device may fall back to shared if it can't open exclusive (reflected live in
    /// the signal path's Exclusive stage).
    exclusive: bool,
    /// Whether **strict** exclusive (exclusive-or-stop) is requested (persisted). With
    /// `exclusive`, the next session won't fall back to shared if exclusive can't open
    /// — playback stops instead (the purist stance). Only meaningful when `exclusive`.
    exclusive_strict: bool,
    /// ReplayGain normalization mode (persisted), rendered by Settings →
    /// ReplayGain; `Off` = no normalization.
    replaygain_mode: ReplayGainMode,
    /// ReplayGain preamp in dB (persisted), a constant offset on top of the mode's
    /// gain. Rendered as a slider in Settings → ReplayGain.
    replaygain_preamp: f32,
    /// Saved audio profiles — each a named snapshot of the whole DSP chain, with one
    /// active. The *active* one's chain is the live `dsp_units`; editing writes back
    /// through to it (auto-save). The set, its switching and the exchange format all
    /// live in the shared `soul-audio-dsp` crate (reused by the DAP); this holds one and
    /// wraps each mutation with persist + engine-push. Always non-empty.
    profile_set: soul_player_host::ProfileSet,
    /// Which profile's name is being renamed inline (the Rename button opens a text
    /// field over the switcher), or `None`. Mirrors `dsp_rename`; its buffer is
    /// `profile_rename_buf`.
    profile_rename: Option<usize>,
    /// The text buffer the inline profile-rename editor draws. Seeded from the
    /// profile's current name when a rename opens; ignored while `profile_rename`
    /// is `None`.
    profile_rename_buf: rune::TextEdit,
    /// The DSP chain (persisted): an ordered list of typed DSP units (EQ is the only
    /// kind today); the signal flows through them in order. This is the active
    /// profile's chain — `apply_dsp` writes it back into `profiles[active_profile]`.
    /// Rendered as the reorderable unit list in the Settings → Audio DSP/EQ card.
    dsp_units: Vec<DspUnit>,
    /// Per-unit card expand/collapse flags, parallel to `dsp_units` (same length,
    /// same order). `true` = that unit's card is open (its preamp + response graph +
    /// band table show). Every `dsp_units` mutator keeps it in sync; the first card
    /// starts open. Every EQ mutator takes an explicit unit index — each card passes
    /// its own position `i`.
    dsp_expanded: Vec<bool>,
    /// Per-unit band-table expand flags, parallel to `dsp_units`. `true` = the band
    /// list under that unit's graph is shown; `false` collapses it to just its header.
    /// Kept in sync alongside `dsp_expanded`; defaults open.
    eq_bands_expanded: Vec<bool>,
    /// In-flight unit-list reorder drag: `(from, to)` indices, or `None`.
    dsp_drag: Option<(usize, usize)>,
    /// In-flight playlist-entry reorder drag: `(from, to)` entry indices while a
    /// detail-row grip is dragged, or `None`. Committed via `move_entry` on
    /// release. Mirrors `dsp_drag`.
    playlist_drag: Option<(usize, usize)>,
    /// In-flight EQ-graph node drag: `(unit, band, band state at press)`, so the
    /// drag applies absolutely from the press point. `None` when not dragging.
    eq_drag: Option<(usize, usize, Band)>,
    /// The `(unit, band)` whose handle is selected/focused (shows its per-band curve
    /// + a readout). `None` when nothing is selected.
    eq_selected: Option<(usize, usize)>,
    /// In-flight Ableton-style number-box drag in the band table: `(unit, band,
    /// which number, the value at the press)`, so the vertical drag applies
    /// absolutely from the press point. `None` when not dragging a number.
    eq_num_drag: Option<(usize, usize, NumField, f32)>,
    /// Which band-table number is being typed into (double-click a value to open an
    /// inline text field): `(unit, band, which number)`, or `None`. The buffer it
    /// edits is `eq_edit_buf` (kept separate so the field's `on_edit` accessor always
    /// has a live `TextEdit` to return, even the frame the edit closes).
    eq_edit: Option<(usize, usize, NumField)>,
    /// The text buffer the inline number editor draws and the user types into. Reset
    /// to the current value when an edit opens; ignored while `eq_edit` is `None`.
    eq_edit_buf: rune::TextEdit,
    /// Which DSP unit's title is being renamed (double-click a card title to open an
    /// inline text field): the unit index, or `None`. The buffer it edits is
    /// `dsp_rename_buf` (kept separate, like `eq_edit`/`eq_edit_buf`, so the field's
    /// `on_edit` accessor always has a live `TextEdit` even the frame the edit closes).
    dsp_rename: Option<usize>,
    /// The text buffer the inline rename editor draws and the user types into. Seeded
    /// from the unit's current name when a rename opens; ignored while `dsp_rename`
    /// is `None`.
    dsp_rename_buf: rune::TextEdit,
    /// Accumulated wheel travel over a band-table number box, so a trackpad's many
    /// small `precise` deltas add up to discrete steps (a notched mouse wheel still
    /// steps once per detent). Self-draining; no reset needed.
    eq_wheel_accum: f32,
    /// Whether Shift is held (tracked via `Key::Modifier`), so a number drag can
    /// fine-tune — the `Drag` event carries no modifiers, unlike the wheel's.
    shift_held: bool,
    /// The latest spectrum-analyzer columns (smoothed dBFS, 20 Hz…20 kHz log) the
    /// EQ graph draws behind the curve. Empty when not playing / tab hidden. Pulled
    /// from the host by `spectrum_poll` at frame rate.
    spectrum: Vec<f32>,
    /// The sequence of the last spectrum frame copied into `spectrum`, so the poll
    /// only re-renders on a genuinely new FFT.
    spectrum_seq: u64,
    /// Managed-library destination folder (persisted; `None` = unset), where the
    /// write-path organizer will copy / move added music. Set in Settings → Music
    /// Data. Configuring only — the "Add music…" apply flow is separate.
    managed_dest: Option<PathBuf>,
    /// Managed-library template preset id (persisted) — the folder layout the
    /// organizer arranges into. Chosen from presets (Rune's IME is pre-release, so
    /// there's no free-text template entry).
    managed_template: String,
    /// Managed-library file strategy (persisted): `Copy` keeps the source file,
    /// `Move` removes it.
    managed_strategy: Strategy,
    /// The in-flight "Add music → organize" flow, or `None` when idle. Drives the
    /// centered organize modal (`views::settings::organize_overlay`); the pure
    /// `organize_*` transitions move it through its phases. Ephemeral.
    organize: Option<OrganizePhase>,
    /// A self-handle onto this app, stashed once at startup (`main`), so the
    /// managed-library organize flow's worker threads can post plan/apply progress
    /// back onto the main thread via `AppHandle::update`. `None` in headless tests
    /// (and until `main` sets it), where the organize transitions stay pure.
    app_handle: Option<rune::AppHandle<State>>,
    /// Channel to the OS media-session thread (`media_session::spawn`), set once
    /// the window is ready (`main`'s `on_ready`). `None` in headless tests / where
    /// the OS session couldn't start; feeding it is then a no-op.
    media_tx: Option<std::sync::mpsc::Sender<media_session::Update>>,
    /// Last now-playing id / play-state / push time pushed to the media session,
    /// for change detection + the periodic refresh (`media_session::decide_push`).
    media_last_id: Option<u32>,
    media_last_playing: bool,
    media_last_stopped: bool,
    media_last_push: Option<std::time::Instant>,
    /// Whether the window currently has OS focus (`App::on_focus`). Gates
    /// now-playing toasts — foreground users already see the UI. Starts `true`.
    focused: bool,
    /// Last track id a now-playing toast was considered for, independent of the
    /// media session (`media_last_id`) so the toast fires on every platform.
    notify_last_id: Option<u32>,
    /// Cover-art decode cache (`views::player::ArtCache`), wired up with the
    /// app's `Waker` in `setup_art` below.
    art: ArtCache,
    /// The version of a newer signed release, set by the launch-time update
    /// check (`update::available`); `Some` makes Settings surface "Install &
    /// Restart". `None` = up to date (or the check hasn't run / failed).
    update_available: Option<String>,
    /// Set at startup (`logging::begin_session`) when the previous session ended
    /// without a clean exit — surfaces the dismissible "report this crash" banner.
    crashed_last_session: bool,
    /// Library filter query (case-insensitive substring over title/artist/album);
    /// empty = no filter. Rune has no text widget yet, so it's typed into via the
    /// key handler (`/` to start), not a focused input field.
    search: String,
    /// Whether typed characters currently build `search` (vs. driving the
    /// transport shortcuts). `/` or clicking the search bar turns it on; Esc/Enter
    /// turn it off.
    searching: bool,
    /// The resizable sidebar's width (clamped 240–480) and whether it's collapsed
    /// to a thin rail. Persisted via [`config::load_sidebar`]/[`config::save_sidebar`]
    /// and driven by the drag handle in `views::sidebar`.
    sidebar_width: f32,
    sidebar_collapsed: bool,
    /// Content-pane zoom factor (Ctrl +/-/0), 1.0 = 100%. Persisted via
    /// [`config::load_ui_zoom`]/[`config::save_ui_zoom`], adjusted by the Ctrl chords
    /// in [`handle_key`], and applied to the content pane in [`content_pane`].
    ui_zoom: f32,
    /// Mobile (narrow window) single-pane state: `false` shows the sidebar
    /// (nav + queue + player), `true` shows the content page with a back bar —
    /// the old app's `mobileShowContent`. Ignored on desktop (both panes show).
    mobile_show_content: bool,
    /// Primary nav items the user has hidden (by id — see `sidebar::NAV_DEFS`);
    /// empty = all shown. Toggled from Settings → Navigation, persisted.
    nav_hidden: Vec<String>,
    /// Per-nav-item reveal progress (0 = gone, 1 = shown), advanced each poll
    /// frame by `views::reveal::tick` so a Settings toggle fades-then-collapses the
    /// item instead of snapping. An id absent from the map sits at its target.
    nav_reveal: std::collections::HashMap<&'static str, f32>,
    /// The absolute poll clock seen last frame, so [`views::reveal::frame_delta`]
    /// can recover the real per-frame delta (Rune's poll arg is the absolute clock,
    /// not a delta). Drives every manual, time-based animation (the nav reveal).
    reveal_clock: f64,
    /// UI language preference ("system"/"en"/"de"/…); drives the global
    /// `rust_i18n` locale via [`apply_locale`]. Changed by Settings → Language.
    locale_pref: String,
    /// Which in-app dropdown is open, by its key (`None` = none). Ephemeral UI
    /// state — the themed `views::dropdown` component reads it to show its panel,
    /// and a full-window backdrop closes it on an outside click.
    open_dropdown: Option<String>,
    /// The key of the slider being dragged right now (`None` = none), so it alone
    /// shows the horizontal-drag cursor. Ephemeral.
    dragging_slider: Option<String>,
    /// While the seek bar is being scrubbed, the previewed position in seconds —
    /// the knob and the elapsed-time label follow it, and the real `Seek` fires
    /// only on release. `None` when not scrubbing. Ephemeral.
    scrub_secs: Option<f64>,
}

impl State {
    fn new(host: PlayerHandle) -> Self {
        let library = host.views();
        // Load user-created playlists from the on-disk store (empty store if none
        // exist yet, or the folder hasn't been created).
        let playlists = playlists::PlaylistStore::load();
        // Load user-uploaded custom artist/album covers from `<config>/covers`
        // (empty if the folder doesn't exist / nothing has been set yet).
        let covers = covers::CoverStore::load();
        // Seed the per-source status from the host (its configured set, each with
        // live availability). Counts are 0 until the first rescan's WatchUpdate —
        // live edits refresh the full status via apply_watch.
        let sources = host.sources();
        // The output-device list is enumerated OFF the UI thread after the window opens and
        // refreshed on a gentle loop (see the device-refresh thread in `main`): cpal's WASAPI
        // enumeration can block for several seconds on a Bluetooth endpoint, so it must never
        // sit on the init/first-paint path, and polling it keeps the picker current when a
        // device is plugged/unplugged. Seed with the currently-selected device so the picker
        // shows that one immediately; the refresh fills in the rest a moment later.
        let output_devices: Vec<String> = config::load_output_device().into_iter().collect();
        // Restore the persisted ReplayGain choice (defaults: Off, 0 dB); main()
        // has already pushed the same value to the host before the first spawn.
        let (replaygain_mode, replaygain_preamp) = config::load_replaygain();
        // Restore the persisted DSP chain (defaults: one off, flat unit); main() has
        // already pushed the same chain to the host before the first spawn. Each
        // card edits `dsp_units[i]` directly (no mirror).
        // Load the saved profiles (always ≥1 — a "Default" is migrated from the legacy
        // chain); the active profile's chain is the live `dsp_units`.
        let profile_set = config::load_profiles();
        let dsp_units = profile_set
            .active_chain()
            .map(|c| c.units.clone())
            .unwrap_or_default();
        // One expand flag per unit; the first card starts open, the rest collapsed.
        let dsp_expanded: Vec<bool> = (0..dsp_units.len()).map(|i| i == 0).collect();
        // Band tables default open within an expanded card.
        let eq_bands_expanded: Vec<bool> = vec![true; dsp_units.len()];
        let (sidebar_width, sidebar_collapsed) = config::load_sidebar();
        let locale_pref = config::load_locale();
        apply_locale(&locale_pref);
        // Restore the managed-library choice (defaults: unset dest, "artist-album"
        // template, Copy); the stored strategy id maps to the organizer enum.
        let (managed_dest, managed_template, managed_strategy_id) = config::load_managed_library();
        let managed_strategy = if managed_strategy_id == "move" {
            Strategy::Move
        } else {
            Strategy::Copy
        };
        Self {
            host,
            library,
            playlists,
            covers,
            playlist_new_buf: rune::TextEdit::new(""),
            playlist_search: rune::TextEdit::new(""),
            playlists_adding: false,
            playlist_renaming: None,
            playlist_rename_buf: rune::TextEdit::new(""),
            playlist_delete_confirm: None,
            add_to_playlist_for: None,
            add_to_playlist_buf: rune::TextEdit::new(""),
            add_to_playlist_search: rune::TextEdit::new(""),
            artwork_crop: None,
            artwork_edit: None,
            now_playing_id: None,
            position_secs: 0.0,
            duration_secs: 0.0,
            playing: false,
            maximized: false,
            play_state: PlayState::Stopped,
            volume: 1.0,
            muted: false,
            shuffle: false,
            repeat: RepeatMode::Off,
            up_next: Vec::new(),
            context_ids: Vec::new(),
            queue_drag: None,
            context_pos: 0,
            context_len: 0,
            signal_path: None,
            signal_path_open: false,
            route: Route::Library,
            open_album: None,
            open_artist: None,
            open_genre: None,
            open_playlist: None,
            nav_history: Vec::new(),
            discography_list: false,
            genre_search: rune::TextEdit::new(""),
            album_search: rune::TextEdit::new(""),
            albums_filters_open: false,
            album_genre_filter: std::collections::BTreeSet::new(),
            artist_search: rune::TextEdit::new(""),
            artist_top_expanded: false,
            settings_tab: SettingsTab::Appearance,
            theme_pref: config::load_theme_pref()
                .map(|s| ThemePref::from_config(&s))
                .unwrap_or(ThemePref::System),
            album_year: config::load_album_year(),
            show_loudness_columns: config::load_show_loudness_columns(),
            sort_axis: BrowseAxis::ArtistAlbumTrack,
            loudness_sort: None,
            loudness_dirty: false,
            analysis_pending: 0,
            sources,
            remote_health_refreshing: false,
            remote_health_open: false,
            removal_notice: None,
            skip_notice: None,
            remote_form_open: false,
            remote_url_buf: rune::TextEdit::new(""),
            remote_error: None,
            remote_connecting: false,
            // Seeded by `reconnect_remotes_on_startup` at real launch (which
            // reads `config::load_remotes()` there, not here, so an ordinary
            // `State::new()` in a unit test never touches the shared config
            // file unless the test calls that function itself).
            remote_urls: Vec::new(),
            remote_reconnecting: Vec::new(),
            remote_synced_index: std::collections::HashMap::new(),
            scan_progress: None,
            scan_anim: 0.0,
            indexing_mode: IndexingMode::from_config(&config::load_indexing_mode()),
            loudness_mode: LoudnessAnalysisMode::from_config(&config::load_loudness_analysis()),
            // Restore the persisted device choice (None = system default); the
            // enumerated list came from `host` just above.
            output_device: config::load_output_device(),
            output_devices,
            // Cleared when the first off-thread enumeration lands (see the device-refresh
            // loop in `main`); until then the picker shows a "scanning" row.
            output_devices_loading: true,
            output_transports: Default::default(),
            pending_open: Vec::new(),
            startup_scan_pending: false,
            exclusive: config::load_output_exclusive(),
            exclusive_strict: config::load_output_exclusive_strict(),
            replaygain_mode,
            replaygain_preamp,
            dsp_units,
            dsp_expanded,
            eq_bands_expanded,
            dsp_drag: None,
            playlist_drag: None,
            eq_drag: None,
            eq_selected: None,
            eq_num_drag: None,
            eq_edit: None,
            eq_edit_buf: rune::TextEdit::new(""),
            dsp_rename: None,
            dsp_rename_buf: rune::TextEdit::new(""),
            profile_set,
            profile_rename: None,
            profile_rename_buf: rune::TextEdit::new(""),
            eq_wheel_accum: 0.0,
            shift_held: false,
            spectrum: Vec::new(),
            spectrum_seq: 0,
            managed_dest,
            managed_template,
            managed_strategy,
            organize: None,
            // Set once in `main` (a self-posting update); stays `None` in tests.
            app_handle: None,
            media_tx: None,
            media_last_id: None,
            media_last_playing: false,
            media_last_stopped: true,
            media_last_push: None,
            focused: true,
            notify_last_id: None,
            art: ArtCache::new(),
            update_available: None,
            // Set from `logging::begin_session()` in `main`; `false` in tests.
            crashed_last_session: false,
            search: String::new(),
            searching: false,
            sidebar_width,
            sidebar_collapsed,
            ui_zoom: sane_zoom(config::load_ui_zoom()),
            // The menu is the mobile home view; a nav tap flips this true.
            mobile_show_content: false,
            nav_hidden: config::load_nav_hidden(),
            nav_reveal: std::collections::HashMap::new(),
            reveal_clock: 0.0,
            locale_pref,
            open_dropdown: None,
            dragging_slider: None,
            scrub_secs: None,
        }
    }

    /// Whether the primary nav item `id` is currently shown.
    pub(crate) fn nav_visible(&self, id: &str) -> bool {
        !self.nav_hidden.iter().any(|h| h == id)
    }

    /// The reveal progress (0..1) for nav item `id`: the stored animation value, or
    /// — before it has ever animated — its resting target (1 shown, 0 hidden).
    pub(crate) fn nav_reveal(&self, id: &str) -> f32 {
        self.nav_reveal
            .get(id)
            .copied()
            .unwrap_or(if self.nav_visible(id) { 1.0 } else { 0.0 })
    }

    /// Resolve a library track by id. Shared by the now-playing panel
    /// (`now_playing_id`) and the sidebar queue (`up_next`) so both compose
    /// the same lookup rather than each re-scanning `library` inline.
    fn track(&self, id: u32) -> Option<&TrackView> {
        self.library.iter().find(|t| t.id == id)
    }

    /// Fold one backend event into the UI state (the UI's event sink).
    fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Tick(t) => {
                self.now_playing_id = t.now_playing_id;
                self.position_secs = t.position_secs;
                // Hold a committed seek's preview until playback reaches it (and
                // keep the live preview while still dragging), so the knob doesn't
                // flick to this stale position and back.
                self.scrub_secs = settle_scrub(
                    self.scrub_secs,
                    self.dragging_slider.as_deref() == Some("seek"),
                    t.position_secs,
                );
                self.duration_secs = t.duration_secs.unwrap_or(0.0);
                self.playing = matches!(t.state, PlayState::Playing);
                self.play_state = t.state;
                // Don't let a stale tick overwrite the volume mid-drag (the drag
                // sets it optimistically; the host echo catches up after release).
                if self.dragging_slider.as_deref() != Some("volume") {
                    self.volume = t.volume;
                }
                // Mute lives in the backend (#105): reflect its `Tick.muted`.
                self.muted = t.muted;
                self.shuffle = t.shuffle;
                self.repeat = t.repeat;
                // Background loudness sweep progress, for the Settings "Analyzing N" line.
                self.analysis_pending = t.analysis_pending;
                // Fresh loudness analysis arrived since the last tick: re-fetch the library
                // so the loudness/DR column fills in. Coalesced here (≤ tick rate) so a
                // full-library sweep doesn't re-project per track.
                if self.loudness_dirty {
                    self.library = self.host.views_by(self.sort_axis);
                    // Re-apply an active loudness sort so freshly-analyzed tracks slot into
                    // their place (and drop out of the un-analyzed tail) as values land.
                    if let Some(key) = self.loudness_sort {
                        sort_by_loudness(&mut self.library, key);
                    }
                    self.loudness_dirty = false;
                }
            }
            Event::Queue(q) => {
                self.up_next = q.up_next_ids.clone();
                self.context_ids = q.context_ids.clone();
                self.context_pos = q.context_pos;
                self.context_len = q.context_len;
            }
            Event::SignalPath(sp) => {
                self.signal_path = *sp;
            }
            // Persisted off-thread in the subscribe closure (see `main`). For the library
            // loudness/DR column, flag a refresh (coalesced on the next Tick, below) rather
            // than re-projecting the whole library per analyzed track during a sweep.
            Event::TrackLoudnessAnalyzed { .. } => {
                self.loudness_dirty = true;
            }
            // A rescan found deletions under an available source — stash the warning
            // for the Sources page (cleared on the next refresh by `apply_watch`).
            Event::TracksRemoved { root, count } => {
                self.removal_notice = Some((root.clone(), *count));
            }
            // A rescan walked past files it could not import. Same lifecycle as the
            // deletion warning: a notice about THIS scan, cleared by the next one.
            Event::TracksSkipped { root, groups } => {
                self.skip_notice = Some((root.clone(), groups.clone()));
            }
        }
    }
}

/// Fold a `WatchUpdate` (from add/remove/set-folders) into UI state: refresh the
/// library + Sources list. Playback is deliberately left untouched — managing
/// what's watched is separate from what's playing (the host's own contract).
pub(crate) fn apply_watch(s: &mut State, up: WatchUpdate) {
    s.library = up.views;
    s.sources = up.sources;
    // Clear any stale deletion warning; this scan's own `TracksRemoved` event (if
    // any) is applied right after and re-sets it, so the notice always reflects the
    // most recent scan rather than lingering.
    s.removal_notice = None;
    // Same for the skip notice: this scan's own `TracksSkipped` (if any) lands
    // right after and re-sets it, so a format fixed since the last scan stops
    // being reported rather than lingering forever.
    s.skip_notice = None;
    // A WatchUpdate marks a scan complete — retire the title-bar activity widget.
    s.scan_progress = None;
}

/// Apply a `WatchUpdate` and persist the new source set (with each source's
/// enabled flag) — used by the Sources add/remove/enable handlers. Kept separate
/// from the pure `apply_watch` so unit tests never touch the user's config on disk.
pub(crate) fn apply_and_save(s: &mut State, up: WatchUpdate) {
    apply_watch(s, up);
    let entries: Vec<(PathBuf, bool)> = s
        .sources
        .iter()
        .map(|x| (x.path.clone(), x.enabled))
        .collect();
    config::save_sources_enabled(&entries);
    // A Sources mutation just re-scanned (the host refreshed its per-source
    // caches); persist them so the next launch stays warm, with this add/remove/
    // toggle already folded in.
    config::save_scan_caches(&s.host.caches_snapshot());
}

/// The restored library's first scan landed: apply it like any Sources edit, then open any
/// files the OS handed us while it ran.
pub(crate) fn apply_startup_scan(s: &mut State, up: WatchUpdate) {
    apply_and_save(s, up);
    tracing::debug!(
        library = s.library.len(),
        queued = s.pending_open.len(),
        "startup scan landed"
    );
    s.startup_scan_pending = false;
    // Pre-warm the audio pipeline now that a library exists: spawn the actor so the output
    // device opens in the BACKGROUND (the device open is seconds on a slow WASAPI/Bluetooth
    // endpoint), instead of making the user's FIRST play wait the whole open. Non-blocking —
    // it just spawns the pump thread; the open happens there, off the UI thread. No-op if the
    // library is empty (nothing to play yet).
    if !s.library.is_empty() {
        s.host.prewarm();
    }
    let waiting = std::mem::take(&mut s.pending_open);
    if !waiting.is_empty() {
        open_files::open(s, waiting);
    }
}

/// Run a library-rescanning host op **off the UI thread**, then apply its result
/// back on the UI thread. The old synchronous Sources handlers ran the whole scan
/// inline, freezing the window; this keeps the UI responsive AND lets the title-bar
/// activity widget animate — the host broadcasts scan progress from the scanning
/// thread, and `on_scan_progress` marshals it onto the UI. Falls back to a
/// synchronous call when there is no `app_handle` (headless tests), so behaviour
/// there is unchanged.
pub(crate) fn scan_in_background(
    s: &mut State,
    op: impl FnOnce(&PlayerHandle) -> anyhow::Result<WatchUpdate> + Send + 'static,
    apply: fn(&mut State, WatchUpdate),
) {
    match s.app_handle.clone() {
        Some(handle) => {
            let host = s.host.clone();
            std::thread::spawn(move || {
                if let Ok(up) = op(&host) {
                    handle.update(move |s: &mut State, _tx: &mut Tx| apply(s, up));
                }
            });
        }
        None => {
            if let Ok(up) = op(&s.host) {
                apply(s, up);
            }
        }
    }
}

/// Resolve the effective dark/light mode from the user's preference.
pub(crate) fn is_dark(pref: ThemePref, system_dark: bool) -> bool {
    match pref {
        ThemePref::System => system_dark,
        ThemePref::Light => false,
        ThemePref::Dark => true,
    }
}

/// Set + persist the theme preference (used by the Appearance settings buttons).
pub(crate) fn set_theme_pref(s: &mut State, pref: ThemePref) {
    s.theme_pref = pref;
    config::save_theme_pref(pref.as_config());
}

/// Set + persist the album-year source (used by the Appearance settings buttons).
pub(crate) fn set_album_year(s: &mut State, src: config::AlbumYearSource) {
    s.album_year = src;
    config::save_album_year(src);
}

/// Set + persist whether the library shows the per-track loudness columns (DR + LUFS),
/// used by the Appearance settings toggle. Hiding them also clears an active loudness
/// sort and reverts to the baked axis order: the DR/Loudness pills vanish with the
/// columns, so the list must not keep sorting by now-hidden data.
pub(crate) fn set_show_loudness_columns(s: &mut State, show: bool) {
    s.show_loudness_columns = show;
    config::save_show_loudness_columns(show);
    if !show && s.loudness_sort.is_some() {
        s.loudness_sort = None;
        s.library = s.host.views_by(s.sort_axis);
    }
}

/// Set + persist the library indexing-speed mode and apply it to the host (takes
/// effect on the next scan). Used by the Sources settings control.
pub(crate) fn set_indexing_mode(s: &mut State, mode: IndexingMode) {
    s.indexing_mode = mode;
    s.host.set_indexing_mode(mode);
    config::save_indexing_mode(mode.as_config());
}

/// Set + persist the background loudness-analysis mode and apply it to the host (Off stops
/// the sweep; Throttled yields to playback; Full runs flat-out). Used by the Settings control.
pub(crate) fn set_loudness_analysis_mode(s: &mut State, mode: LoudnessAnalysisMode) {
    s.loudness_mode = mode;
    s.host.set_loudness_analysis_mode(mode);
    config::save_loudness_analysis(mode.as_config());
}

/// Switch the active Settings category (ephemeral — not persisted). Used by the
/// settings page's category rail. Switching TO Music Data (where the Sources
/// card — and its remote health/telemetry — lives) also kicks an on-demand
/// [`refresh_remote_health`], so health isn't stale the first time this
/// session's Sources panel is seen; the periodic poller alone could be up to
/// its whole interval late.
pub(crate) fn set_settings_tab(s: &mut State, tab: SettingsTab) {
    s.settings_tab = tab;
    if tab == SettingsTab::MusicData {
        refresh_remote_health(s);
    }
}

/// How often the periodic poller (spawned in `main`) re-probes every remote
/// source's health. A tradeoff between freshness and not hammering a remote
/// with pings — the program spec calls for "~30 s".
const REMOTE_HEALTH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// How often the device-refresh loop (spawned in `main`) re-enumerates the output devices so
/// the picker reflects a plugged/unplugged device. Gentle enough not to hammer the audio stack,
/// frequent enough that an unplug updates the list within a few seconds. Self-paced (the loop
/// sleeps this long AFTER each enumeration), so a slow cpal enumeration can never overlap itself
/// — and the gap keeps the background enumeration from contending with a live device-open (cpal's
/// WASAPI enumeration can be multi-second on a Bluetooth-heavy system).
const OUTPUT_DEVICE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Refresh every remote source's health off the UI thread. Mirrors
/// [`scan_in_background`]'s shape (and the `connect_remote_source` thread): the
/// network probe — [`PlayerHandle::refresh_remote_health`], one blocking `GET
/// /v0/ping` per remote — runs on a spawned background thread; its fresh
/// [`PlayerHandle::sources`] folds back onto `State::sources` via
/// `AppHandle::update`. Guarded by `remote_health_refreshing` so the periodic
/// poller (spawned in `main`) and an on-demand refresh (opening Settings →
/// Music Data, see [`set_settings_tab`]) can never have two probes in flight
/// at once, racing which result lands last. A no-op while one is already
/// running. Falls back to an inline call with no `app_handle` (headless tests).
pub(crate) fn refresh_remote_health(s: &mut State) {
    if s.remote_health_refreshing {
        return;
    }
    s.remote_health_refreshing = true;
    match s.app_handle.clone() {
        Some(handle) => {
            let host = s.host.clone();
            std::thread::spawn(move || {
                let sources = host.refresh_remote_health();
                handle.update(move |s: &mut State, _tx: &mut Tx| {
                    seed_synced_baseline(s, &sources);
                    s.sources = sources;
                    s.remote_health_refreshing = false;
                    // This probe may have seen a remote's catalog advance past
                    // our last sync — auto-pull those so the track list stays
                    // current without the user pressing Sync-now (the periodic
                    // "rescan the server" behaviour).
                    for url in remotes_needing_sync(s) {
                        reconnect_remote_now(s, url);
                    }
                });
            });
        }
        None => {
            let sources = s.host.refresh_remote_health();
            seed_synced_baseline(s, &sources);
            s.sources = sources;
            s.remote_health_refreshing = false;
            for url in remotes_needing_sync(s) {
                reconnect_remote_now(s, url);
            }
        }
    }
}

/// Seed `State::remote_synced_index` for any remote whose health has never
/// been read before (the Inc 6 "update available" cue's baseline): the
/// first time a remote's `index_version` is seen, it's recorded as
/// "synced" so the cue only fires on a REAL change afterward. Never
/// overwrites an existing entry — only an explicit sync
/// ([`record_synced_index`], after a connect/reconnect) does that; a bare
/// health probe (this function's only caller, [`refresh_remote_health`])
/// must never reset — let alone hide — a real pending update.
fn seed_synced_baseline(s: &mut State, sources: &[SourceStatus]) {
    for st in sources {
        if let SourceKind::Remote { url } = &st.kind
            && let Some(v) = st.remote.as_ref().and_then(|h| h.index_version)
        {
            s.remote_synced_index.entry(url.clone()).or_insert(v);
        }
    }
}

/// Record that remote `url` was just synced at its freshest known index
/// version, read from `sources` (the just-refreshed snapshot the caller's
/// sync produced) — the Inc 6 "update available" cue's baseline. A no-op if
/// `url` isn't in `sources` (it didn't come back online) or its health
/// hasn't reported a version yet. Unlike [`seed_synced_baseline`]'s
/// `or_insert`, this OVERWRITES any previous entry unconditionally: a sync
/// action, unlike a bare health probe, really did just catch this remote up.
fn record_synced_index(s: &mut State, url: &str, sources: &[SourceStatus]) {
    let version = sources.iter().find_map(|st| match &st.kind {
        SourceKind::Remote { url: u } if u == url => {
            st.remote.as_ref().and_then(|h| h.index_version)
        }
        _ => None,
    });
    if let Some(v) = version {
        s.remote_synced_index.insert(url.to_string(), v);
    }
}

/// Select the connected remotes whose server-side catalog has advanced past
/// what we last synced — the SAME predicate the "update available" cue uses
/// (`views::settings::remote_update_available`): a live `index_version`
/// strictly greater than the recorded `remote_synced_index` baseline. These
/// are the remotes the periodic poller auto-pulls (via [`reconnect_remote_now`])
/// so a connected source's track list stays current without the user pressing
/// Sync-now. A disabled remote is skipped (its tracks are intentionally out of
/// the aggregate); so is one with no baseline yet ([`seed_synced_baseline`]
/// seeds it on first sight, so a brand-new remote never counts as "advanced"
/// until a REAL change afterward) or no live reading (offline — reconnecting
/// that is the offline row's job, not the version-advanced auto-sync's).
pub(crate) fn remotes_needing_sync(s: &State) -> Vec<String> {
    s.sources
        .iter()
        .filter(|st| st.enabled)
        .filter_map(|st| {
            let SourceKind::Remote { url } = &st.kind else {
                return None;
            };
            let live = st.remote.as_ref()?.index_version?;
            let synced = s.remote_synced_index.get(url).copied()?;
            (live > synced).then(|| url.clone())
        })
        .collect()
}

/// Every ENABLED remote source's url, in `sources` order — the set the Rescan
/// action force-resyncs (pull fresh from each server), independent of whether
/// a version change has been detected yet: a plain local-root reconcile
/// rebuilds remote tracks from already-fetched blobs and would miss
/// server-side additions, so Rescan re-fetches each remote's index too.
/// Reuses the same enabled-Remote filter as [`remotes_needing_sync`] without
/// that one's version gate; local roots and disabled remotes are excluded.
pub(crate) fn enabled_remote_urls(s: &State) -> Vec<String> {
    s.sources
        .iter()
        .filter(|st| st.enabled)
        .filter_map(|st| match &st.kind {
            SourceKind::Remote { url } => Some(url.clone()),
            SourceKind::Local { .. } => None,
        })
        .collect()
}

/// Reconnect every persisted remote source at launch (Inc 5): off the UI
/// thread, for each url `config::load_remotes()` returns, silently
/// re-authenticate with the OS-keychain-stored refresh token and re-sync its
/// catalog via [`PlayerHandle::reconnect_remote_source`], then fold the
/// refreshed library + source list back onto the UI thread — mirroring
/// re-authenticate with the OS-keychain-stored refresh token and re-sync its
/// catalog via [`PlayerHandle::reconnect_remote_source`], then fold the
/// refreshed library + source list back onto the UI thread — mirroring
/// [`scan_in_background`]'s / [`refresh_remote_health`]'s shape (a spawned
/// thread, `AppHandle::update` back). Never blocks app launch on the network:
/// this is called once the window is already up, exactly where the local
/// startup scan is kicked off.
///
/// A remote that fails to reconnect (offline, expired/revoked token, server
/// down) is simply skipped — never surfaced as an error, and never dropped
/// from the persisted config or the keychain, so it keeps retrying on the
/// next launch, and the Inc 2 health poller picks it back up once it
/// actually returns. A failed reconnect must NOT crash startup.
///
/// Also seeds `State::remote_urls` from the same `config::load_remotes()`
/// call (Inc 6) — so `views::settings::merged_source_rows` can render an
/// offline row, with a Reconnect action, for a persisted remote that just
/// failed above, instead of it silently vanishing from the Sources list.
/// This is the ONLY place `State::remote_urls` is seeded from disk —
/// `State::new()` itself never touches `config::load_remotes()`, so an
/// ordinary unit test that builds a `State` directly is never affected by
/// another test's leftover persisted remote.
pub(crate) fn reconnect_remotes_on_startup(s: &mut State) {
    let urls = config::load_remotes();
    s.remote_urls = urls.clone();
    if urls.is_empty() {
        return;
    }
    // A fresh `PlayerHandle` starts every remote enabled until told
    // otherwise, so a remote the user had disabled before the last quit must
    // have that flag re-applied once it reconnects — otherwise its tracks
    // would silently come back on every restart.
    let disabled = config::load_remotes_disabled();
    match s.app_handle.clone() {
        Some(handle) => {
            let host = s.host.clone();
            std::thread::spawn(move || {
                for url in &urls {
                    if let Err(err) = host.reconnect_remote_source(url, &KeyringStore) {
                        tracing::warn!(url = %url, error = %err, "remote source reconnect failed");
                    } else if disabled.iter().any(|u| u == url) {
                        let _ = host.set_remote_enabled(url, false);
                    }
                }
                let sources = host.refresh_remote_health();
                let library = host.views();
                handle.update(move |s: &mut State, _tx: &mut Tx| {
                    seed_synced_baseline(s, &sources);
                    s.library = library;
                    s.sources = sources;
                });
            });
        }
        None => {
            for url in &urls {
                if let Err(err) = s.host.reconnect_remote_source(url, &KeyringStore) {
                    tracing::warn!(url, error = %err, "remote source reconnect failed");
                } else if disabled.iter().any(|u| u == url) {
                    let _ = s.host.set_remote_enabled(url, false);
                }
            }
            let sources = s.host.refresh_remote_health();
            seed_synced_baseline(s, &sources);
            s.sources = sources;
            s.library = s.host.views();
        }
    }
}

/// Open the dropdown with this key, or close it if it is already open (clicking
/// the trigger again). Only one dropdown is open at a time.
pub(crate) fn toggle_dropdown(s: &mut State, key: &str) {
    s.open_dropdown = if s.open_dropdown.as_deref() == Some(key) {
        None
    } else {
        Some(key.to_string())
    };
}

/// Close any open dropdown (an outside click, or after a choice is made).
pub(crate) fn close_dropdown(s: &mut State) {
    s.open_dropdown = None;
}

// ---- Remote source connect form (Settings → Add Source → Remote server) ----

/// Open the Remote server connect form: close the add-source menu first (so
/// it isn't left open behind the form), then reset the URL buffer to empty,
/// focus it, and clear any stale error/connecting state. Mirrors
/// `start_dsp_rename`'s "always start from a clean buffer" shape.
pub(crate) fn open_remote_form(s: &mut State) {
    close_dropdown(s);
    s.remote_url_buf = rune::TextEdit::new("");
    s.remote_url_buf.focused = true;
    s.remote_error = None;
    s.remote_connecting = false;
    s.remote_form_open = true;
}

/// Close the connect form (Cancel, Escape, an outside press, or a successful
/// connect) and clear its buffer + error so the next open starts fresh. A
/// connect thread already spawned by `connect_remote_source` keeps running —
/// there is no cancelling the browser round-trip — its result still lands via
/// `apply_remote_connect` once it finishes, just onto a form that (if
/// reopened) has moved on.
pub(crate) fn close_remote_form(s: &mut State) {
    s.remote_form_open = false;
    s.remote_url_buf = rune::TextEdit::new("");
    s.remote_error = None;
}

/// Read the connect form's URL and start a login against it **off the UI
/// thread**: `add_remote_source` opens the system browser and blocks on the
/// OAuth round-trip (the user signing in there, then the redirect back), so
/// running it inline would freeze the window for as long as that takes. This
/// spawns a background thread and folds the result back onto the UI thread
/// via `AppHandle::update` — the same shape `scan_in_background` uses for the
/// Sources scans — falling back to an inline call when there's no
/// `app_handle` (headless tests), so behaviour there is unchanged.
///
/// While the thread is in flight, `remote_connecting` is `true`: the form
/// disables the Connect button and shows a "waiting for your browser" status
/// line instead. A press while already connecting is a no-op, so double
/// presses (or a stray Enter in the URL field) can't start a second attempt.
pub(crate) fn connect_remote_source(s: &mut State) {
    if s.remote_connecting {
        return;
    }
    let url = s.remote_url_buf.text.trim().to_string();
    s.remote_connecting = true;
    s.remote_error = None;
    match s.app_handle.clone() {
        Some(handle) => {
            let host = s.host.clone();
            let url_for_result = url.clone();
            std::thread::spawn(move || {
                let result = host.add_remote_source(&url);
                handle.update(move |s: &mut State, _tx: &mut Tx| {
                    apply_remote_connect(s, &url_for_result, result)
                });
            });
        }
        None => {
            let result = s.host.add_remote_source(&url);
            apply_remote_connect(s, &url, result);
        }
    }
}

/// Fold a finished connect attempt's result back onto the UI thread (see
/// `connect_remote_source`). On success, the library AND the Sources list are
/// refreshed from the host (the remote-aware aggregate is already swapped
/// in — refreshing `sources` too, not just `library`, means the new remote's
/// row shows up immediately instead of waiting for the next health-poller
/// tick or a Settings tab switch), the url is persisted (`config::save_remote`,
/// mirrored into `State::remote_urls`) so it reconnects on the next launch —
/// the OAuth refresh token itself is already durable, written to the OS
/// keychain by `PlayerHandle::add_remote_source` — and the form closes. On
/// failure, the message is kept in `remote_error` and the form (if still
/// open) stays open with its field intact so the user can fix a typo and
/// retry; nothing is persisted for a failed attempt.
fn apply_remote_connect(s: &mut State, url: &str, result: anyhow::Result<()>) {
    s.remote_connecting = false;
    match result {
        Ok(()) => {
            s.library = s.host.views();
            s.sources = s.host.sources();
            if !s.remote_urls.iter().any(|u| u == url) {
                s.remote_urls.push(url.to_string());
            }
            config::save_remote(url);
            close_remote_form(s);
            // A fresh connect leaves the new remote's health UNPROBED (adding a
            // source doesn't ping it), so `sources()` above reports it offline
            // until the next 30s poll — flashing the title-bar indicator red for
            // a remote that just connected fine. Kick an immediate health probe
            // so it reads online within a round-trip instead. Guarded + off the
            // UI thread (see `refresh_remote_health`); a no-op if one's running.
            refresh_remote_health(s);
        }
        Err(err) => {
            s.remote_error = Some(err.to_string());
        }
    }
}

/// Disconnect a remote source — the Disconnect control on a Sources row,
/// connected or offline (Inc 6): drops it from the host's aggregate
/// ([`PlayerHandle::remove_remote_source`]), the persisted config
/// (`config::remove_remote`), and the OS keychain's stored refresh token
/// ([`TokenStore::delete_refresh`]) — the remote counterpart of a local
/// row's ✕ ([`PlayerHandle::remove_folder`]), just spread across the three
/// places a remote's state lives instead of one `set_folders` call. Also
/// drops `url` from the in-memory `remote_urls`/`remote_reconnecting`/
/// `remote_synced_index` bookkeeping so a removed remote doesn't linger as
/// a phantom offline row or a stale update-available cue.
///
/// Off the UI thread, like every other remote op: none of the three calls
/// is scan-sized, but the keychain hit still touches the OS. Falls back to
/// an inline call with no `app_handle` (headless tests).
pub(crate) fn disconnect_remote_source(s: &mut State, url: String) {
    s.remote_urls.retain(|u| u != &url);
    s.remote_reconnecting.retain(|u| u != &url);
    s.remote_synced_index.remove(&url);
    match s.app_handle.clone() {
        Some(handle) => {
            let host = s.host.clone();
            std::thread::spawn(move || {
                host.remove_remote_source(&url);
                config::remove_remote(&url);
                if let Err(err) = KeyringStore.delete_refresh(&url) {
                    tracing::warn!(url = %url, error = %err, "failed to delete keychain entry for a disconnected remote");
                }
                let sources = host.sources();
                let library = host.views();
                handle.update(move |s: &mut State, _tx: &mut Tx| {
                    s.sources = sources;
                    s.library = library;
                });
            });
        }
        None => {
            s.host.remove_remote_source(&url);
            config::remove_remote(&url);
            if let Err(err) = KeyringStore.delete_refresh(&url) {
                tracing::warn!(url = %url, error = %err, "failed to delete keychain entry for a disconnected remote");
            }
            s.sources = s.host.sources();
            s.library = s.host.views();
        }
    }
}

/// Toggle a remote source's enabled flag — the Sources row counterpart of a
/// local folder's enable/disable checkbox ([`PlayerHandle::set_folder_enabled`]
/// via `scan_in_background`/`apply_and_save`): flips
/// [`PlayerHandle::set_remote_enabled`] (the remote stays retained, but its
/// tracks drop out of the aggregate while disabled) and persists the flag
/// (`config::set_remote_enabled`), then refreshes `sources`/`library` so the
/// row's checkbox and the track list reflect it immediately.
///
/// Unlike every other remote op ([`disconnect_remote_source`],
/// [`reconnect_remote_now`], [`reconnect_remotes_on_startup`]), this runs
/// INLINE rather than on a background thread: `set_remote_enabled` touches
/// neither the network nor the OS keychain, only an in-memory flag plus a
/// rebuild from blobs already fetched — the same "local rebuild, no I/O"
/// case `set_folder_enabled` itself is, just without that one's rescan.
pub(crate) fn toggle_remote_enabled(s: &mut State, url: String, enabled: bool) {
    let _ = s.host.set_remote_enabled(&url, enabled);
    config::set_remote_enabled(&url, enabled);
    s.sources = s.host.sources();
    s.library = s.host.views();
}

/// Manually reconnect (or re-sync) one remote source — an offline row's
/// Reconnect action, and a connected row's "update available" Sync-now
/// action (Inc 6; both drive the exact same path: silently re-authenticate
/// with the stored refresh token and re-fetch the catalog index via
/// [`PlayerHandle::reconnect_remote_source`], which for an already-online
/// remote is exactly a resync). Off the UI thread, mirroring
/// [`reconnect_remotes_on_startup`]'s shape for one url on demand. Guarded
/// by `remote_reconnecting` so a double press (or the periodic poller
/// probing mid-attempt) can't race two reconnects for the same url; the row
/// shows a transient "reconnecting…" state meanwhile, cleared whichever way
/// the attempt ends.
///
/// On success the row flips online, `library`/`sources` refresh, and
/// [`record_synced_index`] updates the "last synced" index version so the
/// update-available cue clears. On failure — offline, expired/revoked
/// token, server down — it's logged and the row silently stays
/// offline/stale, mirroring the startup reconnect's "never surfaced as an
/// error" policy; the next press or the periodic poller can retry.
pub(crate) fn reconnect_remote_now(s: &mut State, url: String) {
    if s.remote_reconnecting.iter().any(|u| u == &url) {
        return;
    }
    s.remote_reconnecting.push(url.clone());
    match s.app_handle.clone() {
        Some(handle) => {
            let host = s.host.clone();
            std::thread::spawn(move || {
                let result = host.reconnect_remote_source(&url, &KeyringStore);
                if let Err(err) = &result {
                    tracing::warn!(url = %url, error = %err, "manual remote reconnect failed");
                }
                let ok = result.is_ok();
                let sources = host.sources();
                let library = host.views();
                handle.update(move |s: &mut State, _tx: &mut Tx| {
                    s.remote_reconnecting.retain(|u| u != &url);
                    if ok {
                        record_synced_index(s, &url, &sources);
                    }
                    s.sources = sources;
                    s.library = library;
                });
            });
        }
        None => {
            let result = s.host.reconnect_remote_source(&url, &KeyringStore);
            if let Err(err) = &result {
                tracing::warn!(url = %url, error = %err, "manual remote reconnect failed");
            }
            let ok = result.is_ok();
            let sources = s.host.sources();
            s.library = s.host.views();
            s.remote_reconnecting.retain(|u| u != &url);
            if ok {
                record_synced_index(s, &url, &sources);
            }
            s.sources = sources;
        }
    }
}

/// Re-run the interactive browser login for an ALREADY-configured remote `url`
/// — the escape hatch for when a silent [`reconnect_remote_now`] can't recover
/// it because the stored refresh token is dead (revoked, or orphaned by an
/// unclean exit mid-rotation): no amount of silent retrying will work, so the
/// only fix is a fresh sign-in. Unlike Reconnect, this opens the system browser
/// via [`PlayerHandle::add_remote_source`], which mints and persists a BRAND-NEW
/// refresh token (replacing the dead one) and re-fetches the catalog — turning
/// the old "remove the source, then add it again" dance into one click that
/// keeps the row, its position, and its enabled flag. Off the UI thread (the
/// browser round-trip blocks), guarded by `remote_reconnecting` — shared with
/// [`reconnect_remote_now`] — so it can't race a silent reconnect or a double
/// press. Reuses [`apply_remote_connect`]'s success tail (refresh
/// library/sources, persist the url, mirror into `remote_urls`); on failure the
/// row simply stays offline, same as a failed reconnect.
pub(crate) fn relogin_remote(s: &mut State, url: String) {
    if s.remote_reconnecting.iter().any(|u| u == &url) {
        return;
    }
    s.remote_reconnecting.push(url.clone());
    match s.app_handle.clone() {
        Some(handle) => {
            let host = s.host.clone();
            std::thread::spawn(move || {
                let result = host.add_remote_source(&url);
                handle.update(move |s: &mut State, _tx: &mut Tx| {
                    s.remote_reconnecting.retain(|u| u != &url);
                    apply_remote_connect(s, &url, result);
                });
            });
        }
        None => {
            let result = s.host.add_remote_source(&url);
            s.remote_reconnecting.retain(|u| u != &url);
            apply_remote_connect(s, &url, result);
        }
    }
}

/// Open/close the expanded signal-path panel (pressing the now-playing chain line).
pub(crate) fn toggle_signal_path(s: &mut State) {
    s.signal_path_open = !s.signal_path_open;
}

/// Close the signal-path panel (an outside press / Escape via `El::modal`).
pub(crate) fn close_signal_path(s: &mut State) {
    s.signal_path_open = false;
}

/// Open/close the title-bar remote-health popover (pressing the indicator).
pub(crate) fn toggle_remote_health_popover(s: &mut State) {
    s.remote_health_open = !s.remote_health_open;
}

/// Close the remote-health popover (an outside press / Escape via `El::modal`,
/// or its own "Open sources settings" action).
pub(crate) fn close_remote_health_popover(s: &mut State) {
    s.remote_health_open = false;
}

// ---- Add-to-playlist overlay (a track row's context menu) ----

/// Open the "Add to playlist" overlay for `track_id`: reset the inline
/// New-playlist buffer to empty (so a stale typed name never resurfaces),
/// then mark `track_id` as pending, which is what tells
/// `views::add_to_playlist::overlay` to render.
pub(crate) fn open_add_to_playlist(s: &mut State, track_id: u32) {
    s.add_to_playlist_buf = rune::TextEdit::new("");
    s.add_to_playlist_search = rune::TextEdit::new("");
    s.add_to_playlist_for = Some(track_id);
}

/// Close the overlay (a pick, a successful New-playlist create, Escape via
/// `El::modal`, or an outside press) and reset its buffer + pending track so
/// the next open always starts clean.
pub(crate) fn close_add_to_playlist(s: &mut State) {
    s.add_to_playlist_for = None;
    s.add_to_playlist_buf = rune::TextEdit::new("");
    s.add_to_playlist_search = rune::TextEdit::new("");
}

/// Close the "Edit Artwork" crop dialogue (Cancel, Escape / outside press via
/// `El::modal`, or after a confirmed crop), discarding the in-progress pan/zoom.
/// Mirrors [`close_add_to_playlist`].
pub(crate) fn close_artwork_crop(s: &mut State) {
    s.artwork_crop = None;
}

/// Close the "Edit Artwork" MANAGE dialogue (Cancel, Escape / outside press, or
/// after Change hands off to the crop step / Delete finishes).
pub(crate) fn close_artwork_edit(s: &mut State) {
    s.artwork_edit = None;
}

/// Choose + persist the output device (used by the Output settings buttons).
/// `None` means the system default. Updates the UI selection, writes the config,
/// and tells the host — which applies it to the NEXT playback session (a live
/// device switch mid-playback is a follow-up).
pub(crate) fn set_output_device(s: &mut State, name: Option<String>) {
    tracing::info!(
        device = name.as_deref().unwrap_or("system default"),
        "output device selected"
    );
    s.output_device = name.clone();
    config::save_output_device(s.output_device.as_deref());
    s.host.set_output_device(name);
}

/// Toggle + persist exclusive (bit-perfect) output (used by the Output settings
/// toggle). Updates the UI, writes the config, and tells the host — which applies
/// it to the NEXT playback session (the current stream keeps its mode until then).
/// If the device can't open exclusive, that session falls back to shared, shown
/// live in the signal path's Exclusive stage.
pub(crate) fn set_exclusive(s: &mut State, on: bool) {
    tracing::info!(exclusive = on, "exclusive output toggled");
    s.exclusive = on;
    config::save_output_exclusive(on);
    s.host.set_exclusive(on);
}

/// Toggle + persist **strict** exclusive (exclusive-or-stop). Like
/// [`set_exclusive`], it applies on the next playback session: with exclusive on and
/// strict on, that session won't downgrade to shared if exclusive can't open — it
/// stops instead (shown in the Exclusive card).
pub(crate) fn set_exclusive_strict(s: &mut State, on: bool) {
    tracing::info!(strict = on, "strict exclusive toggled");
    s.exclusive_strict = on;
    config::save_output_exclusive_strict(on);
    s.host.set_exclusive_strict(on);
}

/// Choose + persist the ReplayGain mode (used by the ReplayGain settings buttons).
/// Updates the UI selection, writes the config, and tells the host — which applies
/// it to the live actor immediately (and remembers it for the next session).
pub(crate) fn set_replaygain_mode(s: &mut State, mode: ReplayGainMode) {
    s.replaygain_mode = mode;
    config::save_replaygain(mode, s.replaygain_preamp);
    s.host.set_replay_gain(mode, s.replaygain_preamp);
}

/// Set + persist the ReplayGain preamp in dB (used by the preamp slider). Same
/// store-persist-tell-host path as [`set_replaygain_mode`].
pub(crate) fn set_replaygain_preamp(s: &mut State, preamp_db: f32) {
    s.replaygain_preamp = preamp_db;
    config::save_replaygain(s.replaygain_mode, preamp_db);
    s.host.set_replay_gain(s.replaygain_mode, preamp_db);
}

/// Persist the DSP chain and push the whole chain to the live stream (the device
/// sink reads it each block and bumps its own `generation`). Shared by every EQ
/// edit — the mutators write `dsp_units` directly, so this writes the live chain
/// back into the active profile (auto-save), then saves + pushes.
fn apply_dsp(s: &mut State) {
    // Write the live chain back into the active profile (auto-save), then persist + push.
    s.profile_set
        .set_active_chain(soul_player_host::DspChain::new(s.dsp_units.clone()));
    config::save_profiles(&s.profile_set);
    s.host.set_dsp_chain(s.dsp_units.clone());
}

/// The bands of the EQ unit at `unit` (empty if the index is out of range or the
/// unit isn't an EQ) — the graph/editor read through this.
pub(crate) fn unit_bands(s: &State, unit: usize) -> &[Band] {
    match s.dsp_units.get(unit).and_then(DspUnit::as_eq) {
        Some(eq) => &eq.bands,
        None => &[],
    }
}

/// The preamp (dB) of the EQ unit at `unit` (0 if out of range / not an EQ).
pub(crate) fn unit_preamp(s: &State, unit: usize) -> f32 {
    s.dsp_units
        .get(unit)
        .and_then(DspUnit::as_eq)
        .map_or(0.0, |eq| eq.preamp_db)
}

/// Whether the DSP unit at `unit` is enabled (false if the index is out of range).
pub(crate) fn unit_enabled(s: &State, unit: usize) -> bool {
    s.dsp_units.get(unit).is_some_and(DspUnit::enabled)
}

/// A mutable handle to band `band` of the EQ unit at `unit`, if both exist.
fn band_mut(s: &mut State, unit: usize, band: usize) -> Option<&mut Band> {
    s.dsp_units
        .get_mut(unit)
        .and_then(DspUnit::as_eq_mut)
        .and_then(|eq| eq.bands.get_mut(band))
}

/// Clear the transient editor selection + node drag — used when the chain changes
/// shape (add / remove / reorder), so no card points at a stale (unit, band). Also
/// drops any in-flight inline rename, whose unit index would otherwise dangle onto a
/// moved / removed card.
fn clear_eq_editor(s: &mut State) {
    s.eq_selected = None;
    s.eq_drag = None;
    s.dsp_rename = None;
}

/// Toggle whether unit `i`'s card is expanded (its body shown).
pub(crate) fn toggle_dsp_expanded(s: &mut State, i: usize) {
    if let Some(open) = s.dsp_expanded.get_mut(i) {
        *open = !*open;
    }
}

/// Toggle whether unit `i`'s band table is expanded (within its open card).
pub(crate) fn toggle_eq_bands_expanded(s: &mut State, i: usize) {
    if let Some(open) = s.eq_bands_expanded.get_mut(i) {
        *open = !*open;
    }
}

/// Append a new EQ unit (enabled, default bands) and open its card.
pub(crate) fn add_dsp_unit(s: &mut State) {
    s.dsp_units.push(DspUnit::Eq(EqUnit::default()));
    s.dsp_expanded.push(true);
    s.eq_bands_expanded.push(true);
    clear_eq_editor(s);
    apply_dsp(s);
}

/// Remove EQ unit `i`, dropping its expand flags with it.
pub(crate) fn remove_dsp_unit(s: &mut State, i: usize) {
    if i >= s.dsp_units.len() {
        return;
    }
    s.dsp_units.remove(i);
    if i < s.dsp_expanded.len() {
        s.dsp_expanded.remove(i);
    }
    if i < s.eq_bands_expanded.len() {
        s.eq_bands_expanded.remove(i);
    }
    clear_eq_editor(s);
    apply_dsp(s);
}

/// Enable/disable unit `i` from its own card header (any unit — there is no master
/// toggle, each card owns its enable).
pub(crate) fn set_dsp_unit_enabled(s: &mut State, i: usize, on: bool) {
    if let Some(unit) = s.dsp_units.get_mut(i) {
        unit.set_enabled(on);
    }
    apply_dsp(s);
}

/// Open the inline rename editor for unit `unit`'s title (double-click the title):
/// commits any already-open rename first, seeds the buffer from the unit's current
/// name (empty if unnamed), focuses it, and selects all so the first keystroke
/// replaces it. Mirrors [`eq_edit_start`] for the number editor.
pub(crate) fn start_dsp_rename(s: &mut State, unit: usize) {
    commit_dsp_rename(s);
    let current = s
        .dsp_units
        .get(unit)
        .and_then(DspUnit::name)
        .unwrap_or("")
        .to_string();
    s.dsp_rename_buf = rune::TextEdit::new(current);
    s.dsp_rename_buf.focused = true;
    s.dsp_rename_buf.anchor = 0; // select-all (caret at end, anchor at start) → typing replaces
    s.dsp_rename = Some(unit);
}

/// Commit the open rename: trim the buffer and write it as the unit's name (a blank
/// buffer clears the name, so the unit reverts to showing just its type). No-op when
/// nothing is being renamed. Mirrors [`commit_eq_edit`].
pub(crate) fn commit_dsp_rename(s: &mut State) {
    let Some(unit) = s.dsp_rename.take() else {
        return;
    };
    let name = s.dsp_rename_buf.text.trim().to_string();
    set_dsp_unit_name(s, unit, (!name.is_empty()).then_some(name));
}

/// Set (or clear, with `None`) unit `unit`'s user-given name, then persist + push the
/// chain. The name lives inside `dsp_units[unit]`, so reorder/remove carry it for free.
pub(crate) fn set_dsp_unit_name(s: &mut State, unit: usize, name: Option<String>) {
    if let Some(u) = s.dsp_units.get_mut(unit) {
        u.set_name(name);
    }
    apply_dsp(s);
}

/// Move unit `from` to position `to` (the card drag-reorder), carrying its expand
/// flag along so the card stays open/closed as it moves.
pub(crate) fn reorder_dsp_units(s: &mut State, from: usize, to: usize) {
    let n = s.dsp_units.len();
    if from >= n || to >= n || from == to {
        return;
    }
    let unit = s.dsp_units.remove(from);
    s.dsp_units.insert(to, unit);
    if from < s.dsp_expanded.len() {
        let open = s.dsp_expanded.remove(from);
        s.dsp_expanded.insert(to.min(s.dsp_expanded.len()), open);
    }
    if from < s.eq_bands_expanded.len() {
        let open = s.eq_bands_expanded.remove(from);
        s.eq_bands_expanded
            .insert(to.min(s.eq_bands_expanded.len()), open);
    }
    clear_eq_editor(s);
    apply_dsp(s);
}

// ---- Audio profiles (named snapshots of the whole DSP chain) ----
//
// The set, its switching and the unique-naming live in `soul_player_host::ProfileSet`
// (the shared `soul-audio-dsp` crate, reused by the DAP). These wrappers delegate to it,
// then refresh the UI + persist + push.

/// Point the live UI at the active profile: load its chain into `dsp_units`, reset the
/// expand flags (first card open), and clear the EQ editor. Shared by every profile
/// switch / create / delete.
fn load_active_profile_into_ui(s: &mut State) {
    s.dsp_units = s
        .profile_set
        .active_chain()
        .map(|c| c.units.clone())
        .unwrap_or_default();
    s.dsp_expanded = (0..s.dsp_units.len()).map(|k| k == 0).collect();
    s.eq_bands_expanded = vec![true; s.dsp_units.len()];
    clear_eq_editor(s);
}

/// Make profile `i` active: reload the live chain from it and push. The previously
/// active profile was already auto-saved by edits, so this just swaps. No-op when `i`
/// is already active or out of range.
pub(crate) fn switch_profile(s: &mut State, i: usize) {
    commit_profile_rename(s);
    if !s.profile_set.switch(i) {
        return;
    }
    load_active_profile_into_ui(s);
    apply_dsp(s);
}

/// Append a new profile with an empty chain and a unique default name, and switch to it.
pub(crate) fn new_profile(s: &mut State) {
    commit_profile_rename(s);
    s.profile_set.add_empty("New profile");
    load_active_profile_into_ui(s);
    apply_dsp(s);
}

/// Duplicate the active profile (its chain) under `"<name> copy"` and switch to the copy.
pub(crate) fn duplicate_profile(s: &mut State) {
    commit_profile_rename(s);
    let base = match s.profile_set.active() {
        Some(p) => format!("{} copy", p.name),
        None => return,
    };
    s.profile_set.duplicate_active(&base);
    load_active_profile_into_ui(s);
    apply_dsp(s);
}

/// Delete the active profile and switch to a neighbour. No-op when only one profile
/// remains — there must always be at least one.
pub(crate) fn delete_profile(s: &mut State) {
    commit_profile_rename(s);
    if !s.profile_set.remove_active() {
        return;
    }
    load_active_profile_into_ui(s);
    apply_dsp(s);
}

/// Set profile `i`'s name, disambiguated against the others, and persist.
pub(crate) fn rename_profile(s: &mut State, i: usize, name: String) {
    if s.profile_set.rename(i, &name) {
        config::save_profiles(&s.profile_set);
    }
}

/// Open the inline rename editor over the switcher for the active profile (seed the
/// buffer from its name, focus + select-all). Mirrors [`start_dsp_rename`].
pub(crate) fn start_profile_rename(s: &mut State) {
    commit_profile_rename(s);
    let current = s
        .profile_set
        .active()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    s.profile_rename_buf = rune::TextEdit::new(current);
    s.profile_rename_buf.focused = true;
    s.profile_rename_buf.anchor = 0;
    s.profile_rename = Some(s.profile_set.active_index());
}

/// Commit the open profile rename: trim the buffer and apply it. A blank buffer keeps
/// the old name (a profile is never nameless). No-op when nothing is being renamed.
pub(crate) fn commit_profile_rename(s: &mut State) {
    let Some(i) = s.profile_rename.take() else {
        return;
    };
    let name = s.profile_rename_buf.text.trim().to_string();
    if !name.is_empty() {
        rename_profile(s, i, name);
    }
}

/// Which numeric column of a band the table's Ableton-style number boxes target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NumField {
    Freq,
    Gain,
    Q,
}

/// Drag sensitivity: a 0.1 dB gain step per vertical point dragged.
const GAIN_DRAG_DB_PER_PX: f32 = 0.1;
/// Drag sensitivity: dragging 48 points shifts the frequency one octave.
const FREQ_DRAG_PX_PER_OCTAVE: f32 = 48.0;
/// Drag sensitivity: dragging 90 points doubles (or halves) Q.
const Q_DRAG_PX_PER_DOUBLE: f32 = 90.0;
/// Wheel step: one notch nudges gain 0.5 dB.
const GAIN_SCROLL_DB: f32 = 0.5;
/// Wheel step: one notch shifts the frequency a sixth of an octave (~12%, ≈2^(1/6)).
const FREQ_SCROLL_STEP: f32 = 1.1225;
/// Fine wheel step (Shift held): a 24th of an octave (~3%, ≈2^(1/24)).
const FREQ_SCROLL_STEP_FINE: f32 = 1.0293;

/// Read a band's `field` value (frequency Hz, gain dB, or Q).
fn band_field(b: &Band, field: NumField) -> f32 {
    match field {
        NumField::Freq => b.freq,
        NumField::Gain => b.gain_db,
        NumField::Q => b.q,
    }
}

/// Apply a value to unit `unit`'s band `band` `field` through the right clamped setter.
fn set_eq_band_field(s: &mut State, unit: usize, band: usize, field: NumField, value: f32) {
    match field {
        NumField::Freq => set_eq_band_freq(s, unit, band, value),
        NumField::Gain => set_eq_band_gain(s, unit, band, value),
        NumField::Q => set_eq_band_q(s, unit, band, value),
    }
}

/// The raw text shown when a number opens for editing (no unit suffix): frequency
/// as whole Hz, gain to 0.1 dB, Q to 0.01.
fn edit_text_for(b: &Band, field: NumField) -> String {
    match field {
        NumField::Freq => format!("{:.0}", b.freq),
        NumField::Gain => format!("{:.1}", b.gain_db),
        NumField::Q => format!("{:.2}", b.q),
    }
}

/// Set unit `unit`'s band `band` frequency in Hz (clamped), applying live.
pub(crate) fn set_eq_band_freq(s: &mut State, unit: usize, band: usize, hz: f32) {
    if let Some(slot) = band_mut(s, unit, band) {
        slot.freq = hz.clamp(FREQ_MIN, FREQ_MAX);
    }
    apply_dsp(s);
}

/// Set unit `unit`'s band `band` gain in dB (clamped + rounded to 0.1), applying live.
pub(crate) fn set_eq_band_gain(s: &mut State, unit: usize, band: usize, db: f32) {
    if let Some(slot) = band_mut(s, unit, band) {
        slot.gain_db = (db.clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB) * 10.0).round() / 10.0;
    }
    apply_dsp(s);
}

/// Begin an Ableton-style vertical drag on band `i`'s `field`: caches the value at
/// the press so the drag applies absolutely, selects the band, and closes any open
/// inline edit.
pub(crate) fn eq_num_drag_start(s: &mut State, unit: usize, band: usize, field: NumField) {
    commit_eq_edit(s);
    if let Some(b) = unit_bands(s, unit).get(band).copied() {
        s.eq_selected = Some((unit, band));
        s.eq_num_drag = Some((unit, band, field, band_field(&b, field)));
    }
}

/// How much finer a Shift-held number drag is than a plain one (a quarter the
/// travel per point).
const DRAG_FINE_FACTOR: f32 = 0.25;

/// Continue a number drag: `dy` is the pointer's vertical travel since the press
/// (down is positive). Up raises the value; gain is linear, frequency and Q are
/// geometric (constant octaves / doublings per point). Holding Shift fine-tunes by
/// scaling the travel down.
pub(crate) fn eq_num_drag_move(s: &mut State, dy: f32) {
    let Some((unit, band, field, start)) = s.eq_num_drag else {
        return;
    };
    let dy = if s.shift_held {
        dy * DRAG_FINE_FACTOR
    } else {
        dy
    };
    let value = match field {
        NumField::Gain => start - dy * GAIN_DRAG_DB_PER_PX,
        NumField::Freq => start * 2.0f32.powf(-dy / FREQ_DRAG_PX_PER_OCTAVE),
        NumField::Q => start * 2.0f32.powf(-dy / Q_DRAG_PX_PER_DOUBLE),
    };
    set_eq_band_field(s, unit, band, field, value);
}

/// End a number drag.
pub(crate) fn eq_num_drag_end(s: &mut State) {
    s.eq_num_drag = None;
}

/// Nudge band `i`'s `field` one wheel notch (`up` = scroll up = increase), closing
/// any open inline edit. `fine` (Shift held) takes a smaller step for precise tuning.
/// Gain steps linearly; frequency and Q step geometrically.
pub(crate) fn eq_num_scroll(
    s: &mut State,
    unit: usize,
    band: usize,
    field: NumField,
    up: bool,
    fine: bool,
) {
    commit_eq_edit(s);
    let Some(b) = unit_bands(s, unit).get(band).copied() else {
        return;
    };
    s.eq_selected = Some((unit, band));
    let cur = band_field(&b, field);
    let value = match field {
        NumField::Gain => {
            let step = if fine { 0.1 } else { GAIN_SCROLL_DB };
            cur + if up { step } else { -step }
        }
        NumField::Freq => {
            let factor = if fine {
                FREQ_SCROLL_STEP_FINE
            } else {
                FREQ_SCROLL_STEP
            };
            cur * if up { factor } else { 1.0 / factor }
        }
        NumField::Q => {
            let factor = if fine { 1.03 } else { 1.15 };
            cur * if up { factor } else { 1.0 / factor }
        }
    };
    set_eq_band_field(s, unit, band, field, value);
}

/// Open the inline editor for band `i`'s `field` (double-click a value): seeds the
/// buffer with the current value, focuses it, and selects all so the first keystroke
/// replaces it. Commits any already-open edit first.
pub(crate) fn eq_edit_start(s: &mut State, unit: usize, band: usize, field: NumField) {
    commit_eq_edit(s);
    let Some(b) = unit_bands(s, unit).get(band).copied() else {
        return;
    };
    s.eq_selected = Some((unit, band));
    s.eq_edit_buf = rune::TextEdit::new(edit_text_for(&b, field));
    s.eq_edit_buf.focused = true;
    s.eq_edit_buf.anchor = 0; // select-all (caret at end, anchor at start) → typing replaces
    s.eq_edit = Some((unit, band, field));
}

/// Commit the open inline edit: parse the buffer and, if it is a number, apply it to
/// the band's field. A blank or unparseable buffer just closes the editor unchanged.
/// No-op when nothing is being edited.
pub(crate) fn commit_eq_edit(s: &mut State) {
    let Some((unit, band, field)) = s.eq_edit.take() else {
        return;
    };
    if let Ok(value) = s.eq_edit_buf.text.trim().parse::<f32>() {
        set_eq_band_field(s, unit, band, field, value);
    }
}

/// Clamp a band's parameters into the UI's legal ranges (freq/gain/Q), rounding
/// gain to 0.1 dB so the readout and the drag agree.
fn clamp_band(mut b: Band) -> Band {
    b.freq = b.freq.clamp(FREQ_MIN, FREQ_MAX);
    b.gain_db = (b.gain_db.clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB) * 10.0).round() / 10.0;
    b.q = b.q.clamp(Q_MIN, Q_MAX);
    b
}

/// Replace unit `unit`'s band `band` wholesale (the graph drag computes frequency +
/// gain together), clamped to the legal ranges.
pub(crate) fn set_eq_band(s: &mut State, unit: usize, band: usize, value: Band) {
    if let Some(slot) = band_mut(s, unit, band) {
        *slot = clamp_band(value);
    }
    apply_dsp(s);
}

/// Set unit `unit`'s band `band` Q (the scroll-wheel / curvature gesture).
pub(crate) fn set_eq_band_q(s: &mut State, unit: usize, band: usize, q: f32) {
    if let Some(slot) = band_mut(s, unit, band) {
        slot.q = q.clamp(Q_MIN, Q_MAX);
    }
    apply_dsp(s);
}

/// Cycle unit `unit`'s band `band` to the next filter shape (click the handle's type chip).
pub(crate) fn cycle_eq_band_kind(s: &mut State, unit: usize, band: usize) {
    if let Some(slot) = band_mut(s, unit, band) {
        slot.kind = match slot.kind {
            BandKind::LowShelf => BandKind::Peaking,
            BandKind::Peaking => BandKind::HighShelf,
            BandKind::HighShelf => BandKind::LowCut,
            BandKind::LowCut => BandKind::HighCut,
            BandKind::HighCut => BandKind::Notch,
            BandKind::Notch => BandKind::LowShelf,
        };
    }
    apply_dsp(s);
}

/// Enable/disable a single band of unit `unit` (its handle's bypass toggle).
pub(crate) fn set_eq_band_enabled(s: &mut State, unit: usize, band: usize, on: bool) {
    if let Some(slot) = band_mut(s, unit, band) {
        slot.enabled = on;
    }
    apply_dsp(s);
}

/// Add a new bell band at `freq` Hz / `gain_db` dB to unit `unit` (from a
/// double-click on the graph, or the editor's Add button). Returns the new band's
/// index, or `None` if the cap is reached (or `unit` isn't an EQ). Selects the new band.
pub(crate) fn add_eq_band(s: &mut State, unit: usize, freq: f32, gain_db: f32) -> Option<usize> {
    let bands = &mut s
        .dsp_units
        .get_mut(unit)
        .and_then(DspUnit::as_eq_mut)?
        .bands;
    if bands.len() >= MAX_BANDS {
        return None;
    }
    let new = clamp_band(Band {
        kind: BandKind::Peaking,
        freq,
        gain_db,
        q: 1.0,
        enabled: true,
    });
    let i = bands.len();
    bands.push(new);
    s.eq_selected = Some((unit, i));
    apply_dsp(s);
    Some(i)
}

/// Remove unit `unit`'s band `band` (a double-click on its handle, or the editor's
/// remove button).
pub(crate) fn remove_eq_band(s: &mut State, unit: usize, band: usize) {
    let bands = match s.dsp_units.get_mut(unit).and_then(DspUnit::as_eq_mut) {
        Some(eq) => &mut eq.bands,
        None => return,
    };
    if band >= bands.len() {
        return;
    }
    bands.remove(band);
    // Keep the selection/drag valid after the shift.
    s.eq_selected = None;
    s.eq_drag = None;
    apply_dsp(s);
}

/// Import an AutoEQ / Equalizer-APO ParametricEQ profile from `path` into unit
/// `unit`, replacing its bands + preamp and enabling it. No-op if the file is
/// unreadable, has no usable filters, or `unit` isn't an EQ.
pub(crate) fn import_eq(s: &mut State, unit: usize, path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let (preamp, bands) = soul_player_host::parse_autoeq(&text);
    if bands.is_empty() {
        return;
    }
    // Import replaces this unit's bands + preamp and enables it.
    if let Some(eq) = s.dsp_units.get_mut(unit).and_then(DspUnit::as_eq_mut) {
        eq.bands = bands.into_iter().take(MAX_BANDS).map(clamp_band).collect();
        eq.preamp_db = ((preamp.clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB)) * 10.0).round() / 10.0;
        eq.enabled = true;
    }
    s.eq_selected = None;
    apply_dsp(s);
}

/// Export EQ unit `unit` to `path` as an AutoEQ / Equalizer-APO ParametricEQ
/// profile (shareable, re-importable here and in Equalizer APO / Peace).
pub(crate) fn export_eq(s: &State, unit: usize, path: &std::path::Path) {
    let Some(eq) = s.dsp_units.get(unit).and_then(DspUnit::as_eq) else {
        return;
    };
    let text = soul_player_host::format_autoeq(eq.preamp_db, &eq.bands);
    let _ = std::fs::write(path, text);
}

/// Export the active audio profile (its name + whole DSP chain) to `path` as the
/// portable JSON the future DAP reads. No-op if serialization or the write fails.
pub(crate) fn export_profile(s: &State, path: &std::path::Path) {
    let Some(profile) = s.profile_set.active() else {
        return;
    };
    if let Ok(json) = config::profile_to_json(profile) {
        let _ = std::fs::write(path, json);
    }
}

/// Import an audio profile from `path` (the portable JSON), appending it as a new
/// profile (its name disambiguated against the others) and switching to it. No-op if
/// the file is unreadable or not a valid profile.
pub(crate) fn import_profile(s: &mut State, path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(profile) = config::profile_from_json(&text) else {
        return;
    };
    // Append under a disambiguated name and switch to it (ProfileSet unique-ifies).
    s.profile_set.add_and_select(&profile.name, profile.chain);
    load_active_profile_into_ui(s);
    apply_dsp(s);
}

/// Set unit `unit`'s preamp in dB.
pub(crate) fn set_eq_preamp(s: &mut State, unit: usize, preamp_db: f32) {
    if let Some(eq) = s.dsp_units.get_mut(unit).and_then(DspUnit::as_eq_mut) {
        eq.preamp_db = preamp_db;
    }
    apply_dsp(s);
}

/// Per-frame poll (wired as `AppSpec::poll`). Drives the title-bar scan-activity
/// pulse (while a scan runs, or a Sources row is showing its "Connecting…"/
/// "Syncing…" loading state) *and* the live EQ spectrum analyzer. Returns
/// whether the view must re-render.
fn spectrum_poll(s: &mut State, now: f64, tx: &mut Tx) -> bool {
    // `now` is Rune's ABSOLUTE frame clock (seconds since launch), not a per-frame
    // delta — see `AppSpec::poll`. Recover the real time this frame covered for the
    // manual, time-based animations (clamped so an idle gap can't teleport them).
    let frame_dt = views::reveal::frame_delta(now, &mut s.reveal_clock);
    // Keep the title bar's maximize/restore glyph honest. Piggybacks on frames
    // that were going to happen anyway and never ASKS for one: maximizing,
    // restoring or going full screen always resizes the window, which draws a
    // frame, which runs this — so the glyph updates without an idle app ticking
    // the CPU just to watch a window flag.
    sync_window_maximized(s);
    // Title-bar activity: while a library scan is in flight, OR a Sources row
    // is showing its own "Connecting…"/"Syncing…" loading state (a remote
    // with no health probe yet, or a reconnect/resync in flight), keep the
    // frame clock ticking and breathe the shared activity icon's pulse
    // (`remote_loading_line` reads the same `scan_anim`) — independent of the
    // spectrum, so the pill animates even when nothing is playing. The phase
    // rides the absolute clock directly (a constant real-time rate), bounded
    // modulo to stay finite and f32-precise over a long scan.
    let scanning = s.scan_progress.is_some() || views::settings::any_remote_row_is_loading(s);
    if scanning {
        s.scan_anim = (now as f32) % 1000.0;
        tx.wake_after(std::time::Duration::from_millis(16));
    }
    // Advance the sidebar nav fade+collapse reveal toward each item's visibility,
    // so a Settings toggle glides (fade, then collapse) instead of snapping.
    let mut nav_animating = false;
    for def in views::sidebar::NAV_DEFS {
        let shown = s.nav_visible(def.id);
        let progress = s
            .nav_reveal
            .entry(def.id)
            .or_insert(if shown { 1.0 } else { 0.0 });
        if views::reveal::tick(progress, shown, frame_dt) {
            nav_animating = true;
        }
    }
    if nav_animating {
        tx.wake_after(std::time::Duration::from_millis(16));
    }
    spectrum_poll_inner(s, now, tx) || scanning || nav_animating
}

/// The EQ spectrum half of [`spectrum_poll`] (kept separate so the scan-activity
/// pulse composes cleanly with its several early returns).
fn spectrum_poll_inner(s: &mut State, _now: f64, tx: &mut Tx) -> bool {
    // An open inline number editor: once it loses focus (click elsewhere, or Escape),
    // commit what was typed and close it. Keep a slow tick alive while it is focused so
    // that blur is noticed even when nothing is playing.
    if s.eq_edit.is_some() {
        if s.eq_edit_buf.focused {
            tx.wake_after(std::time::Duration::from_millis(50));
        } else {
            commit_eq_edit(s);
            return true;
        }
    }
    // Same blur-commit for the inline unit-rename editor: once its buffer loses focus
    // (click elsewhere, or Escape), commit the typed name and close it.
    if s.dsp_rename.is_some() {
        if s.dsp_rename_buf.focused {
            tx.wake_after(std::time::Duration::from_millis(50));
        } else {
            commit_dsp_rename(s);
            return true;
        }
    }
    let visible = matches!(s.route, Route::Settings) && s.settings_tab == SettingsTab::Audio;
    if !(visible && s.playing) {
        if s.spectrum.is_empty() {
            return false;
        }
        s.spectrum.clear();
        s.spectrum_seq = 0;
        return true;
    }
    // Keep the frame clock ticking while the analyzer is on screen.
    tx.wake_after(std::time::Duration::from_millis(16));
    let (seq, cols) = s.host.spectrum();
    if seq == s.spectrum_seq {
        return false; // no new FFT since last frame — nothing to redraw
    }
    s.spectrum_seq = seq;
    s.spectrum = cols.to_vec();
    true
}

/// The on-disk token for a managed-library strategy (inverse of the `"move"`
/// mapping in [`State::new`]).
fn strategy_as_config(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::Copy => "copy",
        Strategy::Move => "move",
    }
}

/// Set + persist the managed-library destination folder (Settings → Music Data).
/// `None` clears it (unset). Save-only — the actual copy/move "Add music…" flow is
/// separate, so this never touches the host.
pub(crate) fn set_managed_dest(s: &mut State, dest: Option<PathBuf>) {
    s.managed_dest = dest;
    config::save_managed_library(
        s.managed_dest.as_deref(),
        &s.managed_template,
        strategy_as_config(s.managed_strategy),
    );
}

/// Set + persist the managed-library template preset (by id).
pub(crate) fn set_managed_template(s: &mut State, template_id: &str) {
    s.managed_template = template_id.to_string();
    config::save_managed_library(
        s.managed_dest.as_deref(),
        &s.managed_template,
        strategy_as_config(s.managed_strategy),
    );
}

/// Set + persist the managed-library file strategy (Copy / Move).
pub(crate) fn set_managed_strategy(s: &mut State, strategy: Strategy) {
    s.managed_strategy = strategy;
    config::save_managed_library(
        s.managed_dest.as_deref(),
        &s.managed_template,
        strategy_as_config(s.managed_strategy),
    );
}

// ── Organize flow transitions ────────────────────────────────────────────────
//
// Pure phase transitions for the "Add music → organize" modal, each setting
// `State::organize`. They carry no IO: Task 2 calls `organize_ready` /
// `organize_progress` / `organize_finish` from `organize::plan`/`apply` thread
// callbacks, while the UI (this task) wires the modal buttons to `organize_begin`
// / `organize_confirm` / `organize_dismiss`. Driving these directly is the whole
// headless-testable surface.

/// Open the modal in its scanning/planning phase (the "Add music…" menu entry).
pub(crate) fn organize_begin(s: &mut State) {
    s.organize = Some(OrganizePhase::Planning);
}

/// Planning → Confirm: a computed plan is ready for the user to review.
// Posted from the `organize::plan` worker (the "Add music…" menu handler).
pub(crate) fn organize_ready(s: &mut State, plan: organize::OrganizePlan) {
    s.organize = Some(OrganizePhase::Confirm(plan));
}

/// Enter the Applying phase at 0 / `total` (progress ticks in via
/// [`organize_progress`]).
pub(crate) fn organize_apply_begin(s: &mut State, total: usize) {
    s.organize = Some(OrganizePhase::Applying {
        done: 0,
        total,
        current: String::new(),
    });
}

/// Update the Applying phase with the latest per-entry progress.
// Posted from the `organize::apply` progress callback (see `organize_confirm`).
pub(crate) fn organize_progress(s: &mut State, done: usize, total: usize, current: String) {
    s.organize = Some(OrganizePhase::Applying {
        done,
        total,
        current,
    });
}

/// Applying → Done: the run finished with this outcome tally.
// Posted when the `organize::apply` worker completes (see `organize_confirm`).
pub(crate) fn organize_finish(s: &mut State, summary: organize::Summary) {
    s.organize = Some(OrganizePhase::Done(summary));
}

/// Close the modal (Cancel / Close, an outside press, or Escape) — the flow is
/// idle again.
pub(crate) fn organize_dismiss(s: &mut State) {
    s.organize = None;
}

/// Confirm → Applying, the Confirm button's action: take the reviewed plan out of
/// the modal state, enter the `Applying { 0, total, "" }` phase, then — when an
/// `app_handle` is present (a real run) — spawn a worker that applies the plan,
/// streaming progress back per entry and finishing with the summary. The worker
/// also registers the destination as a watched source so the organized files
/// appear in the library. With no handle (headless tests) it stays the pure
/// Confirm → Applying transition, writing nothing to disk.
pub(crate) fn organize_confirm(s: &mut State) {
    // Only act from Confirm; take the plan so the worker owns it. Anything else is
    // restored untouched.
    let plan = match s.organize.take() {
        Some(OrganizePhase::Confirm(plan)) => plan,
        other => {
            s.organize = other;
            return;
        }
    };
    organize_apply_begin(s, plan.entries.len());
    let Some(handle) = s.app_handle.clone() else {
        // No handle (tests): the modal is in Applying; do not touch the filesystem.
        return;
    };
    let host = s.host.clone();
    let dest = plan.dest_root.clone();
    std::thread::spawn(move || {
        // The progress callback clones the handle per entry and posts an update —
        // that stream drives the Applying modal's `done / total` + current line.
        let summary = organize::apply(&plan, |pr| {
            let h = handle.clone();
            h.update(move |s: &mut State, _tx: &mut Tx| {
                organize_progress(s, pr.done, pr.total, pr.current.display().to_string());
            });
        })
        .unwrap_or_else(|e| organize::Summary {
            failed: vec![(dest.clone(), e.to_string())],
            ..Default::default()
        });
        // Watch the destination so the just-copied files show up in the library,
        // then post the final summary (both on the main thread, in one update).
        let watch = host.add_folder(&dest).ok();
        handle.update(move |s: &mut State, _tx: &mut Tx| {
            if let Some(up) = watch {
                apply_and_save(s, up);
            }
            organize_finish(s, summary);
        });
    });
}

/// Switch the library's sort order — re-fetches the current library's views in
/// the chosen baked axis. Track ids are stable across axes (only the display +
/// play-context order changes), so playback is undisturbed.
pub(crate) fn set_sort(s: &mut State, axis: BrowseAxis) {
    s.sort_axis = axis;
    s.loudness_sort = None;
    s.library = s.host.views_by(axis);
}

/// Which loudness metric the client-side library sort keys on. Unlike the baked
/// `BrowseAxis` sorts, loudness is joined from the live cache (not a baked axis), so
/// these re-order the current `Vec<TrackView>` in memory over the active base axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LoudnessKey {
    /// Dynamic-range indicator (crest factor), highest (most dynamic) first.
    Dr,
    /// Integrated loudness (LUFS), highest (loudest) first.
    Loudness,
}

/// Stable in-place sort of `library` by a loudness metric, descending (highest value
/// first). Un-analyzed tracks (no loudness, or a non-finite reading) keep their relative
/// order and sort last, so the list stays stable while the background sweep fills values
/// in.
pub(crate) fn sort_by_loudness(library: &mut [TrackView], key: LoudnessKey) {
    use std::cmp::Ordering;
    // `sort_by` is stable, so the (None, None) == Equal arm leaves un-analyzed tracks in
    // their original order at the tail, and analyzed ties keep the base axis order.
    library.sort_by(|a, b| {
        match (loudness_metric(a, key), loudness_metric(b, key)) {
            (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal), // descending
            (Some(_), None) => Ordering::Less, // analyzed before un-analyzed
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    });
}

/// The sortable loudness value for a track, or `None` when the track is un-analyzed or the
/// reading is non-finite (both sort last).
fn loudness_metric(t: &TrackView, key: LoudnessKey) -> Option<f32> {
    let l = t.loudness.as_ref()?;
    let v = match key {
        LoudnessKey::Dr => l.dr,
        LoudnessKey::Loudness => l.integrated_lufs,
    };
    v.is_finite().then_some(v)
}

/// Pick the library's loudness sort: re-fetch the current base axis, then stable-sort it
/// by `key` (un-analyzed last). Re-applied on each refresh (see the `loudness_dirty` tick)
/// so values slot into place as analysis lands.
pub(crate) fn set_loudness_sort(s: &mut State, key: LoudnessKey) {
    s.loudness_sort = Some(key);
    s.library = s.host.views_by(s.sort_axis);
    sort_by_loudness(&mut s.library, key);
}

/// Set the output volume (0..1): clamp, update `State::volume` optimistically so
/// the UI (slider + %) tracks immediately, and tell the host. Optimistic because
/// scroll/keyboard nudges are incremental — the next one must read this new value,
/// not wait for the host's echo. Shared by the volume slider, the Up/Down keys,
/// and the scroll-over-volume gesture.
pub(crate) fn set_volume(s: &mut State, v: f32) {
    let v = v.clamp(0.0, 1.0);
    s.volume = v;
    // Raising the volume unmutes (the backend's `VolumeState` clears mute on a
    // positive level); reflect that optimistically so the speaker icon flips
    // without waiting for the next `Tick`.
    if v > 0.0 {
        s.muted = false;
    }
    s.host.send(Command::SetVolume(v));
}

/// Toggle mute (the volume icon). The backend owns the mute state and the
/// pre-mute level (`VolumeState`, #105), so this just dispatches `ToggleMute`
/// and the UI reflects `Tick.muted`.
pub(crate) fn toggle_mute(s: &mut State) {
    s.host.send(Command::ToggleMute);
}

/// How close playback must get to a committed seek target (seconds) before the
/// seek bar stops previewing it and follows real playback again.
const SEEK_SETTLE_SECS: f64 = 0.5;

/// The scrub preview to keep for the next frame, given the committed target
/// (`scrub`), whether the seek bar is still being dragged, and the latest
/// playback `position`. While dragging, the preview is the live drag position and
/// is always kept. Once the user lets go, the target is held until playback lands
/// within [`SEEK_SETTLE_SECS`] of it — so the knob doesn't flick to the stale
/// position and back while the seek is in flight. Pure, so it's unit-tested
/// without a backend.
fn settle_scrub(scrub: Option<f64>, still_dragging: bool, position: f64) -> Option<f64> {
    match scrub {
        Some(target) if !still_dragging && (position - target).abs() < SEEK_SETTLE_SECS => None,
        other => other,
    }
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let p = theme::palette(is_dark(s.theme_pref, cx.system_dark()));
    let scale = cx.env().scale;
    // Keyboard focus ring in the theme's own colour (the old app's
    // `focus-visible:ring-1 ring-ring`, where `ring` == `primary`), in place of
    // Rune's default OS-accent ring.
    cx.focus_ring(2.0, 2.0, p.primary);

    // The root's committed width this frame (= the window width). The first
    // frame has no prior layout to read, so it falls back to a desktop width —
    // a one-frame settle, invisible in the running app.
    let window_w = cx
        .layout_size("app-root")
        .map(|size| size[0])
        .filter(|w| *w > 0.0)
        .unwrap_or(1100.0);
    let mobile = window_w < views::sidebar::MOBILE_BREAKPOINT;

    // The window shell. `app-root` keeps the key `window_w`/mobile detection
    // measure (a row or col reports its laid-out width identically).
    let shell = if mobile {
        // Mobile single-pane (the old app's `isMobile`): the title-bar band spans
        // the top, the mobile layout (menu home, or content with a back bar) below.
        let body = if s.mobile_show_content {
            col()
                .key("shell")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .fill(p.bg)
                .child(views::sidebar::mobile_top_bar(mobile_title(s), &p))
                .child(content_pane(s, &p, cx, scale))
        } else {
            col()
                .key("shell")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .fill(p.bg)
                .child(views::sidebar::sidebar(s, &p, cx, scale, true, 0.0))
        };
        col()
            .key("app-root")
            .w_full()
            .h_full()
            .fill(p.bg)
            .child(views::titlebar::bar(s, &p, cx, mobile))
            .child(body)
    } else {
        // Desktop: the left sidebar's fill runs to the very top, but its content is
        // inset by the band height so it lines up with the content pane below the
        // band — one clean title strip across the whole window. On macOS that same
        // inset is where the traffic lights sit (top-left, over the sidebar).
        let sidebar_top = views::titlebar::HEIGHT;
        row()
            .key("app-root")
            .w_full()
            .h_full()
            .fill(p.bg)
            .child(if s.sidebar_collapsed {
                // Collapsed: a 0-width placeholder keeps the resize handle at a
                // stable sibling slot, so one drag survives the collapse→expand
                // swap. The handle itself renders the visible rail.
                col().key("sidebar-slot").w(0.0).h_full()
            } else {
                views::sidebar::sidebar(s, &p, cx, scale, false, sidebar_top)
            })
            .child(views::sidebar::resize_handle(s.sidebar_collapsed, &p))
            .child(
                col()
                    .key("shell")
                    .grow(1.0)
                    .min_h(0.0)
                    .child(views::titlebar::bar(s, &p, cx, mobile))
                    .child(content_pane(s, &p, cx, scale)),
            )
    };

    // Overlay layer: the shell, plus any open dropdown's panel (Settings →
    // Language) and the expanded signal-path panel, mounted on top so they draw
    // over everything and escape the content scroll viewport's clip (see
    // `views::dropdown`).
    //
    // ALWAYS wrap the shell in this keyed root, even with no overlays. A conditional
    // wrap (`shell` alone when empty, wrapped when not) moved the shell one level
    // deeper the instant a dropdown opened — which rune read as a brand-new
    // `settings-scroll` and reset its scroll to the top (the "opening a dropdown jumps
    // the page to the top" bug). A constant structure keeps every scroll viewport's
    // identity — and thus its offset — stable whether or not an overlay is open.
    let mut overlays = views::settings::settings_overlays(s, &p, cx);
    overlays.extend(views::signal_path::overlay(s, &p));
    overlays.extend(views::titlebar::overlay(s, &p));
    overlays.extend(views::add_to_playlist::overlay(s, &p));
    overlays.extend(views::artwork_crop::overlay(s, &p, cx));
    // The crash-report modal draws over everything, on any route — last, so it
    // stacks above any panel still mounted beneath it.
    if let Some(dialog) = crash_overlay(s, &p) {
        overlays.push(dialog);
    }
    el().key("overlay-root")
        .w_full()
        .h_full()
        .child(shell)
        .children(overlays)
}

/// The routed main content area, keyed `"content"`. On desktop it fills the
/// space beside the sidebar; on mobile it is the whole window.
fn content_pane(s: &State, p: &theme::Palette, cx: &mut Cx<State>, scale: f32) -> El {
    let page = match s.route {
        Route::Home => views::home::home_page(s, p, cx, scale).grow(1.0),
        Route::Library => views::library::track_list(s, p, cx, scale).grow(1.0),
        Route::Albums => views::albums::albums_page(s, p, cx, scale).grow(1.0),
        Route::Album => views::albums::album_detail(s, p, scale).grow(1.0),
        Route::Artists => views::artists::artists_page(s, p, cx, scale).grow(1.0),
        Route::Artist => views::artists::artist_detail(s, p, cx, scale).grow(1.0),
        Route::Genres => views::genres::genres_page(s, p, cx).grow(1.0),
        Route::Genre => views::genres::genre_detail(s, p, scale).grow(1.0),
        Route::Playlists => views::playlists::page(s, p, cx, scale).grow(1.0),
        Route::Playlist => views::playlist_detail::page(s, p, scale).grow(1.0),
        Route::Settings => views::settings::settings_page(s, p, cx).grow(1.0),
    };
    // Content zoom (Ctrl +/-/0): lay the page out in a `pane / zoom` logical box and
    // scale it back up to fill the pane. Because the page — and its responsive grid —
    // lays out in that smaller logical width, the grid re-columns: a true reflow zoom,
    // not a paint-only magnify. At 100 % we skip the wrapper so the common path's
    // layout is untouched. `content`'s own size is parent-driven (grow), so reading it
    // back here is stable (no feedback with the wrapper we size from it).
    let inner = if (s.ui_zoom - 1.0).abs() < 0.001 {
        page
    } else {
        let [pane_w, pane_h] = cx.layout_size("content").unwrap_or([900.0, 700.0]);
        col()
            .key("content-zoom")
            .w(pane_w / s.ui_zoom)
            .h(pane_h / s.ui_zoom)
            .scale(s.ui_zoom)
            .origin(0.0, 0.0)
            .child(page)
    };
    col().key("content").grow(1.0).min_h(0.0).child(inner)
}

/// Hide the crash prompt for this session — also the modal's Escape / outside-press
/// dismiss handler.
fn dismiss_crash(s: &mut State) {
    s.crashed_last_session = false;
}

/// The crash-report **modal**, mounted on the overlay layer (over any route) when
/// the previous session ended abnormally (`logging::begin_session`): a centered
/// card over a dim scrim. Dismisses on Escape or an outside press (`El::modal`),
/// the same as its Dismiss button.
fn crash_overlay(s: &State, p: &theme::Palette) -> Option<El> {
    if !s.crashed_last_session {
        return None;
    }
    Some(
        col()
            .key("crash-modal")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .fill(rune::Color::BLACK.alpha(0.45))
            .child(crash_dialog(p).modal(true, dismiss_crash)),
    )
}

/// The centered crash-report dialog card: title, a short message, and Dismiss /
/// Report actions. Report opens the same pre-filled GitHub issue as Settings →
/// About and closes the dialog.
fn crash_dialog(p: &theme::Palette) -> El {
    use crate::ui::{radius, space};
    col()
        .key("crash-dialog")
        .w(380.0)
        .fill(p.card)
        .radius(radius::LG)
        .border(1.0, p.border)
        .padding(space::LG)
        .gap(space::MD)
        .child(rune::text(t!("crash.body").to_string()).color(p.fg).wrap())
        .child(
            row()
                .gap(space::SM)
                .justify_end()
                .child(
                    el().key("crash-dismiss")
                        .role("button")
                        .label(t!("crash.dismiss").to_string())
                        .focusable(true)
                        .padding(space::MD)
                        .radius(radius::MD)
                        .fill(p.bg)
                        .border(1.0, p.border)
                        .center()
                        .cursor(rune::Cursor::Pointer)
                        .child(rune::text(t!("crash.dismiss").to_string()).color(p.fg))
                        .on_press(dismiss_crash),
                )
                .child(
                    el().key("crash-report")
                        .role("button")
                        .label(t!("crash.report").to_string())
                        .focusable(true)
                        .padding(space::MD)
                        .radius(radius::MD)
                        .fill(p.primary)
                        .center()
                        .cursor(rune::Cursor::Pointer)
                        .child(
                            row()
                                .gap(space::SM)
                                .center_y()
                                .child(rune::icon(&views::icons::GITHUB, 16.0).color(p.bg))
                                .child(rune::text(t!("crash.report").to_string()).color(p.bg)),
                        )
                        .on_press(|s: &mut State| {
                            let device = s.output_device.clone();
                            dismiss_crash(s);
                            std::thread::spawn(move || crate::report::open_issue(device));
                        }),
                ),
        )
}

/// The current page's title, shown in the mobile back bar.
fn mobile_title(s: &State) -> String {
    match s.route {
        Route::Home => t!("nav.home").to_string(),
        Route::Library => t!("nav.tracks").to_string(),
        Route::Albums => t!("nav.albums").to_string(),
        Route::Album => s
            .open_album
            .clone()
            .unwrap_or_else(|| t!("nav.albums").to_string()),
        Route::Artists => t!("nav.artists").to_string(),
        Route::Artist => s
            .open_artist
            .clone()
            .unwrap_or_else(|| t!("nav.artists").to_string()),
        Route::Genres => t!("nav.genres").to_string(),
        Route::Genre => s
            .open_genre
            .clone()
            .unwrap_or_else(|| t!("nav.genres").to_string()),
        Route::Playlists => t!("nav.playlists").to_string(),
        Route::Playlist => s
            .open_playlist
            .and_then(|id| s.playlists.get(id))
            .map(|pl| pl.name.clone())
            .unwrap_or_else(|| t!("nav.playlists").to_string()),
        Route::Settings => t!("settings.title").to_string(),
    }
}

/// Show or hide a primary nav item (Settings → Navigation), persisting the
/// change. Hiding only removes the sidebar entry; the Settings gear is always
/// reachable, so the nav can never strand the user.
pub(crate) fn set_nav_visible(s: &mut State, id: &str, visible: bool) {
    let was = s.nav_visible(id);
    if visible == was {
        return;
    }
    if visible {
        s.nav_hidden.retain(|h| h != id);
    } else {
        s.nav_hidden.push(id.to_string());
    }
    config::save_nav_hidden(&s.nav_hidden);
}

/// The window handle, captured in `on_ready`, so `flush_on_exit` can read the
/// final geometry (`placement()`). A static because the exit hooks receive only
/// `&mut State`, and set synchronously there (a queued `ah.update` may not run
/// before exit). `None` in headless tests.
static WINDOW: std::sync::Mutex<Option<rune::WindowHandle>> = std::sync::Mutex::new(None);

/// Whether this process is running inside a Flatpak sandbox.
///
/// `FLATPAK_ID` is set by the Flatpak runtime for every app it starts, and is
/// the documented way to detect it from inside.
pub(crate) fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// How this app identifies itself to the desktop: the Wayland `app_id` / X11
/// `WM_CLASS`, and the MPRIS bus-name suffix. Those two and the installed
/// `.desktop` file's basename must all be the SAME string — that equality is
/// the only thing letting a compositor resolve a window, or a media applet
/// resolve a player, to this app's icon.
///
/// Normally the binary name, which is what cargo-packager names the entry
/// after (`{main_binary_name}.desktop`) for the .deb and the AppImage.
///
/// Inside a **Flatpak** it must instead be `$FLATPAK_ID`, and not merely as a
/// convention: Flatpak installs the entry as `$FLATPAK_ID.desktop`, and its
/// D-Bus policy auto-grants exactly one MPRIS name —
/// `org.mpris.MediaPlayer2.$FLATPAK_ID`. Keeping the binary name there would
/// mean the sandbox silently refuses the bus name the app asks for, so media
/// keys and the desktop's media controls would not see the player at all,
/// while everything else kept working. The alternative — asking for a broader
/// `--own-name` in the manifest — is both unnecessary and something Flathub
/// review pushes back on.
pub(crate) fn desktop_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| resolve_desktop_id(std::env::var("FLATPAK_ID").ok().as_deref()))
        .as_str()
}

/// The pure half of [`desktop_id`], split out so the fallback is testable
/// without mutating the process environment (which is racy across threads, and
/// `unsafe` as of the 2024 edition).
fn resolve_desktop_id(flatpak_id: Option<&str>) -> String {
    match flatpak_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => id.to_string(),
        None => env!("CARGO_BIN_NAME").to_string(),
    }
}

/// Refresh [`State::maximized`] from the live window, so the title bar's
/// maximize button can show the restore glyph while maximized.
///
/// `placement()` is the only thing Rune exposes that carries `is_maximized`,
/// and it returns `None` **on Wayland** — it also reads the window position,
/// which Wayland deliberately hides from clients. There, the flag keeps
/// whatever [`toggle_maximized_from_titlebar`] last set, which is correct for
/// the app's own button and wrong only if the compositor maximizes the window
/// some other way (a keybind, a titlebar double-click). That is the best
/// available answer until Rune exposes `is_maximized` on its own; it is not a
/// reason to leave the glyph permanently stuck on every platform.
///
/// Full screen counts as maximized here: either way the useful action is
/// "put it back", which is what the restore glyph says.
fn sync_window_maximized(s: &mut State) {
    if let Some(p) = WINDOW
        .lock()
        .ok()
        .and_then(|w| w.as_ref().and_then(|w| w.placement()))
    {
        s.maximized = p.maximized || p.full_screen;
    }
}

/// Toggle the window between maximized and restored from the app's own caption
/// button, flipping [`State::maximized`] optimistically.
///
/// The optimistic flip is what makes the glyph swap on Wayland, where
/// [`sync_window_maximized`] can't read the real state back. Everywhere else
/// the next frame's sync overwrites it with the truth, so a compositor that
/// refused the request self-corrects rather than leaving the glyph lying.
fn toggle_maximized_from_titlebar(s: &mut State, tx: &mut Tx) {
    s.maximized = !s.maximized;
    tx.toggle_maximized();
}

/// Start playing the loaded library on launch when `SOUL_PLAYER_AUTOPLAY` is
/// set — the hook the Linux session E2E test uses to get a track playing.
///
/// Why a hook rather than driving the UI: the end-to-end test asserts what the
/// OS media session publishes (MPRIS metadata, artwork, transport), and that
/// needs a track LOADED. Clicking a row to get there would mean injecting
/// synthetic pointer events into a compositor — a far more fragile dependency
/// than one env var, and it would test the click path rather than the thing
/// under test.
///
/// Joins the existing `SOUL_PLAYER_*` dev/e2e family (`SOUL_PLAYER_FOLDER`,
/// `SOUL_PLAYER_NO_UPDATE`); unset in any normal run, so a user never trips it.
/// Issues the same `PlayContext` the "Play All" pill does, so the test exercises
/// the real play path.
fn autoplay_from_env(s: &mut State) {
    if std::env::var_os("SOUL_PLAYER_AUTOPLAY").is_none() {
        return;
    }
    let ids: Vec<u32> = s.library.iter().map(|t| t.id).collect();
    if ids.is_empty() {
        tracing::warn!("SOUL_PLAYER_AUTOPLAY set but the library is empty");
        return;
    }
    tracing::info!(tracks = ids.len(), "autoplay: starting the loaded library");
    s.host.send(Command::PlayContext { ids, start: 0 });
}

/// Final best-effort flush on app exit — window close, Quit, or OS logout — run
/// once even if both `on_close` and `on_quit` fire. Config is already persisted
/// on change; this captures the warm-startup scan caches updated this session and
/// marks a clean exit (so the next launch doesn't show the crash prompt).
fn flush_on_exit(s: &mut State) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    config::save_scan_caches(&s.host.caches_snapshot());
    // Persist the window geometry so the next launch reopens where we left off.
    // `placement()` reads the live window, so it must happen before teardown.
    if let Some(p) = WINDOW
        .lock()
        .ok()
        .and_then(|w| w.as_ref().and_then(|w| w.placement()))
    {
        config::save_window_geometry(p.position, p.content_size, p.maximized, p.minimized);
    }
    logging::end_session();
    tracing::info!("Soul Player exiting");
}

/// Dispatch an OS media-control event (routed from the media-session thread onto
/// the main thread), resolving relative seeks against the live playback position.
pub(crate) fn apply_media_event(s: &mut State, tx: &mut Tx, event: souvlaki::MediaControlEvent) {
    use media_session::SessionAction;
    match media_session::map_event(event, s.position_secs, s.duration_secs) {
        Some(SessionAction::Command(cmd)) => s.host.send(cmd),
        Some(SessionAction::Quit) => tx.quit(),
        Some(SessionAction::Raise) => raise_window(),
        None => {}
    }
}

/// Bring the window to the user: MPRIS `Raise`, sent when someone clicks the
/// player's name in a media applet (GNOME's media controls, KDE's, waybar …).
///
/// Worth implementing rather than ignoring because souvlaki advertises
/// `CanRaise = true` unconditionally — the desktop is *told* the player can be
/// raised, so dropping the event makes a control that looks live do nothing.
///
/// `show()` before `focus()`: the window may be hidden to the tray, in which
/// case focusing something invisible achieves nothing.
///
/// **This cannot raise the window on Wayland**, and that is a protocol rule
/// rather than something to fix here. Wayland prevents focus stealing: a client
/// is activated only by presenting an `xdg_activation_v1` token minted by
/// whoever asked for it, and an MPRIS `Raise` carries no token — the interface
/// has no field for one. winit's `focus_window()` is documented as a no-op
/// there, and measured as one: driving `Raise` against a real headless sway
/// session with a second window holding focus moved nothing, and set no urgency
/// hint either. Rune's own activation path (`platform/activation.rs`) needs the
/// same token, and its `reopen::front` is macOS-only.
///
/// It is still worth doing rather than dropping the event. On X11 —
/// including an Xwayland session — `focus_window()` works and the window comes
/// forward; and `show()` un-hides a window that was put away to the tray on
/// every backend. The alternative is a control the desktop is *told* is live
/// (souvlaki hardcodes `CanRaise = true`) that provably does nothing at all.
fn raise_window() {
    let Ok(guard) = WINDOW.lock() else {
        return;
    };
    if let Some(w) = guard.as_ref() {
        w.show();
        w.focus();
    }
}

/// Push now-playing changes to the OS media session after each event is folded
/// into `State`. No-op until the session is up (`media_tx`).
fn feed_media_session(s: &mut State) {
    // Now-playing toast on a genuine track change, gated to backgrounded-only.
    // Done before the media-session gate below so it fires regardless of whether
    // the OS session (`media_tx`) is up, and tracked by its own `notify_last_id`.
    if s.now_playing_id != s.notify_last_id {
        s.notify_last_id = s.now_playing_id;
        if let Some(track) = s.now_playing_id.and_then(|id| s.track(id)) {
            let title = track.title.clone().unwrap_or_default();
            let artist = track.artist.clone().unwrap_or_default();
            notify::now_playing(s.focused, &title, &artist);
        }
    }

    let Some(tx) = s.media_tx.clone() else {
        return;
    };
    // A track that is loaded but stopped (MPRIS `Stop`, or a deck that ended)
    // is NOT the same as paused: the OS transport has a distinct Stopped
    // status. `now_playing_id.is_none()` alone misses it, because Stop leaves
    // the track loaded.
    let stopped_now = s.now_playing_id.is_none() || s.play_state == PlayState::Stopped;
    let prev = media_session::Snapshot {
        id: s.media_last_id,
        playing: s.media_last_playing,
        stopped: s.media_last_stopped,
    };
    let cur = media_session::Snapshot {
        id: s.now_playing_id,
        playing: s.playing,
        stopped: stopped_now,
    };
    let since = s
        .media_last_push
        .map(|t| t.elapsed())
        .unwrap_or(std::time::Duration::from_secs(3600));
    let push = media_session::decide_push(&prev, &cur, since);

    if push == media_session::Push::TrackAndPlayback
        && let Some(track) = s.now_playing_id.and_then(|id| s.track(id))
    {
        let _ = tx.send(media_session::Update::Track(media_session::TrackMeta {
            id: track.id,
            title: track.title.clone().unwrap_or_default(),
            artist: track.artist.clone().unwrap_or_default(),
            album: track.album.clone().unwrap_or_default(),
            duration_secs: track.duration_secs,
        }));
    }
    if push != media_session::Push::Nothing {
        let _ = tx.send(media_session::Update::Playback(media_session::Playback {
            playing: s.playing,
            stopped: stopped_now,
            position_secs: s.position_secs,
        }));
        s.media_last_push = Some(std::time::Instant::now());
    }
    s.media_last_id = s.now_playing_id;
    s.media_last_playing = s.playing;
    s.media_last_stopped = stopped_now;
}

/// The window's native handle as an `isize` (Windows HWND) for the media session's
/// SMTC; `None` on macOS/Linux, where souvlaki needs no handle.
#[cfg(target_os = "windows")]
fn window_hwnd(wh: &rune::WindowHandle) -> Option<isize> {
    wh.with_raw_handle(|handle| match handle.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    })
    .ok()
    .flatten()
}

#[cfg(not(target_os = "windows"))]
fn window_hwnd(_wh: &rune::WindowHandle) -> Option<isize> {
    None
}

/// `AppSpec::setup`: wires the app's `Waker` into the cover-art cache
/// (cheat-sheet §5b/§6a) so decoded covers wake a redraw instead of needing
/// a poll loop.
fn setup_art(s: &mut State, waker: Waker) {
    s.art.wire(waker);
}

/// Content-pane zoom bounds and step (Ctrl +/-/0). 1.0 = 100 %. **Zoom-IN only**:
/// the reflow lays the pane out at `size / zoom`, so below 100 % that box is
/// *larger* than the window and Rune's paint-scale (which doesn't shrink the
/// layout box) clips it. The floor is therefore 100 %; a sub-100 % "fit more"
/// zoom is a follow-up that needs logical size-scaling, not a paint transform.
const MIN_ZOOM: f32 = 1.0;
const MAX_ZOOM: f32 = 2.0;
const ZOOM_STEP: f32 = 0.1;

/// A loaded/requested zoom coerced to a sane value: non-finite → 1.0, else clamped
/// to `[MIN_ZOOM, MAX_ZOOM]`. Guards a hand-edited config and keeps `set_zoom` total.
fn sane_zoom(z: f32) -> f32 {
    if z.is_finite() {
        z.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// The zoom one step `dir` (+1 in / −1 out) from `current`, snapped to the
/// `ZOOM_STEP` grid and clamped to the legal range. Pure, so view and test agree.
fn stepped_zoom(current: f32, dir: i32) -> f32 {
    let steps = (current / ZOOM_STEP).round() + dir as f32;
    let next = (steps * ZOOM_STEP * 100.0).round() / 100.0;
    next.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Set the content-pane zoom and persist it (a no-op when unchanged).
fn set_zoom(s: &mut State, zoom: f32) {
    let zoom = sane_zoom(zoom);
    if (s.ui_zoom - zoom).abs() > f32::EPSILON {
        s.ui_zoom = zoom;
        config::save_ui_zoom(zoom);
    }
}

/// `AppSpec::on_key`: transport and volume shortcuts, so the head is
/// drivable from the keyboard alone (Space, arrows, `s`/`r`). Only
/// key-*down* events ever reach `on_key` in the first place — Rune's
/// `Runtime::key`/physical-input dispatch never forwards a release as a
/// logical `KeyEvent` (`crates/rune/src/runtime/input/keys.rs`'s
/// `queue_key`/`physical_then`: a release with no text/IME fallback is
/// handled as a `PhysicalInput` observation, not queued as a `KeyEvent`) —
/// and `Tx::key_repeat()` flags the OS's auto-repeat of a held key, so a
/// held arrow doesn't re-fire every frame. Letters with no modifier arrive
/// as `Key::Text` (`Key::Command(c)` is reserved for Cmd/Ctrl chords), so
/// `s`/`r` are matched there.
fn handle_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    // Track Shift (bare press/release arrives here thanks to `App::modifier_keys`;
    // other keys carry it in `mods`) so an EQ number drag can fine-tune while held.
    // The `Key::Modifier` event itself falls through to the `_` arms below.
    s.shift_held = ev.mods.shift;
    if tx.key_repeat() {
        return false;
    }
    // An open dropdown is light-dismiss (no modal): Escape closes it. Its backdrop
    // handles the outside-press + scroll dismissals.
    if s.open_dropdown.is_some() && matches!(ev.key, Key::Escape) {
        close_dropdown(s);
        return true;
    }
    // Search-input mode: typed characters build the library filter instead of
    // driving the transport. `/` enters it; Esc clears + exits, Enter just exits
    // (keeping the filter), Backspace edits.
    if s.searching {
        match &ev.key {
            Key::Escape => {
                s.search.clear();
                s.searching = false;
            }
            Key::Enter => s.searching = false,
            Key::Backspace => {
                s.search.pop();
            }
            Key::Text(t) => s.search.push_str(t),
            _ => return false,
        }
        return true;
    }
    match &ev.key {
        // Ctrl-chord playback bindings, matching the original soul-player's
        // Keyboard settings tab (Play/Pause, Next, Prev, Volume Up/Down, Mute).
        // Guarded on `ev.mods.ctrl` and placed BEFORE the plain arms so a ctrl
        // chord never falls through to the unmodified handler. Search-input mode
        // already returned above, so these never fire while typing a filter.
        Key::Text(t) if t == " " && ev.mods.ctrl => s.host.send(Command::TogglePlay),
        Key::Right if ev.mods.ctrl => s.host.send(Command::Next),
        Key::Left if ev.mods.ctrl => s.host.send(Command::Prev),
        Key::Up if ev.mods.ctrl => set_volume(s, s.volume + 0.05),
        Key::Down if ev.mods.ctrl => set_volume(s, s.volume - 0.05),
        Key::Text(t) if ev.mods.ctrl && t.eq_ignore_ascii_case("m") => {
            s.host.send(Command::ToggleMute)
        }
        Key::Text(t) if t == "/" && matches!(s.route, Route::Library) => s.searching = true,
        Key::Text(t) if t == " " => s.host.send(Command::TogglePlay),
        Key::Right => s.host.send(Command::Next),
        Key::Left => s.host.send(Command::Prev),
        Key::Up => set_volume(s, s.volume + 0.05),
        Key::Down => set_volume(s, s.volume - 0.05),
        Key::Text(t) if t.eq_ignore_ascii_case("s") => {
            s.host.send(Command::SetShuffle(!s.shuffle));
        }
        Key::Text(t) if t.eq_ignore_ascii_case("r") => {
            let next = match s.repeat {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            s.host.send(Command::SetRepeat(next));
        }
        // Content zoom (Ctrl +/-/0, ⌘ on macOS): "=" / "+" zoom in, "-" / "_" out,
        // "0" resets to 100 %. Ctrl/⌘ chords arrive as `Key::Command(char)`.
        Key::Command('=') | Key::Command('+') => set_zoom(s, stepped_zoom(s.ui_zoom, 1)),
        Key::Command('-') | Key::Command('_') => set_zoom(s, stepped_zoom(s.ui_zoom, -1)),
        Key::Command('0') => set_zoom(s, 1.0),
        _ => return false,
    }
    true
}

/// Snapshot scenes for headless e2e rendering (`--scene NAME`): each is a named
/// tweak to the initial state so `--snap` can capture a specific view. Inert in
/// normal use — only Rune's tooling `--scene` flag ever runs them.
fn scene_settings(s: &mut State) {
    s.route = Route::Settings;
}
/// Home with the window reported as maximized, so `--snap` can show the
/// title bar's RESTORE glyph (the state a screenshot otherwise can't reach:
/// headless runs have no window to maximize).
fn scene_maximized(s: &mut State) {
    s.maximized = true;
}
fn scene_home(s: &mut State) {
    s.route = Route::Home;
}
fn scene_settings_music(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
}
fn scene_settings_keyboard(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Keyboard;
}
fn scene_settings_audio(s: &mut State) {
    // Populate a live chain so the Signal-path card reads out (not "nothing playing").
    scene_signal_path(s);
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Audio;
}
/// Settings → Audio with the EQ enabled and a shaped curve, so `--snap` shows the
/// response-curve graph with its bands moved (not a flat line).
fn scene_settings_audio_eq(s: &mut State) {
    scene_settings_audio(s);
    // A gentle smiley: lift the lows (shelf) and highs (shelf), dip the mids.
    let mut bands = default_bands().to_vec();
    let gains = [6.0, 3.0, -2.0, -3.5, -2.5, 1.5, 4.0, 5.0];
    for (band, g) in bands.iter_mut().zip(gains) {
        band.gain_db = g;
    }
    // A 3-unit DSP chain so `--snap` shows the reorderable unit list; unit 0 (the
    // smile) is active.
    s.dsp_units = vec![
        DspUnit::Eq(EqUnit {
            bands,
            preamp_db: -2.0,
            enabled: true,
            name: None,
        }),
        // A named unit so `--snap` shows the "<name> (EQ)" rename in a header.
        DspUnit::Eq(EqUnit {
            name: Some("Vocal clarity".into()),
            ..EqUnit::default()
        }),
        DspUnit::Eq(EqUnit {
            enabled: false,
            ..EqUnit::default()
        }),
    ];
    // Open unit 0's card (the smile) so `--snap` shows its response graph; the
    // other two stay collapsed to their header rows.
    s.dsp_expanded = vec![true, false, false];
    // Select a mid band (unit 0, band 4) so its per-band curve shows.
    s.eq_selected = Some((0, 4));
    // A synthetic spectrum (pink-ish decline + a few bumps) so `--snap` shows the
    // analyzer behind the curve — there is no live audio in a headless snapshot.
    s.spectrum = (0..SPECTRUM_COLS)
        .map(|i| {
            let t = i as f32 / (SPECTRUM_COLS - 1) as f32;
            let tilt = -24.0 - 30.0 * t;
            let bump = |c: f32, w: f32, g: f32| g * (-(((t - c) / w).powi(2))).exp();
            // A deterministic jitter so the snapshot looks like a real (jagged)
            // analyzer rather than a smooth synthetic curve.
            let n = i as f32;
            let jitter = 3.0 * (n * 2.3).sin() + 2.0 * (n * 0.9 + 1.0).cos();
            tilt + bump(0.12, 0.05, 15.0) + bump(0.55, 0.09, 11.0) + bump(0.82, 0.04, 8.0) + jitter
        })
        .collect();
}
/// Like [`scene_settings_audio_eq`] but with the master enable OFF, so `--snap`
/// shows the dimmed-when-off card (only the On/Off toggle stays lit).
fn scene_settings_audio_eq_off(s: &mut State) {
    scene_settings_audio_eq(s);
    // Unit 0 (the open card) disabled, so its body dims under the lit header toggle.
    s.dsp_units[0].set_enabled(false);
    s.eq_selected = None;
}
fn scene_settings_about(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::About;
}
/// Settings → Appearance (the tab the sweep audits for theme + nav toggles).
fn scene_settings_appearance(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Appearance;
}
/// Settings → Music Data with the add-source menu open (its scrim makes the page
/// inert), for sweeping the modal's option buttons.
fn scene_add_source_open(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
    s.open_dropdown = Some("add-source".to_string());
}
/// The crash-report dialog shown after a crashed session.
fn scene_crash_dialog(s: &mut State) {
    s.crashed_last_session = true;
}
/// German settings scenes: the longest-label locale we ship, for catching text
/// overflow in `--snap`. `apply_locale` (not `set_locale_pref`) so the snapshot
/// never writes the user's persisted language.
/// Music Data with a skip report: the Sources card after a scan that passed over
/// files it could not import. A dedicated scene because the notice only ever
/// exists transiently (it is cleared by the next refresh), so there is otherwise
/// no way to snapshot the wording or catch it overflowing its card.
fn scene_settings_music_skips(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
    s.skip_notice = Some((
        PathBuf::from("/music/Bootlegs"),
        vec![
            SkippedGroup {
                extension: "aac".into(),
                reason: soul_player_host::SkipReason::DecoderDisabled,
                count: 2,
            },
            SkippedGroup {
                extension: "wma".into(),
                reason: soul_player_host::SkipReason::UnsupportedFormat,
                count: 1,
            },
        ],
    ));
}
fn scene_settings_music_de(s: &mut State) {
    apply_locale("de");
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
}
fn scene_settings_audio_de(s: &mut State) {
    apply_locale("de");
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Audio;
}
fn scene_settings_appearance_de(s: &mut State) {
    apply_locale("de");
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Appearance;
}
/// Appearance with the language dropdown open, for snapshotting the themed panel.
fn scene_settings_lang_open(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Appearance;
    s.open_dropdown = Some("lang-picker".to_string());
}
/// Same, forced to the light (Day) palette, for a theme-robustness snapshot.
fn scene_settings_lang_open_light(s: &mut State) {
    s.theme_pref = ThemePref::Light;
    scene_settings_lang_open(s);
}
/// Same, in German — confirms the panel's localized "System" + native names.
fn scene_settings_lang_open_de(s: &mut State) {
    apply_locale("de");
    s.theme_pref = ThemePref::Dark;
    scene_settings_lang_open(s);
}
fn scene_albums(s: &mut State) {
    s.route = Route::Albums;
}
fn scene_artists(s: &mut State) {
    s.route = Route::Artists;
}
fn scene_genres(s: &mut State) {
    s.route = Route::Genres;
}
/// Seed a synthetic library for `--snap` grid/zoom validation: 20 distinct albums
/// across 8 artists and 6 genres — enough to fill and scroll the grid. Covers
/// resolve to the music-note placeholder (no real files in a headless snapshot).
fn seed_demo_library(s: &mut State) {
    const ARTISTS: &[&str] = &[
        "Aurora Fields",
        "Beacon",
        "Cobalt Trio",
        "Dust & Echo",
        "Elian Vos",
        "Fenwick",
        "Glass Harbor",
        "Halcyon",
    ];
    const GENRES: &[&str] = &[
        "Ambient",
        "Classical",
        "Electronic",
        "Folk",
        "Jazz",
        "Hip-Hop",
    ];
    const TITLES: &[&str] = &["Nightfall", "Open Skies", "Reverie", "Lantern", "Meridian"];
    let mut lib = Vec::new();
    let mut id = 1u32;
    for a in 0..20u32 {
        let artist = ARTISTS[a as usize % ARTISTS.len()];
        let genre = GENRES[a as usize % GENRES.len()];
        let album = format!("{} {:02}", TITLES[a as usize % TITLES.len()], a + 1);
        for t in 1..=4u32 {
            lib.push(soul_player_host::TrackView {
                id,
                title: Some(format!("Track {t}")),
                artist: Some(artist.to_string()),
                album: Some(album.clone()),
                genre: Some(genre.to_string()),
                duration_secs: f64::from(120 + id % 180),
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            });
            id += 1;
        }
    }
    s.library = lib;
}
/// The Albums grid, seeded + at 100 % zoom (for `--snap` size/zoom validation).
fn scene_grid(s: &mut State) {
    seed_demo_library(s);
    s.route = Route::Albums;
}
fn scene_grid_z150(s: &mut State) {
    scene_grid(s);
    s.ui_zoom = 1.5;
}
fn scene_grid_z200(s: &mut State) {
    scene_grid(s);
    s.ui_zoom = 2.0;
}
/// Albums at 150 %/200 % zoom over the *loaded* library (no seed): pair with
/// `SOUL_PLAYER_FOLDER=<dir>` to snapshot real covers under zoom.
fn scene_albums_z150(s: &mut State) {
    s.route = Route::Albums;
    s.ui_zoom = 1.5;
}
fn scene_albums_z200(s: &mut State) {
    s.route = Route::Albums;
    s.ui_zoom = 2.0;
}
/// Seeded Artists browse (circular cards), for `--snap` validation.
fn scene_demo_artists(s: &mut State) {
    seed_demo_library(s);
    s.route = Route::Artists;
}
/// Seeded Artist detail (circular hero + Top Songs + Discography) — the first
/// seeded artist has three albums, so the Discography grid is populated.
fn scene_demo_artist(s: &mut State) {
    seed_demo_library(s);
    s.route = Route::Artist;
    s.open_artist = s.library.first().and_then(|t| t.artist.clone());
}
/// Same, at 150 % zoom — checks the content-pane zoom on a non-grid detail page.
fn scene_demo_artist_z150(s: &mut State) {
    scene_demo_artist(s);
    s.ui_zoom = 1.5;
}
/// Seeded Genre detail (icon-tile hero + track list), for `--snap` validation.
fn scene_demo_genre(s: &mut State) {
    seed_demo_library(s);
    s.route = Route::Genre;
    s.open_genre = s.library.first().and_then(|t| t.genre.clone());
}
/// Seeded Artist detail with the Discography in LIST mode (the row view), so
/// `--snap` can validate the grid/list toggle's compact-row layout — the lit
/// list icon and each album as a cover thumb + "{n} tracks · {duration}".
fn scene_demo_artist_list(s: &mut State) {
    scene_demo_artist(s);
    s.discography_list = true;
}
/// Seeded Genre detail with an active in-page search ("lantern" matches only the
/// "Lantern 19" album's tracks within the Ambient genre), so `--snap` can
/// validate the filter narrowing the list while the header counts stay unfiltered.
fn scene_demo_genre_search(s: &mut State) {
    scene_demo_genre(s);
    s.genre_search = rune::TextEdit::new("lantern");
}
fn scene_artist_detail(s: &mut State) {
    s.route = Route::Artist;
    s.open_artist = s.library.first().and_then(|t| t.artist.clone());
    s.now_playing_id = s.library.first().map(|t| t.id);
    s.playing = true;
}
fn scene_album_detail(s: &mut State) {
    s.route = Route::Album;
    s.open_album = s.library.first().and_then(|t| t.album.clone());
    s.now_playing_id = s.library.first().map(|t| t.id);
    s.playing = true;
}
fn scene_search(s: &mut State) {
    s.search = "aria".to_string();
}
fn scene_no_match(s: &mut State) {
    s.search = "zzz-no-such-track".to_string();
}
fn scene_collapsed(s: &mut State) {
    s.sidebar_collapsed = true;
}
/// The mobile content view (narrow window + a nav tap taken): the track list
/// full-width under the back bar. Pair with `--size 460x720` to see it; the
/// bare narrow window (no scene) shows the mobile menu.
fn scene_mobile_content(s: &mut State) {
    s.route = Route::Library;
    s.mobile_show_content = true;
}
/// Seed a populated banded queue from the loaded library so `--snap` can capture
/// the "Up Next" sidebar (which is empty until playback otherwise).
fn scene_queue(s: &mut State) {
    let ids: Vec<u32> = s.library.iter().take(9).map(|t| t.id).collect();
    if let Some(&first) = ids.first() {
        s.now_playing_id = Some(first);
        s.playing = true;
        s.up_next = ids.iter().skip(1).take(2).copied().collect();
        s.context_ids = ids.iter().skip(3).copied().collect();
        s.context_pos = 0;
        s.context_len = s.library.len() as u32;
    }
}

/// Now-playing with a signal path (DSD64 decoded to 44.1 kHz, resampled to a
/// 48 kHz device, ReplayGain on) so `--snap` can capture the collapsed
/// signal-path line under the artist.
fn scene_signal_path(s: &mut State) {
    scene_queue(s);
    s.replaygain_mode = ReplayGainMode::Track;
    s.signal_path = Some(SignalPath {
        source: SourceFormat::Dsd { rate_hz: 2_822_400 },
        codec: SourceCodec::Dsd,
        decoded_rate: 44_100,
        decoded_bits: 24,
        channels: 2,
        replay_gain_db: 2.3,
        device_rate: 48_000,
        resampling: true,
        output_bits: 24,
        exclusive: false,
        loudness: Some(TrackLoudness {
            integrated_lufs: -14.2,
            lra: 9.5,
            true_peak_dbtp: -0.8,
            plr: 13.4,
            dr: 12.0,
        }),
    });
}

/// Same, with the expanded stage-by-stage signal-path panel open.
fn scene_signal_path_open(s: &mut State) {
    scene_signal_path(s);
    s.signal_path_open = true;
}

/// The managed-library organize modal mid-flow, on the Music Data tab, with a
/// small real `Confirm` plan so `--snap` captures the scrim + card + the
/// copy/skip counts + the Confirm/Cancel buttons. Pair with `--size 1100x860`.
fn scene_organize_confirm(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
    let dest = PathBuf::from("/Users/you/Music/Managed");
    s.organize = Some(OrganizePhase::Confirm(organize::OrganizePlan {
        dest_root: dest.clone(),
        entries: vec![
            organize::PlanEntry {
                src: PathBuf::from("Daft Punk - One More Time.flac"),
                dst: dest.join("Daft Punk/Discovery/01 - One More Time.flac"),
                action: organize::Action::Copy,
            },
            organize::PlanEntry {
                src: PathBuf::from("Daft Punk - Aerodynamic.flac"),
                dst: dest.join("Daft Punk/Discovery/02 - Aerodynamic.flac"),
                action: organize::Action::Copy,
            },
            organize::PlanEntry {
                src: PathBuf::from("already-there.flac"),
                dst: dest.join("Daft Punk/Discovery/01 - One More Time.flac"),
                action: organize::Action::SkipDuplicate,
            },
        ],
    }));
}

/// Two user playlists (one populated, one empty) built entirely in memory, so
/// the Playlists list + detail pages and the add-to-playlist overlay render
/// their rows and controls without touching the user's on-disk store. Returns
/// the populated playlist's id. Entries use synthetic local locators (the
/// snapshot/scene host has no scanned library; rows render regardless).
fn a11y_seed_playlists(s: &mut State) -> playlists::PlaylistId {
    let entry = |title: &str, path: &str| playlists::PlaylistEntry {
        locator: soul_player_host::TrackLocator::Local {
            path: path.to_string(),
        },
        snapshot: soul_player_host::TrackSnapshot {
            title: title.to_string(),
            artist: "Test Artist".to_string(),
            album: "Test Album".to_string(),
            duration_secs: 212,
            track_no: None,
            disc_no: None,
        },
        mbid: None,
    };
    s.playlists = playlists::PlaylistStore::in_memory(vec![
        (
            "Evening Mix",
            vec![
                entry("Aria", "/music/aria.flac"),
                entry("Bolero", "/music/bolero.flac"),
            ],
        ),
        ("Empty Set", Vec::new()),
    ]);
    s.playlists.list()[0].id
}

/// The Playlists list page, with one populated and one empty playlist.
fn scene_playlists(s: &mut State) {
    a11y_seed_playlists(s);
    s.route = Route::Playlists;
}

/// A playlist's detail page: hero, track rows, reorder grips, and the
/// cover/rename/delete controls.
fn scene_playlist_detail(s: &mut State) {
    let pid = a11y_seed_playlists(s);
    s.route = Route::Playlist;
    s.open_playlist = Some(pid);
}

/// The add-to-playlist overlay (pick rows + the new-playlist field) over the
/// now-playing track.
fn scene_add_to_playlist_open(s: &mut State) {
    a11y_seed_playlists(s);
    open_add_to_playlist(s, 1);
}

/// The "Edit Artwork" CROP dialogue over an album detail page: pan/zoom a real
/// gradient image behind the fixed SQUARE frame (an album → square; an artist
/// would be a circle), so `--snap` captures the frame + image + zoom buttons +
/// footer and the a11y sweep covers its buttons. Writes a real, decodable
/// landscape PNG to a temp file (the cropper decodes + renders it; wider than
/// tall so the square frame leaves horizontal pan room).
fn scene_artwork_crop(s: &mut State) {
    use views::detail_page::{ArtworkCrop, ArtworkTarget};
    let album = s
        .library
        .iter()
        .find_map(|t| t.album.clone())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "Album".to_string());
    s.route = Route::Album;
    s.open_album = Some(album.clone());
    let (w, h) = (480u32, 320u32);
    let mut img = image::RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 160]);
    }
    let path = std::env::temp_dir().join(format!(
        "soul_rune_scene_artwork_crop_{}.png",
        std::process::id()
    ));
    let _ = img.save(&path);
    let scale = views::artwork_crop::FRAME / (w.min(h) as f32);
    s.artwork_crop = Some(ArtworkCrop {
        target: ArtworkTarget::Album(album),
        source: path,
        src_w: w,
        src_h: h,
        scale,
        offset: (0.0, 0.0),
    });
}

/// The "Edit Artwork" MANAGE dialogue (the pencil's select step: Change/Cancel,
/// no cover set so no Delete) for the a11y sweep + snapshots.
fn scene_artwork_edit(s: &mut State) {
    use views::detail_page::ArtworkTarget;
    let album = s
        .library
        .iter()
        .find_map(|t| t.album.clone())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "Album".to_string());
    s.route = Route::Album;
    s.open_album = Some(album.clone());
    s.artwork_edit = Some(ArtworkTarget::Album(album));
}

/// The title-bar remote-health popover, over a seeded online remote server, so
/// the indicator renders and the popover opens.
fn scene_remote_health_open(s: &mut State) {
    s.sources.push(soul_player_host::SourceStatus {
        path: PathBuf::from("http://library.test"),
        enabled: true,
        available: true,
        track_count: 128,
        kind: soul_player_host::SourceKind::Remote {
            url: "http://library.test".to_string(),
        },
        remote: Some(soul_player_host::RemoteHealth {
            online: true,
            latency_ms: Some(24),
            server_version: Some("0.3.0".to_string()),
            index_version: Some(7),
            last_checked_unix: Some(1_700_000_000),
        }),
    });
    s.remote_health_open = true;
}

fn main() -> anyhow::Result<()> {
    // Start file logging before anything else, so startup, every subsystem, and
    // any panic are recorded. The guard must outlive the process — binding it to
    // a named local keeps the non-blocking writer thread alive until `main` ends.
    let _log_guard = logging::init();
    let os = os_info::get();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        os = %os,
        arch = std::env::consts::ARCH,
        cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "Soul Player starting",
    );
    // Whether the previous session ended without a clean exit (and mark this one
    // running). Handed to `State` below so the UI can offer to report the crash.
    let crashed_last_session = logging::begin_session();
    if crashed_last_session {
        tracing::warn!("previous session did not exit cleanly");
    }
    // Register our AppUserModelID so Windows will render/attribute our toasts
    // (unpackaged apps need a known AUMID). No-op off Windows.
    notify::register_identity();

    // Single-instance (release builds only — dev can run several for comparison):
    // a second launch focuses the running window (and forwards its links to
    // `on_open_url`) instead of opening a duplicate that would fight over the
    // audio device. Returns false when another instance handled this launch →
    // exit without opening a window. macOS/other platforms: always true.
    if !cfg!(debug_assertions) && !rune::single_instance("co.datadir.soulplayer") {
        return Ok(());
    }

    let host = PlayerHandle::new();
    // Restore the persisted output-device choice BEFORE any autoload/open spawns
    // the actor, so the first playback session already targets the chosen device
    // (empty / absent config = system default).
    host.set_output_device(config::load_output_device());
    // Restore the persisted exclusive-output choice the same way, so the first
    // session opens exclusive (bit-perfect) if the user asked for it — falling back
    // to shared if the device refuses, which the signal path then shows.
    host.set_exclusive(config::load_output_exclusive());
    host.set_exclusive_strict(config::load_output_exclusive_strict());
    // Restore the persisted ReplayGain choice before any spawn, so the first
    // playback session is already normalized (the host banks it and tells the
    // actor on spawn).
    let (rg_mode, rg_preamp) = config::load_replaygain();
    host.set_replay_gain(rg_mode, rg_preamp);
    // Restore the persisted DSP chain before any spawn too, so the first stream
    // opens with the user's units already applied.
    host.set_dsp_chain(config::load_dsp_chain());
    // Seed the hybrid-ReplayGain loudness cache from the persisted file (stale
    // entries already dropped on load) BEFORE any spawn, so a previously-analyzed
    // untagged track is normalized this session with no re-scan.
    host.seed_loudness(config::load_loudness_cache());
    // Seed the per-source scan caches from the persisted file BEFORE restoring the
    // sources, so the startup scan is a warm diff (only new/changed files re-read)
    // instead of a cold full read of the whole library. A stale-version cache was
    // already discarded on load.
    host.seed_caches(config::load_scan_caches());
    // Restore the indexing-speed choice BEFORE the startup scan so it uses the
    // user's mode (default Automatic → adapts to CPU load).
    host.set_indexing_mode(IndexingMode::from_config(&config::load_indexing_mode()));
    // Restore the background-loudness-analysis mode too, so the startup scan's Tier-2 sweep
    // honours it (Off = no sweep) from the first spawned actor.
    host.set_loudness_analysis_mode(LoudnessAnalysisMode::from_config(
        &config::load_loudness_analysis(),
    ));
    // The dev/e2e harness loads a single folder from the env; otherwise restore
    // the persisted watched-folder set so Sources survive a restart.
    //
    // `needs_startup_scan`: a restored real library is scanned OFF the main thread
    // once the window is up (see the `on_ready`/first-frame kickoff below), so a
    // cold or large library never blocks the first paint (#102). The env/e2e path
    // stays synchronous — its testdata is tiny and the harness expects it loaded.
    let mut needs_startup_scan = false;
    if std::env::var_os("SOUL_PLAYER_FOLDER").is_some() {
        host.autoload_from_env()?;
        // e2e harness only (never set in normal use): start playing the loaded folder
        // from track 0, so OS-integration tests (Now Playing, media keys) can drive a
        // real playing app without UI clicks.
        if std::env::var_os("SOUL_PLAYER_AUTOPLAY").is_some() {
            let ids: Vec<u32> = host.views().iter().map(|t| t.id).collect();
            tracing::info!(
                tracks = ids.len(),
                "SOUL_PLAYER_AUTOPLAY: playing the loaded folder"
            );
            host.send(soul_player_host::Command::PlayContext { ids, start: 0 });
        }
    } else {
        let saved = config::load_sources_with_enabled();
        if !saved.is_empty() {
            tracing::info!(
                count = saved.len(),
                "restoring saved sources (scan deferred)"
            );
            // Set the configured set now (cheap) so the window opens on the Sources
            // list immediately; the scan itself is deferred to a background worker.
            host.configure_sources(&saved);
            needs_startup_scan = true;
        }
    }

    let init_host = host.clone();
    let spec = AppSpec::<State> {
        setup: Some(setup_art),
        on_key: Some(handle_key),
        // Drives the live EQ spectrum analyzer while the Audio tab is open + playing.
        poll: Some(spectrum_poll),
        scenes: &[
            ("settings", scene_settings),
            ("home", scene_home),
            ("maximized", scene_maximized),
            ("settings-music", scene_settings_music),
            ("settings-keyboard", scene_settings_keyboard),
            ("settings-audio", scene_settings_audio),
            ("settings-audio-eq", scene_settings_audio_eq),
            ("settings-audio-eq-off", scene_settings_audio_eq_off),
            ("settings-about", scene_settings_about),
            ("settings-appearance", scene_settings_appearance),
            ("add-source-open", scene_add_source_open),
            ("crash-dialog", scene_crash_dialog),
            ("settings-music-skips", scene_settings_music_skips),
            ("settings-music-de", scene_settings_music_de),
            ("settings-audio-de", scene_settings_audio_de),
            ("settings-appearance-de", scene_settings_appearance_de),
            ("settings-lang-open", scene_settings_lang_open),
            ("settings-lang-open-light", scene_settings_lang_open_light),
            ("settings-lang-open-de", scene_settings_lang_open_de),
            ("albums", scene_albums),
            ("album-detail", scene_album_detail),
            ("artists", scene_artists),
            ("artist-detail", scene_artist_detail),
            ("genres", scene_genres),
            ("grid", scene_grid),
            ("grid-z150", scene_grid_z150),
            ("grid-z200", scene_grid_z200),
            ("albums-z150", scene_albums_z150),
            ("albums-z200", scene_albums_z200),
            ("demo-artists", scene_demo_artists),
            ("demo-artist", scene_demo_artist),
            ("demo-artist-z150", scene_demo_artist_z150),
            ("demo-artist-list", scene_demo_artist_list),
            ("demo-genre", scene_demo_genre),
            ("demo-genre-search", scene_demo_genre_search),
            ("search", scene_search),
            ("no-match", scene_no_match),
            ("collapsed", scene_collapsed),
            ("mobile-content", scene_mobile_content),
            ("queue", scene_queue),
            ("signal-path", scene_signal_path),
            ("signal-path-open", scene_signal_path_open),
            ("organize-confirm", scene_organize_confirm),
            ("playlists", scene_playlists),
            ("playlist-detail", scene_playlist_detail),
            ("add-to-playlist-open", scene_add_to_playlist_open),
            ("artwork-crop", scene_artwork_crop),
            ("artwork-edit", scene_artwork_edit),
            ("remote-health-open", scene_remote_health_open),
        ],
        ..AppSpec::new(
            "Soul Player",
            (1100.0, 720.0),
            || unreachable!("init overridden"),
            view,
        )
    };
    // A shared cell so `on_ready` (which fires during `run`, when the window and
    // its HWND exist) can reach an `AppHandle` — which only exists after the app is
    // built. Set just below, before `run`.
    let media_ah_cell = std::sync::Arc::new(std::sync::OnceLock::<rune::AppHandle<State>>::new());
    let mut builder = App::new(spec);
    // Restore the last window geometry (outer position, content size, maximized)
    // so the app reopens where the user left it, clamped onto a currently-
    // connected display (recovering from an unplugged monitor or resolution
    // change). Absent on a fresh install → Rune's default placement.
    if let Some(g) = config::restore_window_geometry() {
        let wo = rune::WindowOptions {
            position: Some((g.x, g.y)),
            physical_size: Some((g.w, g.h)),
            ..Default::default()
        };
        builder = builder.configured_options(wo.into()).maximized(g.maximized);
    }
    // macOS: native Controls menu (menu bar) + Dock menu transport. Rune's native
    // menus are macOS-only; Windows/Linux expose the same controls via the tray.
    #[cfg(target_os = "macos")]
    {
        for item in macos_menu::items() {
            builder = builder.menu_item(item);
        }
    }
    let app = builder
        .titlebar(Titlebar::new(views::titlebar::HEIGHT).traffic_lights(
            views::titlebar::TRAFFIC_LIGHTS.0,
            views::titlebar::TRAFFIC_LIGHTS.1,
        ))
        // Deliver bare Shift/Ctrl/… press+release to `on_key` so the EQ number
        // drags can fine-tune while Shift is held (the `Drag` event carries no
        // modifiers). `handle_key` only acts on the modifier state, ignoring the
        // keys themselves.
        .modifier_keys()
        // System-wide transport hotkeys (fire even when the window is unfocused).
        // Dedicated hardware media keys + OS "Now Playing" controls are a separate
        // integration (souvlaki) — these are user-reachable chords in the meantime.
        .global_shortcut("ctrl+alt+space", |s: &mut State, _: &mut Tx| {
            s.host.send(Command::TogglePlay)
        })
        .global_shortcut("ctrl+alt+right", |s: &mut State, _: &mut Tx| {
            s.host.send(Command::Next)
        })
        .global_shortcut("ctrl+alt+left", |s: &mut State, _: &mut Tx| {
            s.host.send(Command::Prev)
        })
        .init(move || {
            let mut s = State::new(init_host.clone());
            s.crashed_last_session = crashed_last_session;
            // Set from the start (not in the first-frame update that kicks the scan off): a
            // file that launched the app arrives during AppKit's launch, before that frame.
            s.startup_scan_pending = needs_startup_scan;
            s
        })
        .on_ready({
            // Start the OS media session + system tray once the window (and its
            // HWND, for Windows SMTC) exists. `on_ready` may fire more than once —
            // guard it so we don't spawn duplicates.
            let cell = media_ah_cell.clone();
            // Shared by the tray and the media session; the session thread reads
            // cover art itself (`host.art`) so that disk I/O stays off the UI thread.
            let tray_host = host.clone();
            let mut started = false;
            move |wh: &rune::WindowHandle| {
                if started {
                    return;
                }
                let Some(ah) = cell.get().cloned() else {
                    return;
                };
                started = true;
                // Keep the handle so `flush_on_exit` can read the final geometry.
                // Set it synchronously (a queued `ah.update` from `on_ready` may not
                // apply before exit), via a static since the exit hooks get only
                // `&mut State`.
                if let Ok(mut w) = WINDOW.lock() {
                    *w = Some(wh.clone());
                }
                let hwnd = window_hwnd(wh);
                if let Some(tx) = media_session::spawn(ah.clone(), tray_host.clone(), hwnd) {
                    ah.update(move |s: &mut State, _tx: &mut Tx| s.media_tx = Some(tx));
                }
                // The tray must stay alive for the app's lifetime (dropping it
                // removes the icon); it's an app-lifetime singleton, so leak it.
                // Not on macOS: Rune has no macOS tray backend (it would only log
                // "Unsupported"); the native Controls + Dock menus (`macos_menu`) carry
                // the same transport there. A menu-bar extra is on the roadmap.
                #[cfg(not(target_os = "macos"))]
                if let Some(tray) = systray::create(tray_host.clone(), ah.clone()) {
                    std::mem::forget(tray);
                }
            }
        })
        // Flush on the real exit paths: a window close (Windows/Linux → quit) and
        // an app Quit / OS logout (Cmd+Q, `WM_ENDSESSION`), where the run loop may
        // not return cleanly.
        .on_close(|s: &mut State, _tx: &mut Tx| flush_on_exit(s))
        .on_quit(|s: &mut State, _tx: &mut Tx| flush_on_exit(s))
        // Track focus so now-playing toasts fire only while backgrounded.
        .on_focus(|s: &mut State, focused: bool, _tx: &mut Tx| s.focused = focused);
    let ah = app.handle();
    let _ = media_ah_cell.set(app.handle());
    // kill / Ctrl-C / a closed terminal quit like ⌘Q (quit hooks run, no crash report).
    #[cfg(unix)]
    {
        let qh = ah.clone();
        signal_quit::spawn(move |_| qh.quit());
    }
    // Output devices + their transports, enumerated OFF the UI thread and refreshed on a gentle
    // self-paced loop. cpal's WASAPI enumeration can block for several seconds (a Bluetooth
    // endpoint especially), so it must never sit on the init/first-paint path — that was the
    // multi-second blocked first paint. The loop then re-enumerates so plugging or unplugging a
    // device updates the picker automatically; it only posts an update when the list actually
    // changed, so it never churns the UI, and it sleeps AFTER each pass so a slow enumeration
    // can't overlap itself.
    {
        let dh = ah.clone();
        let dev_host = host.clone();
        std::thread::spawn(move || {
            type DeviceSnapshot = (
                Vec<String>,
                std::collections::HashMap<String, soul_player_host::device_transport::Transport>,
            );
            let mut last: Option<DeviceSnapshot> = None;
            loop {
                let names = dev_host.output_devices();
                let transports = soul_player_host::device_transport::query();
                let snapshot: DeviceSnapshot = (names, transports);
                if last.as_ref() != Some(&snapshot) {
                    last = Some(snapshot.clone());
                    let (names, transports) = snapshot;
                    dh.update(move |s: &mut State, _tx: &mut Tx| {
                        s.output_devices = names;
                        if !transports.is_empty() {
                            s.output_transports = transports;
                        }
                        // The first enumeration has landed — stop showing the "scanning" row.
                        s.output_devices_loading = false;
                    });
                }
                std::thread::sleep(OUTPUT_DEVICE_REFRESH_INTERVAL);
            }
        });
    }
    // Stash a self-handle in State (once, on the main thread) so the managed-library
    // organize flow can post plan/apply progress back from its worker threads. This
    // self-posting update runs on the first frame; `update` only borrows `ah`, so it
    // stays available for the subscribe below.
    {
        let h = ah.clone();
        ah.update(move |s: &mut State, _tx: &mut Tx| {
            s.app_handle = Some(h);
            // Now that the window is up (first frame painted on the count-0 Sources
            // list + empty library), run the restored library's first scan OFF the
            // main thread. Results fold in via `apply_and_save` and the title-bar
            // activity widget animates meanwhile — no blocked first paint (#102).
            if needs_startup_scan {
                scan_in_background(s, |host| host.rescan_sources(), apply_startup_scan);
            }
            // Reconnect every persisted remote source (Inc 5), also off the main
            // thread — same "don't block the first paint" reasoning as the local
            // startup scan just above, since each reconnect does a blocking
            // refresh-token round-trip (plus a catalog re-fetch) over the network.
            // Before the remote reconnect: that path can reach the OS keychain,
            // and on a session with no Secret Service D-Bus spends up to two
            // minutes trying to ACTIVATE one. Playback has nothing to do with
            // remotes and shouldn't queue behind that.
            autoplay_from_env(s);
            reconnect_remotes_on_startup(s);
        });
    }
    // Audio files among the launch arguments (CLI, and file associations once Rune can
    // deliver Finder opens — anomalyco/rune#157). Queued after the first-frame update above,
    // so `app_handle` is set by then.
    {
        let args = open_files::from_args(std::env::args().skip(1));
        if !args.is_empty() {
            ah.update(move |s: &mut State, _tx: &mut Tx| open_files::open(s, args));
        }
    }
    // Periodic remote-health poller (Inc 2): its own dedicated sleep-loop
    // thread — never the UI thread — wakes every ~30 s and asks the UI thread
    // to refresh. It posts through the SAME guarded `refresh_remote_health`
    // the on-demand refresh (opening Settings → Music Data) uses, so the two
    // can never have overlapping probes in flight. Runs for the app's whole
    // lifetime (no retained `JoinHandle` to stop it — it simply stops mattering
    // once the process exits).
    {
        let ph = ah.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(REMOTE_HEALTH_POLL_INTERVAL);
                ph.update(|s: &mut State, _tx: &mut Tx| refresh_remote_health(s));
            }
        });
    }
    // Launch-time update check (release builds only): off-thread so it never
    // blocks startup, and it only *flags* an available update — Settings offers
    // the actual "Install & Restart" so we never yank a playing track away
    // (unless `SOUL_PLAYER_AUTO_INSTALL_UPDATE` asks to install it unattended).
    if update::enabled() {
        let uh = app.handle();
        std::thread::spawn(move || match update::available() {
            Some(version) => {
                tracing::info!(version, "update available");
                notify::update_available(&version);
                if update::auto_install() {
                    install_update(uh.clone());
                }
                uh.update(move |s: &mut State, _tx: &mut Tx| {
                    s.update_available = Some(version);
                });
            }
            None => tracing::debug!("no update available"),
        });
    }
    host.subscribe(move |ev: Event| {
        // Persist a completed background analysis to the on-disk cache (best-effort,
        // off the UI thread) so it seeds the NEXT launch; the fold into playback is
        // the backend's, already in its in-memory cache.
        if let Event::TrackLoudnessAnalyzed { path, loudness } = &ev {
            config::record_loudness(path, loudness);
        }
        ah.update(move |s: &mut State, tx: &mut Tx| {
            s.apply(&ev);
            feed_media_session(s); // publish now-playing changes to the OS session
            tx.instant(); // don't animate fast-moving numeric fields
        });
    });
    // Live filesystem watcher: when files change under a watched root while the app
    // is open, the host runs a debounced background rescan and hands us the refreshed
    // library/Sources via this callback — applied on the UI thread like any other
    // watch update. The host re-arms the watcher itself as sources are added/removed.
    {
        let wh = app.handle();
        host.on_watch_update(move |up: WatchUpdate| {
            wh.update(move |s: &mut State, _tx: &mut Tx| {
                apply_watch(s, up);
            });
        });
        // Live scan progress → the title-bar activity widget. The host sends `Some(p)`
        // per tick while an off-UI-thread scan runs (the watcher, a threaded add/
        // rescan) and a terminal `None` when it ends — show and hide on one ordered
        // stream, so the widget can't be left stuck. Registered before `start_watching`
        // so the first watcher-triggered rescan is already observed.
        let sph = app.handle();
        host.on_scan_progress(move |p: Option<ScanProgress>| {
            sph.update(move |s: &mut State, _tx: &mut Tx| {
                s.scan_progress = p;
            });
        });
        host.start_watching();
    }
    // The identity the window advertises to the desktop: Wayland `app_id`, X11
    // `WM_CLASS`. Without it the compositor reports an EMPTY class, and nothing
    // that identifies a window by it works — the dock and task switcher show a
    // generic icon instead of ours, and `windowrule`-style rules can't target
    // the app at all.
    //
    // The value must equal the basename of the shipped `.desktop` entry, which
    // is how a compositor resolves a window to its icon — see [`desktop_id`].
    rune::set_app_id(desktop_id());
    // Window + taskbar icon: the Soul Player logo, set at runtime (the packaged
    // installer uses the same icons via [package.metadata.packager]).
    // macOS: the tiled Dock icon (matches the bundle's .icns); elsewhere the bare mark.
    #[cfg(target_os = "macos")]
    let icon: &[u8] = include_bytes!("../icons/macos/icon-512.png");
    #[cfg(not(target_os = "macos"))]
    let icon: &[u8] = include_bytes!("../icons/icon.png");
    if let Err(e) = rune::set_app_icon(icon.to_vec()) {
        tracing::warn!("set_app_icon failed: {e}");
    }
    app.run();
    // Backstop: `flush_on_exit` (wired to on_close/on_quit) normally marks the
    // clean exit first; this covers the rare path where the run loop returns
    // without either firing. `end_session` is idempotent.
    logging::end_session();
    // "Install & Restart" quit us to get here: now that every quit hook has run,
    // put the verified update in place and start it (no-op without one).
    update::apply_pending();
    Ok(())
}

/// "Install & Restart": download and verify the update off the UI thread, then
/// quit normally; `main` installs it once the run loop has returned (see
/// [`update::apply_pending`]). A failed download leaves the app running.
pub(crate) fn install_update(ah: rune::AppHandle<State>) {
    std::thread::spawn(move || match update::download() {
        Ok(Some(version)) => {
            tracing::info!(
                version,
                "update downloaded and verified; quitting to install it"
            );
            ah.quit();
        }
        Ok(None) => tracing::info!("no update to install"),
        Err(e) => tracing::error!("update download failed: {e}"),
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use rune::{Cursor, Env, FileDialogKind, Mods, Runtime, WheelEvent};
    use soul_player_host::{RemoteHealth, SourceKind};

    use super::*;
    // The profile model types are only used by the profile tests (production code
    // reaches them through `soul_player_host::` full paths).
    use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH, STRIP_WIDTH};
    use soul_player_host::{DspChain, Profile, ProfileSet};

    fn testdata_dir() -> PathBuf {
        // app/rune -> repo-root/testdata
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
    }

    /// Cold launch, no library: the empty-state's Open Folder button
    /// (`views::library::track_list`) is the only entry point, so it must be
    /// findable headlessly — `Runtime::new` + `rt.frame` + `rt.find`, the
    /// Rust-level harness `examples/hello`'s own test module uses
    /// (cheat-sheet §8). This is the `app/rune` head's whole
    /// headless-testable claim, proven with no window and no device.
    #[test]
    fn empty_library_points_to_sources() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            State::new(PlayerHandle::offline()),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.frame(0.0);
        assert!(
            rt.find("go-to-sources").is_some(),
            "an empty library routes the user to Sources"
        );
        assert!(
            rt.find("open-folder").is_none(),
            "the single-folder Open Folder button is gone"
        );
    }

    /// A running scan paints the title-bar activity widget; a wide window shows the
    /// full set (icon + label + bar + count).
    #[test]
    fn scan_progress_shows_the_titlebar_activity_widget() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.scan_progress = Some(ScanProgress {
            phase: soul_player_host::ScanPhase::Extracting,
            done: 1024,
            total: 1210,
        });
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        // Two frames: the second reads the band's committed width, so the
        // responsive branch sees the real desktop band (window − sidebar), not
        // the first-frame fallback.
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("titlebar-scan").is_some(),
            "the activity widget shows while a scan runs"
        );
        assert!(rt.find("titlebar-scan-icon").is_some(), "icon present");
        assert!(
            rt.find("titlebar-scan-label").is_some(),
            "wide window shows the phase label"
        );
        assert!(
            rt.find("titlebar-scan-bar").is_some(),
            "progress bar present"
        );
        assert!(
            rt.find("titlebar-scan-count").is_some(),
            "done/total count present"
        );
    }

    /// Linux has no native window controls under a Rune custom titlebar (Rune
    /// places them on macOS/Windows only), and on Wayland — where a tiling
    /// compositor draws no titlebar at all — that left the window with NO way to
    /// minimize, maximize or close from the UI. The band must draw its own.
    ///
    /// Clicking each one (rather than only asserting it exists) is the point:
    /// the buttons sit INSIDE the `.window_drag()` strip, so the real risk is
    /// that the drag region swallows the press and the button is inert. A click
    /// that lands and runs its handler proves the child kept its press.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_draws_working_caption_buttons_in_the_titlebar() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = State::new(PlayerHandle::offline());
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        // Two frames: the band's responsive branch reads its own committed
        // width from the previous frame (same reason the scan-widget test does).
        rt.frame(0.0);
        rt.frame(0.0);

        for key in ["caption-min", "caption-max", "caption-close"] {
            let at = rt
                .find(key)
                .unwrap_or_else(|| panic!("{key} should be drawn on Linux"));
            assert!(
                at.0 > 900.0,
                "{key} should sit in the band's trailing edge, found at x={}",
                at.0
            );
            // Drives a full move/down/up at the button and re-renders. The
            // window request itself goes to the run loop, which a headless
            // Runtime has no view of — so this asserts the press is RECEIVED,
            // not that the window minimized.
            click_key(&mut rt, key);
        }
    }

    /// The app-drawn caption button must RE-LABEL itself while maximized, not
    /// just redraw: "Maximize" announced by a button that will restore is the
    /// same defect as the frozen glyph, and it is the half a screen-reader user
    /// actually hits. Exercises the real wiring through the view, unlike
    /// `titlebar`'s unit tests of the two pure helpers.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_maximize_button_relabels_itself_while_maximized() {
        let _locale = config::use_test_locale("en");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let label_of = |maximized: bool| -> String {
            let mut s = populated_state();
            s.maximized = maximized;
            let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
            rt.frame(0.0);
            rt.settle_all();
            rt.frame(0.0);
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some("caption-max"))
                .and_then(|e| e.label)
                .unwrap_or_else(|| {
                    panic!("caption-max should carry a label (maximized={maximized})")
                })
        };
        assert_eq!(
            label_of(false),
            "Maximize",
            "a normal window offers maximize"
        );
        assert_eq!(
            label_of(true),
            "Restore",
            "a maximized window offers restore"
        );
    }

    /// With no scan in flight the widget is absent (the title bar is the plain band).
    #[test]
    fn no_scan_hides_the_titlebar_activity_widget() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = State::new(PlayerHandle::offline()); // scan_progress defaults to None
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("titlebar-scan").is_none(),
            "no activity widget when nothing is scanning"
        );
    }

    /// At the smallest widths the widget collapses to just the animated icon — the
    /// label, bar, and count drop as the window narrows.
    #[test]
    fn narrow_window_collapses_activity_to_icon_only() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.scan_progress = Some(ScanProgress {
            phase: soul_player_host::ScanPhase::Extracting,
            done: 10,
            total: 100,
        });
        let mut rt = Runtime::new(s, view, (360.0, 720.0), env);
        // Two frames: the second reads the first's committed band width, so the
        // width-responsive branch sees the real (narrow) band, not the fallback.
        // At 360 the window is mobile, so the band spans the whole width.
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("titlebar-scan-icon").is_some(),
            "the pulsing icon survives at the smallest width"
        );
        assert!(
            rt.find("titlebar-scan-label").is_none(),
            "the label drops when narrow"
        );
        assert!(
            rt.find("titlebar-scan-bar").is_none(),
            "the bar drops when narrow"
        );
        assert!(
            rt.find("titlebar-scan-count").is_none(),
            "the count drops at the smallest width"
        );
    }

    /// A finished scan retires the widget: once the scan completes, `scan_progress`
    /// clears (the host's terminal `None`, mirrored here by `apply_watch` — the
    /// WatchUpdate path) and the widget, shown mid-scan, disappears. Guards the
    /// "doesn't hide after the scan finishes" regression.
    #[test]
    fn a_finished_scan_retires_the_activity_widget() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.scan_progress = Some(ScanProgress {
            phase: soul_player_host::ScanPhase::Extracting,
            done: 50,
            total: 100,
        });
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("titlebar-scan").is_some(),
            "the widget shows mid-scan"
        );
        // The scan completes → the WatchUpdate path clears scan_progress.
        rt.update(|s: &mut State, _tx: &mut Tx| apply_watch(s, WatchUpdate::default()));
        rt.frame(0.0);
        assert!(
            rt.find("titlebar-scan").is_none(),
            "a finished scan retires the activity widget"
        );
    }

    /// The scan widget docks into the title-bar band just left of the trailing
    /// Windows caption buttons — the band's own grow-spacer right-aligns it and the
    /// caption-glyph cells reserve the trailing gutter, so it never overlaps the
    /// controls (nor runs off the leading edge). Swept across window widths AND
    /// sidebar widths (the headless equivalent of resizing both): keying the dock off
    /// the band's own width (`window − sidebar`) rather than the full window is the
    /// whole point, so a wide or collapsed sidebar must not push the widget into the
    /// controls. Asserts the widget stays inside the band and clears the gutter.
    #[test]
    fn activity_widget_clears_the_window_controls_across_widths() {
        use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH};
        const TRAILING: f32 = 138.0; // Windows caption buttons
        // Narrow desktop pane → default → wide → maximized-ish, each paired with a
        // default, a maxed-out, and a collapsed sidebar. All widths ≥ the mobile
        // breakpoint so the desktop shell (sidebar + band) is in play.
        for width in [760.0_f32, 1100.0, 1400.0, 1920.0] {
            for (sidebar_width, collapsed) in [
                (DEFAULT_WIDTH, false),
                (MAX_WIDTH, false),
                (DEFAULT_WIDTH, true),
            ] {
                let env = Env {
                    system_dark: true,
                    scale: 2.0,
                };
                let mut s = State::new(PlayerHandle::offline());
                s.sidebar_width = sidebar_width;
                s.sidebar_collapsed = collapsed;
                s.scan_progress = Some(ScanProgress {
                    phase: soul_player_host::ScanPhase::Extracting,
                    done: 500,
                    total: 1000,
                });
                let mut rt = Runtime::new(s, view, (width, 720.0), env);
                let mut insets = rune::ChromeInsets::default();
                insets.trailing = TRAILING;
                insets.height = views::titlebar::HEIGHT;
                rt.set_chrome_insets(insets);
                rt.frame(0.0);
                rt.frame(0.0); // settle the band width → the widget's dock
                let ctx = format!("width={width} sidebar={sidebar_width} collapsed={collapsed}");
                let (scan, _) = rt
                    .presented("titlebar-scan")
                    .unwrap_or_else(|| panic!("the activity widget renders ({ctx})"));
                let (band, _) = rt
                    .presented("titlebar")
                    .unwrap_or_else(|| panic!("the title band renders ({ctx})"));
                let band_right = band[0] + band[2];
                let scan_right = scan[0] + scan[2];
                // The band's right edge is the window's right edge (the shell is the
                // trailing column); the widget must end before the caption gutter.
                assert!(
                    scan_right <= band_right - TRAILING + 0.5,
                    "{ctx}: clears the trailing controls, scan_right={scan_right} band_right={band_right}"
                );
                // …and stay inside the band on the leading side (never over the sidebar).
                assert!(
                    scan[0] >= band[0] - 0.5,
                    "{ctx}: stays inside the band, scan_left={} band_left={}",
                    scan[0],
                    band[0]
                );
            }
        }
    }

    /// Two enabled remotes, both online: the title-bar indicator shows and its
    /// status dot reads the healthy/green color (`remote_health_summary`'s
    /// `Healthy` case, see `views/titlebar.rs`'s own pure unit tests for the
    /// aggregation logic itself — this proves the RENDERED indicator reflects it).
    #[test]
    fn remote_health_indicator_is_green_when_every_enabled_remote_is_online() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sources.push(online_remote_status("http://a.test"));
        s.sources.push(online_remote_status("http://b.test"));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("remote-health-indicator").is_some(),
            "the indicator shows with enabled remotes present"
        );
        let p = theme::palette(true);
        assert_eq!(
            el_info(&rt, "remote-health-dot").fill,
            p.online,
            "every enabled remote online reads as the healthy/green color"
        );
    }

    /// One online, one offline (both enabled): the indicator reads the
    /// partial/amber color.
    #[test]
    fn remote_health_indicator_is_amber_when_some_enabled_remotes_are_offline() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sources.push(online_remote_status("http://a.test"));
        s.sources.push(offline_remote_status("http://b.test"));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        let p = theme::palette(true);
        assert_eq!(
            el_info(&rt, "remote-health-dot").fill,
            p.warning,
            "a mix of online and offline enabled remotes reads as the partial/amber color"
        );
    }

    /// Every enabled remote offline: the indicator reads the down/red color.
    #[test]
    fn remote_health_indicator_is_red_when_every_enabled_remote_is_offline() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sources.push(offline_remote_status("http://a.test"));
        s.sources.push(offline_remote_status("http://b.test"));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        let p = theme::palette(true);
        assert_eq!(
            el_info(&rt, "remote-health-dot").fill,
            p.danger,
            "every enabled remote offline reads as the down/red color"
        );
    }

    /// No remote sources at all — neither none configured, nor local-only —
    /// shows no indicator: there's nothing remote to report health on.
    #[test]
    fn remote_health_indicator_is_absent_with_no_remote_sources() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = State::new(PlayerHandle::offline());
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("remote-health-indicator").is_none(),
            "no sources at all → no indicator"
        );

        let dir = testdata_dir();
        let mut s2 = State::new(PlayerHandle::offline());
        let up = s2.host.add_folder(&dir).expect("add_folder");
        apply_watch(&mut s2, up);
        let mut rt2 = Runtime::new(s2, view, (1100.0, 720.0), env);
        rt2.frame(0.0);
        assert!(
            rt2.find("remote-health-indicator").is_none(),
            "local-only sources → still no indicator (locals have no health concept)"
        );
    }

    /// The scan widget and the remote-health indicator are independent and must
    /// coexist in the same title-bar dock (the shared `content` row's grow-spacer
    /// + gap) rather than one hiding the other.
    #[test]
    fn scan_widget_and_remote_health_indicator_coexist_in_the_titlebar() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sources.push(online_remote_status("http://a.test"));
        s.scan_progress = Some(ScanProgress {
            phase: soul_player_host::ScanPhase::Extracting,
            done: 10,
            total: 100,
        });
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("titlebar-scan").is_some(),
            "the scan widget still renders"
        );
        assert!(
            rt.find("remote-health-indicator").is_some(),
            "the remote-health indicator renders alongside it"
        );
    }

    /// Pressing the indicator opens the popover, which lists EVERY remote (not
    /// just the unhealthy ones) with its own status: the online remote's row
    /// reads a real latency, the offline one reads the "—" placeholder — and the
    /// popover offers a way to jump to Settings → Music Data.
    #[test]
    fn remote_health_popover_lists_every_remote_with_its_status() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sources.push(online_remote_status("http://a.test"));
        s.sources.push(offline_remote_status("http://b.test"));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("remote-health-popover").is_none(),
            "the popover is closed initially"
        );

        click_key(&mut rt, "remote-health-indicator");
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.state.remote_health_open,
            "pressing the indicator opens the popover"
        );
        assert!(
            rt.find("remote-health-popover").is_some(),
            "the popover renders"
        );
        assert!(rt.find("remote-health-row-0").is_some(), "row 0 renders");
        assert!(rt.find("remote-health-row-1").is_some(), "row 1 renders");
        assert_eq!(
            el_info(&rt, "remote-health-row-latency-0").text.as_deref(),
            Some("42ms"),
            "the online remote's row shows a real latency"
        );
        assert_eq!(
            el_info(&rt, "remote-health-row-latency-1").text.as_deref(),
            Some("—"),
            "the offline remote's row shows the dash placeholder"
        );
        assert!(
            rt.find("remote-health-open-settings").is_some(),
            "the popover offers a way to jump to Settings"
        );

        // Jumping to settings closes the popover and routes there.
        click_key(&mut rt, "remote-health-open-settings");
        assert!(matches!(rt.state.route, Route::Settings));
        assert_eq!(rt.state.settings_tab, SettingsTab::MusicData);
        assert!(
            !rt.state.remote_health_open,
            "jumping to settings closes the popover"
        );
    }

    /// A DISABLED remote still lists in the popover (it's configured, just
    /// turned off) — it must never silently vanish the way it does from the
    /// aggregate indicator's color calculation.
    #[test]
    fn remote_health_popover_still_lists_a_disabled_remote() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sources.push(online_remote_status("http://a.test"));
        let mut disabled = offline_remote_status("http://b.test");
        disabled.enabled = false;
        s.sources.push(disabled);
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);

        click_key(&mut rt, "remote-health-indicator");
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.find("remote-health-row-1").is_some(),
            "the disabled remote still gets a row"
        );
    }

    #[test]
    fn new_state_starts_on_library_with_no_sources() {
        let s = State::new(PlayerHandle::offline());
        assert!(matches!(s.route, Route::Library));
        assert!(s.sources.is_empty());
    }

    #[test]
    fn new_state_marks_unavailable_restored_roots_offline() {
        let missing = testdata_dir().join("does-not-exist-at-launch");
        let host = PlayerHandle::offline();
        host.set_folders(std::slice::from_ref(&missing)).unwrap();
        let s = State::new(host);
        assert_eq!(s.sources.len(), 1);
        assert_eq!(s.sources[0].path, missing);
        assert!(
            !s.sources[0].available,
            "a restored, currently-unavailable root is offline at launch"
        );
    }

    #[test]
    fn apply_watch_folds_sources_into_state() {
        let mut s = State::new(PlayerHandle::offline());
        assert!(s.sources.is_empty());
        // add_folder scans the committed testdata (real audio) and returns the
        // WatchUpdate the UI folds in.
        let up = s.host.add_folder(&testdata_dir()).expect("add_folder");
        apply_watch(&mut s, up);
        assert_eq!(s.sources.len(), 1);
        assert_eq!(s.sources[0].path, testdata_dir());
        assert!(s.sources[0].enabled && s.sources[0].available);
        assert!(!s.library.is_empty(), "testdata scanned into the library");
    }

    /// #102: a restored library paints before it is scanned. `configure_sources`
    /// seeds the Sources list (count 0) with no scan, so the first frame has
    /// something to render and an empty library; the deferred off-thread scan then
    /// folds the real tracks + counts in. Guards against the startup scan creeping
    /// back onto the first-paint path.
    #[test]
    fn restored_sources_paint_before_the_deferred_scan() {
        let host = PlayerHandle::offline();
        host.configure_sources(&[(testdata_dir(), true)]);
        let s = State::new(host);
        // First paint: the source is listed (count 0, loading) but nothing scanned.
        assert_eq!(s.sources.len(), 1);
        assert_eq!(s.sources[0].path, testdata_dir());
        assert_eq!(s.sources[0].track_count, 0, "count 0 before the scan");
        assert!(
            s.library.is_empty(),
            "no blocking scan — the library is empty until the deferred scan runs"
        );
        // The deferred scan (synchronous here) folds the real library + counts in.
        let mut s = s;
        let up = s.host.rescan_sources().expect("deferred rescan");
        apply_watch(&mut s, up);
        assert!(
            !s.library.is_empty(),
            "the deferred scan populates the library"
        );
        assert!(
            s.sources[0].track_count > 0,
            "real counts replace the loading 0"
        );
    }

    /// The now-playing toast fires once per genuine track change: `feed_media_session`
    /// advances `notify_last_id` on a new track and dedups repeat events for the same
    /// one (the toast itself is cfg!(test)-suppressed; this covers the change detection).
    #[test]
    fn now_playing_detection_fires_once_per_track() {
        let mut s = State::new(PlayerHandle::offline());
        let up = s.host.add_folder(&testdata_dir()).expect("add_folder");
        apply_watch(&mut s, up);
        let first = s.library.first().map(|t| t.id).expect("a scanned track");

        s.now_playing_id = Some(first);
        feed_media_session(&mut s);
        assert_eq!(
            s.notify_last_id,
            Some(first),
            "new track advances the marker"
        );

        feed_media_session(&mut s);
        assert_eq!(s.notify_last_id, Some(first), "same track does not re-fire");

        s.now_playing_id = None;
        feed_media_session(&mut s);
        assert_eq!(s.notify_last_id, None, "stopping advances the marker back");
    }

    /// A `State` routed to Settings seeded with a given source set. Each source's
    /// availability is now derived by the host (a path that isn't a readable dir
    /// reads as offline), so the second argument is retained only for call-site
    /// compatibility and no longer drives the offline state.
    fn settings_state(folders: &[PathBuf], _unavailable: &[PathBuf]) -> State {
        let host = PlayerHandle::offline();
        if !folders.is_empty() {
            let _ = host.set_folders(folders);
        }
        let mut s = State::new(host);
        pin_persisted_layout(&mut s);
        s.route = Route::Settings;
        s
    }

    /// Reset the layout settings `State::new` loads from the SHARED test config dir —
    /// UI zoom and sidebar geometry — to their defaults. Another test persisting e.g.
    /// a Ctrl+= zoom concurrently would otherwise leak into a layout assertion (seen
    /// in CI: the every-locale sweep rendered Genres at ~1.1× zoom and overflowed the
    /// 900 px window). Tests that exercise those settings set them explicitly anyway.
    fn pin_persisted_layout(s: &mut State) {
        s.ui_zoom = 1.0;
        s.sidebar_width = views::sidebar::DEFAULT_WIDTH;
        s.sidebar_collapsed = false;
    }

    /// Click the element with `key`: find its position, drive a full
    /// move/down/up pointer gesture there, then re-render. The harness-level
    /// analogue of a real click, for e2e navigation tests.
    fn click_key(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt
            .find(key)
            .unwrap_or_else(|| panic!("no element with key {key:?} to click"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    /// Walk every laid-out node and return human-readable offenders: a visible
    /// node whose box leaves the window horizontally, or a text node an ancestor
    /// clips horizontally. Both are the signature of a label or action row a
    /// longer locale overflowed because it didn't wrap or shrink. We judge the
    /// horizontal axis only — vertical overflow is legitimate (the settings pane
    /// and the queue scroll), so comparing `rect` against `visible`/the window on
    /// x alone keeps scrolled content from reading as a failure.
    fn horizontal_overflows(rt: &Runtime<State>, viewport_w: f32) -> Vec<String> {
        const TOL: f32 = 1.0;
        let mut bad = Vec::new();
        for e in rt.elements() {
            // A node mid-exit or faded out is allowed off-stage.
            if e.exiting || e.opacity <= 0.01 {
                continue;
            }
            let [x, _y, w, _h] = e.rect;
            let id = e.key.clone().unwrap_or_else(|| e.path.clone());
            let shows = e.text.as_deref().unwrap_or("");
            // (a) the drawn box must stay inside the window horizontally.
            if x < -TOL || x + w > viewport_w + TOL {
                bad.push(format!(
                    "off-window [{id}] text={shows:?} x={x:.0} w={w:.0} (window {viewport_w:.0})"
                ));
                continue;
            }
            // (b) text must not be horizontally clipped by an ancestor: `visible`
            // (what ancestors' clips leave) must still span the drawn width.
            if !shows.is_empty() {
                let [vx, _vy, vw, _vh] = e.visible;
                if vx > x + TOL || vw + TOL < w {
                    bad.push(format!(
                        "clipped text [{id}] {shows:?} rect_w={w:.0} visible_w={vw:.0}"
                    ));
                }
            }
        }
        bad
    }

    /// Settings must survive every shipped locale without a label or action row
    /// overflowing the window or being clipped — German and Spanish run far
    /// longer than the English the layout was first sized against. Renders each
    /// category in each locale through the real `view` and asserts the layout
    /// contains itself. This is the regression net for i18n text robustness.
    #[test]
    fn settings_layout_contains_itself_in_every_locale() {
        // Hold the locale lock across the whole sweep: this test flips the
        // global locale per iteration, so it must not interleave with a test
        // asserting a specific localised string (see `use_test_locale`).
        let _locale = config::use_test_locale("en");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let tabs = [
            ("Appearance", SettingsTab::Appearance),
            ("MusicData", SettingsTab::MusicData),
            ("Audio", SettingsTab::Audio),
            ("Keyboard", SettingsTab::Keyboard),
            ("About", SettingsTab::About),
        ];
        // A roomy window and a cramped one: the narrow pass is where button rows
        // (theme / language / ReplayGain) must wrap rather than overflow.
        let viewports = [(1100.0_f32, 720.0_f32), (900.0, 640.0)];
        // One watched folder so the Music Data source row (name + remove ✕) is
        // exercised too, not just the empty state.
        let folder = testdata_dir().join("a-reasonably-long-source-folder-name");
        let mut failures = Vec::new();
        for &viewport in &viewports {
            for &loc in LOCALES {
                for (tab_name, tab) in tabs {
                    let mut s = settings_state(std::slice::from_ref(&folder), &[]);
                    s.settings_tab = tab;
                    // AFTER State::new, which re-applies the persisted locale over ours.
                    apply_locale(loc);
                    let mut rt = Runtime::new(s, view, viewport, env);
                    rt.frame(0.0);
                    for bad in horizontal_overflows(&rt, viewport.0) {
                        failures.push(format!("[{loc}/{tab_name}@{}] {bad}", viewport.0));
                    }
                }
            }
        }
        apply_locale("en"); // don't leak the locale into sibling tests
        assert!(
            failures.is_empty(),
            "{} settings layout overflow(s) across locales:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// A library scanned from the committed testdata, so Library / Albums / the
    /// queue render real rows (not just empty states) for the layout sweep.
    fn populated_state() -> State {
        let mut s = State::new(PlayerHandle::offline());
        pin_persisted_layout(&mut s);
        if let Ok(up) = s.host.add_folder(&testdata_dir()) {
            apply_watch(&mut s, up);
        }
        s
    }

    /// Unit 0's EQ (test convenience — every EQ test keeps unit 0 an EQ, and the
    /// first card is the one these tests drive). Replaces the old
    /// `eq_bands`/`eq_preamp`/`eq_enabled` mirror reads.
    fn active_eq(s: &State) -> &EqUnit {
        s.dsp_units[0].as_eq().unwrap()
    }

    /// Unit 0's EQ, mutable (for tests that seed bands/preamp/enable).
    fn active_eq_mut(s: &mut State) -> &mut EqUnit {
        s.dsp_units[0].as_eq_mut().unwrap()
    }

    /// Clicking the now-playing heart opens the add-to-playlist dialogue for the
    /// current track (sets `add_to_playlist_for`). Driven through the full app
    /// `view`; the panel-only harness in `views::player` does not route pointer
    /// events to the panel's buttons, so the click-through lives here.
    #[test]
    fn now_playing_heart_opens_the_add_to_playlist_dialogue() {
        let _g = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        let id = s
            .library
            .first()
            .map(|t| t.id)
            .expect("the test library has tracks");
        s.now_playing_id = Some(id);
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        click_key(&mut rt, "np-heart");
        assert_eq!(rt.state.add_to_playlist_for, Some(id));
    }

    /// Full heart -> dialogue -> toggle membership flow, driven end-to-end
    /// through the real app `view` (not the overlay-only harness in
    /// `views::add_to_playlist`, which already covers the toggle in
    /// isolation): clicking the now-playing heart opens the dialogue for the
    /// current track, pressing a playlist's pick row adds the track and
    /// persists it, and pressing the SAME row again removes it, all without
    /// the dialogue closing in between.
    #[test]
    fn e2e_heart_opens_dialogue_and_toggles_membership() {
        let _g = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        let id = s
            .library
            .first()
            .map(|t| t.id)
            .expect("the test library has tracks");
        s.now_playing_id = Some(id);
        let pid = s.playlists.create("Mix");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);

        click_key(&mut rt, "np-heart");
        rt.settle_all();
        rt.frame(0.0);
        assert_eq!(
            rt.state.add_to_playlist_for,
            Some(id),
            "the heart opens the add-to-playlist dialogue for the playing track"
        );

        let pick_key = format!("add-to-playlist-pick-{pid}");
        click_key(&mut rt, &pick_key);
        assert_eq!(
            playlists::PlaylistStore::load()
                .get(pid)
                .unwrap()
                .entries
                .len(),
            1,
            "the first press on the row adds the track to the playlist"
        );
        assert_eq!(
            rt.state.add_to_playlist_for,
            Some(id),
            "the dialogue stays open across the add"
        );

        click_key(&mut rt, &pick_key);
        assert_eq!(
            playlists::PlaylistStore::load()
                .get(pid)
                .unwrap()
                .entries
                .len(),
            0,
            "a second press on the same row removes the track again"
        );
        assert_eq!(
            rt.state.add_to_playlist_for,
            Some(id),
            "the dialogue stays open across the remove"
        );
    }

    /// End-to-end feed pipeline: as the player's now-playing state changes, the
    /// media session receives the right `Update`s over its channel — track
    /// metadata (resolved from the library) on a track change, playback on a
    /// play/pause flip, and `stopped` when nothing is playing.
    #[test]
    fn media_session_feed_publishes_track_then_playback_on_change() {
        let mut s = populated_state();
        let id = s
            .library
            .first()
            .map(|t| t.id)
            .expect("the test library has tracks");
        let expected = s.track(id).cloned().expect("the track resolves");
        let (tx, rx) = std::sync::mpsc::channel();
        s.media_tx = Some(tx);

        // A track starts playing → a Track update then a Playback update.
        // `play_state` is set alongside `playing` throughout: the media session
        // reads the three-way state to tell Paused from Stopped, so a fixture
        // that only flips the bool would describe a state the backend never
        // produces (playing, yet stopped).
        s.now_playing_id = Some(id);
        s.playing = true;
        s.play_state = PlayState::Playing;
        s.position_secs = 12.0;
        s.duration_secs = 200.0;
        feed_media_session(&mut s);

        match rx.try_recv().expect("a track-metadata update") {
            media_session::Update::Track(meta) => {
                assert_eq!(meta.title, expected.title.clone().unwrap_or_default());
                assert_eq!(meta.duration_secs, expected.duration_secs);
            }
            media_session::Update::Playback(_) => panic!("expected a Track update first"),
        }
        match rx.try_recv().expect("a playback update") {
            media_session::Update::Playback(pb) => {
                assert!(pb.playing && !pb.stopped);
                assert_eq!(pb.position_secs, 12.0);
            }
            media_session::Update::Track(_) => panic!("expected a Playback update"),
        }

        // Pausing the same track pushes only a playback update.
        s.playing = false;
        s.play_state = PlayState::Paused;
        feed_media_session(&mut s);
        match rx.try_recv().expect("a playback update on pause") {
            media_session::Update::Playback(pb) => assert!(!pb.playing && !pb.stopped),
            media_session::Update::Track(_) => panic!("expected Playback on pause"),
        }

        // Stopping a track that is STILL LOADED reports stopped, not paused —
        // MPRIS has a distinct `Stopped` status, and the loaded track means
        // `now_playing_id` is no help in telling the two apart.
        s.play_state = PlayState::Stopped;
        feed_media_session(&mut s);
        match rx.try_recv().expect("a playback update on stop") {
            media_session::Update::Playback(pb) => assert!(pb.stopped, "stop must report stopped"),
            media_session::Update::Track(_) => panic!("expected Playback on stop"),
        }

        // Unloading the track keeps reporting stopped.
        s.now_playing_id = None;
        feed_media_session(&mut s);
        match rx.try_recv().expect("a playback update on unload") {
            media_session::Update::Playback(pb) => assert!(pb.stopped),
            media_session::Update::Track(_) => panic!("expected Playback on unload"),
        }
        assert!(rx.try_recv().is_err(), "no extra updates queued");
    }

    /// The same contain-itself invariant as settings, but swept across the whole
    /// app — Library, Albums and a populated queue/now-playing panel — in every
    /// locale at two widths. This is the general i18n-overflow net; new views get
    /// coverage by adding a scene here.
    #[test]
    fn app_layout_contains_itself_in_every_locale() {
        // Hold the locale lock across the whole sweep (see `use_test_locale`):
        // this test cycles every locale and must not race a localised-string
        // assertion in a sibling test.
        let _locale = config::use_test_locale("en");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let viewports = [(1100.0_f32, 720.0_f32), (900.0, 640.0)];
        // Name → state tweak, the same `fn(&mut State)` shape as a `--scene`.
        type NamedScene = (&'static str, fn(&mut State));
        let scenes: [NamedScene; 5] = [
            ("library", |s: &mut State| s.route = Route::Library),
            ("albums", |s: &mut State| s.route = Route::Albums),
            ("genres", |s: &mut State| s.route = Route::Genres),
            // Populates now-playing + both queue bands from the library; the
            // sidebar panel then renders over whatever route is active.
            ("queue", scene_queue),
            ("settings-music", |s: &mut State| {
                s.route = Route::Settings;
                s.settings_tab = SettingsTab::MusicData;
            }),
        ];
        let mut failures = Vec::new();
        for &viewport in &viewports {
            for &loc in LOCALES {
                for (scene_name, scene) in scenes {
                    let mut s = populated_state();
                    scene(&mut s);
                    // AFTER State::new, which re-applies the persisted locale.
                    apply_locale(loc);
                    let mut rt = Runtime::new(s, view, viewport, env);
                    rt.frame(0.0);
                    for bad in horizontal_overflows(&rt, viewport.0) {
                        failures.push(format!("[{loc}/{scene_name}@{}] {bad}", viewport.0));
                    }
                }
            }
        }
        apply_locale("en");
        assert!(
            failures.is_empty(),
            "{} app layout overflow(s) across locales:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// The Task 11 completeness-plus-overflow safety net for the Playlists
    /// feature: the same contain-itself invariant
    /// `app_layout_contains_itself_in_every_locale` checks, but for the two
    /// routes that test's scene list doesn't cover: the Playlists list and a
    /// playlist's detail page. A fresh on-disk playlist is seeded through the
    /// real `PlaylistStore` (two of the testdata library's own tracks, so the
    /// hero's "{n} tracks · {duration}" string and a resolved track row both
    /// render, not just the empty state) under the shared test config dir.
    #[test]
    fn playlists_layout_contains_itself_in_every_locale() {
        // This test cycles the process-GLOBAL i18n locale over every locale, so it must
        // hold the locale lock (acquired FIRST, before the config lock, per the documented
        // ordering in config.rs) or it races sibling tests asserting exact English strings
        // — the known i18n flake. Its sibling sweepers (app_layout_/settings_layout_) do
        // the same.
        let _locale = config::use_test_locale("en");
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let viewports = [(1100.0_f32, 720.0_f32), (900.0, 640.0)];
        let mut failures = Vec::new();
        for &viewport in &viewports {
            for &loc in LOCALES {
                // A clean store per pass: exactly one playlist, so the list
                // page's row count (and thus its layout) doesn't grow across
                // iterations of this loop.
                let _ = std::fs::remove_dir_all(playlists::playlist_store_path());

                let mut list_state = populated_state();
                let seed_tracks: Vec<(u32, String, String, String, f64)> = list_state
                    .library
                    .iter()
                    .take(2)
                    .map(|t| {
                        (
                            t.id,
                            t.title.clone().unwrap_or_default(),
                            t.artist.clone().unwrap_or_default(),
                            t.album.clone().unwrap_or_default(),
                            t.duration_secs,
                        )
                    })
                    .collect();
                assert_eq!(
                    seed_tracks.len(),
                    2,
                    "the testdata library has at least two tracks to seed a playlist from"
                );
                let id = list_state.playlists.create("Road Trip");
                for (tid, title, artist, album, duration_secs) in seed_tracks {
                    if let Some(locator) = list_state.host.locator_of(tid) {
                        list_state.playlists.add_entry(
                            id,
                            playlists::PlaylistEntry {
                                locator,
                                snapshot: soul_player_host::TrackSnapshot {
                                    title,
                                    artist,
                                    album,
                                    duration_secs: duration_secs.round() as u32,
                                    track_no: None,
                                    disc_no: None,
                                },
                                mbid: None,
                            },
                        );
                    }
                }
                list_state.route = Route::Playlists;
                apply_locale(loc);
                let mut rt = Runtime::new(list_state, view, viewport, env);
                rt.frame(0.0);
                for bad in horizontal_overflows(&rt, viewport.0) {
                    failures.push(format!("[{loc}/playlists@{}] {bad}", viewport.0));
                }

                // The same seeded playlist's detail page, re-loaded from disk
                // (what the list pass above just persisted) the way a real
                // navigation would.
                let mut detail_state = populated_state();
                detail_state.route = Route::Playlist;
                detail_state.open_playlist = Some(id);
                apply_locale(loc);
                let mut rt = Runtime::new(detail_state, view, viewport, env);
                rt.frame(0.0);
                for bad in horizontal_overflows(&rt, viewport.0) {
                    failures.push(format!("[{loc}/playlist-detail@{}] {bad}", viewport.0));
                }
            }
        }
        apply_locale("en");
        assert!(
            failures.is_empty(),
            "{} playlists layout overflow(s) across locales:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// Whether any rendered element's text contains `needle`. Mirrors
    /// `views::playlists`' and `views::playlist_detail`'s own `shows`, redone
    /// here since those are private to their own test modules.
    fn shows(rt: &Runtime<State>, needle: &str) -> bool {
        rt.elements()
            .into_iter()
            .any(|e| e.text.as_deref().is_some_and(|t| t.contains(needle)))
    }

    /// Capstone end-to-end: the full user flow through the real `Runtime`, from
    /// an empty Playlists page to a playing playlist, over a real scanned
    /// `testdata/` library. Exercises every handoff Tasks 1-11 built in one
    /// pass: the real inline New-playlist field (click to focus, IME commit,
    /// Enter) persists "Mix" to the on-disk `PlaylistStore`; the shared
    /// `add_track_to_playlist` helper (the same call the Add-to-playlist
    /// overlay's pick list and its own New-playlist field both make) appends
    /// the library's first two tracks, in order; navigating into the playlist
    /// renders its name; and Play All issues a `PlayContext` carrying exactly
    /// those two resolved ids, in that order.
    #[test]
    fn e2e_new_playlist_add_tracks_then_play_all() {
        let _g = config::use_test_config_dir();
        let _ = std::fs::remove_dir_all(playlists::playlist_store_path());

        let host = PlayerHandle::recording();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        s.route = Route::Playlists;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        // Create "Mix" through the real inline New-playlist field: reveal it via
        // the top bar's "New playlist" button, click to focus, type via IME
        // commit, confirm with Enter (the same mechanics `views::playlists`' own
        // `trigger_new_playlist` test helper drives).
        click_key(&mut rt, "playlists-new");
        rt.frame(0.0);
        // The field drops in animated; settle so it is hittable/focusable.
        rt.settle_all();
        rt.frame(0.0);
        let (x, y) = rt
            .find("playlist-new-input")
            .expect("the new-playlist field renders");
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        rt.ime(rune::Ime::Commit("Mix".to_string()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);

        let pid = playlists::PlaylistStore::load().list()[0].id;

        // Add the library's first two tracks, in order, via the shared helper
        // production code's own "add to playlist" surfaces call
        // (`views::add_to_playlist::add_track_to_playlist`), bypassing the
        // overlay's click plumbing exactly as that module's own tests do.
        let ids: Vec<u32> = rt.state.library.iter().take(2).map(|t| t.id).collect();
        assert_eq!(
            ids.len(),
            2,
            "the scanned testdata library has at least two tracks"
        );
        for &tid in &ids {
            rt.update(move |st: &mut State, _tx: &mut Tx| {
                crate::views::add_to_playlist::add_track_to_playlist(st, tid, pid);
            });
        }
        rt.frame(0.0);

        // Navigate into the playlist: its name renders.
        rt.update(move |st: &mut State, _tx: &mut Tx| {
            navigate_to(st, Route::Playlist, None, None, None, Some(pid));
        });
        rt.frame(0.0);
        assert!(
            shows(&rt, "Mix"),
            "the playlist's name renders on its detail page"
        );

        // The freshly-navigated detail page's hero (and its Play All pill)
        // enter-animates in (opacity/position mid-flight on the first frame);
        // settle instantly so it's actually pickable, the same `settle_all`
        // pattern used elsewhere in this app (e.g. `playlist_detail.rs`'s own
        // delete-confirm test).
        rt.settle_all();
        rt.frame(0.0);

        // Play All issues PlayContext with both resolved ids, in entry order.
        click(&mut rt, "playlist-play-all");
        let cmd = rt
            .state
            .host
            .recorded_commands()
            .last()
            .cloned()
            .expect("Play All sent a command");
        match cmd {
            Command::PlayContext {
                ids: played,
                start: 0,
            } => assert_eq!(
                played, ids,
                "Play All plays exactly the two added tracks, in entry order"
            ),
            other => panic!("expected PlayContext starting at 0, got {other:?}"),
        }
    }

    /// Write a tiny but real PNG (just the 8-byte file signature; it never needs
    /// to decode, only to exist as a `.png` file under the custom-cover size
    /// cap) to a fresh temp dir, unique per call so parallel tests in this
    /// binary never collide. Mirrors `playlist_detail.rs`'s own `write_temp_png`
    /// test helper, redone here since that one is private to its own module.
    fn write_temp_cover_png() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let dir = std::env::temp_dir().join(format!(
            "soul_rune_playlist_cover_e2e_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.png");
        std::fs::write(&path, PNG_SIGNATURE).unwrap();
        path
    }

    /// Capstone end-to-end for the custom-cover feature, driven through the
    /// real `Runtime`: create a playlist over two of the testdata library's
    /// tracks and open it, press **Set cover** and answer the scripted
    /// open-file dialog with a real temp PNG (the `playlist-set-cover` →
    /// `tx.open_file` → `PlaylistStore::set_custom_cover` handoff
    /// `playlist_detail.rs`'s own scripted-dialog test already covers in
    /// isolation), then assert the detail hero renders the custom image
    /// (`playlist-hero-image`) AND, back on the Playlists list, the row's own
    /// thumbnail does too (`playlist-cover-{id}-custom`,
    /// `views::playlists::playlist_cover_el`'s `Custom` branch) AND the cover
    /// file persisted on disk. Then press **Remove cover** and assert
    /// everything reverts: the hero image is gone, `custom_cover_path` is
    /// `None` again, and the file itself is deleted.
    #[test]
    fn e2e_set_then_remove_a_custom_playlist_cover() {
        let _g = config::use_test_config_dir();
        let _ = std::fs::remove_dir_all(playlists::playlist_store_path());

        let mut s = populated_state();
        let seed_ids: Vec<u32> = s.library.iter().take(2).map(|t| t.id).collect();
        assert_eq!(
            seed_ids.len(),
            2,
            "the testdata library has at least two tracks to seed a playlist from"
        );
        let pid = s.playlists.create("Mix");
        for &tid in &seed_ids {
            let track = s.library.iter().find(|t| t.id == tid).unwrap().clone();
            let locator = s
                .host
                .locator_of(tid)
                .expect("a scanned track resolves a durable locator");
            s.playlists.add_entry(
                pid,
                playlists::PlaylistEntry {
                    locator,
                    snapshot: soul_player_host::TrackSnapshot {
                        title: track.title.clone().unwrap_or_default(),
                        artist: track.artist.clone().unwrap_or_default(),
                        album: track.album.clone().unwrap_or_default(),
                        duration_secs: track.duration_secs.round() as u32,
                        track_no: None,
                        disc_no: None,
                    },
                    mbid: None,
                },
            );
        }
        s.route = Route::Playlist;
        s.open_playlist = Some(pid);
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("playlist-hero-image").is_none(),
            "no custom cover yet, so the hero falls back to its default"
        );

        // Set cover: press the button, answer the scripted open-file dialog
        // with a real temp PNG, mirroring `playlist_detail.rs`'s own
        // scripted-dialog test and the EQ import/export e2e tests above.
        click_key(&mut rt, "playlist-set-cover");
        rt.frame(0.0);
        let (kind, _opts) = rt.file_dialog().expect("Set cover opens a file dialog");
        assert_eq!(kind, FileDialogKind::Open);
        let picked = write_temp_cover_png();
        rt.resolve_file_dialog(Some(picked)).unwrap();
        rt.frame(0.0);

        assert!(
            rt.find("playlist-hero-image").is_some(),
            "the detail hero now renders the custom cover image"
        );
        let cover_path = playlists::PlaylistStore::load()
            .custom_cover_path(pid)
            .expect("the picked image persisted as the playlist's custom cover");
        assert!(cover_path.exists(), "the stored cover file exists on disk");

        // Back to the Playlists list: the row's own thumbnail picks up the
        // same custom cover. The back button itself was part of the initial
        // mount (not freshly inserted), so it's clickable without settling;
        // the list page it lands on IS a fresh mid-session insert, so settle
        // before the next click.
        click_key(&mut rt, "detail-back");
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.find(&format!("playlist-cover-{pid}-custom")).is_some(),
            "the Playlists list row's thumbnail also renders the custom cover"
        );

        // Back into the detail page (a fresh mid-session insert again):
        // settle before pressing Remove cover.
        click_key(&mut rt, &format!("playlist-card-{pid}"));
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "playlist-remove-cover");

        assert!(
            rt.find("playlist-hero-image").is_none(),
            "the hero reverts to its default once the cover is removed"
        );
        assert!(
            playlists::PlaylistStore::load()
                .custom_cover_path(pid)
                .is_none(),
            "the store no longer reports a custom cover"
        );
        assert!(!cover_path.exists(), "the cover file is deleted from disk");
    }

    /// Clicking an album's "Edit Artwork" button opens the image file dialog
    /// and, on a pick, opens the CROP dialogue (the old Soul Player's crop step)
    /// rather than setting the cover directly: the pick seeds `State::artwork_crop`
    /// and the crop modal renders, but NO cover is stored until the user confirms.
    /// The full pick -> crop -> confirm -> stored-cover path is covered by
    /// `e2e_edit_artwork_crop_then_set_album_cover` below.
    #[test]
    fn edit_artwork_pick_opens_the_crop_dialogue() {
        let _g = config::use_test_config_dir();
        let mut s = populated_state();
        let album_name = s
            .library
            .iter()
            .find_map(|t| t.album.clone())
            .filter(|a| !a.is_empty())
            .expect("the testdata library has at least one named album");
        // Isolation: a sibling album-cover test shares this per-process config
        // dir, so clear any cover it left for this same album first.
        s.covers.clear_album_cover(&album_name);
        s.route = Route::Album;
        s.open_album = Some(album_name.clone());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.state.covers.album_cover_path(&album_name).is_none(),
            "no custom cover yet"
        );

        // The pencil opens the MANAGE dialogue; Change opens the file dialog.
        click_key(&mut rt, "album-edit-artwork");
        rt.frame(0.0);
        assert!(
            rt.state.artwork_edit.is_some() && rt.find("artwork-edit-dialog").is_some(),
            "the pencil opens the manage dialogue"
        );
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "artwork-edit-change");
        rt.frame(0.0);
        let (kind, _opts) = rt.file_dialog().expect("Change opens an image file dialog");
        assert_eq!(kind, FileDialogKind::Open);
        rt.resolve_file_dialog(Some(write_temp_real_png())).unwrap();
        rt.frame(0.0);

        // The pick opens the crop step; it does NOT store a cover by itself.
        assert!(
            rt.state.artwork_crop.is_some(),
            "picking an image opens the crop dialogue"
        );
        assert!(
            rt.find("artwork-crop-dialog").is_some(),
            "the crop dialogue renders"
        );
        assert!(
            rt.state.covers.album_cover_path(&album_name).is_none(),
            "picking alone stores no cover (the crop confirm does)"
        );
    }

    /// A REAL, decodable PNG (a tiny landscape gradient) written to a fresh temp
    /// dir, unique per call. Unlike `write_temp_cover_png` (which writes only the
    /// 8-byte PNG signature — enough for the cover store's extension/size check
    /// but NOT for `image::open`), the crop pipeline actually decodes + crops
    /// this one, so the crop e2e exercises the whole path.
    fn write_temp_real_png() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "soul_rune_crop_e2e_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.png");
        let mut img = image::RgbImage::new(64, 48);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([(x * 4) as u8, (y * 5) as u8, 120]);
        }
        img.save(&path).unwrap();
        path
    }

    /// The album name of the testdata library's first named album, for the crop
    /// e2e tests below.
    fn first_album_name(s: &State) -> String {
        s.library
            .iter()
            .find_map(|t| t.album.clone())
            .filter(|a| !a.is_empty())
            .expect("the testdata library has at least one named album")
    }

    /// Open an album detail page's "Edit Artwork", answer the file dialog with a
    /// real PNG, and settle so the crop modal is mounted + hittable. Returns the
    /// running `Runtime`. Shared by the two crop e2e tests.
    fn open_artwork_crop(album_name: &str) -> Runtime<State> {
        let mut s = populated_state();
        // The per-process test config dir persists across tests in this binary
        // (the `CONFIG_TEST_LOCK` only serialises them), so clear any cover a
        // sibling album-cover test left for this same album before we start.
        s.covers.clear_album_cover(album_name);
        s.route = Route::Album;
        s.open_album = Some(album_name.to_string());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Pencil -> the manage dialogue (a fresh modal: settle before its buttons)
        // -> Change -> the file dialog.
        click_key(&mut rt, "album-edit-artwork");
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "artwork-edit-change");
        rt.frame(0.0);
        let (kind, _opts) = rt.file_dialog().expect("Change opens an image file dialog");
        assert_eq!(kind, FileDialogKind::Open);
        rt.resolve_file_dialog(Some(write_temp_real_png())).unwrap();
        rt.frame(0.0);
        // The crop modal is also a fresh insert, so settle before driving it.
        rt.settle_all();
        rt.frame(0.0);
        rt
    }

    /// The crop flow end-to-end: Edit Artwork → file dialog → the CROP dialogue
    /// opens → the zoom slider is present → Set-as-cover crops + stores the result.
    /// Picking no longer sets the cover directly; the crop step does, and it
    /// stores a freshly-encoded PNG (never touching the source music files).
    #[test]
    fn e2e_edit_artwork_crop_then_set_album_cover() {
        let _g = config::use_test_config_dir();
        let album_name = first_album_name(&populated_state());

        let mut rt = open_artwork_crop(&album_name);
        assert!(
            rt.find("artwork-crop-dialog").is_some(),
            "the crop dialogue appeared after the pick"
        );
        assert!(
            rt.state.artwork_crop.is_some(),
            "the crop state is set (picking opens the crop step, not a direct cover set)"
        );
        assert!(
            rt.state.covers.album_cover_path(&album_name).is_none(),
            "no cover is stored until the crop is confirmed"
        );

        // Zoom is a slider now; it is present in the dialogue. (Its value->scale
        // mapping is covered by the `zoom01`/`set_zoom01` unit tests.)
        assert!(
            rt.find("artwork-crop-zoom").is_some(),
            "the crop dialogue has a zoom slider"
        );

        // Confirm: crop + store.
        click_key(&mut rt, "artwork-crop-confirm");
        let stored = rt
            .state
            .covers
            .album_cover_path(&album_name)
            .expect("Set as cover stored a cropped cover");
        assert!(
            rt.state.artwork_crop.is_none(),
            "confirming closes the dialogue"
        );
        assert_eq!(
            stored.extension().and_then(|e| e.to_str()),
            Some("png"),
            "the stored cover is a PNG (the re-encoded crop)"
        );
        assert!(stored.exists(), "the stored cover file exists on disk");
    }

    /// Cancelling the crop dialogue closes it and sets NO cover.
    #[test]
    fn edit_artwork_crop_cancel_sets_no_cover() {
        let _g = config::use_test_config_dir();
        let album_name = first_album_name(&populated_state());

        let mut rt = open_artwork_crop(&album_name);
        assert!(rt.state.artwork_crop.is_some(), "the crop dialogue is open");

        click_key(&mut rt, "artwork-crop-cancel");
        assert!(
            rt.state.artwork_crop.is_none(),
            "cancel closes the crop dialogue"
        );
        assert!(
            rt.state.covers.album_cover_path(&album_name).is_none(),
            "cancelling stored no cover"
        );
    }

    /// Albums get the SAME Edit Artwork path as artists: hovering the square album
    /// hero reveals the pencil, and clicking it opens the manage dialogue (Change /
    /// Cancel). Guards that `album_detail` wires the artwork target like
    /// `artist_detail` does, so album covers can be changed the same way.
    #[test]
    fn hovering_an_album_hero_reveals_edit_artwork_and_opens_the_manage_dialogue() {
        let _g = config::use_test_config_dir();
        let mut s = populated_state();
        let album = first_album_name(&s);
        s.covers.clear_album_cover(&album);
        s.route = Route::Album;
        s.open_album = Some(album);

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);

        let overlay_opacity = |rt: &Runtime<State>| {
            rt.elements()
                .iter()
                .find(|e| e.key.as_deref() == Some("album-artwork-overlay"))
                .map(|e| e.opacity)
        };
        assert!(
            matches!(overlay_opacity(&rt), Some(o) if o < 0.01),
            "at rest the album Edit Artwork overlay is hidden, got {:?}",
            overlay_opacity(&rt)
        );

        // Hover the album cover -> the pencil overlay fades in.
        let (cx, cy) = rt
            .find("album-cover")
            .expect("the album hero cover is on screen");
        rt.pointer_move(cx, cy);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            matches!(overlay_opacity(&rt), Some(o) if o > 0.5),
            "hovering the album cover reveals Edit Artwork, got {:?}",
            overlay_opacity(&rt)
        );

        // Click the pencil -> the same manage dialogue (Change / Cancel) opens.
        click_key(&mut rt, "album-edit-artwork");
        rt.frame(0.0);
        assert!(
            rt.state.artwork_edit.is_some() && rt.find("artwork-edit-change").is_some(),
            "the album pencil opens the manage dialogue"
        );
    }

    /// The detail-hero Edit Artwork affordance is revealed ON HOVER (like the old
    /// app), not parked in the actions row: the overlay rests at opacity 0 and
    /// fades in while the pointer is over the cover. Guards the `hover_group`
    /// wiring on the hero cover, without which the overlay never leaves opacity 0.
    #[test]
    fn hovering_an_artist_hero_reveals_the_edit_artwork_overlay() {
        let _g = config::use_test_config_dir();
        let mut s = populated_state();
        let artist = s
            .library
            .iter()
            .filter_map(|t| t.artist.clone())
            .find(|a| !a.is_empty())
            .expect("the testdata library has a named artist");
        // A plain avatar (no custom cover) so the hero is the circular
        // `artist-cover`, and sibling tests can't leave a cover behind.
        s.covers.clear_artist_cover(&artist);
        s.route = Route::Artist;
        s.open_artist = Some(artist);

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);

        let overlay_opacity = |rt: &Runtime<State>| {
            rt.elements()
                .iter()
                .find(|e| e.key.as_deref() == Some("artist-artwork-overlay"))
                .map(|e| e.opacity)
        };
        assert!(
            matches!(overlay_opacity(&rt), Some(o) if o < 0.01),
            "at rest the Edit Artwork overlay is hidden, got {:?}",
            overlay_opacity(&rt)
        );

        // Move the pointer onto the hero cover: the overlay fades in.
        let (cx, cy) = rt
            .find("artist-cover")
            .expect("the artist hero cover is on screen");
        rt.pointer_move(cx, cy);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            matches!(overlay_opacity(&rt), Some(o) if o > 0.5),
            "hovering the cover reveals the Edit Artwork overlay, got {:?}",
            overlay_opacity(&rt)
        );
    }

    /// The whole artist-artwork journey end-to-end through the real `view`: open
    /// an artist page, HOVER the cover to reveal Edit Artwork, click it, pick an
    /// image, the crop dialogue opens, confirm it — the artist gains a stored
    /// custom cover and the hero switches from the generated avatar to that image.
    /// Ties together the hover reveal, the crop pipeline, and the cover store.
    #[test]
    fn e2e_artist_hover_edit_crop_sets_the_cover() {
        let _g = config::use_test_config_dir();
        let mut s = populated_state();
        let artist = s
            .library
            .iter()
            .filter_map(|t| t.artist.clone())
            .find(|a| !a.is_empty())
            .expect("the testdata library has a named artist");
        s.covers.clear_artist_cover(&artist);
        s.route = Route::Artist;
        s.open_artist = Some(artist.clone());

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);

        // A plain avatar hero (no custom cover yet); its Edit Artwork overlay rests hidden.
        assert!(
            rt.find("artist-cover").is_some(),
            "the hero is the generated circular avatar"
        );
        let overlay_opacity = |rt: &Runtime<State>| {
            rt.elements()
                .iter()
                .find(|e| e.key.as_deref() == Some("artist-artwork-overlay"))
                .map(|e| e.opacity)
        };
        assert!(
            matches!(overlay_opacity(&rt), Some(o) if o < 0.01),
            "Edit Artwork rests hidden, got {:?}",
            overlay_opacity(&rt)
        );

        // 1. Hover the cover -> the Edit Artwork overlay fades in.
        let (cx, cy) = rt.find("artist-cover").unwrap();
        rt.pointer_move(cx, cy);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            matches!(overlay_opacity(&rt), Some(o) if o > 0.5),
            "hovering the cover reveals Edit Artwork, got {:?}",
            overlay_opacity(&rt)
        );

        // 2. Pencil -> the manage dialogue -> Change -> file dialog -> pick -> crop.
        click_key(&mut rt, "artist-edit-artwork");
        rt.frame(0.0);
        assert!(
            rt.state.artwork_edit.is_some(),
            "the pencil opens the manage dialogue"
        );
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "artwork-edit-change");
        rt.frame(0.0);
        let (kind, _opts) = rt.file_dialog().expect("Change opens an image file dialog");
        assert_eq!(kind, FileDialogKind::Open);
        rt.resolve_file_dialog(Some(write_temp_real_png())).unwrap();
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.state.artwork_crop.is_some() && rt.find("artwork-crop-dialog").is_some(),
            "picking an image opens the crop dialogue"
        );

        // 3. Confirm the crop -> the artist gains a stored custom cover (a re-encoded PNG).
        click_key(&mut rt, "artwork-crop-confirm");
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.state.artwork_crop.is_none(),
            "confirming closes the crop dialogue"
        );
        let stored = rt
            .state
            .covers
            .artist_cover_path(&artist)
            .expect("the artist now has a stored custom cover");
        assert!(stored.exists(), "the stored cover file exists on disk");
        assert_eq!(
            stored.extension().and_then(|e| e.to_str()),
            Some("png"),
            "the stored cover is the re-encoded PNG crop"
        );

        // 4. The hero now renders the custom image, not the generated avatar.
        assert!(
            rt.find("artist-hero-image").is_some(),
            "the hero switched to the custom cover image"
        );
    }

    /// Deleting a cover lives in the MANAGE dialogue the pencil opens (not on the
    /// hover overlay, not the crop step): with a cover already set, the dialogue
    /// offers Delete, which clears the cover and closes. Without a cover, no Delete
    /// control is shown (the manage snap scene's a11y sweep covers Change/Cancel).
    #[test]
    fn e2e_manage_dialogue_delete_clears_an_existing_cover() {
        let _g = config::use_test_config_dir();
        let album_name = first_album_name(&populated_state());

        let mut s = populated_state();
        // Pre-set a custom cover so the manage dialogue offers Delete.
        assert!(
            s.covers
                .set_album_cover(&album_name, &write_temp_real_png()),
            "the test PNG is a valid cover"
        );
        assert!(s.covers.album_cover_path(&album_name).is_some());
        s.route = Route::Album;
        s.open_album = Some(album_name.clone());

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        // The pencil opens the manage dialogue, which offers Delete (a cover is set).
        click_key(&mut rt, "album-edit-artwork");
        rt.frame(0.0);
        assert!(
            rt.state.artwork_edit.is_some(),
            "the pencil opens the manage dialogue"
        );
        assert!(
            rt.find("artwork-edit-delete").is_some(),
            "the dialogue offers Delete because a cover is set"
        );

        // Click Delete -> the cover is cleared and the dialogue closes.
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "artwork-edit-delete");
        rt.frame(0.0);
        assert!(
            rt.state.artwork_edit.is_none(),
            "deleting closes the dialogue"
        );
        assert!(
            rt.state.covers.album_cover_path(&album_name).is_none(),
            "Delete cleared the custom cover"
        );
    }

    /// The Keyboard-tab playback shortcuts actually fire through the real key
    /// path: Ctrl+Space/Right/Left/M each send their transport Command, and
    /// Ctrl+Up raises the volume. Guards that the `ev.mods.ctrl` arms in
    /// `handle_key` stay wired (and keep winning over the plain arms).
    #[test]
    fn ctrl_playback_shortcuts_drive_the_transport() {
        let s = State::new(PlayerHandle::recording());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        let ctrl = |key: Key| KeyEvent {
            key,
            mods: Mods {
                ctrl: true,
                ..Mods::default()
            },
        };
        // Start below max so Ctrl+Up has headroom (the default sits at 1.0).
        rt.state.volume = 0.5;
        let before = rt.state.volume;
        rt.key(ctrl(Key::Text(" ".to_string())));
        rt.key(ctrl(Key::Right));
        rt.key(ctrl(Key::Left));
        rt.key(ctrl(Key::Text("m".to_string())));
        rt.key(ctrl(Key::Up));
        rt.frame(0.0);

        let cmds = rt.state.host.recorded_commands();
        assert!(
            cmds.contains(&Command::TogglePlay),
            "Ctrl+Space toggles play, got {cmds:?}"
        );
        assert!(cmds.contains(&Command::Next), "Ctrl+Right skips to next");
        assert!(cmds.contains(&Command::Prev), "Ctrl+Left skips to previous");
        assert!(cmds.contains(&Command::ToggleMute), "Ctrl+M toggles mute");
        assert!(rt.state.volume > before, "Ctrl+Up raises the volume");
    }

    /// Capstone end-to-end: the whole Playlists lifecycle driven through ONE
    /// running `Runtime` over the real scanned `testdata/` library, chaining
    /// every feature the earlier capstones (above) proved in isolation
    /// (create, add from two different surfaces, reorder, rename, custom
    /// cover, remove a track), so a cross-feature or state-leak bug has
    /// somewhere to hide. The second half is a hard persistence check: a
    /// brand-new `Runtime`/`State` (a simulated restart) reads the same
    /// playlist back off disk with everything intact, then deletes it.
    /// Every step asserts both the rendered UI (`rt.find`/`shows`) and the
    /// on-disk `PlaylistStore`, never one alone.
    #[test]
    fn e2e_playlist_full_lifecycle_journey() {
        use soul_player_host::TrackLocator;

        let _g = config::use_test_config_dir();
        let _ = std::fs::remove_dir_all(playlists::playlist_store_path());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };

        // ---- 1. Create, via the sidebar nav and the real inline field ----
        let s = populated_state();
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);

        let track_a = rt.state.library[0].id;
        let track_b = rt.state.library[1].id;
        let track_c = rt.state.library[2].id;
        let loc_a = rt
            .state
            .host
            .locator_of(track_a)
            .expect("a scanned track resolves a durable locator");
        let loc_b = rt.state.host.locator_of(track_b).expect("track b resolves");
        let loc_c = rt.state.host.locator_of(track_c).expect("track c resolves");

        click_key(&mut rt, "nav-playlists");
        assert!(matches!(rt.state.route, Route::Playlists));
        rt.settle_all();
        rt.frame(0.0);

        // The inline New-playlist field lives behind the top bar's "New playlist"
        // button now; reveal it (and settle its drop-in animation) before typing.
        click_key(&mut rt, "playlists-new");
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "playlist-new-input");
        rt.ime(rune::Ime::Commit("Summer Mix".to_string()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);

        let pid = rt
            .state
            .open_playlist
            .expect("creating a playlist navigates straight into it");
        assert!(matches!(rt.state.route, Route::Playlist));
        assert_eq!(
            playlists::PlaylistStore::load().get(pid).unwrap().name,
            "Summer Mix",
            "the new playlist persisted to disk"
        );

        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "detail-back");
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            shows(&rt, "Summer Mix"),
            "the new playlist shows on the Playlists list"
        );
        assert!(rt.find(&format!("playlist-card-{pid}")).is_some());

        // ---- 2. Add from two surfaces ----

        // Surface A: a library track row's context menu. This harness can't
        // drive a real OS right-click popup, so it opens the overlay the same
        // way `views::add_to_playlist`'s own tests do, then drives the real
        // pick row and Done button through the full app `view`.
        click_key(&mut rt, "nav-library");
        for &tid in &[track_a, track_b] {
            rt.update(move |st: &mut State, _tx: &mut Tx| {
                crate::open_add_to_playlist(st, tid);
            });
            rt.settle_all();
            rt.frame(0.0);
            click_key(&mut rt, &format!("add-to-playlist-pick-{pid}"));
            click_key(&mut rt, "add-to-playlist-done");
        }
        assert_eq!(
            playlists::PlaylistStore::load()
                .get(pid)
                .unwrap()
                .entries
                .len(),
            2,
            "both library-surface adds persisted"
        );

        // Surface B: the now-playing heart. Starts a third track "playing"
        // (a fresh insert of the now-playing panel, hence the settle), opens
        // the same membership dialogue from there, and ticks the same
        // playlist.
        rt.update(move |st: &mut State, _tx: &mut Tx| {
            st.now_playing_id = Some(track_c);
        });
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "np-heart");
        assert_eq!(rt.state.add_to_playlist_for, Some(track_c));
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, &format!("add-to-playlist-pick-{pid}"));
        click_key(&mut rt, "add-to-playlist-done");
        assert_eq!(
            playlists::PlaylistStore::load()
                .get(pid)
                .unwrap()
                .entries
                .len(),
            3,
            "the heart-surface add grew membership to three"
        );

        // ---- 3. Open + verify ----
        click_key(&mut rt, "nav-playlists");
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, &format!("playlist-card-{pid}"));
        rt.settle_all();
        rt.frame(0.0);
        assert!(rt.find("playlist-row-0").is_some());
        assert!(rt.find("playlist-row-1").is_some());
        assert!(rt.find("playlist-row-2").is_some());
        let entries_before_reorder: Vec<TrackLocator> = rt
            .state
            .playlists
            .get(pid)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        assert_eq!(
            entries_before_reorder,
            vec![loc_a.clone(), loc_b.clone(), loc_c.clone()],
            "the three adds landed in the order they happened"
        );

        // ---- 4. Reorder: a drag, then a move button ----
        let (gx, gy0) = rt.find("playlist-grip-0").expect("grip 0 renders");
        let (_, gy2) = rt.find("playlist-grip-2").expect("grip 2 renders");
        rt.pointer_down(gx, gy0);
        rt.pointer_move(gx, gy2);
        rt.frame(0.0);
        rt.pointer_up(gx, gy2);
        rt.frame(0.0);
        let after_drag: Vec<TrackLocator> = rt
            .state
            .playlists
            .get(pid)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        assert_eq!(
            after_drag,
            vec![loc_b.clone(), loc_c.clone(), loc_a.clone()],
            "dragging entry 0 onto entry 2 moves it to the end"
        );

        click_key(&mut rt, "playlist-down-0");
        let reordered: Vec<TrackLocator> = playlists::PlaylistStore::load()
            .get(pid)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        assert_eq!(
            reordered,
            vec![loc_c.clone(), loc_b.clone(), loc_a.clone()],
            "the move-down button swaps the leading pair, and it persists"
        );

        // ---- 5. Rename ----
        click_key(&mut rt, "playlist-rename");
        rt.ime(rune::Ime::Commit("  Festival Favorites  ".to_string()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        assert!(
            shows(&rt, "Festival Favorites"),
            "the renamed, trimmed name renders on the detail page"
        );
        assert_eq!(
            playlists::PlaylistStore::load().get(pid).unwrap().name,
            "Festival Favorites",
            "the rename persisted"
        );

        click_key(&mut rt, "detail-back");
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            shows(&rt, "Festival Favorites"),
            "the new name also renders on the Playlists list"
        );
        assert!(
            !shows(&rt, "Summer Mix"),
            "the old name no longer shows anywhere"
        );

        click_key(&mut rt, &format!("playlist-card-{pid}"));
        rt.settle_all();
        rt.frame(0.0);

        // ---- 6. Custom cover: set, then remove ----
        assert!(
            rt.find("playlist-hero-image").is_none(),
            "no custom cover yet"
        );
        click_key(&mut rt, "playlist-set-cover");
        rt.frame(0.0);
        let (kind, _opts) = rt.file_dialog().expect("Set cover opens a file dialog");
        assert_eq!(kind, FileDialogKind::Open);
        let picked = write_temp_cover_png();
        rt.resolve_file_dialog(Some(picked)).unwrap();
        rt.frame(0.0);
        assert!(
            rt.find("playlist-hero-image").is_some(),
            "the hero now renders the custom cover"
        );
        let cover_path = playlists::PlaylistStore::load()
            .custom_cover_path(pid)
            .expect("the picked image persisted as the playlist's custom cover");
        assert!(cover_path.exists(), "the stored cover file exists on disk");

        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "playlist-remove-cover");
        assert!(
            rt.find("playlist-hero-image").is_none(),
            "the hero reverts to its default once the cover is removed"
        );
        assert!(
            playlists::PlaylistStore::load()
                .custom_cover_path(pid)
                .is_none(),
            "the store no longer reports a custom cover"
        );
        assert!(!cover_path.exists(), "the cover file is deleted from disk");

        // ---- 7. Remove a track ----
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "playlist-remove-0");
        let after_remove: Vec<TrackLocator> = playlists::PlaylistStore::load()
            .get(pid)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        assert_eq!(
            after_remove,
            vec![loc_b.clone(), loc_a.clone()],
            "removing entry 0 drops the reordered leader and persists the rest, in order"
        );

        // ---- 8. PERSISTENCE RELOAD: a brand-new Runtime and State, as if
        // the app had restarted, must read everything above straight off
        // disk: name, order, membership, and the removed cover. ----
        let s2 = populated_state();
        let mut rt2 = Runtime::new(s2, view, (1100.0, 720.0), env);
        rt2.frame(0.0);
        rt2.frame(0.0);

        click_key(&mut rt2, "nav-playlists");
        rt2.settle_all();
        rt2.frame(0.0);
        assert!(
            shows(&rt2, "Festival Favorites"),
            "the renamed playlist survived the reload and shows on the list"
        );
        assert!(
            !shows(&rt2, "Summer Mix"),
            "the old name never resurfaces after a reload"
        );
        assert!(
            rt2.find(&format!("playlist-cover-{pid}-custom")).is_none(),
            "the list row's thumbnail does not show the removed cover either"
        );

        click_key(&mut rt2, &format!("playlist-card-{pid}"));
        rt2.settle_all();
        rt2.frame(0.0);
        let reloaded = rt2
            .state
            .playlists
            .get(pid)
            .expect("the playlist reloads from disk");
        assert_eq!(reloaded.name, "Festival Favorites");
        let reloaded_locators: Vec<TrackLocator> =
            reloaded.entries.iter().map(|e| e.locator.clone()).collect();
        assert_eq!(
            reloaded_locators,
            vec![loc_b.clone(), loc_a.clone()],
            "the reordered, minus-removed track set survived the reload, in order"
        );
        assert!(
            rt2.state.playlists.custom_cover_path(pid).is_none(),
            "the removed cover stayed removed across the reload"
        );
        assert!(
            rt2.find("playlist-hero-image").is_none(),
            "the hero falls back to the mosaic/placeholder, not a stale cover"
        );
        assert!(rt2.find("playlist-row-0").is_some());
        assert!(rt2.find("playlist-row-1").is_some());
        assert!(
            rt2.find("playlist-row-2").is_none(),
            "the removed track never resurfaces"
        );

        // ---- 9. Delete ----
        click_key(&mut rt2, "playlist-delete");
        rt2.settle_all();
        rt2.frame(0.0);
        click_key(&mut rt2, "playlist-delete-confirm");
        assert!(
            matches!(rt2.state.route, Route::Playlists),
            "confirming delete returns to the list"
        );
        assert!(
            rt2.find(&format!("playlist-card-{pid}")).is_none(),
            "the deleted playlist no longer shows on the list"
        );
        assert!(
            playlists::PlaylistStore::load().get(pid).is_none(),
            "the delete persisted: a fresh load no longer has it"
        );
    }

    /// A Sources refresh clears any stale deletion warning (the next scan's own
    /// `TracksRemoved`, applied right after, re-sets it if there were deletions).
    /// e2e: a playlist track GREYS OUT ("unavailable", not playable) when its
    /// source is disabled mid-session, stays in the playlist, and becomes
    /// playable again when the source is re-enabled — the universal
    /// disabled-state the Sources toggle drives, rendered through the real
    /// playlist detail `view`. Complements `playlist_detail`'s static-ghost
    /// greying test by exercising the DYNAMIC "lose access to a source" path end
    /// to end, and pairs with the host-level
    /// `a_local_track_locator_greys_out_when_its_source_is_disabled`.
    #[test]
    fn e2e_playlist_track_greys_when_its_source_is_disabled_then_returns() {
        let _locale = config::use_test_locale("en");
        let _g = config::use_test_config_dir();
        let _ = std::fs::remove_dir_all(playlists::playlist_store_path());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };

        // A playlist over two real library tracks.
        let mut s = populated_state();
        let pid = s.playlists.create("Availability");
        let ids: Vec<u32> = s.library.iter().take(2).map(|t| t.id).collect();
        assert_eq!(ids.len(), 2, "the testdata library has at least two tracks");
        for &tid in &ids {
            crate::views::add_to_playlist::add_track_to_playlist(&mut s, tid, pid);
        }
        s.route = Route::Playlist;
        s.open_playlist = Some(pid);

        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);

        // A row is "playable" when it renders as a button (vs the greyed,
        // roleless `unavailable_row`). `playlist_detail`'s own helper is
        // module-private, so inline the same check here.
        let playable = |rt: &Runtime<State>, key: &str| {
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some(key))
                .is_some_and(|e| e.role == "button")
        };

        // Available: both rows are playable.
        assert!(
            playable(&rt, "playlist-row-0"),
            "a track is playable while its source is enabled"
        );
        assert!(playable(&rt, "playlist-row-1"));

        // Lose access: disable the source (the same call the Sources toggle fires).
        scan_in_background(
            &mut rt.state,
            move |h| h.set_folder_enabled(&testdata_dir(), false),
            apply_watch,
        );
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);

        // Greyed: the entries stay in the playlist but are unavailable, not playable.
        assert!(
            !playable(&rt, "playlist-row-0"),
            "a disabled source greys the track (no play affordance)"
        );
        assert!(
            shows(&rt, t!("playlists.unavailable").as_ref()),
            "the greyed row shows the Unavailable label"
        );
        assert_eq!(
            rt.state.playlists.get(pid).unwrap().entries.len(),
            2,
            "greyed entries are NOT dropped from the playlist"
        );

        // Regain access: re-enable the source → playable again.
        scan_in_background(
            &mut rt.state,
            move |h| h.set_folder_enabled(&testdata_dir(), true),
            apply_watch,
        );
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            playable(&rt, "playlist-row-0"),
            "re-enabling the source restores playability"
        );
    }

    #[test]
    fn apply_watch_clears_the_removal_notice() {
        let mut s = populated_state();
        s.removal_notice = Some((PathBuf::from("/music/live"), 3));
        apply_watch(&mut s, WatchUpdate::default());
        assert_eq!(
            s.removal_notice, None,
            "a Sources refresh clears the stale deletion warning"
        );
    }

    /// The deletion warning renders in Settings → Music Data when set (with the
    /// count + folder name), and is absent when there were no deletions.
    #[test]
    fn removal_notice_renders_in_the_sources_page() {
        // This asserts English UI text. `populated_state()` → `State::new` LOADS the
        // persisted locale and applies it to the shared `rust_i18n` global, and the
        // language-picker test persists "de" — so set the locale AFTER building the
        // state (and hold the config lock to serialize against that test).
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::MusicData;
        s.removal_notice = Some((PathBuf::from("/music/Concerts"), 2));
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        // The settings page carries a scrollbar whose live-bound layout needs a waker
        // to settle before the scrolled content is laid out for `find`.
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        // Assert on the folder name + count, NOT the full phrase: other tests mutate
        // the process-global i18n locale in parallel, so the notice may render in any
        // language — but "Concerts" and "2" appear in every translation. (Also proves
        // it renders: `find` returns the notice element's own text.)
        let notice = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("removal-notice"))
            .and_then(|e| e.text)
            .expect("the deletion warning renders when set");
        assert!(
            notice.contains("Concerts") && notice.contains('2'),
            "the notice names the folder and the count (any locale): {notice:?}"
        );

        // Absent when there were no deletions.
        let mut s2 = populated_state();
        s2.route = Route::Settings;
        s2.settings_tab = SettingsTab::MusicData;
        s2.removal_notice = None;
        let mut rt2 = Runtime::new(s2, view, (1100.0, 720.0), env);
        rt2.set_live_waker(std::sync::Arc::new(|| {}));
        rt2.frame(0.0);
        rt2.settle_all();
        rt2.frame(0.0);
        assert!(
            rt2.find("removal-notice").is_none(),
            "no warning when there were no deletions"
        );
    }

    /// The clearing in `apply_watch` and the setting from `TracksSkipped` are
    /// ordered: the refresh lands FIRST and wipes the previous scan's notice, then
    /// this scan's own event re-sets it. Asserted because the two are a hair apart
    /// from cancelling each other out — if the clear ever ran last, the notice
    /// would be computed, emitted, and then silently thrown away, which is
    /// indistinguishable from the bug this whole feature exists to fix.
    #[test]
    fn a_skip_event_after_the_refresh_survives() {
        let mut s = populated_state();
        let groups = vec![SkippedGroup {
            extension: "aac".into(),
            reason: soul_player_host::SkipReason::DecoderDisabled,
            count: 2,
        }];
        // Previous scan's notice, then this scan's refresh + event, in order.
        s.skip_notice = Some((PathBuf::from("/music/old"), groups.clone()));
        apply_watch(&mut s, WatchUpdate::default());
        s.apply(&Event::TracksSkipped {
            root: PathBuf::from("/music/new"),
            groups: groups.clone(),
        });
        assert_eq!(
            s.skip_notice,
            Some((PathBuf::from("/music/new"), groups)),
            "the notice reflects THIS scan, not the previous one, and is not \
             wiped by the refresh that precedes it"
        );
    }

    /// The desktop identity must be the Flatpak id inside a Flatpak and the
    /// binary name everywhere else.
    ///
    /// This is not cosmetic. Flatpak's D-Bus policy auto-grants exactly one
    /// MPRIS name — `org.mpris.MediaPlayer2.$FLATPAK_ID` — so keeping the
    /// binary name there means the sandbox refuses the name the app asks for,
    /// and media keys plus the desktop's media controls stop seeing the player
    /// while the rest of the app carries on looking healthy.
    #[test]
    fn desktop_identity_follows_the_flatpak_id_when_sandboxed() {
        assert_eq!(
            resolve_desktop_id(Some("co.datadir.soulplayer")),
            "co.datadir.soulplayer",
            "inside a Flatpak the id IS the desktop entry's basename"
        );
        assert_eq!(
            resolve_desktop_id(None),
            env!("CARGO_BIN_NAME"),
            "outside, cargo-packager names the entry after the binary"
        );
        // An empty or whitespace-only value is not an identity. Falling through
        // to the binary name beats registering `org.mpris.MediaPlayer2.` — which
        // is not a legal bus name, so the session would fail to publish at all.
        assert_eq!(resolve_desktop_id(Some("")), env!("CARGO_BIN_NAME"));
        assert_eq!(resolve_desktop_id(Some("   ")), env!("CARGO_BIN_NAME"));
    }

    #[test]
    fn apply_watch_clears_the_skip_notice() {
        let mut s = populated_state();
        s.skip_notice = Some((
            PathBuf::from("/music/rips"),
            vec![SkippedGroup {
                extension: "aac".into(),
                reason: soul_player_host::SkipReason::DecoderDisabled,
                count: 4,
            }],
        ));
        apply_watch(&mut s, WatchUpdate::default());
        assert_eq!(
            s.skip_notice, None,
            "a Sources refresh clears the previous scan's skip notice, so a \
             format that is no longer skipped stops being reported"
        );
    }

    /// The skip report renders in Settings → Music Data with the count, the
    /// folder and the offending extension — proving the notice is actually wired
    /// into the view, which the formatter's own unit test cannot show.
    #[test]
    fn skip_notice_renders_in_the_sources_page() {
        // Same locale discipline as `removal_notice_renders_in_the_sources_page`.
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::MusicData;
        s.skip_notice = Some((
            PathBuf::from("/music/Bootlegs"),
            vec![SkippedGroup {
                extension: "wma".into(),
                reason: soul_player_host::SkipReason::UnsupportedFormat,
                count: 5,
            }],
        ));
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        // Folder, count and extension appear in every translation (the reason text
        // does not), so this stays locale-independent like the deletion test.
        let notice = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("skip-notice"))
            .and_then(|e| e.text)
            .expect("the skip report renders when set");
        assert!(
            notice.contains("Bootlegs") && notice.contains('5') && notice.contains("wma"),
            "the notice names the folder, the count and the format: {notice:?}"
        );

        // Absent when the scan imported everything it found — a library this build
        // fully decodes must show no notice at all.
        let mut s2 = populated_state();
        s2.route = Route::Settings;
        s2.settings_tab = SettingsTab::MusicData;
        s2.skip_notice = None;
        let mut rt2 = Runtime::new(s2, view, (1100.0, 720.0), env);
        rt2.set_live_waker(std::sync::Arc::new(|| {}));
        rt2.frame(0.0);
        rt2.settle_all();
        rt2.frame(0.0);
        assert!(
            rt2.find("skip-notice").is_none(),
            "nothing was skipped, so no notice"
        );
    }

    #[test]
    fn crash_dialog_shows_only_after_an_unclean_exit() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        apply_locale("en");

        // A clean start shows no dialog.
        let mut clean = populated_state();
        clean.crashed_last_session = false;
        let mut rt = Runtime::new(clean, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("crash-dialog").is_none(),
            "no crash dialog on a clean start"
        );

        // After an unclean exit the modal dialog and its report button render.
        let mut crashed = populated_state();
        crashed.crashed_last_session = true;
        let mut rt2 = Runtime::new(crashed, view, (1100.0, 720.0), env);
        rt2.frame(0.0);
        assert!(
            rt2.find("crash-dialog").is_some(),
            "crash dialog shows after an unclean exit"
        );
        assert!(
            rt2.find("crash-report").is_some(),
            "the report button is offered"
        );
    }

    #[test]
    fn set_indexing_mode_updates_state_host_and_persists() {
        let _g = config::use_test_config_dir();
        let mut s = populated_state();
        set_indexing_mode(&mut s, IndexingMode::Gentle);
        assert_eq!(
            s.indexing_mode,
            IndexingMode::Gentle,
            "State reflects the choice"
        );
        assert_eq!(
            s.host.indexing_mode(),
            IndexingMode::Gentle,
            "applied to the host for the next scan"
        );
        assert_eq!(config::load_indexing_mode(), "gentle", "persisted");
    }

    /// The Sources page shows all three indexing-speed buttons (idiot-proof: every
    /// option visible) with the current mode selectable.
    #[test]
    fn indexing_section_renders_the_three_modes_in_sources() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        apply_locale("en");
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::MusicData;
        s.indexing_mode = IndexingMode::Gentle;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.find("indexing-auto").is_some()
                && rt.find("indexing-max").is_some()
                && rt.find("indexing-gentle").is_some(),
            "all three indexing-mode buttons render"
        );
    }

    #[test]
    fn set_loudness_analysis_mode_updates_state_host_and_persists() {
        let _g = config::use_test_config_dir();
        let mut s = populated_state();
        set_loudness_analysis_mode(&mut s, LoudnessAnalysisMode::Off);
        assert_eq!(
            s.loudness_mode,
            LoudnessAnalysisMode::Off,
            "State reflects it"
        );
        assert_eq!(
            s.host.loudness_analysis_mode(),
            LoudnessAnalysisMode::Off,
            "applied to the host (gates the sweep + worker pause)"
        );
        assert_eq!(config::load_loudness_analysis(), "off", "persisted");
    }

    /// The Sources page shows all three background-loudness-analysis buttons, and clicking
    /// one applies the mode through to the state.
    #[test]
    fn loudness_analysis_section_renders_and_sets_the_mode() {
        let _g = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        apply_locale("en");
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.find("loudness-off").is_some()
                && rt.find("loudness-throttled").is_some()
                && rt.find("loudness-full").is_some(),
            "all three loudness-analysis buttons render"
        );
        click_key(&mut rt, "loudness-off");
        assert_eq!(
            rt.state.loudness_mode,
            LoudnessAnalysisMode::Off,
            "clicking Off applies the mode"
        );
    }

    #[test]
    fn tick_carries_the_analysis_pending_count() {
        let mut s = State::new(PlayerHandle::offline());
        assert_eq!(s.analysis_pending, 0, "starts with no pending analysis");
        s.apply(&Event::Tick(soul_player_host::Tick {
            now_playing_id: None,
            position_secs: 0.0,
            duration_secs: None,
            state: PlayState::Stopped,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume: 1.0,
            muted: false,
            analysis_pending: 12,
        }));
        assert_eq!(s.analysis_pending, 12, "the tick's count reaches the state");
    }

    /// The "Analyzing N" line shows in the Settings loudness section while the sweep has work
    /// left, and clears once it is caught up.
    #[test]
    fn analysis_pending_shows_then_clears_the_analyzing_line() {
        let _g = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        apply_locale("en");
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::MusicData;
        s.analysis_pending = 7;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.find("loudness-analyzing").is_some(),
            "the Analyzing line shows while the sweep has work"
        );
        rt.update(|s: &mut State, _tx: &mut Tx| s.analysis_pending = 0);
        rt.frame(0.0);
        assert!(
            rt.find("loudness-analyzing").is_none(),
            "the Analyzing line clears once caught up"
        );
    }

    /// The now-playing signal-path line renders the formatted chain when a
    /// `SignalPath` is present, and is absent when it isn't.
    #[test]
    fn signal_path_line_renders_in_now_playing() {
        use soul_player_host::{SignalPath, SourceFormat};
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        scene_queue(&mut s); // populates now-playing
        s.signal_path = Some(SignalPath {
            source: SourceFormat::Dsd { rate_hz: 2_822_400 },
            codec: SourceCodec::Dsd,
            decoded_rate: 44_100,
            decoded_bits: 24,
            channels: 2,
            replay_gain_db: 0.0,
            device_rate: 48_000,
            resampling: true,
            output_bits: 24,
            exclusive: false,
            loudness: None,
        });
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("signal-path").is_some(),
            "the signal-path line renders when a chain is present"
        );
        assert!(
            rt.elements()
                .into_iter()
                .any(|e| e.text.as_deref() == Some("DSD64 · 44.1→48 kHz")),
            "it shows the formatted DSD→resample chain"
        );

        // Absent when there is no signal path.
        let mut s2 = populated_state();
        scene_queue(&mut s2);
        s2.signal_path = None;
        let mut rt2 = Runtime::new(s2, view, (1100.0, 720.0), env);
        rt2.frame(0.0);
        assert!(
            rt2.find("signal-path").is_none(),
            "no signal-path line when there is no chain"
        );
    }

    /// Settings → Audio shows the signal-path card with the same stage rows
    /// (shared `pipeline_rows`), including the not-yet-built "soon" stages.
    #[test]
    fn signal_path_card_renders_in_settings_audio() {
        use soul_player_host::{SignalPath, SourceFormat};
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        scene_queue(&mut s); // a live chain to read out
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.signal_path = Some(SignalPath {
            source: SourceFormat::Pcm { bits: 24 },
            codec: SourceCodec::Pcm,
            decoded_rate: 96_000,
            decoded_bits: 24,
            channels: 2,
            replay_gain_db: 0.0,
            device_rate: 96_000,
            resampling: false,
            output_bits: 24,
            exclusive: false,
            loudness: None,
        });
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Settings → Audio is laid out AS the pipeline: one card per stage.
        for k in [
            "sp-stage-source",
            "sp-stage-replaygain",
            "sp-stage-resample",
            "sp-stage-dsp",
            "sp-stage-output",
            "sp-stage-exclusive",
        ] {
            assert!(
                rt.find(k).is_some(),
                "stage card {k} renders in the Settings → Audio pipeline"
            );
        }
    }

    /// The strict (exclusive-or-stop) sub-toggle only appears once exclusive output
    /// is on — it's a sub-option of exclusive, so hiding it keeps the card uncluttered
    /// for the common (shared) case.
    #[test]
    fn strict_exclusive_toggle_only_shows_when_exclusive_is_on() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        apply_locale("en");

        // Exclusive OFF → the Exclusive card renders but no strict sub-toggle.
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.exclusive = false;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("sp-stage-exclusive").is_some(), "the card renders");
        assert!(
            rt.find("exclusive-strict-toggle").is_none(),
            "no strict toggle when exclusive is off"
        );

        // Exclusive ON → the strict sub-toggle appears.
        let mut s2 = populated_state();
        s2.route = Route::Settings;
        s2.settings_tab = SettingsTab::Audio;
        s2.exclusive = true;
        let mut rt2 = Runtime::new(s2, view, (1100.0, 720.0), env);
        rt2.frame(0.0);
        assert!(
            rt2.find("exclusive-strict-toggle").is_some(),
            "the strict toggle shows once exclusive is on"
        );
    }

    /// The DSP chain's unit card renders the real graphic-EQ controls — a per-unit
    /// enable chip, a preamp, the graph with its nodes, and the Import/Export/Add
    /// band controls — all namespaced to the card's position (unit 0 here).
    #[test]
    fn eq_card_renders_bands_and_enable_in_settings_audio() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        // Set the bands explicitly so a sibling test's persisted EQ (e.g. a 1-band
        // import) can't leak in via `populated_state()` and drop `eq-node-0-7`.
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        // Card 0 open so its body (graph + bands) lays out.
        s.dsp_expanded = vec![true];
        apply_locale("en");
        // Tall window so every band row lays out within the scroll content.
        let mut rt = Runtime::new(s, view, (1100.0, 2200.0), env);
        rt.frame(0.0);
        for key in [
            "sp-stage-dsp",
            "dsp-card-0",
            "dsp-en-0",
            "eq-preamp-0",
            "eq-graph-0",
            "eq-node-0-0",
            "eq-node-0-7",
            "eq-import-0",
            "eq-export-0",
            "eq-add-band-0",
        ] {
            assert!(rt.find(key).is_some(), "the EQ card renders {key}");
        }
    }

    /// Clicking a unit card's enable chip flips that unit's enable.
    #[test]
    fn toggling_the_eq_enable_flips_state() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        // Start from a known-off state (the shared test config dir may carry an
        // `enabled` flag from another test's persisted EQ).
        active_eq_mut(&mut s).enabled = false;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2200.0), env);
        rt.frame(0.0);
        assert!(
            !active_eq(&rt.state).enabled,
            "the EQ starts off for this test"
        );
        click_key(&mut rt, "dsp-en-0");
        assert!(
            active_eq(&rt.state).enabled,
            "clicking the chip enables the EQ"
        );
    }

    /// Dragging a band's handle on the graph moves its frequency (x) and gain (y) —
    /// the end-to-end path through the graph's `on_drag` → `set_eq_band`.
    #[test]
    fn dragging_a_band_moves_its_frequency_and_gain() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        active_eq_mut(&mut s).enabled = true;
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2200.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("eq-graph-0").expect("the EQ graph renders");
        // Press near the centre: grabs the band nearest that frequency.
        rt.pointer_down(x, y);
        rt.frame(0.0);
        let (_, i) = rt.state.eq_selected.expect("pressing selects a band");
        let before = active_eq(&rt.state).bands[i];
        // Drag right (higher frequency) and up (more gain).
        rt.pointer_move(x + 60.0, y - 50.0);
        rt.frame(0.0);
        let after = active_eq(&rt.state).bands[i];
        assert!(
            after.freq > before.freq,
            "drag right raised the frequency ({} -> {})",
            before.freq,
            after.freq
        );
        assert!(
            after.gain_db > before.gain_db,
            "drag up raised the gain ({} -> {})",
            before.gain_db,
            after.gain_db
        );
        rt.pointer_up(x + 60.0, y - 50.0);
        rt.frame(0.0);
        assert_eq!(rt.state.eq_drag, None, "the drag ended");
    }

    /// Scrolling the wheel over the graph narrows/widens the nearest band's Q — the
    /// path through the graph's `on_wheel` → `set_eq_band_q`.
    #[test]
    fn scrolling_over_the_graph_changes_a_bands_q() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        active_eq_mut(&mut s).enabled = true;
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2200.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("eq-graph-0").expect("the EQ graph renders");
        // Scroll up → narrower (higher Q) on the nearest band (default Q 1.0).
        rt.wheel_event(WheelEvent::new(0.0, -10.0).at(x, y));
        rt.frame(0.0);
        let (_, i) = rt
            .state
            .eq_selected
            .expect("the wheel selects the nearest band");
        let q_up = active_eq(&rt.state).bands[i].q;
        assert!(q_up > 1.0, "scroll up narrowed the band (Q {q_up} > 1.0)");
        // Two notches down → wider again.
        rt.wheel_event(WheelEvent::new(0.0, 10.0).at(x, y));
        rt.wheel_event(WheelEvent::new(0.0, 10.0).at(x, y));
        rt.frame(0.0);
        assert!(
            active_eq(&rt.state).bands[i].q < q_up,
            "scroll down widened it again ({} < {q_up})",
            active_eq(&rt.state).bands[i].q
        );
    }

    /// The expandable editor adds and removes bands and cycles a band's shape.
    #[test]
    fn the_band_editor_adds_removes_and_edits_bands() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        active_eq_mut(&mut s).enabled = true;
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2600.0), env);
        rt.frame(0.0);
        let n0 = active_eq(&rt.state).bands.len();
        // Add a band.
        click_key(&mut rt, "eq-add-band-0");
        rt.frame(0.0);
        assert_eq!(
            active_eq(&rt.state).bands.len(),
            n0 + 1,
            "Add band appends a band"
        );
        // Cycle band 1's shape.
        let k0 = active_eq(&rt.state).bands[1].kind;
        click_key(&mut rt, "eq-band-type-0-1");
        rt.frame(0.0);
        assert_ne!(
            active_eq(&rt.state).bands[1].kind,
            k0,
            "the type chip cycles the shape"
        );
        // Remove band 0.
        click_key(&mut rt, "eq-band-rm-0-0");
        rt.frame(0.0);
        assert_eq!(active_eq(&rt.state).bands.len(), n0, "remove drops a band");
    }

    /// The band-table numbers are Ableton-style number boxes: unit-test the gesture
    /// mutators directly — a vertical drag applies absolutely from the press value
    /// (up raises it), the wheel nudges it one notch, and gain/freq/Q each use their
    /// own scale (linear dB, geometric Hz, geometric Q).
    #[test]
    fn band_table_number_gestures_change_the_value() {
        let _guard = config::use_test_config_dir();
        let mut s = populated_state();
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        // Gain drag: start at band 1's gain; dragging up (negative dy) raises it,
        // and the result is measured from the press value, not accumulated.
        let g0 = active_eq(&s).bands[1].gain_db;
        eq_num_drag_start(&mut s, 0, 1, NumField::Gain);
        eq_num_drag_move(&mut s, -30.0); // 30 pt up ≈ +3.0 dB
        let g_up = active_eq(&s).bands[1].gain_db;
        assert!(g_up > g0, "drag up raised gain ({g0} -> {g_up})");
        eq_num_drag_move(&mut s, 0.0); // back to the press point
        assert!(
            (active_eq(&s).bands[1].gain_db - g0).abs() < 0.05,
            "drag is absolute from the press value, not cumulative"
        );
        eq_num_drag_end(&mut s);
        assert_eq!(s.eq_num_drag, None, "the drag ended");
        // Frequency drag is geometric: dragging up raises Hz.
        let f0 = active_eq(&s).bands[2].freq;
        eq_num_drag_start(&mut s, 0, 2, NumField::Freq);
        eq_num_drag_move(&mut s, -FREQ_DRAG_PX_PER_OCTAVE); // one octave up
        assert!(
            (active_eq(&s).bands[2].freq / f0 - 2.0).abs() < 0.05,
            "one octave of drag doubled the frequency ({f0} -> {})",
            active_eq(&s).bands[2].freq
        );
        eq_num_drag_end(&mut s);
        // Wheel: up widens/raises; here Q up = narrower (higher Q).
        let q0 = active_eq(&s).bands[0].q;
        eq_num_scroll(&mut s, 0, 0, NumField::Q, true, false);
        assert!(active_eq(&s).bands[0].q > q0, "scroll up raised Q");
        eq_num_scroll(&mut s, 0, 0, NumField::Q, false, false);
        eq_num_scroll(&mut s, 0, 0, NumField::Q, false, false);
        assert!(
            active_eq(&s).bands[0].q < q0,
            "two notches down lowered it below start"
        );
    }

    /// Holding Shift while scrolling a band number makes a finer step than a plain
    /// notch — the DAW fine-adjust gesture (the wheel handler passes `ev.mods.shift`).
    #[test]
    fn shift_scroll_fine_tunes_a_band_number() {
        let _guard = config::use_test_config_dir();
        let mut s = populated_state();
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        let g0 = active_eq(&s).bands[0].gain_db;
        // One coarse notch up.
        eq_num_scroll(&mut s, 0, 0, NumField::Gain, true, false);
        let coarse = active_eq(&s).bands[0].gain_db - g0;
        // Reset, then one Shift (fine) notch up.
        set_eq_band_gain(&mut s, 0, 0, g0);
        eq_num_scroll(&mut s, 0, 0, NumField::Gain, true, true);
        let fine = active_eq(&s).bands[0].gain_db - g0;
        assert!(
            fine > 0.0 && fine < coarse,
            "Shift makes a finer gain step (fine {fine} < coarse {coarse})"
        );
    }

    /// Regression: scrolling a band number must step in BOTH directions — a notched
    /// wheel up raises it and down lowers it (the bug had both directions lowering).
    /// Drives the real `on_wheel` handler through the harness.
    #[test]
    fn number_box_wheel_steps_both_directions() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        active_eq_mut(&mut s).enabled = true;
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2600.0), env);
        rt.frame(0.0);
        let (x, y) = rt
            .find("eq-num-freq-0-0")
            .expect("band 0's freq box renders");
        let f0 = active_eq(&rt.state).bands[0].freq;
        rt.wheel_event(WheelEvent::new(0.0, -10.0).at(x, y)); // up
        rt.frame(0.0);
        let up = active_eq(&rt.state).bands[0].freq;
        assert!(up > f0, "scroll up raised the frequency ({f0} -> {up})");
        rt.wheel_event(WheelEvent::new(0.0, 10.0).at(x, y)); // down
        rt.wheel_event(WheelEvent::new(0.0, 10.0).at(x, y));
        rt.frame(0.0);
        assert!(
            active_eq(&rt.state).bands[0].freq < up,
            "scroll down lowered it again ({up} -> {})",
            active_eq(&rt.state).bands[0].freq
        );
    }

    /// A trackpad's fine `precise` wheel deltas accumulate into discrete steps and
    /// still reverse direction (the one-direction bug was worst on precise devices).
    #[test]
    fn number_box_trackpad_wheel_accumulates_and_reverses() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        active_eq_mut(&mut s).enabled = true;
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2600.0), env);
        rt.frame(0.0);
        let (x, y) = rt
            .find("eq-num-freq-0-0")
            .expect("band 0's freq box renders");
        let f0 = active_eq(&rt.state).bands[0].freq;
        // Several small upward precise deltas sum past a notch → at least one step up.
        for _ in 0..4 {
            rt.wheel_event(WheelEvent::new(0.0, -8.0).at(x, y).precise(true));
        }
        rt.frame(0.0);
        let up = active_eq(&rt.state).bands[0].freq;
        assert!(
            up > f0,
            "precise scroll up raised the frequency ({f0} -> {up})"
        );
        for _ in 0..4 {
            rt.wheel_event(WheelEvent::new(0.0, 8.0).at(x, y).precise(true));
        }
        rt.frame(0.0);
        assert!(
            active_eq(&rt.state).bands[0].freq < up,
            "precise scroll down reversed it ({up} -> {})",
            active_eq(&rt.state).bands[0].freq
        );
    }

    /// Windows turns Shift+wheel into a HORIZONTAL wheel: the delta arrives in `dx`
    /// with `dy == 0`. The number box must still step (the "Shift+scroll does
    /// nothing" bug), with the same direction convention as the vertical wheel.
    #[test]
    fn number_box_horizontal_wheel_steps_like_vertical() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        active_eq_mut(&mut s).enabled = true;
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2600.0), env);
        rt.frame(0.0);
        let (x, y) = rt
            .find("eq-num-freq-0-0")
            .expect("band 0's freq box renders");
        let f0 = active_eq(&rt.state).bands[0].freq;
        // dx < 0 (horizontal "up") raises, mirroring dy < 0.
        rt.wheel_event(WheelEvent::new(-40.0, 0.0).at(x, y));
        rt.frame(0.0);
        let up = active_eq(&rt.state).bands[0].freq;
        assert!(
            up > f0,
            "horizontal (dx) scroll up raised freq ({f0} -> {up})"
        );
        rt.wheel_event(WheelEvent::new(40.0, 0.0).at(x, y));
        rt.wheel_event(WheelEvent::new(40.0, 0.0).at(x, y));
        rt.frame(0.0);
        assert!(
            active_eq(&rt.state).bands[0].freq < up,
            "horizontal (dx) scroll down lowered it ({up} -> {})",
            active_eq(&rt.state).bands[0].freq
        );
    }

    /// Holding Shift while dragging a band number fine-tunes it — the same drag
    /// travel changes the value less (the drag reads `State::shift_held`, which the
    /// key handler keeps current).
    #[test]
    fn shift_drag_fine_tunes_a_band_number() {
        let _guard = config::use_test_config_dir();
        let mut s = populated_state();
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        let g0 = active_eq(&s).bands[1].gain_db;
        // Plain drag up by 20 points.
        eq_num_drag_start(&mut s, 0, 1, NumField::Gain);
        eq_num_drag_move(&mut s, -20.0);
        let coarse = active_eq(&s).bands[1].gain_db - g0;
        eq_num_drag_end(&mut s);
        // Same drag with Shift held — a finer change.
        set_eq_band_gain(&mut s, 0, 1, g0);
        s.shift_held = true;
        eq_num_drag_start(&mut s, 0, 1, NumField::Gain);
        eq_num_drag_move(&mut s, -20.0);
        let fine = active_eq(&s).bands[1].gain_db - g0;
        assert!(
            fine > 0.0 && fine < coarse,
            "Shift+drag is finer (fine {fine} < coarse {coarse})"
        );
    }

    /// Double-clicking a band number opens an inline text field; typing a value and
    /// committing applies it. Drives the real widget through the harness (double
    /// press → IME commit → Return).
    #[test]
    fn double_clicking_a_band_number_types_a_new_value() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        active_eq_mut(&mut s).enabled = true;
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2600.0), env);
        rt.frame(0.0);
        let (x, y) = rt
            .find("eq-num-freq-0-1")
            .expect("band 1's freq number renders");
        // Double-click the frequency to open the editor.
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert!(
            rt.state.eq_edit.is_some(),
            "double-click opens the inline editor"
        );
        // Type a new frequency and commit with Return.
        rt.ime(rune::Ime::Commit("5000".into()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        assert!(rt.state.eq_edit.is_none(), "Return closes the editor");
        assert!(
            (active_eq(&rt.state).bands[1].freq - 5000.0).abs() < 1.0,
            "the typed frequency applied (got {})",
            active_eq(&rt.state).bands[1].freq
        );
    }

    /// Double-clicking a DSP card's title opens an inline rename field; typing a name
    /// and committing applies it to the unit (and clears the rename marker). Drives the
    /// real widget through the harness (double press → IME commit → Return), mirroring
    /// `double_clicking_a_band_number_types_a_new_value`.
    #[test]
    fn double_clicking_a_dsp_title_renames_the_unit() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.dsp_units = vec![DspUnit::Eq(EqUnit::default())];
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2600.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("dsp-name-0").expect("the unit title renders");
        // Double-click the title to open the rename editor.
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert_eq!(
            rt.state.dsp_rename,
            Some(0),
            "double-click opens the inline rename editor"
        );
        // Type a name and commit with Return.
        rt.ime(rune::Ime::Commit("my-name".into()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        assert_eq!(rt.state.dsp_rename, None, "Return closes the rename editor");
        assert_eq!(
            rt.state.dsp_units[0].name(),
            Some("my-name"),
            "the typed name applied to the unit (got {:?})",
            rt.state.dsp_units[0].name()
        );
    }

    /// The unit title reads "<name> (EQ)" when the unit is named, and just "EQ" when it
    /// isn't — the rename feature's read side.
    #[test]
    fn a_named_unit_shows_its_name_beside_the_type() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let label_for = |name: Option<&str>| {
            let mut s = populated_state();
            s.route = Route::Settings;
            s.settings_tab = SettingsTab::Audio;
            s.dsp_units = vec![DspUnit::Eq(EqUnit {
                name: name.map(String::from),
                ..EqUnit::default()
            })];
            // Collapse the card so the whole stage lays out within the window.
            s.dsp_expanded = vec![false];
            apply_locale("en");
            let mut rt = Runtime::new(s, view, (1100.0, 2600.0), env);
            rt.frame(0.0);
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some("dsp-name-0"))
                .and_then(|e| e.text)
                .expect("the unit title renders")
        };
        assert_eq!(label_for(Some("my-name")), "my-name (EQ)");
        assert_eq!(label_for(None), "EQ");
    }

    /// Opening the "+ Add DSP" dropdown while scrolled down must NOT jump the page
    /// to the top (the reported bug): the dropdown anchors to the button where it is.
    #[test]
    fn opening_add_dsp_does_not_scroll_the_page() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        // A tall chain (first card expanded) so the content overflows a short window
        // and the Add control sits well below the fold.
        let bands = soul_player_host::default_bands().to_vec();
        s.dsp_units = vec![
            DspUnit::Eq(EqUnit {
                bands,
                ..EqUnit::default()
            }),
            DspUnit::Eq(EqUnit::default()),
            DspUnit::Eq(EqUnit::default()),
        ];
        s.dsp_expanded = vec![true, false, false];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1300.0, 640.0), env);
        rt.frame(0.0);
        // Scroll down until the Add control is in view.
        for _ in 0..30 {
            rt.wheel(900.0, 400.0, -120.0);
        }
        rt.frame(0.0);
        let (ax, ay) = rt
            .find("dsp-add")
            .expect("Add DSP is in view after scrolling");
        // Click it, then check the Add control stays roughly where it was (the page
        // did not jump to the top, which would shove the control far off-screen).
        rt.pointer_down(ax, ay);
        rt.pointer_up(ax, ay);
        rt.settle_all();
        rt.frame(0.0);
        assert_eq!(
            rt.state.open_dropdown.as_deref(),
            Some("dsp-add"),
            "the dropdown opened"
        );
        let add_after = rt.find("dsp-add").map(|(_, y)| y);
        assert!(
            add_after.is_some_and(|y| (y - ay).abs() < 60.0),
            "the Add control stayed put (was {ay}, now {add_after:?}) — the page did not jump"
        );
    }

    /// "+ Add DSP" opens the themed type dropdown — EQ selectable plus a disabled
    /// "coming soon" teaser row; picking EQ appends a unit and opens its card.
    #[test]
    fn add_dsp_menu_offers_eq_and_a_coming_soon_row() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.dsp_units = vec![DspUnit::Eq(EqUnit::default())];
        s.dsp_expanded = vec![false];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1500.0, 2200.0), env);
        rt.frame(0.0);
        // Open the dropdown, settle its enter animation.
        click_key(&mut rt, "dsp-add");
        rt.settle_all();
        rt.frame(0.0);
        assert_eq!(rt.state.open_dropdown.as_deref(), Some("dsp-add"));
        assert!(rt.find("dsp-add-opt-eq").is_some(), "EQ is listed");
        assert!(
            rt.find("dsp-add-opt-soon").is_some(),
            "a 'more DSP coming soon' teaser row is shown"
        );
        // Pick EQ → appends a unit + opens its card + closes the menu.
        click_key(&mut rt, "dsp-add-opt-eq");
        rt.frame(0.0);
        assert_eq!(rt.state.dsp_units.len(), 2, "picking EQ appends a unit");
        assert_eq!(rt.state.open_dropdown, None, "picking closes the menu");
        assert_eq!(
            rt.state.dsp_expanded,
            vec![false, true],
            "the appended card opens, the existing one is untouched"
        );
    }

    /// The reorder drop line marks the correct GAP for the drag direction: dragging
    /// the top card down to the bottom shows the line BELOW the last card (not above
    /// it), and dragging the bottom card up shows it ABOVE the first.
    #[test]
    fn the_drop_line_follows_the_drag_direction() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let drag_scene = |drag: (usize, usize)| {
            let mut s = populated_state();
            s.route = Route::Settings;
            s.settings_tab = SettingsTab::Audio;
            s.dsp_units = vec![
                DspUnit::Eq(EqUnit::default()),
                DspUnit::Eq(EqUnit::default()),
                DspUnit::Eq(EqUnit::default()),
            ];
            s.dsp_expanded = vec![false, false, false];
            s.dsp_drag = Some(drag);
            apply_locale("en");
            let mut rt = Runtime::new(s, view, (1500.0, 2600.0), env);
            rt.frame(0.0);
            let (_, drop) = rt.find("dsp-drop").expect("the drop line renders");
            let (_, first) = rt.find("dsp-card-0").expect("the first card renders");
            let (_, last) = rt.find("dsp-card-2").expect("the last card renders");
            (drop, first, last)
        };
        // Dragging card 0 down onto slot 2 → the line lands below the last card.
        let (drop_down, _, last_card) = drag_scene((0, 2));
        assert!(
            drop_down > last_card,
            "dragging down, the line is below the last card ({drop_down} > {last_card})"
        );
        // Dragging card 2 up onto slot 0 → the line lands above the first card.
        let (drop_up, first_card, _) = drag_scene((2, 0));
        assert!(
            drop_up < first_card,
            "dragging up, the line is above the first card ({drop_up} < {first_card})"
        );
    }

    /// Per-card chips + grip: a card's enable chip toggles only its unit, the grip
    /// reorders the chain (carrying the expand flag), and the remove chip drops it.
    #[test]
    fn per_card_toggle_remove_and_reorder() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        // Three collapsed units so every header (and its chips/grip) lays out.
        s.dsp_units = vec![
            DspUnit::Eq(EqUnit::default()),
            DspUnit::Eq(EqUnit::default()),
            DspUnit::Eq(EqUnit::default()),
        ];
        set_dsp_unit_enabled(&mut s, 0, true);
        set_dsp_unit_enabled(&mut s, 1, true);
        set_dsp_unit_enabled(&mut s, 2, true);
        // Distinct expand flags so the reorder's flag-tracking is observable.
        s.dsp_expanded = vec![true, false, false];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1500.0, 2600.0), env);
        rt.frame(0.0);
        // Enable chip toggles ONLY unit 1.
        click_key(&mut rt, "dsp-en-1");
        rt.frame(0.0);
        assert!(!rt.state.dsp_units[1].enabled(), "the chip disabled unit 1");
        assert!(rt.state.dsp_units[0].enabled(), "unit 0 untouched");
        assert!(rt.state.dsp_units[2].enabled(), "unit 2 untouched");
        // Grip-drag unit 0 ONTO unit 1's row → it reorders to index 1, its open flag
        // riding along. The drop target is whichever card the pointer is over (exact,
        // height-agnostic), resolved by `element_at` — so settle a frame after the move
        // for the pick to land before releasing.
        let (gx, gy0) = rt.find("dsp-grip-0").expect("unit 0's grip renders");
        let (_, gy1) = rt.find("dsp-grip-1").expect("unit 1's grip renders");
        rt.pointer_down(gx, gy0);
        rt.pointer_move(gx, gy1);
        rt.frame(0.0);
        rt.pointer_up(gx, gy1);
        rt.frame(0.0);
        assert_eq!(
            rt.state.dsp_expanded,
            vec![false, true, false],
            "the dragged card's open flag moved with it to index 1"
        );
        // Remove chip drops its unit.
        let n = rt.state.dsp_units.len();
        click_key(&mut rt, "dsp-rm-2");
        rt.frame(0.0);
        assert_eq!(rt.state.dsp_units.len(), n - 1, "the chip removed a unit");
        assert_eq!(
            rt.state.dsp_expanded.len(),
            n - 1,
            "the expand flags stay parallel to the units"
        );
    }

    /// Audio profiles: editing auto-saves into the active profile; new/switch/duplicate/
    /// delete manage the set; the live chain follows the active profile; the last profile
    /// can't be deleted; and editing one profile leaves the others untouched.
    #[test]
    fn profile_mutators_switch_create_duplicate_delete_and_autosave() {
        let _g = config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        // The shared test config dir may carry another test's profiles, so set a known
        // starting state explicitly (per the config-test convention).
        s.profile_set = ProfileSet::new(
            vec![Profile::new(
                "Default",
                DspChain::new(vec![DspUnit::Eq(EqUnit::default())]),
            )],
            0,
        );
        s.dsp_units = s.profile_set.active_chain().unwrap().units.clone();

        // Editing the live chain auto-saves into the active profile.
        set_eq_preamp(&mut s, 0, -5.0);
        assert_eq!(
            s.profile_set.active_chain().unwrap().units[0]
                .as_eq()
                .unwrap()
                .preamp_db,
            -5.0,
            "an edit writes through to the active profile"
        );

        // A new profile starts empty and becomes active.
        new_profile(&mut s);
        assert_eq!(s.profile_set.len(), 2);
        assert_eq!(s.profile_set.active_index(), 1);
        assert!(s.dsp_units.is_empty(), "a new profile has an empty chain");

        // Switching back restores the first profile's chain.
        switch_profile(&mut s, 0);
        assert_eq!(s.profile_set.active_index(), 0);
        assert_eq!(
            s.dsp_units[0].as_eq().unwrap().preamp_db,
            -5.0,
            "switching reloads the profile's chain"
        );

        // Duplicate carries the chain under "<name> copy" and switches to the copy;
        // editing the copy leaves the original untouched.
        duplicate_profile(&mut s);
        assert_eq!(s.profile_set.len(), 3);
        let dup = s.profile_set.active_index();
        assert!(
            s.profile_set.profiles()[dup].name.ends_with("copy"),
            "copy is suffixed"
        );
        assert_eq!(s.dsp_units[0].as_eq().unwrap().preamp_db, -5.0);
        set_eq_preamp(&mut s, 0, 2.0);
        assert_eq!(
            s.profile_set.profiles()[dup].chain.units[0]
                .as_eq()
                .unwrap()
                .preamp_db,
            2.0
        );
        assert_eq!(
            s.profile_set.profiles()[0].chain.units[0]
                .as_eq()
                .unwrap()
                .preamp_db,
            -5.0,
            "the original profile is untouched by edits to the copy"
        );

        // Rename the active profile (disambiguated against siblings).
        rename_profile(&mut s, dup, "Studio".into());
        assert_eq!(s.profile_set.profiles()[dup].name, "Studio");

        // Delete drops the active and switches to a neighbour; the last can't be deleted.
        let before = s.profile_set.len();
        delete_profile(&mut s);
        assert_eq!(s.profile_set.len(), before - 1);
        while s.profile_set.len() > 1 {
            delete_profile(&mut s);
        }
        delete_profile(&mut s);
        assert_eq!(s.profile_set.len(), 1, "the last profile can't be deleted");
    }

    /// A profile round-trips through the portable JSON, and importing a file appends it
    /// as a new (disambiguated) profile that becomes the live one.
    #[test]
    fn profile_json_round_trips_and_import_appends_disambiguated() {
        // Config-level: profile → JSON → profile preserves name + chain.
        let studio = Profile::new(
            "Studio",
            DspChain::new(vec![DspUnit::Eq(EqUnit {
                preamp_db: -3.0,
                ..EqUnit::default()
            })]),
        );
        let json = config::profile_to_json(&studio).unwrap();
        assert_eq!(config::profile_from_json(&json).unwrap(), studio);

        // State-level: importing a file appends + switches; a clashing name is suffixed.
        let _g = config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        // Known starting state (the shared test config dir may carry other tests' data).
        s.profile_set = ProfileSet::new(
            vec![Profile::new(
                "Default",
                DspChain::new(vec![DspUnit::Eq(EqUnit::default())]),
            )],
            0,
        );
        s.dsp_units = s.profile_set.active_chain().unwrap().units.clone();

        let clash = config::profile_to_json(&Profile::new(
            "Default",
            DspChain::new(vec![DspUnit::Eq(EqUnit {
                preamp_db: 4.0,
                ..EqUnit::default()
            })]),
        ))
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("soul-profile-in-{}.json", std::process::id()));
        std::fs::write(&path, clash).unwrap();
        import_profile(&mut s, &path);
        let _ = std::fs::remove_file(&path);

        assert_eq!(s.profile_set.len(), 2, "import appended a profile");
        assert_eq!(s.profile_set.active_index(), 1, "and switched to it");
        assert_eq!(
            s.profile_set.profiles()[1].name,
            "Default 2",
            "the clashing name is disambiguated"
        );
        assert_eq!(
            s.dsp_units[0].as_eq().unwrap().preamp_db,
            4.0,
            "the imported chain is live"
        );
    }

    /// Exporting the active profile to a real file and importing it back reproduces the
    /// whole chain (name disambiguated) and the imported profile goes live on the engine.
    #[test]
    fn profile_export_then_import_round_trips_through_a_file_and_goes_live() {
        let _g = config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        s.profile_set = ProfileSet::new(
            vec![Profile::new(
                "Mastering",
                DspChain::new(vec![
                    DspUnit::Eq(EqUnit {
                        preamp_db: -3.5,
                        name: Some("low".into()),
                        ..EqUnit::default()
                    }),
                    DspUnit::Eq(EqUnit {
                        preamp_db: 1.0,
                        ..EqUnit::default()
                    }),
                ]),
            )],
            0,
        );
        s.dsp_units = s.profile_set.active_chain().unwrap().units.clone();
        let original = s.profile_set.profiles()[0].chain.clone();

        let path =
            std::env::temp_dir().join(format!("soul-profile-rt-{}.json", std::process::id()));
        export_profile(&s, &path);
        assert!(path.exists(), "export wrote the file");

        import_profile(&mut s, &path);
        let _ = std::fs::remove_file(&path);

        assert_eq!(s.profile_set.len(), 2, "import appended a profile");
        assert_eq!(s.profile_set.active_index(), 1, "and switched to it");
        assert_eq!(
            s.profile_set.profiles()[1].name,
            "Mastering 2",
            "the re-imported name is disambiguated"
        );
        assert_eq!(
            s.profile_set.profiles()[1].chain,
            original,
            "the whole chain survives export → file → import"
        );
        assert_eq!(
            s.host.dsp_chain().units,
            original.units,
            "the imported profile is live on the engine"
        );
    }

    /// Switching profiles while playback is live reloads the chain AND pushes it to the
    /// engine with a `generation` bump (what makes the device sink re-apply mid-stream);
    /// playback keeps running across the switch.
    #[test]
    fn switching_profiles_while_playing_pushes_the_new_chain_to_the_engine() {
        let _g = config::use_test_config_dir();
        let mut s = populated_state();
        s.profile_set = ProfileSet::new(
            vec![
                Profile::new(
                    "Flat",
                    DspChain::new(vec![DspUnit::Eq(EqUnit {
                        preamp_db: 0.0,
                        enabled: false,
                        ..EqUnit::default()
                    })]),
                ),
                Profile::new(
                    "Loud",
                    DspChain::new(vec![DspUnit::Eq(EqUnit {
                        preamp_db: 9.0,
                        enabled: true,
                        ..EqUnit::default()
                    })]),
                ),
            ],
            0,
        );
        s.dsp_units = s.profile_set.active_chain().unwrap().units.clone();
        apply_dsp(&mut s); // seed the engine with the active (Flat) chain

        // Simulate live playback.
        s.playing = true;
        s.now_playing_id = s.library.first().map(|t| t.id);

        assert_eq!(
            s.host.dsp_chain().units,
            s.profile_set.profiles()[0].chain.units,
            "the engine holds the active (Flat) chain"
        );
        let gen_before = s.host.dsp_chain().generation;

        switch_profile(&mut s, 1);

        assert!(s.playing, "playback keeps running across the switch");
        assert_eq!(s.profile_set.active_index(), 1);
        assert_eq!(
            s.dsp_units,
            s.profile_set.profiles()[1].chain.units,
            "the live chain is now Loud"
        );
        let live = s.host.dsp_chain();
        assert_eq!(
            live.units,
            s.profile_set.profiles()[1].chain.units,
            "the engine received the switched chain"
        );
        assert!(
            live.generation > gen_before,
            "a generation bump tells the sink to re-apply live"
        );
    }

    /// The profile bar renders its switcher + action buttons; switching via the dropdown
    /// makes the chosen profile's chain live; and Rename opens the inline field.
    #[test]
    fn profile_bar_renders_switches_and_renames() {
        let _g = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.profile_set = ProfileSet::new(
            vec![
                Profile::new(
                    "Studio",
                    DspChain::new(vec![DspUnit::Eq(EqUnit {
                        preamp_db: -5.0,
                        ..EqUnit::default()
                    })]),
                ),
                Profile::new(
                    "Bass",
                    DspChain::new(vec![DspUnit::Eq(EqUnit {
                        preamp_db: 6.0,
                        ..EqUnit::default()
                    })]),
                ),
            ],
            0,
        );
        s.dsp_units = s.profile_set.active_chain().unwrap().units.clone();
        s.dsp_expanded = vec![false];
        s.eq_bands_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1300.0, 900.0), env);
        rt.frame(0.0);

        // The bar + each control renders.
        for key in [
            "profile-bar",
            "profile-new",
            "profile-duplicate",
            "profile-rename",
            "profile-delete",
            "profile-import",
            "profile-export",
        ] {
            assert!(rt.find(key).is_some(), "{key} renders");
        }

        // Open the switcher and pick "Bass" (index 1) → it becomes active and live.
        click_key(&mut rt, "profile-picker");
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "profile-picker-opt-1");
        rt.frame(0.0);
        assert_eq!(
            rt.state.profile_set.active_index(),
            1,
            "picking an entry switches profile"
        );
        assert_eq!(
            rt.state.dsp_units[0].as_eq().unwrap().preamp_db,
            6.0,
            "the chosen profile's chain is now live"
        );

        // Rename opens the inline editor field (distinct key from the Rename button).
        click_key(&mut rt, "profile-rename");
        rt.frame(0.0);
        assert!(
            rt.state.profile_rename.is_some(),
            "Rename opens the inline editor"
        );
        assert!(
            rt.find("profile-rename-field").is_some(),
            "the inline rename field renders"
        );
    }

    /// Reorder snaps to the card UNDER the pointer, not a fixed row height: dragging a
    /// collapsed card deep into a tall EXPANDED card lands it at that card's slot. A
    /// fixed dy→slot mapping over-counts the expanded card's height and clamps the drop
    /// to the chain's end instead — this is the "dragging isn't exact" bug.
    #[test]
    fn reorder_snaps_to_the_hovered_card_regardless_of_height() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.dsp_units = vec![
            DspUnit::Eq(EqUnit::default()),
            DspUnit::Eq(EqUnit::default()),
            DspUnit::Eq(EqUnit::default()),
        ];
        set_dsp_unit_enabled(&mut s, 0, true);
        set_dsp_unit_enabled(&mut s, 1, true);
        set_dsp_unit_enabled(&mut s, 2, true);
        // Unit 1 EXPANDED (tall: preamp + graph + band table); 0 and 2 collapsed. Tag
        // each unit with a distinct preamp so we can see where it lands post-reorder.
        s.dsp_expanded = vec![false, true, false];
        set_eq_preamp(&mut s, 0, -6.0);
        set_eq_preamp(&mut s, 1, 0.0);
        set_eq_preamp(&mut s, 2, 6.0);
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1500.0, 2600.0), env);
        rt.frame(0.0);
        // Drag unit 0's grip DEEP into unit 1's expanded body (far past one row height).
        let (gx, gy0) = rt.find("dsp-grip-0").expect("unit 0's grip");
        let (_, gy1) = rt.find("dsp-grip-1").expect("unit 1's grip");
        let into_body = gy1 + 160.0;
        rt.pointer_down(gx, gy0);
        rt.pointer_move(gx, into_body);
        rt.frame(0.0);
        rt.pointer_up(gx, into_body);
        rt.frame(0.0);
        // Unit 0 landed AT unit 1 (the hovered card) → order [1, 0, 2]; NOT clamped to
        // the chain's end ([1, 2, 0]) as the old fixed-height mapping would.
        let preamps: Vec<f32> = rt
            .state
            .dsp_units
            .iter()
            .map(|u| u.as_eq().expect("eq unit").preamp_db)
            .collect();
        assert_eq!(
            preamps,
            vec![0.0, -6.0, 6.0],
            "the dragged card dropped onto the hovered expanded card, not past it"
        );
    }

    /// The band table collapses within an open card: its chevron hides the band rows
    /// while the response graph stays.
    #[test]
    fn the_band_table_collapses_within_a_card() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.dsp_units = vec![DspUnit::Eq(EqUnit {
            bands: soul_player_host::default_bands().to_vec(),
            preamp_db: 0.0,
            enabled: true,
            name: None,
        })];
        s.dsp_expanded = vec![true];
        s.eq_bands_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1200.0, 2600.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("eq-band-row-0-0").is_some(),
            "band rows show when the table is expanded"
        );
        // Collapse the band table via its chevron.
        click_key(&mut rt, "eq-bands-expand-0");
        rt.frame(0.0);
        assert!(
            !rt.state.eq_bands_expanded[0],
            "the chevron collapsed the band table"
        );
        assert!(
            rt.find("eq-band-row-0-0").is_none(),
            "band rows are hidden when collapsed"
        );
        assert!(
            rt.find("eq-graph-0").is_some(),
            "the response graph stays visible"
        );
    }

    /// Two expanded EQ cards are fully independent: editing a band in card 1 leaves
    /// card 0's bands untouched, and both graphs render their own namespaced nodes.
    #[test]
    fn two_expanded_eq_cards_edit_independently() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        // Two EQ units, both enabled, both with the full default band set, both open.
        s.dsp_units = vec![
            DspUnit::Eq(EqUnit::default()),
            DspUnit::Eq(EqUnit::default()),
        ];
        for unit in s.dsp_units.iter_mut().take(2) {
            let eq = unit.as_eq_mut().unwrap();
            eq.enabled = true;
            eq.bands = soul_player_host::default_bands().to_vec();
        }
        s.dsp_expanded = vec![true, true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1500.0, 4200.0), env);
        rt.frame(0.0);
        // Both graphs render their own nodes, namespaced by unit.
        for key in ["eq-node-0-0", "eq-node-0-7", "eq-node-1-0", "eq-node-1-7"] {
            assert!(rt.find(key).is_some(), "both graphs render {key}");
        }
        let u0_before = rt.state.dsp_units[0].as_eq().unwrap().bands[0].freq;
        let u1_before = rt.state.dsp_units[1].as_eq().unwrap().bands[0].freq;
        // Scroll card 1's band-0 frequency box up — only unit 1 changes.
        let (x, y) = rt
            .find("eq-num-freq-1-0")
            .expect("card 1's band-0 freq box renders");
        rt.wheel_event(WheelEvent::new(0.0, -10.0).at(x, y));
        rt.frame(0.0);
        let u1_after = rt.state.dsp_units[1].as_eq().unwrap().bands[0].freq;
        let u0_after = rt.state.dsp_units[0].as_eq().unwrap().bands[0].freq;
        assert!(
            u1_after > u1_before,
            "scrolling card 1 raised its own band ({u1_before} -> {u1_after})"
        );
        assert_eq!(
            u0_after, u0_before,
            "card 0's bands are untouched by editing card 1"
        );
    }

    /// Importing an AutoEQ profile replaces the bands + preamp and enables the EQ;
    /// exporting round-trips the format.
    #[test]
    fn importing_and_exporting_an_autoeq_profile() {
        let _guard = config::use_test_config_dir();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("soul-eq-in-{}.txt", std::process::id()));
        std::fs::write(
            &path,
            "Preamp: -5.0 dB\n\
             Filter 1: ON PK Fc 120 Hz Gain 4.0 dB Q 1.00\n\
             Filter 2: ON HSC Fc 8000 Hz Gain -3.0 dB Q 0.70\n",
        )
        .unwrap();
        let mut s = populated_state();
        import_eq(&mut s, 0, &path);
        assert!(active_eq(&s).enabled, "import enables the EQ");
        assert_eq!(active_eq(&s).bands.len(), 2, "both filters import");
        assert!(
            (active_eq(&s).preamp_db + 5.0).abs() < 0.11,
            "preamp imported: {}",
            active_eq(&s).preamp_db
        );
        assert_eq!(active_eq(&s).bands[0].kind, BandKind::Peaking);
        assert!((active_eq(&s).bands[0].freq - 120.0).abs() < 1.0);
        assert_eq!(active_eq(&s).bands[1].kind, BandKind::HighShelf);
        // Export writes a re-importable profile.
        let out = dir.join(format!("soul-eq-out-{}.txt", std::process::id()));
        export_eq(&s, 0, &out);
        let txt = std::fs::read_to_string(&out).unwrap();
        assert!(
            txt.contains("Filter 1: ON PK Fc 120 Hz"),
            "exported profile round-trips: {txt}"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&out);
    }

    /// End-to-end: clicking **Import** opens an open-file dialog; answering it with
    /// a profile path imports the bands + preamp and enables the EQ — exercising the
    /// real `eq-import` button → `tx.open_file` → the runtime's scripted dialog →
    /// the `import_eq` callback.
    #[test]
    fn e2e_importing_an_autoeq_profile_through_the_button() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let path = std::env::temp_dir().join(format!("soul-eq-e2e-in-{}.txt", std::process::id()));
        std::fs::write(
            &path,
            "Preamp: -4.0 dB\nFilter 1: ON PK Fc 440 Hz Gain 6.0 dB Q 1.50\n",
        )
        .unwrap();
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.dsp_expanded = vec![true];
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2200.0), env);
        rt.frame(0.0);
        // Press Import → an open-file dialog is pending.
        click_key(&mut rt, "eq-import-0");
        rt.frame(0.0);
        let (kind, _opts) = rt.file_dialog().expect("Import opens a file dialog");
        assert_eq!(kind, FileDialogKind::Open);
        // Answer it with our profile; the import callback runs on the next frame.
        rt.resolve_file_dialog(Some(path.clone())).unwrap();
        rt.frame(0.0);
        assert!(active_eq(&rt.state).enabled, "import enabled the EQ");
        assert_eq!(active_eq(&rt.state).bands.len(), 1);
        assert_eq!(active_eq(&rt.state).bands[0].kind, BandKind::Peaking);
        assert!((active_eq(&rt.state).bands[0].freq - 440.0).abs() < 1.0);
        assert!((active_eq(&rt.state).preamp_db + 4.0).abs() < 0.11);
        let _ = std::fs::remove_file(&path);
    }

    /// End-to-end: clicking **Export** opens a save dialog; answering it writes a
    /// re-importable ParametricEQ profile of the current EQ.
    #[test]
    fn e2e_exporting_an_autoeq_profile_through_the_button() {
        let _guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.dsp_expanded = vec![true];
        active_eq_mut(&mut s).bands = soul_player_host::default_bands().to_vec();
        active_eq_mut(&mut s).bands[4].gain_db = 5.0; // the 2 kHz bell
        active_eq_mut(&mut s).preamp_db = -1.0;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 2200.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "eq-export-0");
        rt.frame(0.0);
        let (kind, _opts) = rt.file_dialog().expect("Export opens a file dialog");
        assert_eq!(kind, FileDialogKind::Save);
        let out = std::env::temp_dir().join(format!("soul-eq-e2e-out-{}.txt", std::process::id()));
        rt.resolve_file_dialog(Some(out.clone())).unwrap();
        rt.frame(0.0);
        let txt = std::fs::read_to_string(&out).expect("export wrote the file");
        assert!(txt.contains("Preamp: -1.0 dB"), "wrote the preamp: {txt}");
        assert!(
            txt.contains("Fc 2000 Hz Gain 5.0 dB"),
            "wrote the 2 kHz bell: {txt}"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Build Settings → Audio in a short window so its stage cards overflow the
    /// viewport and it must scroll. Returns the ready runtime (live waker set, one
    /// frame drawn) so the scroll callback has run and the thumb is laid out.
    fn overflowing_audio_settings(height: f32) -> Runtime<State> {
        use soul_player_host::{SignalPath, SourceFormat};
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Settings;
        s.settings_tab = SettingsTab::Audio;
        s.signal_path = Some(SignalPath {
            source: SourceFormat::Pcm { bits: 24 },
            codec: SourceCodec::Pcm,
            decoded_rate: 96_000,
            decoded_bits: 24,
            channels: 2,
            replay_gain_db: 0.0,
            device_rate: 96_000,
            resampling: false,
            output_bits: 24,
            exclusive: false,
            loudness: None,
        });
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, height), env);
        // Live-bound thumb rects need a waker to land; the engine's own scrollbar
        // test does the same before framing.
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt
    }

    /// When the Settings content overflows, a scrollbar thumb renders in the
    /// viewport's right gutter with real height.
    #[test]
    fn settings_scroll_shows_a_scrollbar_thumb_when_content_overflows() {
        let rt = overflowing_audio_settings(420.0);
        let (thumb, _) = rt
            .presented("settings-scrollbar")
            .expect("the scrollbar thumb renders over overflowing content");
        assert!(
            thumb[3] > 0.0,
            "the thumb has height when content overflows, got {thumb:?}"
        );
        let (vp, _) = rt
            .presented("settings-scroll")
            .expect("the scroll viewport renders");
        // The thumb sits in the right half of the viewport (its gutter), not over
        // the cards on the left.
        assert!(
            thumb[0] > vp[0] + vp[2] * 0.5,
            "thumb hugs the right gutter: thumb {thumb:?} viewport {vp:?}"
        );
    }

    /// Scrolling to the end leaves a margin below the last stage card — the
    /// content's bottom padding travels with it instead of sitting flush.
    #[test]
    fn settings_scroll_leaves_a_bottom_margin_below_the_last_card() {
        let mut rt = overflowing_audio_settings(420.0);
        rt.update(|_, tx| tx.scroll_to_instant("settings-scroll", 0.0, 100_000.0));
        rt.frame(0.0);
        let (vp, _) = rt
            .presented("settings-scroll")
            .expect("the scroll viewport renders");
        let (last, _) = rt
            .presented("sp-stage-exclusive")
            .expect("the last stage card is scrolled into view");
        let margin = (vp[1] + vp[3]) - (last[1] + last[3]);
        assert!(
            margin > 8.0,
            "the last card clears the viewport bottom (margin {margin}): viewport {vp:?} card {last:?}"
        );
    }

    /// Build Library in a short window so its ~38-track testdata list overflows
    /// the viewport and must scroll. Returns the ready runtime (live waker set,
    /// one frame drawn) so the scroll callback has run and the thumb is laid out.
    fn overflowing_library(height: f32) -> Runtime<State> {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        s.route = Route::Library;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, height), env);
        // Live-bound thumb rects need a waker to land (same as the settings test).
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt
    }

    /// When the track list overflows, a scrollbar thumb lays out in the right
    /// gutter with real height.
    #[test]
    fn library_scroll_shows_a_scrollbar_thumb_when_content_overflows() {
        let rt = overflowing_library(300.0);
        let (thumb, _) = rt
            .presented("library-scrollbar")
            .expect("the library scrollbar thumb lays out over an overflowing list");
        assert!(
            thumb[3] > 0.0,
            "the thumb has height when content overflows, got {thumb:?}"
        );
    }

    /// The title-bar band sits at the top, `HEIGHT` tall, over the content only —
    /// to the right of the sidebar, reaching the window's right edge (where the
    /// Windows caption buttons live).
    #[test]
    fn titlebar_band_renders_over_the_content() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = populated_state();
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (bar, _) = rt
            .presented("titlebar")
            .expect("the title-bar band renders");
        assert!(bar[1].abs() < 1.0, "band sits at the top, got y={}", bar[1]);
        assert!(
            (bar[3] - views::titlebar::HEIGHT).abs() < 1.0,
            "band is HEIGHT tall, got h={}",
            bar[3]
        );
        assert!(
            bar[0] > 1.0,
            "band starts right of the sidebar, got x={}",
            bar[0]
        );
        assert!(
            (bar[0] + bar[2] - 1100.0).abs() < 1.0,
            "band reaches the window's right edge, got right={}",
            bar[0] + bar[2]
        );
    }

    /// The band's open content area keeps clear of the Windows caption buttons at
    /// the trailing edge, as reported through `Cx::chrome_insets`.
    #[test]
    fn titlebar_content_clears_the_trailing_controls() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = populated_state();
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        let mut insets = rune::ChromeInsets::default();
        insets.trailing = 138.0; // Windows caption buttons
        insets.height = views::titlebar::HEIGHT;
        rt.set_chrome_insets(insets);
        rt.frame(0.0);
        let (content, _) = rt
            .presented("titlebar-content")
            .expect("the band content area renders");
        assert!(
            content[0] + content[2] <= 1100.0 - 138.0 + 0.5,
            "content clears the trailing (Windows) controls, right={}",
            content[0] + content[2]
        );
    }

    /// On Windows (trailing inset > 0) the band draws its own `_ [] X` glyphs over
    /// the native caption buttons, hard against the window's right edge.
    #[test]
    fn windows_caption_glyphs_sit_at_the_right_edge() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(populated_state(), view, (1100.0, 720.0), env);
        let mut insets = rune::ChromeInsets::default();
        insets.trailing = 138.0; // three Windows caption buttons
        insets.height = views::titlebar::HEIGHT;
        rt.set_chrome_insets(insets);
        rt.settle_all();
        rt.frame(0.0);

        let (close, _op) = rt
            .presented("caption-close")
            .expect("the close glyph renders over the native button");
        assert!(
            (close[0] + close[2] - 1100.0).abs() < 2.0,
            "close glyph sits at the right edge, got right={}",
            close[0] + close[2]
        );
    }

    /// The custom caption glyphs are always visible (native Windows 11 behavior):
    /// where the app draws its own controls (trailing inset > 0), all three render
    /// at full opacity with no hover needed. (Hover-to-reveal was removed — the
    /// native caption buttons are a non-client region the app's hover group can't
    /// track, so a reveal blanks the button exactly as you reach for it.)
    #[test]
    fn windows_caption_glyphs_are_always_visible() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(populated_state(), view, (1100.0, 720.0), env);
        let mut insets = rune::ChromeInsets::default();
        insets.trailing = 138.0;
        insets.height = views::titlebar::HEIGHT;
        rt.set_chrome_insets(insets);
        rt.settle_all();
        rt.frame(0.0);

        for key in ["caption-min", "caption-max", "caption-close"] {
            let (_el, op) = rt
                .presented(key)
                .unwrap_or_else(|| panic!("the {key} glyph renders"));
            assert!(op > 0.9, "{key} is fully visible at rest, opacity {op}");
        }
    }

    /// The custom caption glyphs render only where the app draws its own controls
    /// (trailing chrome inset > 0, i.e. Windows), not under a native frame.
    #[test]
    fn caption_glyphs_only_where_the_app_draws_controls() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        // Custom controls (trailing inset set): glyphs present.
        let mut rt = Runtime::new(populated_state(), view, (1100.0, 720.0), env);
        let mut insets = rune::ChromeInsets::default();
        insets.trailing = 138.0;
        insets.height = views::titlebar::HEIGHT;
        rt.set_chrome_insets(insets);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.presented("caption-close").is_some(),
            "glyphs show where the app draws its own controls"
        );

        // No trailing inset. On macOS/Windows that means the native frame owns
        // the controls, so the app draws none. On Linux it means the opposite:
        // nothing owns them (Rune's custom titlebar places controls on
        // macOS/Windows only), so the app must draw its own working buttons —
        // see `titlebar::DRAWS_OWN_CAPTION_BUTTONS`.
        let mut rt2 = Runtime::new(populated_state(), view, (1100.0, 720.0), env);
        rt2.frame(0.0);
        rt2.settle_all();
        rt2.frame(0.0);
        if cfg!(target_os = "linux") {
            assert!(
                rt2.presented("caption-close").is_some(),
                "Linux has no native controls, so the app draws its own"
            );
        } else {
            assert!(
                rt2.presented("caption-close").is_none(),
                "no custom glyphs under a native frame"
            );
        }
    }

    /// The left sidebar runs to the very top of the window (no strip above it),
    /// and `"app-root"` still spans the window width that mobile detection reads.
    #[test]
    fn sidebar_runs_to_the_top() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = populated_state();
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (root, _) = rt.presented("app-root").expect("app-root renders");
        let (sidebar, _) = rt.presented("sidebar").expect("the sidebar renders");
        assert!(
            (root[2] - 1100.0).abs() < 1.0,
            "app-root is window-width, got {}",
            root[2]
        );
        assert!(
            sidebar[1].abs() < 1.0,
            "sidebar runs to the top, got y={}",
            sidebar[1]
        );
        assert!(
            sidebar[0].abs() < 1.0,
            "sidebar starts at the left edge, got x={}",
            sidebar[0]
        );
    }

    /// The scrollbar is invisible while the list sits at rest and fades in only
    /// while the pointer is over the list container — a clean resting list.
    #[test]
    fn library_scrollbar_is_hidden_at_rest_and_revealed_on_hover() {
        let mut rt = overflowing_library(300.0);
        rt.settle_all();
        rt.frame(0.0);
        let (_r, rest_op) = rt
            .presented("library-scrollbar")
            .expect("the thumb element exists even at rest");
        assert!(
            rest_op < 0.05,
            "the scrollbar is hidden at rest, opacity {rest_op}"
        );

        // Hover anywhere inside the track list.
        let (x, y) = rt.find("tracks").expect("the track-list viewport");
        rt.pointer_move(x, y);
        rt.settle_all();
        rt.frame(0.0);
        let (_r2, hover_op) = rt
            .presented("library-scrollbar")
            .expect("the thumb element exists while hovering");
        assert!(
            hover_op > 0.9,
            "the scrollbar is revealed while hovering the list, opacity {hover_op}"
        );
    }

    /// The thumb rides the scroll: jumping to the bottom drops it down the gutter.
    #[test]
    fn library_scrollbar_thumb_follows_scroll_to_the_bottom() {
        let mut rt = overflowing_library(300.0);
        let (top, _) = rt
            .presented("library-scrollbar")
            .expect("the thumb lays out at the top");
        rt.update(|_, tx| tx.scroll_to_instant("tracks", 0.0, 100_000.0));
        rt.frame(0.0);
        let (bottom, _) = rt
            .presented("library-scrollbar")
            .expect("the thumb lays out after scrolling");
        assert!(
            bottom[1] > top[1] + 1.0,
            "the thumb moved down as the list scrolled: top {top:?} bottom {bottom:?}"
        );
    }

    /// Dragging the thumb itself down scrolls the list down — the thumb is a
    /// handle, not just an indicator — and it follows the drag down the gutter.
    #[test]
    fn dragging_the_library_scrollbar_thumb_scrolls_the_list() {
        let mut rt = overflowing_library(300.0);
        let (thumb0, _) = rt
            .presented("library-scrollbar")
            .expect("the thumb lays out at the top");
        // Reveal it (hover) and let it fade in, then press on its centre, drag
        // down, release.
        let cx = thumb0[0] + thumb0[2] / 2.0;
        let cy = thumb0[1] + thumb0[3] / 2.0;
        rt.pointer_move(cx, cy);
        rt.settle_all();
        rt.frame(0.0);
        rt.pointer_down(cx, cy);
        rt.pointer_move(cx, cy + 80.0);
        rt.frame(0.0);
        rt.pointer_up(cx, cy + 80.0);
        rt.frame(0.0);
        let (thumb1, _) = rt
            .presented("library-scrollbar")
            .expect("the thumb lays out after the drag");
        assert!(
            thumb1[1] > thumb0[1] + 20.0,
            "dragging the thumb down scrolled the list: thumb {} -> {}",
            thumb0[1],
            thumb1[1]
        );
    }

    /// Dragging the thumb up from a scrolled position scrolls back toward the top.
    #[test]
    fn dragging_the_library_scrollbar_thumb_up_scrolls_back() {
        let mut rt = overflowing_library(300.0);
        rt.update(|_, tx| tx.scroll_to_instant("tracks", 0.0, 100_000.0));
        rt.frame(0.0);
        let (thumb0, _) = rt
            .presented("library-scrollbar")
            .expect("the thumb lays out near the bottom");
        let cx = thumb0[0] + thumb0[2] / 2.0;
        let cy = thumb0[1] + thumb0[3] / 2.0;
        rt.pointer_move(cx, cy);
        rt.settle_all();
        rt.frame(0.0);
        rt.pointer_down(cx, cy);
        rt.pointer_move(cx, cy - 80.0);
        rt.frame(0.0);
        rt.pointer_up(cx, cy - 80.0);
        rt.frame(0.0);
        let (thumb1, _) = rt
            .presented("library-scrollbar")
            .expect("the thumb lays out after the drag");
        assert!(
            thumb1[1] < thumb0[1] - 20.0,
            "dragging the thumb up scrolled back: thumb {} -> {}",
            thumb0[1],
            thumb1[1]
        );
    }

    /// While a thumb drag is held, the scrollbar stays visible even if the pointer
    /// leaves the list (dragging off to the side must not fade it out mid-drag).
    #[test]
    fn library_scrollbar_stays_visible_while_dragging_off_the_list() {
        let mut rt = overflowing_library(300.0);
        let (thumb0, _) = rt
            .presented("library-scrollbar")
            .expect("the thumb lays out");
        let cx = thumb0[0] + thumb0[2] / 2.0;
        let cy = thumb0[1] + thumb0[3] / 2.0;
        rt.pointer_move(cx, cy); // hover reveals it
        rt.pointer_down(cx, cy); // grab the thumb
        // Keep holding, but drag the pointer off the list into the sidebar.
        rt.pointer_move(50.0, cy);
        rt.settle_all();
        rt.frame(0.0);
        let (_t, op) = rt
            .presented("library-scrollbar")
            .expect("the thumb while dragging off-list");
        rt.pointer_up(50.0, cy);
        assert!(
            op > 0.9,
            "the scrollbar stays visible while the drag continues off the list, opacity {op}"
        );
    }

    /// Build the sidebar queue with a long context so its list overflows the
    /// panel and must scroll. Returns the ready runtime (waker set, one frame).
    fn overflowing_queue(height: f32) -> Runtime<State> {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        scene_queue(&mut s);
        // Many context rows so the queue list overflows a short sidebar for sure.
        s.context_ids = s.library.iter().map(|t| t.id).collect();
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, height), env);
        rt.set_live_waker(std::sync::Arc::new(|| {}));
        rt.frame(0.0);
        rt
    }

    /// The queue thumb rides the scroll and stays within the viewport frame — the
    /// regression this whole change fixes (it used to be dragged off the top as a
    /// child of the scrolled content).
    #[test]
    fn queue_scrollbar_thumb_stays_in_the_viewport_while_scrolling() {
        let mut rt = overflowing_queue(1200.0);
        let (vp, _) = rt
            .presented("queue-content")
            .expect("the queue viewport renders");
        let (top, _) = rt
            .presented("queue-scrollbar")
            .expect("the queue thumb lays out");
        assert!(
            top[3] > 0.0,
            "precondition: the queue overflows, got {top:?}"
        );
        rt.update(|_, tx| tx.scroll_to_instant("queue-content", 0.0, 100_000.0));
        rt.frame(0.0);
        let (bottom, _) = rt
            .presented("queue-scrollbar")
            .expect("the queue thumb after scroll");
        assert!(
            bottom[1] > top[1] + 1.0,
            "the thumb moved down with the scroll: top {top:?} bottom {bottom:?}"
        );
        assert!(
            bottom[1] >= vp[1] - 1.0 && bottom[1] + bottom[3] <= vp[1] + vp[3] + 1.0,
            "the thumb stays inside the viewport frame: thumb {bottom:?} viewport {vp:?}"
        );
    }

    /// The queue scrollbar is hidden at rest and revealed only while the pointer
    /// is over the queue list.
    #[test]
    fn queue_scrollbar_is_hidden_at_rest_and_revealed_on_hover() {
        let mut rt = overflowing_queue(500.0);
        rt.settle_all();
        rt.frame(0.0);
        let (_r, rest_op) = rt
            .presented("queue-scrollbar")
            .expect("the thumb exists at rest");
        assert!(rest_op < 0.05, "hidden at rest, opacity {rest_op}");

        let (x, y) = rt.find("queue-content").expect("the queue viewport");
        rt.pointer_move(x, y);
        rt.settle_all();
        rt.frame(0.0);
        let (_r2, hover_op) = rt
            .presented("queue-scrollbar")
            .expect("the thumb exists while hovering");
        assert!(
            hover_op > 0.9,
            "revealed while hovering the queue, opacity {hover_op}"
        );
    }

    /// Pressing the signal-path line opens the stage-by-stage panel: every stage
    /// row renders, every pipeline stage is present (Exclusive output and DSP/EQ
    /// now live, not "soon"), and the ReplayGain toggle flips the mode.
    #[test]
    fn signal_path_panel_opens_with_stages_and_rg_toggle() {
        use soul_player_host::{SignalPath, SourceFormat};
        // The RG toggle persists config — isolate it from the real settings dir.
        let _config_guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = populated_state();
        scene_queue(&mut s);
        s.signal_path = Some(SignalPath {
            source: SourceFormat::Dsd { rate_hz: 2_822_400 },
            codec: SourceCodec::Dsd,
            decoded_rate: 44_100,
            decoded_bits: 24,
            channels: 2,
            replay_gain_db: 0.0,
            device_rate: 48_000,
            resampling: true,
            output_bits: 24,
            exclusive: false,
            loudness: Some(TrackLoudness {
                integrated_lufs: -14.2,
                lra: 9.5,
                true_peak_dbtp: -0.8,
                plr: 13.4,
                dr: 12.0,
            }),
        });
        s.replaygain_mode = ReplayGainMode::Off;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("signal-path-panel").is_none(),
            "the panel is closed initially"
        );

        // Press the line → the panel opens (settle its enter transition).
        click_key(&mut rt, "signal-path");
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.state.signal_path_open,
            "pressing the line opens the panel"
        );
        assert!(rt.find("signal-path-panel").is_some(), "the panel renders");
        for k in [
            "sp-row-source",
            "sp-row-decode",
            "sp-row-replaygain",
            "sp-row-resample",
            "sp-row-output",
            "sp-row-exclusive", // live (bit-perfect) or dim "shared"
            "sp-row-dsp",       // live: accent "Active" or dim "Bypassed"
        ] {
            assert!(rt.find(k).is_some(), "stage row {k} renders");
        }

        // The per-track loudness & dynamic-range readouts render (#110): integrated
        // LUFS, LRA, true-peak, PLR, and the DR value.
        for k in [
            "signal-path-integrated",
            "signal-path-lra",
            "signal-path-truepeak",
            "signal-path-plr",
            "signal-path-dr",
        ] {
            assert!(rt.find(k).is_some(), "loudness row {k} renders");
        }

        // The ReplayGain toggle is the one real pipeline toggle: Off → Track.
        click_key(&mut rt, "sp-rg-toggle");
        rt.frame(0.0);
        assert_eq!(
            rt.state.replaygain_mode,
            ReplayGainMode::Track,
            "the ReplayGain toggle turns it on"
        );
    }

    /// The signal-path panel adds the live loudness meter rows (#110): momentary +
    /// short-term LUFS and the live peak, read from the host's `live_loudness()` tap.
    /// They render only while a track plays, and are absent when stopped.
    #[test]
    fn signal_path_panel_shows_live_meter_rows_while_playing() {
        use soul_player_host::{SignalPath, SourceFormat};
        let _config_guard = config::use_test_config_dir();
        // Whether the live meter rows render in the open panel at a given `playing`.
        let live_rows_present = |playing: bool| {
            let env = Env {
                system_dark: true,
                scale: 2.0,
            };
            let mut s = populated_state();
            scene_queue(&mut s);
            s.playing = playing;
            // loudness `None` isolates the LIVE rows from the static whole-track readouts.
            s.signal_path = Some(SignalPath {
                source: SourceFormat::Pcm { bits: 24 },
                codec: SourceCodec::Flac,
                decoded_rate: 44_100,
                decoded_bits: 24,
                channels: 2,
                replay_gain_db: 0.0,
                device_rate: 48_000,
                resampling: true,
                output_bits: 24,
                exclusive: false,
                loudness: None,
            });
            let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
            rt.frame(0.0);
            click_key(&mut rt, "signal-path");
            rt.settle_all();
            rt.frame(0.0);
            assert!(rt.find("signal-path-panel").is_some(), "the panel renders");
            [
                "signal-path-live-heading",
                "signal-path-momentary",
                "signal-path-shortterm",
                "signal-path-livepeak",
            ]
            .iter()
            .all(|k| rt.find(k).is_some())
        };
        assert!(
            live_rows_present(true),
            "the live meter rows render while a track plays"
        );
        assert!(
            !live_rows_present(false),
            "the live meter rows are absent when not playing"
        );
    }

    /// The language picker is the themed `views::dropdown`: pressing the trigger
    /// opens a floating panel (mounted at the root, escaping the settings scroll
    /// clip), the choices render, and picking one commits the locale + closes.
    #[test]
    fn language_dropdown_opens_renders_and_picks() {
        // Picking commits a non-"en" locale (German) globally for part of this
        // test, so hold the locale lock too (acquired BEFORE the config lock, the
        // documented order) — otherwise it races a sibling test asserting a
        // localised string. Picking also writes config, hence the config lock.
        let _locale = config::use_test_locale("en");
        let _config_guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::Appearance;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Closed: the trigger renders, no panel.
        assert!(
            rt.find("lang-picker").is_some(),
            "the language dropdown trigger renders"
        );
        assert!(
            rt.find("lang-picker-menu").is_none(),
            "the panel is closed initially"
        );
        // Open it, then settle the panel's enter transition (it fades/scales in
        // from opacity 0, so it isn't hit-testable until the animation finishes).
        click_key(&mut rt, "lang-picker");
        rt.settle_all();
        rt.frame(0.0);
        assert_eq!(rt.state.open_dropdown.as_deref(), Some("lang-picker"));
        assert!(
            rt.find("lang-picker-menu").is_some(),
            "pressing the trigger opens the panel"
        );
        assert!(
            rt.find("lang-picker-opt-de").is_some(),
            "the panel lists the language choices"
        );
        // Pick German: commits the locale and closes.
        click_key(&mut rt, "lang-picker-opt-de");
        assert_eq!(rt.state.locale_pref, "de", "picking commits the locale");
        assert_eq!(rt.state.open_dropdown, None, "picking closes the dropdown");

        // Reset the PERSISTED locale and the shared rust_i18n global — sibling tests'
        // `populated_state()` loads the persisted locale, so "de" must not leak.
        config::save_locale("en");
        apply_locale("en");
    }

    /// A dropdown is light-dismiss, NOT a scroll-locking modal: an outside scroll or
    /// Escape closes it (so a trigger deep in a scroll never locks the page).
    #[test]
    fn a_dropdown_light_dismisses_on_scroll_and_escape() {
        let _config_guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let open = |rt: &mut Runtime<State>| {
            click_key(rt, "lang-picker");
            rt.settle_all();
            rt.frame(0.0);
            assert_eq!(rt.state.open_dropdown.as_deref(), Some("lang-picker"));
            assert!(rt.find("lang-picker-menu").is_some(), "the panel opened");
        };
        // Scrolling anywhere closes it (the backdrop's on_wheel).
        {
            let mut s = settings_state(&[], &[]);
            s.settings_tab = SettingsTab::Appearance;
            apply_locale("en");
            let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
            rt.frame(0.0);
            open(&mut rt);
            rt.wheel_event(WheelEvent::new(0.0, -20.0).at(80.0, 400.0));
            rt.frame(0.0);
            assert_eq!(
                rt.state.open_dropdown, None,
                "a scroll outside the panel closes the dropdown"
            );
        }
        // Escape closes it (the app key handler — wired as the real app does).
        {
            let mut s = settings_state(&[], &[]);
            s.settings_tab = SettingsTab::Appearance;
            apply_locale("en");
            let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
            rt.on_key = Some(handle_key);
            rt.frame(0.0);
            open(&mut rt);
            rt.key(KeyEvent {
                key: Key::Escape,
                mods: Mods::default(),
            });
            rt.frame(0.0);
            assert_eq!(rt.state.open_dropdown, None, "Escape closes the dropdown");
        }
    }

    /// The add-source control is a themed `views::dropdown` menu: the trigger
    /// renders on the Music Data tab and, when opened (the same press the language
    /// picker uses), offers two real, focusable choices — "Local folder…" and
    /// "Remote server…" (which opens the connect form — see
    /// `remote_add_source_opens_the_connect_form`).
    #[test]
    fn add_source_menu_offers_local_and_remote_options() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Closed: the trigger renders, no panel.
        assert!(
            rt.find("add-source").is_some(),
            "the add-source menu trigger renders"
        );
        assert!(
            rt.find("add-source-menu").is_none(),
            "the menu is closed initially"
        );
        // Open it (same mechanism the language dropdown test uses), then settle the
        // panel's enter transition.
        click_key(&mut rt, "add-source");
        rt.settle_all();
        rt.frame(0.0);
        assert_eq!(rt.state.open_dropdown.as_deref(), Some("add-source"));
        // Local folder: a real, focusable choice.
        let local = el_info(&rt, "add-source-opt-local");
        assert!(local.focusable, "the Local folder item is focusable");
        // Remote server: also a real, focusable choice (it opens the connect form).
        let remote = el_info(&rt, "add-source-opt-remote");
        assert!(remote.focusable, "the Remote server item is focusable");
    }

    /// Activating "Remote server" in the add-source menu closes the menu and
    /// opens the connect form: a labelled, focusable `remote-url` field and a
    /// `remote-connect` button, each reachable by key with a non-empty
    /// accessible label and an interactive role. There is no username/password
    /// field — the password is entered in the system browser (OAuth), not the
    /// app — so `remote-user`/`remote-pass` must not render at all. The actual
    /// network connect (`PlayerHandle::add_remote_source`) is covered by the
    /// host's `remote_e2e` integration test — this only proves the UI is
    /// reachable, so it never touches the network.
    #[test]
    fn remote_add_source_opens_the_connect_form() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "add-source");
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "add-source-opt-remote");
        rt.settle_all();
        rt.frame(0.0);
        assert!(rt.state.remote_form_open, "the connect form is open");
        assert!(
            rt.state.open_dropdown.is_none(),
            "opening the connect form closes the add-source menu"
        );
        for (key, role) in [("remote-url", "textbox"), ("remote-connect", "button")] {
            let info = el_info(&rt, key);
            assert!(info.focusable, "{key} is focusable");
            assert!(
                info.label.as_deref().is_some_and(|l| !l.is_empty()),
                "{key} carries a non-empty label"
            );
            assert_eq!(info.role, role, "{key} has an interactive role");
        }
        // The username/password fields are gone: this is a browser-login form
        // now, not a password form.
        assert!(
            rt.find("remote-user").is_none(),
            "remote-user no longer renders (login happens in the browser)"
        );
        assert!(
            rt.find("remote-pass").is_none(),
            "remote-pass no longer renders (login happens in the browser)"
        );
    }

    /// Investigation, pinned as a regression: the connect form's URL
    /// field (`remote-url`, a plain `rune::TextEdit`/`text_input` — see
    /// `settings::remote_field`) is a standard Rune text field, which
    /// natively handles typing, Backspace, and the platform's primary edit
    /// chords (`TextEdit::key`'s `Key::Command(_)` arm) — Ctrl+A (⌘A on
    /// macOS) selects the whole field (`anchor = 0`, `focus = text.len()`),
    /// entirely inside the crate; the app wires none of it itself. This
    /// drives the REAL open-form flow (menu click → "Remote server") and the
    /// REAL key dispatch (`rt.key`, no app-level `on_key` needed — a focused
    /// field claims its own chords first) to prove end to end that the form,
    /// as actually wired (inside `.modal()` + `.focus_group()`), doesn't
    /// block any of this: typing inserts, Backspace edits, and Ctrl+A
    /// selects all.
    #[test]
    fn remote_connect_form_field_types_and_ctrl_a_selects_all() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "add-source");
        rt.settle_all();
        rt.frame(0.0);
        click_key(&mut rt, "add-source-opt-remote");
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            rt.state.remote_url_buf.focused,
            "opening the form focuses the URL field (TextEdit::focused)"
        );

        let press = |rt: &mut Runtime<State>, key: Key| {
            rt.key(KeyEvent {
                key,
                mods: Mods::default(),
            });
            rt.frame(0.0);
        };
        for ch in "http://x.test".chars() {
            press(&mut rt, Key::Text(ch.to_string()));
        }
        assert_eq!(
            rt.state.remote_url_buf.text, "http://x.test",
            "typed characters reach the focused field"
        );
        press(&mut rt, Key::Backspace);
        assert_eq!(
            rt.state.remote_url_buf.text, "http://x.tes",
            "Backspace edits it like any other field"
        );

        rt.key(cmd_chord('a'));
        rt.frame(0.0);
        assert_eq!(
            rt.state.remote_url_buf.anchor, 0,
            "Ctrl+A/Cmd+A selects from the start"
        );
        assert_eq!(
            rt.state.remote_url_buf.focus,
            rt.state.remote_url_buf.text.len(),
            "…to the end — the whole field is selected"
        );

        // And it actually replaces the selection, proving this is a real
        // selection the field will act on (not just two numbers that happen
        // to bracket the text): typing now overwrites everything selected.
        press(&mut rt, Key::Text("y".into()));
        assert_eq!(
            rt.state.remote_url_buf.text, "y",
            "typing over a Ctrl+A selection replaces the whole field"
        );
    }

    /// First open of the Remote connect form must render as one fully formed
    /// card, not with its content trailing far behind the card's own fade-in.
    /// Before the fix, the card (`remote-form`) and everything it holds each
    /// played their OWN entrance fade on mount — nested independently, so the
    /// opacities multiplied with depth. A few real frames into the real
    /// `+ Add Source → Remote` click flow, the card's own border/fill was
    /// already ~28% visible while the Connect button — nested two levels
    /// deeper inside it (card → button row → button) — was still under 8%,
    /// i.e. effectively invisible: the card read as rendering "cut", missing
    /// its content, on the very first open (then settled fine moments later,
    /// matching the user report). `instant_changes` on the scrim lands the
    /// whole held subtree at once, so the card and its fields/buttons now
    /// share a single fade instead of compounding. This checks a frame BEFORE
    /// the fade settles (no `settle_all`) — the only window where the old
    /// desync, and thus the clip, was visible — and FAILS without the fix.
    #[test]
    fn remote_form_first_open_renders_fully_formed_before_it_settles() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "add-source");
        rt.settle_all();
        rt.frame(0.0);
        // The real first-open click, then a few real-time frames into the
        // mount fade — deliberately NOT settled, so this is the frame the
        // user actually saw.
        click_key(&mut rt, "add-source-opt-remote");
        for _ in 0..4 {
            rt.frame(1.0 / 60.0);
        }

        let card = el_info(&rt, "remote-form");
        let url = el_info(&rt, "remote-url");
        let connect = el_info(&rt, "remote-connect");
        assert!(
            card.opacity > 0.05,
            "the card should already be visibly fading in by now (opacity={:.3})",
            card.opacity
        );
        assert!(
            (connect.opacity - card.opacity).abs() < 0.05,
            "the Connect button should fade in together with the card, not trail \
             far behind it (card={:.3} connect={:.3})",
            card.opacity,
            connect.opacity
        );
        assert!(
            (url.opacity - card.opacity).abs() < 0.05,
            "the url field should fade in together with the card (card={:.3} url={:.3})",
            card.opacity,
            url.opacity
        );

        // Geometric half of "first open clips": present, real-sized, and not
        // clipped by an ancestor's visible rect (the same signature
        // `horizontal_overflows` checks for elsewhere in this module).
        for key in ["remote-url", "remote-connect"] {
            let e = el_info(&rt, key);
            let [x, y, w, h] = e.rect;
            assert!(w > 0.0 && h > 0.0, "{key} has a real size");
            let [vx, vy, vw, vh] = e.visible;
            assert!(
                (vx - x).abs() < 1.0 && (vw - w).abs() < 1.0,
                "{key} clipped horizontally: rect={:?} visible={:?}",
                e.rect,
                e.visible
            );
            assert!(
                (vy - y).abs() < 1.0 && (vh - h).abs() < 1.0,
                "{key} clipped vertically: rect={:?} visible={:?}",
                e.rect,
                e.visible
            );
        }
    }

    /// While a connect attempt is in flight (`State::remote_connecting`), the
    /// Connect button goes inert (nothing to press until the browser
    /// round-trip returns) and a "waiting for your browser" status line
    /// renders in the form, in place of where an error would show.
    #[test]
    fn remote_connect_form_shows_a_waiting_state_while_connecting() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.remote_form_open = true;
        s.remote_connecting = true;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let connect = el_info(&rt, "remote-connect");
        assert!(
            !connect.focusable,
            "the Connect button is inert while a connect attempt is in flight"
        );
        assert!(
            rt.find("remote-connecting-status").is_some(),
            "a waiting-for-browser status line renders while connecting"
        );
    }

    /// Drain `config`'s persisted remote urls to empty: `save_remote` appends
    /// and never overwrites, so — unlike most other settings, which fully
    /// replace their field on save — a sibling test sharing this process-wide
    /// config file could otherwise leave a stray url behind that breaks an
    /// exact-equality (or "nothing persisted") assertion below.
    fn drain_remotes() {
        for url in config::load_remotes() {
            config::remove_remote(&url);
        }
    }

    /// Inc 5 persistence: a successful connect (`apply_remote_connect`'s `Ok`
    /// branch) persists the server url via `config::save_remote`, so it
    /// reconnects on the next launch, and closes the form.
    #[test]
    fn apply_remote_connect_success_persists_the_url() {
        let _config_guard = config::use_test_config_dir();
        drain_remotes();
        let mut s = State::new(PlayerHandle::offline());
        s.remote_form_open = true;

        apply_remote_connect(&mut s, "http://example.test", Ok(()));

        assert_eq!(
            config::load_remotes(),
            vec!["http://example.test".to_string()],
            "a successful connect should persist the server url"
        );
        assert!(!s.remote_form_open, "a successful connect closes the form");
    }

    /// The failure counterpart: nothing is persisted, and the form stays open
    /// with the error message so the user can fix a typo and retry.
    #[test]
    fn apply_remote_connect_failure_does_not_persist_the_url() {
        let _config_guard = config::use_test_config_dir();
        drain_remotes();
        let mut s = State::new(PlayerHandle::offline());
        s.remote_form_open = true;

        apply_remote_connect(
            &mut s,
            "http://example.test",
            Err(anyhow::anyhow!("connection refused")),
        );

        assert!(
            config::load_remotes().is_empty(),
            "a failed connect must not persist anything"
        );
        assert!(s.remote_form_open, "the form stays open on failure");
        assert_eq!(s.remote_error, Some("connection refused".to_string()));
    }

    /// Inc 5 startup reconnect: with no persisted remotes, `reconnect_remotes_on_startup`
    /// is a no-op (early return) rather than touching the keychain or the network.
    #[test]
    fn reconnect_remotes_on_startup_with_nothing_persisted_is_a_noop() {
        let _config_guard = config::use_test_config_dir();
        drain_remotes();
        let mut s = State::new(PlayerHandle::offline());
        let sources_before = s.sources.clone();

        reconnect_remotes_on_startup(&mut s);

        assert_eq!(
            s.sources, sources_before,
            "nothing persisted means nothing to reconnect, so state is untouched"
        );
    }

    /// A persisted remote with no stored keychain credential (the common case
    /// in a fresh test config dir) fails to reconnect — gracefully: no panic,
    /// and the headless (no `app_handle`) path still refreshes `sources`/
    /// `library` from the host's (unchanged) state afterward, same as a real
    /// offline-remote launch would.
    #[test]
    fn reconnect_remotes_on_startup_with_an_unreachable_remote_does_not_crash() {
        let _config_guard = config::use_test_config_dir();
        drain_remotes();
        config::save_remote("http://unreachable-remote.invalid");
        let mut s = State::new(PlayerHandle::offline());

        reconnect_remotes_on_startup(&mut s);

        assert!(
            s.sources
                .iter()
                .all(|src| !matches!(&src.kind, SourceKind::Remote { url } if url == "http://unreachable-remote.invalid")),
            "a remote that never reconnected (no stored credential) is not added to Inner::remotes"
        );
    }

    /// The Managed Library card renders on the Music Data tab with its destination
    /// Browse button, template picker, example line, and Copy/Move strategy buttons.
    #[test]
    fn managed_library_card_renders() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        for k in [
            "managed-dest-browse",
            "managed-template",
            "managed-copy",
            "managed-move",
            "managed-example",
        ] {
            assert!(rt.find(k).is_some(), "the Managed Library card shows {k}");
        }
    }

    /// Picking a template preset from the dropdown commits its id to state and
    /// persists it to config.
    #[test]
    fn selecting_a_template_preset_persists() {
        // Picking flows through `set_managed_template`, which writes config —
        // isolate it from the user's real settings, and serialise with every other
        // config writer (shared per-process test config dir).
        let _config_guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Open the template dropdown, then settle its enter transition (it fades in
        // from opacity 0, so the option isn't hit-testable until it finishes).
        click_key(&mut rt, "managed-template");
        rt.settle_all();
        rt.frame(0.0);
        assert_eq!(rt.state.open_dropdown.as_deref(), Some("managed-template"));
        // Pick the genre/artist/album preset.
        click_key(&mut rt, "managed-template-opt-genre-artist-album");
        assert_eq!(
            rt.state.managed_template, "genre-artist-album",
            "picking commits the preset id to state"
        );
        assert_eq!(rt.state.open_dropdown, None, "picking closes the dropdown");
        assert_eq!(
            config::load_managed_library().1,
            "genre-artist-album",
            "and persists the preset id to config"
        );
        // Reset the PERSISTED locale and the shared rust_i18n global — sibling tests'
        // `populated_state()` loads the persisted locale, so "de" must not leak.
        config::save_locale("en");
        apply_locale("en");
    }

    /// Toggling the strategy to Move commits it to state and persists `"move"`.
    #[test]
    fn toggling_strategy_persists() {
        // The toggle writes config — isolate it from the user's real settings, and
        // serialise with every other config writer (shared per-process test
        // config dir).
        let _config_guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "managed-move");
        assert_eq!(
            rt.state.managed_strategy,
            Strategy::Move,
            "clicking Move selects the move strategy"
        );
        assert_eq!(
            config::load_managed_library().2,
            "move",
            "and persists the strategy to config"
        );
    }

    // ── Managed-library organize flow (Task 1: phases, modal, menu entry) ─────

    /// Open the add-source menu on the Music Data tab and settle its enter
    /// transition, so its rows (including "Add music…") are mounted + hit-testable.
    fn open_add_source(dest: Option<PathBuf>) -> Runtime<State> {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.managed_dest = dest;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "add-source");
        rt.settle_all();
        rt.frame(0.0);
        rt
    }

    /// A tiny plan literal (no filesystem) for exercising the modal phases.
    fn sample_plan() -> organize::OrganizePlan {
        organize::OrganizePlan {
            dest_root: testdata_dir(),
            entries: vec![
                organize::PlanEntry {
                    src: PathBuf::from("a.flac"),
                    dst: testdata_dir().join("A/a.flac"),
                    action: organize::Action::Copy,
                },
                organize::PlanEntry {
                    src: PathBuf::from("b.flac"),
                    dst: testdata_dir().join("B/b.flac"),
                    action: organize::Action::SkipDuplicate,
                },
            ],
        }
    }

    /// Build a frame with the organize modal in a given phase, settled so its
    /// controls are present + hit-testable.
    fn organize_frame(phase: OrganizePhase) -> Runtime<State> {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.organize = Some(phase);
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        rt
    }

    /// Without a managed destination, the "Add music…" entry is present but a
    /// disabled hint — not focusable, so the organize flow can't start.
    #[test]
    fn add_music_item_hint_when_no_destination() {
        let rt = open_add_source(None);
        assert!(
            rt.find("add-source-opt-organize").is_some(),
            "the Add music… entry renders in the menu"
        );
        assert!(
            !el_info(&rt, "add-source-opt-organize").focusable,
            "without a destination it is a disabled hint (not focusable)"
        );
        apply_locale("en");
    }

    /// With a managed destination set, the "Add music…" entry becomes a focusable
    /// option that can start the organize flow.
    #[test]
    fn add_music_item_enabled_with_destination() {
        let rt = open_add_source(Some(testdata_dir()));
        assert!(
            el_info(&rt, "add-source-opt-organize").focusable,
            "a configured destination makes Add music… focusable"
        );
        apply_locale("en");
    }

    /// The organize modal renders the right keyed controls for each phase.
    #[test]
    fn organize_overlay_renders_each_phase() {
        // Confirm: title/summary + the Confirm and Cancel buttons.
        let rt = organize_frame(OrganizePhase::Confirm(sample_plan()));
        for k in ["organize-confirm", "organize-confirm-go", "organize-cancel"] {
            assert!(rt.find(k).is_some(), "Confirm phase renders {k}");
        }

        // Applying: the progress line.
        let rt = organize_frame(OrganizePhase::Applying {
            done: 3,
            total: 10,
            current: "a.flac".to_string(),
        });
        assert!(
            rt.find("organize-progress").is_some(),
            "Applying phase renders the progress line"
        );

        // Done: the summary + Close button.
        let rt = organize_frame(OrganizePhase::Done(organize::Summary {
            copied: 2,
            skipped: 1,
            ..Default::default()
        }));
        for k in ["organize-done", "organize-close"] {
            assert!(rt.find(k).is_some(), "Done phase renders {k}");
        }
        apply_locale("en");
    }

    /// Confirming the plan advances the flow from Confirm to Applying (pressing
    /// the Confirm button drives `organize_confirm`).
    #[test]
    fn confirm_advances_to_applying() {
        let mut rt = organize_frame(OrganizePhase::Confirm(sample_plan()));
        click_key(&mut rt, "organize-confirm-go");
        assert!(
            matches!(rt.state.organize, Some(OrganizePhase::Applying { .. })),
            "Confirm → Applying on confirm"
        );
        apply_locale("en");
    }

    /// Closing a finished run dismisses the modal (clears `State::organize`).
    #[test]
    fn dismiss_clears() {
        let mut rt = organize_frame(OrganizePhase::Done(organize::Summary::default()));
        click_key(&mut rt, "organize-close");
        assert!(
            rt.state.organize.is_none(),
            "Close dismisses the organize modal"
        );
        apply_locale("en");
    }

    /// Architecture: `views::dropdown` is the ONE way to build a dropdown. No view
    /// may fall back to rune's native picker (`PopupMenu::picker`), which draws an
    /// OS-styled menu that clashes with the app's look. Native `popup_menu`
    /// CONTEXT menus (library/queue right-click) are a different control and stay
    /// allowed — this bans only the picker/select constructor. New pickers go
    /// through `dropdown::trigger` + `dropdown::panel`.
    #[test]
    fn dropdowns_only_use_the_shared_component() {
        let views_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/views");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&views_dir).expect("read views dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let src = std::fs::read_to_string(&path).expect("read view source");
            for (i, line) in src.lines().enumerate() {
                // Only code, not doc/comments that may name the banned API.
                let code = line.split("//").next().unwrap_or("");
                if code.contains("PopupMenu::picker") || code.contains(".picker(") {
                    offenders.push(format!("{name}:{} {}", i + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "dropdowns must use views::dropdown, not rune's native PopupMenu::picker:\n{}",
            offenders.join("\n")
        );
    }

    /// A slider shows the horizontal-drag cursor only while it is actually being
    /// dragged — hovering keeps the plain cursor, so the change reads as "you are
    /// dragging" rather than a persistent hint.
    #[test]
    fn slider_shows_a_drag_cursor_only_while_dragging() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.volume = 0.5;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("volume").expect("the volume slider renders");
        // Hover: plain cursor.
        rt.pointer_move(x, y);
        rt.frame(0.0);
        assert_eq!(
            rt.cursor(),
            Cursor::Default,
            "hovering the slider keeps the default cursor"
        );
        // Press + move = a drag: the horizontal-resize cursor appears.
        rt.pointer_down(x, y);
        rt.pointer_move(x + 12.0, y);
        rt.frame(0.0);
        assert_eq!(rt.state.dragging_slider.as_deref(), Some("volume"));
        assert_eq!(
            rt.cursor(),
            Cursor::ResizeColumn,
            "dragging the slider shows the horizontal-drag cursor"
        );
        // Release clears the drag state.
        rt.pointer_up(x + 12.0, y);
        rt.frame(0.0);
        assert_eq!(rt.state.dragging_slider, None);
    }

    /// Dragging the seek bar scrubs a live preview (knob + elapsed time follow the
    /// pointer) without seeking on every move, then commits one Seek on release.
    #[test]
    fn dragging_the_seek_bar_scrubs_then_commits_on_release() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.duration_secs = 100.0;
        s.position_secs = 10.0;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("seek").expect("the seek slider renders");
        // Drag toward the right: a scrub preview appears and moves forward, with
        // no seek committed mid-drag.
        rt.pointer_down(x, y);
        rt.pointer_move(x + 40.0, y);
        rt.frame(0.0);
        let scrub = rt
            .state
            .scrub_secs
            .expect("dragging previews a scrub position");
        assert!(scrub > 10.0, "scrubbed forward from 10s, got {scrub}");
        // Release commits the seek (sent to the host) and HOLDS that target as the
        // preview — it is not cleared here, so the knob can't flick back to the
        // stale position; `State::apply`'s `settle_scrub` clears it once playback
        // reaches the target (covered by `settle_scrub_holds_the_target...`).
        rt.pointer_up(x + 40.0, y);
        rt.frame(0.0);
        let held = rt
            .state
            .scrub_secs
            .expect("the committed target stays previewed until playback arrives");
        assert!(
            (held - scrub).abs() < 5.0,
            "held target ≈ the dropped position ({held} vs {scrub})"
        );
        assert_eq!(rt.state.dragging_slider, None, "the drag has ended");
    }

    /// Scrolling the wheel over the volume row nudges the volume — up (dy < 0)
    /// louder, down quieter — the end-to-end path through `on_wheel` →
    /// `wheel_volume_delta` → `set_volume`. Starts mid-range so both directions
    /// move (the default 1.0 is already at the ceiling).
    #[test]
    fn scrolling_over_the_volume_row_changes_the_volume() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false; // the player panel (volume row) must render
        s.volume = 0.5;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("volume-row").expect("the volume row renders");
        // Scroll up → one 5% step louder.
        rt.wheel_event(WheelEvent::new(0.0, -10.0).at(x, y));
        rt.frame(0.0);
        assert!(
            (rt.state.volume - 0.55).abs() < 1e-6,
            "scroll up raised the volume, got {}",
            rt.state.volume
        );
        // Scroll down → back to 0.5.
        rt.wheel_event(WheelEvent::new(0.0, 10.0).at(x, y));
        rt.frame(0.0);
        assert!(
            (rt.state.volume - 0.50).abs() < 1e-6,
            "scroll down lowered the volume, got {}",
            rt.state.volume
        );
    }

    // (The volume icon is a mute toggle driven by the command-based backend mute,
    // #105 — see `the_speaker_icon_sends_toggle_mute`. The old UI-side toggle test
    // was dropped in the settings-refresh reconciliation.)

    /// After a seek is committed (the user let go), the knob keeps previewing the
    /// target until playback actually reaches it, so it doesn't flick to the stale
    /// position and back. While still dragging, the live preview is always kept.
    #[test]
    fn settle_scrub_holds_the_target_until_playback_arrives() {
        // No scrub pending: nothing to hold.
        assert_eq!(settle_scrub(None, false, 10.0), None);
        // Released, seek in flight (playback far from target): keep previewing it.
        assert_eq!(settle_scrub(Some(50.0), false, 10.0), Some(50.0));
        // Playback has reached the target: stop previewing.
        assert_eq!(settle_scrub(Some(50.0), false, 50.2), None);
        // Still dragging: always keep the live preview, even at the target.
        assert_eq!(settle_scrub(Some(50.0), true, 50.0), Some(50.0));
    }

    #[test]
    fn settings_route_shows_sources_section_and_add_button() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("sources").is_some(),
            "Settings shows a Sources section"
        );
        assert!(rt.find("add-source").is_some(), "Sources offers Add Source");
        assert!(rt.find("rescan-sources").is_some(), "Sources offers Rescan");
    }

    #[test]
    fn sidebar_resize_handle_present_and_collapses_to_a_rail() {
        // Expanded: the full sidebar and its always-present resize handle render,
        // and there is no collapsed rail.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false; // explicit — don't depend on persisted config
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(rt.find("sidebar").is_some(), "the full sidebar renders");
        assert!(
            rt.find("sidebar-resize").is_some(),
            "the resize handle is present"
        );
        assert!(
            rt.find("grabber").is_none(),
            "no collapsed-rail grabber while expanded"
        );

        // Collapsed: the handle itself becomes the thin rail (same key, so a drag
        // survives the swap); the full sidebar is gone and the grabber appears.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = true;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("sidebar-resize").is_some(),
            "the handle (now the rail) is present",
        );
        assert!(
            rt.find("grabber").is_some(),
            "collapsing shows the thin rail with its grabber"
        );
        assert!(
            rt.find("sidebar").is_none(),
            "the full sidebar is gone when collapsed"
        );
    }

    // ── Sidebar resize & collapsed-mode e2e, driven through the real pointer ──
    // gestures and measured against the original soul-player's live
    // `useCollapsibleSidebar` + `CollapsedSidebarStrip`. Parity asserted here
    // (original → ours): width range 240–480 default 288; collapse snap < 200 px;
    // collapsed rail 12 px (`w-3`) with a 4×56 px grabber (`w-1 h-14`), centred;
    // `cursor-ew-resize` on both the handle and the rail; and a collapse that
    // preserves the expanded width (the original never overwrites `sidebar-width`
    // on snap, so re-opening restores it).
    //
    // Two intentional divergences, documented so a future reader doesn't "fix"
    // them into regressions: (1) ours resizes by pointer *delta*
    // (`width_at_press + dx`) rather than the original's absolute
    // `width = clientX`, so grabbing the handle never jumps the edge to the
    // cursor; (2) there is no 20 px re-open hysteresis — we re-open the instant
    // the drag target clears 200 px where the original waits for 220 px.
    //
    // Every drag deliberately stops at `Phase::Move` and never calls
    // `pointer_up`: `config::save_sidebar` only fires on release, so no test
    // here writes the user's real `config.toml`.

    /// A framed headless runtime whose sidebar starts at an explicit geometry,
    /// overriding whatever the user's persisted `config.toml` holds so the test
    /// is deterministic (as the other sidebar tests do). One frame is pumped so
    /// `find`/`presented` have layout.
    fn sidebar_rt(width: f32, collapsed: bool) -> Runtime<State> {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_width = width;
        s.sidebar_collapsed = collapsed;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        rt
    }

    #[test]
    fn expanded_sidebar_draws_at_its_configured_width() {
        // The old app applies the width via an inline `style={{ width }}`; ours
        // binds `.w(s.sidebar_width)`. Prove a non-default 360 px reaches the
        // drawn element (not merely the state), and that the right edge offers
        // the resize handle's `cursor-ew-resize`.
        let mut rt = sidebar_rt(360.0, false);
        let drawn = rt.presented("sidebar").expect("sidebar renders").0;
        assert!(
            (drawn[2] - 360.0).abs() < 0.5,
            "the sidebar draws at its configured width, got {}",
            drawn[2]
        );
        let (hx, hy) = rt.find("sidebar-resize").expect("handle present");
        rt.pointer_move(hx, hy);
        rt.frame(0.0);
        assert_eq!(
            rt.cursor(),
            Cursor::ResizeColumn,
            "the right-edge handle shows the resize cursor"
        );
    }

    #[test]
    fn dragging_the_handle_resizes_within_the_min_max_range() {
        // One captured drag with several moves — the e2e twin of the pure
        // `resize_to` unit test. `Drag::data` caches 288 at press; each move's
        // `dx` is travel from the press point.
        let mut rt = sidebar_rt(DEFAULT_WIDTH, false);
        let (hx, hy) = rt.find("sidebar-resize").expect("handle present");
        rt.pointer_move(hx, hy);
        rt.pointer_down(hx, hy);
        // Pointer events dispatch on the next `frame`, so each move is applied by
        // the `frame` that follows it. `Drag::data` (288, cached at press) and the
        // press origin both persist across the gesture, so every `dx` below is
        // travel from the original press point.
        // Out by 100 → 388, inside the range.
        rt.pointer_move(hx + 100.0, hy);
        rt.frame(0.0);
        assert_eq!(
            rt.state.sidebar_width, 388.0,
            "drag-out widens by the pointer travel"
        );
        // Keep pulling out hard → clamps at MAX, never wider.
        rt.pointer_move(hx + 400.0, hy);
        rt.frame(0.0);
        assert_eq!(rt.state.sidebar_width, MAX_WIDTH, "clamps to the max width");
        // Back in, but still above the collapse threshold (288 - 60 = 228) →
        // clamps up to MIN rather than collapsing.
        rt.pointer_move(hx - 60.0, hy);
        rt.frame(0.0);
        assert_eq!(rt.state.sidebar_width, MIN_WIDTH, "clamps to the min width");
        assert!(
            !rt.state.sidebar_collapsed,
            "stays expanded while above the threshold"
        );
    }

    #[test]
    fn dragging_the_handle_below_the_threshold_collapses_to_the_rail() {
        let mut rt = sidebar_rt(DEFAULT_WIDTH, false);
        let (hx, hy) = rt.find("sidebar-resize").expect("handle present");
        rt.pointer_move(hx, hy);
        rt.pointer_down(hx, hy);
        // 288 - 150 = 138 < 200 → snap collapsed (applied on the next frame).
        rt.pointer_move(hx - 150.0, hy);
        rt.frame(0.0);
        assert!(
            rt.state.sidebar_collapsed,
            "dragging below the 200 px threshold snaps collapsed"
        );
        assert_eq!(
            rt.state.sidebar_width, DEFAULT_WIDTH,
            "a collapse preserves the expanded width for re-opening"
        );
        rt.frame(0.0);
        assert!(
            rt.find("grabber").is_some(),
            "the thin rail (with its grabber) replaces the full sidebar"
        );
        assert!(rt.find("sidebar").is_none(), "the full sidebar is gone");
        assert!(
            rt.find("sidebar-resize").is_some(),
            "the handle (now the rail) stays, so one gesture drags back out"
        );
    }

    #[test]
    fn dragging_out_from_the_collapsed_rail_reopens_the_sidebar() {
        let mut rt = sidebar_rt(DEFAULT_WIDTH, true);
        let (hx, hy) = rt
            .find("sidebar-resize")
            .expect("handle present beside the rail");
        rt.pointer_move(hx, hy);
        // Phase::Down caches the rail width (STRIP_WIDTH) as the drag origin.
        rt.pointer_down(hx, hy);
        // 12 + 250 = 262 ≥ 200 → re-opens to the dragged width (next frame).
        rt.pointer_move(hx + 250.0, hy);
        rt.frame(0.0);
        assert!(
            !rt.state.sidebar_collapsed,
            "dragging out past the threshold re-opens the sidebar"
        );
        assert_eq!(
            rt.state.sidebar_width,
            STRIP_WIDTH + 250.0,
            "re-opens to the dragged width"
        );
        rt.frame(0.0);
        assert!(rt.find("sidebar").is_some(), "the full sidebar is back");
        assert!(
            rt.find("grabber").is_none(),
            "the collapsed rail (its grabber) is gone"
        );
    }

    /// Dragging the collapsed rail out LIVE-follows the cursor — the sidebar keeps
    /// widening as the pointer moves, across the rail→sidebar swap, instead of
    /// jumping only on release. This is the fix: the rail *is* the resize handle
    /// (same key), so its drag survives the expand.
    #[test]
    fn dragging_out_from_the_collapsed_rail_live_follows_the_cursor() {
        let mut rt = sidebar_rt(DEFAULT_WIDTH, true);
        let (hx, hy) = rt.find("sidebar-resize").expect("the collapsed rail");
        rt.pointer_move(hx, hy);
        rt.pointer_down(hx, hy); // Down caches STRIP_WIDTH as the drag origin
        // Past the threshold: expands live during the drag (not on release).
        rt.pointer_move(hx + 250.0, hy);
        rt.frame(0.0);
        assert!(
            !rt.state.sidebar_collapsed,
            "expands live mid-drag, not on release"
        );
        assert_eq!(rt.state.sidebar_width, STRIP_WIDTH + 250.0);
        // Keep dragging — the width MUST keep following after the rail became the
        // full sidebar (the drag persisted because the handle kept its key).
        rt.pointer_move(hx + 300.0, hy);
        rt.frame(0.0);
        assert_eq!(
            rt.state.sidebar_width,
            (STRIP_WIDTH + 300.0).clamp(MIN_WIDTH, MAX_WIDTH),
            "the sidebar keeps following the cursor after it expands"
        );
        rt.pointer_up(hx + 300.0, hy);
        rt.frame(0.0);
        assert!(!rt.state.sidebar_collapsed, "stays open on release");
    }

    #[test]
    fn collapsed_rail_is_a_thin_strip_with_a_centered_grabber() {
        let mut rt = sidebar_rt(DEFAULT_WIDTH, true);
        // The rail is the old app's `w-3` (12 px) CollapsedSidebarStrip — now the
        // resize handle itself, so dragging it live-resizes.
        let rail = rt.presented("sidebar-resize").expect("rail renders").0;
        assert!(
            (rail[2] - STRIP_WIDTH).abs() < 0.5,
            "the collapsed rail is a 12 px strip, got {}",
            rail[2]
        );
        // Its grabber pill is the old app's `w-1 h-14` → 4 × 56 px.
        let pill = rt.presented("grabber").expect("grabber renders").0;
        assert!(
            (pill[2] - 4.0).abs() < 0.5,
            "the grabber is 4 px wide, got {}",
            pill[2]
        );
        assert!(
            (pill[3] - 56.0).abs() < 0.5,
            "the grabber is 56 px tall, got {}",
            pill[3]
        );
        // Centred within the rail on BOTH axes (horizontal and vertical).
        let (rail_cx, rail_cy) = rt.find("sidebar-resize").unwrap();
        let (pill_cx, pill_cy) = rt.find("grabber").unwrap();
        assert!(
            (pill_cx - rail_cx).abs() < 0.5,
            "the grabber is centred horizontally in the rail"
        );
        assert!(
            (pill_cy - rail_cy).abs() < 1.0,
            "the grabber is centred vertically in the rail (not pinned to the top)"
        );
        // The rail itself offers the resize cursor (old app's `cursor-ew-resize`),
        // sampled above the pill so the pointer is over bare rail.
        rt.pointer_move(rail_cx, 120.0);
        rt.frame(0.0);
        assert_eq!(
            rt.cursor(),
            Cursor::ResizeColumn,
            "the collapsed rail shows the resize cursor"
        );
    }

    #[test]
    fn sources_list_renders_a_watched_folder_with_remove() {
        let dir = testdata_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(std::slice::from_ref(&dir), &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("source-0").is_some(), "the watched folder renders");
        assert!(rt.find("remove-0").is_some(), "with a remove control");
    }

    #[test]
    fn unavailable_folder_shows_offline_badge() {
        let missing = testdata_dir().join("does-not-exist");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(
            std::slice::from_ref(&missing),
            std::slice::from_ref(&missing),
        );
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("offline-0").is_some(),
            "an unavailable source shows the offline badge"
        );
    }

    /// A rich source row surfaces the source's FULL path (under its name) and a
    /// per-source track-count label — not just the basename.
    #[test]
    fn source_row_shows_full_path_and_track_count() {
        let dir = testdata_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(std::slice::from_ref(&dir), &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("source-0").is_some(), "the source row renders");
        // The FULL path string appears in some element's text (not just basename).
        let full = dir.to_str().unwrap();
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.text.as_deref() == Some(full)),
            "the full source path is shown somewhere in the row"
        );
        // A track-count label is rendered. Assert by KEY (not the localized text),
        // so a sibling test flipping the global locale can't flake this.
        assert!(rt.find("count-0").is_some(), "a track-count label is shown");
    }

    /// Unix seconds right now — for building a fresh [`RemoteHealth`] probe
    /// timestamp directly in a test, with no real server needed (the Inc-2
    /// program spec: `SourceStatus`/`RemoteHealth` values are built by hand).
    fn test_now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before the Unix epoch")
            .as_secs()
    }

    /// An online, fully-populated remote `SourceStatus`, built directly (no
    /// server) — the Inc-2 Sources panel's job is to render exactly these
    /// fields: a Remote badge, an online dot, latency, server version, track
    /// count, and a relative last-checked time.
    fn online_remote_status(url: &str) -> SourceStatus {
        SourceStatus {
            path: PathBuf::from(url),
            enabled: true,
            available: true,
            track_count: 7,
            kind: SourceKind::Remote {
                url: url.to_string(),
            },
            remote: Some(RemoteHealth {
                online: true,
                latency_ms: Some(42),
                server_version: Some("0.9.0".to_string()),
                index_version: Some(3),
                last_checked_unix: Some(test_now_unix()),
            }),
        }
    }

    /// An offline remote `SourceStatus` — the [`online_remote_status`] counterpart
    /// for the title-bar health indicator's partial/down cases and the health
    /// popover's offline row (no latency/version reading, since there was no
    /// successful probe to read one from).
    fn offline_remote_status(url: &str) -> SourceStatus {
        SourceStatus {
            path: PathBuf::from(url),
            enabled: true,
            available: false,
            track_count: 0,
            kind: SourceKind::Remote {
                url: url.to_string(),
            },
            remote: Some(RemoteHealth {
                online: false,
                latency_ms: None,
                server_version: None,
                index_version: None,
                last_checked_unix: Some(test_now_unix()),
            }),
        }
    }

    /// Inc 2: a Remote source's row renders a Remote badge plus its health
    /// telemetry (an online dot + a "42ms" latency reading), while a sibling
    /// Local source's row renders a Local badge and NEITHER of those
    /// remote-only elements — the "clear local/remote distinction" the
    /// program spec asks for. Every new element is keyed.
    #[test]
    fn remote_source_row_shows_badge_latency_and_online_indicator() {
        let dir = testdata_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(std::slice::from_ref(&dir), &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources
            .push(online_remote_status("http://example.test:8080"));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        // Local row (index 0): a Local badge, no remote-only telemetry.
        assert!(
            rt.find("source-badge-0").is_some(),
            "the local row shows a kind badge"
        );
        assert!(
            rt.find("source-latency-0").is_none(),
            "a local row has no latency element"
        );
        assert!(
            rt.find("source-online-0").is_none(),
            "a local row has no online indicator"
        );

        // Remote row (index 1): a Remote badge + its health telemetry.
        assert!(
            rt.find("source-badge-1").is_some(),
            "the remote row shows a kind badge"
        );
        assert!(
            rt.find("source-online-1").is_some(),
            "the remote row shows an online indicator"
        );
        assert_eq!(
            el_info(&rt, "source-latency-1").text.as_deref(),
            Some("42ms"),
            "latency renders as \"42ms\""
        );
    }

    /// An offline remote reads differently from an online one: the latency
    /// reads as the "—" placeholder (never a stale/misleading number) and the
    /// online indicator still renders (dim, not green — this test only
    /// checks it's present; the color is a rendering detail, not textual).
    #[test]
    fn offline_remote_source_row_shows_dash_latency_and_offline_dot() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources.push(SourceStatus {
            path: PathBuf::from("http://example.test:8080"),
            enabled: true,
            available: false,
            track_count: 0,
            kind: SourceKind::Remote {
                url: "http://example.test:8080".to_string(),
            },
            remote: Some(RemoteHealth {
                online: false,
                latency_ms: None,
                server_version: None,
                index_version: None,
                last_checked_unix: Some(test_now_unix()),
            }),
        });
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("source-online-0").is_some(),
            "an offline remote still shows its online/offline dot"
        );
        assert_eq!(
            el_info(&rt, "source-latency-0").text.as_deref(),
            Some("—"),
            "no latency reading renders as the dash placeholder"
        );
    }

    /// Regression: a remote that was JUST connected (its row exists in
    /// `s.sources` — the add succeeded — but no health probe has landed yet,
    /// `RemoteHealth::last_checked_unix: None`) must NOT render the bare
    /// latency/version/last-checked "— — —" trio `offline_remote_source_row_
    /// shows_dash_latency_and_offline_dot` pins for a genuinely offline
    /// (probed-and-failed) remote. It renders the keyed loading affordance
    /// instead, reading "Connecting…" — the "better loading than just
    /// '- - -'" fix.
    #[test]
    fn freshly_connected_remote_shows_connecting_not_dash_placeholders() {
        // Holds the locale lock (see `use_test_locale`): this asserts the exact
        // English "Connecting…" string, so it must not interleave with a test
        // that flips the process-global locale (the i18n test race).
        let _locale = config::use_test_locale("en");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources.push(SourceStatus {
            path: PathBuf::from("http://example.test:8080"),
            enabled: true,
            available: false,
            track_count: 3,
            kind: SourceKind::Remote {
                url: "http://example.test:8080".to_string(),
            },
            remote: Some(RemoteHealth {
                online: false,
                latency_ms: None,
                server_version: None,
                index_version: None,
                last_checked_unix: None, // never probed
            }),
        });
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("source-loading-0").is_some(),
            "a never-probed remote shows the Connecting…/Syncing… loading affordance"
        );
        assert_eq!(
            el_info(&rt, "source-loading-label-0").text.as_deref(),
            Some("Connecting…"),
            "the loading line reads Connecting…, not a bare dash"
        );
        assert!(
            rt.find("source-latency-0").is_none(),
            "no dash-placeholder latency element renders while connecting"
        );
    }

    /// Regression: a LIVE remote (already probed at least once, with
    /// real telemetry) that is mid a manual reconnect/resync
    /// (`State::remote_reconnecting`) shows "Syncing…" in place of its
    /// (possibly stale) health line, rather than leaving the old
    /// latency/version reading up as if nothing were happening.
    #[test]
    fn live_remote_mid_reconnect_shows_syncing_not_its_health_line() {
        // Holds the locale lock (see `use_test_locale`): asserts the exact
        // English "Syncing…" string — same race guard as the matching note in
        // `freshly_connected_remote_shows_connecting_not_dash_placeholders`.
        let _locale = config::use_test_locale("en");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let url = "http://example.test:8080";
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources.push(online_remote_status(url));
        s.remote_reconnecting = vec![url.to_string()];
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("source-loading-0").is_some(),
            "a mid-reconnect live remote shows the loading affordance"
        );
        assert_eq!(
            el_info(&rt, "source-loading-label-0").text.as_deref(),
            Some("Syncing…"),
            "mid-reconnect reads Syncing…, distinct from the fresh-connect Connecting… wording"
        );
        assert!(
            rt.find("source-latency-0").is_none(),
            "the stale latency reading is hidden while a resync is in flight"
        );
    }

    /// Inc 6 item 1: a persisted remote with NO live entry at all in
    /// `s.sources` (it failed to reconnect at launch, or hasn't tried yet) —
    /// the gap this increment closes, where the remote used to simply be
    /// absent from the Sources list — still renders: its URL, the offline
    /// dot (item (c): the SAME `health_dot` a live-but-unreachable remote
    /// uses), no latency element at all (there has been no probe to read one
    /// from), and a keyed Reconnect control.
    #[test]
    fn offline_remote_with_no_live_entry_renders_url_and_reconnect_control() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        // No `s.sources` entry at all for this url — only the persisted url
        // is known, simulating a remote that never (re)connected this session.
        s.remote_urls = vec!["http://offline-remote.test".to_string()];
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.elements()
                .iter()
                .any(|e| e.text.as_deref() == Some("http://offline-remote.test")),
            "the persisted remote's URL renders even with no live entry"
        );
        assert!(
            rt.find("source-online-0").is_some(),
            "the offline row shows the offline dot"
        );
        assert!(
            rt.find("source-latency-0").is_none(),
            "an offline row with no live entry renders no latency element at all"
        );
        assert!(
            rt.find("remote-reconnect-0").is_some(),
            "a keyed Reconnect control renders for the offline remote"
        );
        assert!(
            rt.find("remote-relogin-0").is_some(),
            "a keyed Sign-in-again control renders for the offline remote, so a \
             dead credential is fixable in one click without remove + re-add"
        );
    }

    /// `merged_source_rows` must never duplicate a source: a persisted
    /// `remote_urls` entry that differs from its LIVE `s.sources` entry only
    /// by a trailing slash (a real-world mismatch — the host normalizes on
    /// add, but a config file written by an older build, or a URL typed
    /// with a slash, could still carry one) must collapse onto the SAME row
    /// as the live one, not render a second "offline" row alongside it.
    #[test]
    fn persisted_url_with_trailing_slash_collapses_onto_the_live_row() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources
            .push(online_remote_status("http://127.0.0.1:8080"));
        s.remote_urls = vec!["http://127.0.0.1:8080/".to_string()];
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let badge_count = rt
            .elements()
            .iter()
            .filter(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("source-badge-"))
            })
            .count();
        assert_eq!(
            badge_count, 1,
            "a trailing-slash variant of an already-live url must render ONE row, not two"
        );
        assert!(
            rt.find("source-badge-0").is_some(),
            "the single row is the live one at index 0"
        );
        assert!(
            rt.find("remote-reconnect-0").is_none(),
            "the row is the LIVE (connected) one, so it offers Disconnect, not Reconnect"
        );
    }

    /// `merged_source_rows` must also dedupe within `s.remote_urls` itself:
    /// the same url listed twice (even differing by a trailing slash) must
    /// still render only ONE offline row, not one per occurrence.
    #[test]
    fn duplicate_entries_within_remote_urls_render_only_one_offline_row() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.remote_urls = vec![
            "http://dup-remote.test".to_string(),
            "http://dup-remote.test/".to_string(),
        ];
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let badge_count = rt
            .elements()
            .iter()
            .filter(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("source-badge-"))
            })
            .count();
        assert_eq!(
            badge_count, 1,
            "two remote_urls entries for the same url (trailing slash aside) must render ONE row"
        );
        assert!(
            rt.find("remote-reconnect-0").is_some(),
            "the single surviving row is still the offline one, with its Reconnect control"
        );
        assert!(
            rt.find("remote-reconnect-1").is_none(),
            "no second offline row for the duplicate entry"
        );
    }

    /// Regression: pins the host-side rescan fix (soul-player-host commits
    /// b2fa8da/103068c + the `rebuild_remote_library` fix alongside this PR) at
    /// the UI layer. A rescan's `WatchUpdate.sources` now always carries any
    /// still-connected remote alongside the local roots passed to
    /// `set_folders` — `apply_watch` must fold that remote in as the SAME live
    /// row it already had (telemetry + Disconnect), never demote it to the
    /// stale "offline merge" row `merged_source_rows` falls back to for a
    /// persisted `remote_urls` entry with no live `s.sources` match. Before the
    /// host fix, a rescan's `WatchUpdate` dropped the remote from `.sources`
    /// entirely, so this exact fold (a local root's `SourceStatus` plus the
    /// SAME remote's `SourceStatus`, both present at once — the shape a fixed
    /// rescan now returns) is what must render as one local row + one live
    /// remote row, not a duplicate or an offline/Reconnect row.
    #[test]
    fn apply_watch_keeps_a_rescanned_remote_as_a_live_row_not_offline() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        // Seed the pre-rescan state the way a real session would: the remote
        // is both connected (s.sources) and persisted (s.remote_urls, as
        // `apply_remote_connect`/`reconnect_remotes_on_startup` leave it).
        s.sources
            .push(online_remote_status("http://127.0.0.1:8080"));
        s.remote_urls = vec!["http://127.0.0.1:8080".to_string()];

        // The WatchUpdate a "Rescan" over a local root now returns (per the
        // host fix): the local root's status AND the retained remote's,
        // still carrying its live health — exactly `set_folders`' shape.
        let up = WatchUpdate {
            views: Vec::new(),
            sources: vec![
                SourceStatus {
                    path: PathBuf::from("local-root"),
                    enabled: true,
                    available: true,
                    track_count: 2,
                    kind: SourceKind::Local {
                        path: PathBuf::from("local-root"),
                    },
                    remote: None,
                },
                online_remote_status("http://127.0.0.1:8080"),
            ],
        };
        apply_watch(&mut s, up);

        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let badge_count = rt
            .elements()
            .iter()
            .filter(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("source-badge-"))
            })
            .count();
        assert_eq!(
            badge_count, 2,
            "one local row + one remote row after the rescan, no duplicate/offline-merge row"
        );
        assert!(
            rt.find("source-online-1").is_some(),
            "the remote (index 1, after the local root) still renders its live online dot"
        );
        assert_eq!(
            el_info(&rt, "source-latency-1").text.as_deref(),
            Some("42ms"),
            "a rescanned remote keeps its live telemetry, not the offline dash placeholder"
        );
        assert!(
            rt.find("remote-reconnect-1").is_none(),
            "a live remote row shows no Reconnect control"
        );
    }

    /// Inc 6 item 2: a connected remote's row offers a keyed Disconnect
    /// (remove) control — the remote counterpart of a local row's ✕, under
    /// its own `remote-remove-{index}` key, wired to `disconnect_remote_source`
    /// rather than `remove_folder`.
    #[test]
    fn connected_remote_row_shows_a_keyed_disconnect_control() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources
            .push(online_remote_status("http://example.test:8080"));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("remote-remove-0").is_some(),
            "a connected remote row shows a keyed Disconnect control"
        );
    }

    /// A connected remote row shows the SAME enable/disable checkbox a local
    /// root's row has (keyed `enable-{index}`, focusable, labelled) —
    /// ALONGSIDE the online/offline dot, not instead of it: the toggle is
    /// user intent on/off, the dot is live reachability, and a remote row
    /// now carries both.
    #[test]
    fn connected_remote_row_shows_the_enable_toggle_alongside_the_dot() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources
            .push(online_remote_status("http://example.test:8080"));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let toggle = el_info(&rt, "enable-0");
        assert!(
            toggle.focusable,
            "the remote row's enable toggle is focusable"
        );
        assert!(
            toggle.label.as_deref().is_some_and(|l| !l.is_empty()),
            "the remote row's enable toggle carries a non-empty label"
        );
        assert!(
            rt.find("source-online-0").is_some(),
            "the online dot still renders alongside the toggle"
        );
    }

    /// Clicking a connected remote row's enable toggle drives
    /// `toggle_remote_enabled` with that row's own url: it persists the
    /// disabled flag via `config::set_remote_enabled`, the same way clicking
    /// a local row's toggle persists through `apply_and_save`. The host-side
    /// half (the remote's tracks leaving `views()`) is covered against a
    /// REAL connected remote by the host's own
    /// `remote_disable_drops_its_tracks_and_reenable_restores_them` (crates/
    /// soul-player-host/tests/remote_e2e.rs) — this proves the UI wires the
    /// press to the right url/flag, not a re-test of the host logic.
    #[test]
    fn toggling_a_remote_row_persists_the_disabled_flag_for_its_url() {
        let _config_guard = config::use_test_config_dir();
        for url in config::load_remotes() {
            config::remove_remote(&url);
        }
        let url = "http://example.test:8080";
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.sources.push(online_remote_status(url));
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            config::load_remotes_disabled().is_empty(),
            "nothing disabled before the click"
        );

        click_key(&mut rt, "enable-0");

        assert_eq!(
            config::load_remotes_disabled(),
            vec![url.to_string()],
            "clicking the toggle persists this remote's url as disabled"
        );
    }

    /// Inc 6 item 3: while a manual reconnect is in flight for an offline
    /// remote (`State::remote_reconnecting`), its row still renders a status
    /// line, and the Reconnect control switches to its disabled/inert shape
    /// (not focusable) so a double press — or the periodic poller probing
    /// mid-attempt — can't start a second reconnect for the same url.
    #[test]
    fn offline_remote_row_shows_an_inert_reconnect_control_while_in_flight() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        s.remote_urls = vec!["http://offline-remote.test".to_string()];
        s.remote_reconnecting = vec!["http://offline-remote.test".to_string()];
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("remote-status-0").is_some(),
            "the offline row still renders a status line while reconnecting"
        );
        assert!(
            !el_info(&rt, "remote-reconnect-0").focusable,
            "the Reconnect control is inert while already reconnecting"
        );
    }

    /// Inc 6 item 2 (behavior): disconnecting drops the url from the
    /// in-memory bookkeeping (`remote_urls`) AND the persisted config
    /// (`config::remove_remote`), so a disconnected remote neither lingers
    /// as a phantom offline row nor comes back on the next launch.
    #[test]
    fn disconnect_remote_source_drops_url_from_state_and_config() {
        let _config_guard = config::use_test_config_dir();
        drain_remotes();
        let url = "http://disconnect-me.test";
        config::save_remote(url);
        let mut s = State::new(PlayerHandle::offline());
        s.remote_urls = vec![url.to_string()];

        disconnect_remote_source(&mut s, url.to_string());

        assert!(
            s.remote_urls.is_empty(),
            "the url drops out of the in-memory bookkeeping"
        );
        assert!(
            config::load_remotes().is_empty(),
            "the url drops out of the persisted config"
        );
    }

    /// Inc 6 item 3 (behavior): a reconnect already in flight for a url
    /// guards against a second concurrent attempt for the SAME url —
    /// pressing Reconnect twice (or the periodic poller firing mid-attempt)
    /// can't start two reconnects racing each other.
    #[test]
    fn reconnect_remote_now_guards_against_a_second_concurrent_attempt() {
        let mut s = State::new(PlayerHandle::offline());
        let url = "http://already-reconnecting.test".to_string();
        s.remote_reconnecting = vec![url.clone()];

        reconnect_remote_now(&mut s, url.clone());

        assert_eq!(
            s.remote_reconnecting,
            vec![url],
            "a second attempt for the same url already in flight is a no-op"
        );
    }

    /// `relogin_remote` shares `reconnect_remote_now`'s in-flight guard: a
    /// relogin (or reconnect) already running for a url makes a second press a
    /// no-op, so a stray double-click can't open two browser logins racing for
    /// the same source. (Only the guard's early return is exercised here — the
    /// success path would open the system browser, untestable headlessly.)
    #[test]
    fn relogin_remote_guards_against_a_second_concurrent_attempt() {
        let mut s = State::new(PlayerHandle::offline());
        let url = "http://already-relogging-in.test".to_string();
        s.remote_reconnecting = vec![url.clone()];

        relogin_remote(&mut s, url.clone());

        assert_eq!(
            s.remote_reconnecting,
            vec![url],
            "a relogin for a url already in flight is a no-op (no second browser login)"
        );
    }

    /// Inc 6 item 3 (behavior): a failed manual reconnect (no stored
    /// credential — the common case in a fresh test config dir) still
    /// clears the transient "reconnecting" state — the row must not get
    /// stuck reading "reconnecting…" forever just because the attempt
    /// didn't succeed, mirroring the startup reconnect's "never surfaced as
    /// an error, never stuck" policy.
    #[test]
    fn reconnect_remote_now_clears_the_transient_state_on_failure() {
        let mut s = State::new(PlayerHandle::offline());
        let url = "http://unreachable-remote.invalid".to_string();

        reconnect_remote_now(&mut s, url.clone());

        assert!(
            s.remote_reconnecting.is_empty(),
            "the transient reconnecting state clears whichever way the attempt ends"
        );
    }

    /// Robustness: a failed manual reconnect must leave the url LISTED in
    /// `remote_urls` — the persisted-remote bookkeeping `merged_source_rows`
    /// renders an offline row from (see `offline_remote_with_no_live_entry_
    /// renders_url_and_reconnect_control`). `reconnect_remote_now` never
    /// touches `remote_urls` on any path, but a future refactor easily could
    /// (e.g. folding it into the same cleanup as `remote_reconnecting`) — this
    /// pins the "never drop a configured remote on a transient failure" rule
    /// the module doc already states.
    #[test]
    fn reconnect_remote_now_failure_keeps_the_url_in_remote_urls() {
        let mut s = State::new(PlayerHandle::offline());
        let url = "http://unreachable-remote.invalid".to_string();
        s.remote_urls = vec![url.clone()];

        reconnect_remote_now(&mut s, url.clone());

        assert_eq!(
            s.remote_urls,
            vec![url],
            "a failed reconnect must not drop the url from the persisted-remote list"
        );
    }

    /// Robustness: `remote_health_refreshing` guards `refresh_remote_health`
    /// against a second overlapping probe (the periodic poller and an
    /// on-demand refresh racing each other) — documented on the function but,
    /// until now, never actually exercised. With the flag already set, a call
    /// must be a complete no-op: it returns before touching the host OR
    /// `s.sources`, so a stale-but-in-flight refresh can never stomp a fresher
    /// one that's about to land.
    #[test]
    fn refresh_remote_health_guards_against_a_second_concurrent_probe() {
        let mut s = State::new(PlayerHandle::offline());
        let marker = vec![online_remote_status("http://marker.test")];
        s.sources = marker.clone();
        s.remote_health_refreshing = true;

        refresh_remote_health(&mut s);

        assert_eq!(
            s.sources, marker,
            "a refresh already in flight must not overwrite sources()"
        );
        assert!(
            s.remote_health_refreshing,
            "the guard flag is untouched by the no-op call"
        );
    }

    /// Robustness: re-enabling a remote through `toggle_remote_enabled`
    /// clears its persisted disabled flag — the restore half of "toggling a
    /// remote off then on restores it". The disable direction is already
    /// covered end-to-end via a UI click
    /// (`toggling_a_remote_row_persists_the_disabled_flag_for_its_url`); that
    /// test can't also exercise re-enabling in the same `Runtime` session
    /// because disabling repopulates `s.sources` from the (unconfigured,
    /// offline) host, which drops the row the click target lived on. Calling
    /// the function directly sidesteps that and proves the other direction of
    /// the SAME persistence seam `config::set_remote_enabled` round-trips.
    #[test]
    fn toggle_remote_enabled_true_clears_a_previously_disabled_urls_persisted_flag() {
        let _config_guard = config::use_test_config_dir();
        let url = "http://re-enable-me.test";
        config::set_remote_enabled(url, false);
        assert_eq!(
            config::load_remotes_disabled(),
            vec![url.to_string()],
            "sanity: the url starts disabled"
        );
        let mut s = State::new(PlayerHandle::offline());

        toggle_remote_enabled(&mut s, url.to_string(), true);

        assert!(
            config::load_remotes_disabled().is_empty(),
            "re-enabling clears the url's persisted disabled flag"
        );
    }

    /// Inc 6 item 4: a connected remote's live `index_version` reading above
    /// the version recorded at its last actual sync (`remote_synced_index`)
    /// shows the "update available" cue with a keyed Sync-now action (reusing
    /// the same reconnect/re-sync control).
    #[test]
    fn update_available_cue_shows_when_live_version_exceeds_the_synced_one() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let url = "http://example.test:8080";
        let mut status = online_remote_status(url);
        if let Some(health) = &mut status.remote {
            health.index_version = Some(4);
        }
        s.sources.push(status);
        s.remote_synced_index.insert(url.to_string(), 3);
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("remote-update-0").is_some(),
            "a newer live index version than the last synced one shows the update cue"
        );
        assert!(
            rt.find("remote-reconnect-0").is_some(),
            "the cue offers a keyed Sync-now action"
        );
    }

    /// Inc 6 item 4 (negative case): a remote already caught up to its last
    /// synced index version shows no update cue — `online_remote_status`
    /// reports `index_version: Some(3)`, matching what we record as synced.
    #[test]
    fn update_available_cue_is_absent_when_already_caught_up() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let url = "http://example.test:8080";
        s.sources.push(online_remote_status(url));
        s.remote_synced_index.insert(url.to_string(), 3);
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("remote-update-0").is_none(),
            "no cue when the live version matches the last synced one"
        );
    }

    /// The periodic poller auto-pulls exactly the connected remotes whose live
    /// catalog version has advanced past the last synced baseline — the same
    /// predicate as the "update available" cue, but driving an automatic
    /// re-sync rather than a manual button. (Selection only; the reconnect glue
    /// is the already-tested `reconnect_remote_now`.)
    #[test]
    fn remotes_needing_sync_picks_only_the_advanced_connected_ones() {
        let mut s = settings_state(&[], &[]);
        let fresh = "http://fresh.test:8080"; // live 5 > synced 3 -> needs sync
        let caught_up = "http://same.test:8080"; // live 3 == synced 3 -> no
        let unseen = "http://new.test:8080"; // live 3, no baseline yet -> no

        let mut fresh_status = online_remote_status(fresh);
        if let Some(h) = &mut fresh_status.remote {
            h.index_version = Some(5);
        }
        s.sources.push(fresh_status);
        s.remote_synced_index.insert(fresh.to_string(), 3);

        s.sources.push(online_remote_status(caught_up)); // index_version Some(3)
        s.remote_synced_index.insert(caught_up.to_string(), 3);

        s.sources.push(online_remote_status(unseen)); // Some(3), no baseline

        assert_eq!(remotes_needing_sync(&s), vec![fresh.to_string()]);
    }

    /// A disabled remote is never auto-pulled even when its server advanced —
    /// its tracks are intentionally out of the aggregate, so re-fetching them
    /// would be pointless churn.
    #[test]
    fn remotes_needing_sync_skips_disabled_remotes() {
        let mut s = settings_state(&[], &[]);
        let url = "http://off.test:8080";
        let mut status = online_remote_status(url);
        status.enabled = false;
        if let Some(h) = &mut status.remote {
            h.index_version = Some(9);
        }
        s.sources.push(status);
        s.remote_synced_index.insert(url.to_string(), 1);
        assert!(remotes_needing_sync(&s).is_empty());
    }

    /// An offline remote (no live index reading) is not auto-pulled — there is
    /// nothing to compare against, and reconnecting it is the offline row's job,
    /// not the version-advanced auto-sync's.
    #[test]
    fn remotes_needing_sync_skips_offline_remotes() {
        let mut s = settings_state(&[], &[]);
        let url = "http://down.test:8080";
        s.sources.push(offline_remote_status(url)); // index_version None
        s.remote_synced_index.insert(url.to_string(), 1);
        assert!(remotes_needing_sync(&s).is_empty());
    }

    /// The Rescan action force-resyncs every ENABLED remote regardless of a
    /// detected version change (a plain local-root reconcile rebuilds remote
    /// tracks from cached blobs and would miss server-side additions); a
    /// disabled remote and local roots are excluded.
    #[test]
    fn enabled_remote_urls_lists_enabled_remotes_only() {
        let mut s = settings_state(&[], &[]);
        let on = "http://on.test:8080";
        let off = "http://off.test:8080";
        s.sources.push(online_remote_status(on));
        let mut disabled = online_remote_status(off);
        disabled.enabled = false;
        s.sources.push(disabled);
        s.sources.push(SourceStatus {
            path: PathBuf::from("/music"),
            enabled: true,
            available: true,
            track_count: 3,
            kind: SourceKind::Local {
                path: PathBuf::from("/music"),
            },
            remote: None,
        });
        assert_eq!(enabled_remote_urls(&s), vec![on.to_string()]);
    }

    /// Several remotes advancing at once are all returned, in `sources` order —
    /// the poller pulls each rather than only the first it finds.
    #[test]
    fn remotes_needing_sync_returns_all_advanced_in_source_order() {
        let mut s = settings_state(&[], &[]);
        for (url, live) in [("http://a.test:8080", 4u64), ("http://b.test:8080", 7)] {
            let mut st = online_remote_status(url);
            if let Some(h) = &mut st.remote {
                h.index_version = Some(live);
            }
            s.sources.push(st);
            s.remote_synced_index.insert(url.to_string(), 1);
        }
        assert_eq!(
            remotes_needing_sync(&s),
            vec![
                "http://a.test:8080".to_string(),
                "http://b.test:8080".to_string()
            ],
        );
    }

    /// Only a STRICTLY greater live version counts: a live reading at or below
    /// the synced baseline (a server reindex that lowered the counter, or a
    /// stale probe) must not trigger a pointless re-sync. Guards `>` vs `>=`/`!=`.
    #[test]
    fn remotes_needing_sync_ignores_a_live_version_at_or_below_baseline() {
        let mut s = settings_state(&[], &[]);
        let url = "http://roll.test:8080";
        let mut st = online_remote_status(url);
        if let Some(h) = &mut st.remote {
            h.index_version = Some(2); // below the synced baseline
        }
        s.sources.push(st);
        s.remote_synced_index.insert(url.to_string(), 5);
        assert!(remotes_needing_sync(&s).is_empty());
    }

    /// The auto-sync converges: once a pull records the fresh version as synced
    /// (`record_synced_index`), the same remote no longer needs syncing — the
    /// loop-prevention guarantee that keeps the 30s poller from re-pulling an
    /// already-current remote forever.
    #[test]
    fn remotes_needing_sync_is_empty_after_recording_the_synced_index() {
        let mut s = settings_state(&[], &[]);
        let url = "http://conv.test:8080";
        let mut st = online_remote_status(url);
        if let Some(h) = &mut st.remote {
            h.index_version = Some(8);
        }
        s.sources.push(st);
        s.remote_synced_index.insert(url.to_string(), 3);
        assert_eq!(remotes_needing_sync(&s), vec![url.to_string()]);

        // Simulate the successful pull catching the baseline up to the live
        // version (what `reconnect_remote_now` does via `record_synced_index`).
        let sources = s.sources.clone();
        record_synced_index(&mut s, url, &sources);
        assert!(
            remotes_needing_sync(&s).is_empty(),
            "after the pull records the fresh version, the remote is caught up",
        );
    }

    /// No remotes at all (only local roots) → nothing to force-resync on Rescan.
    #[test]
    fn enabled_remote_urls_is_empty_with_only_local_sources() {
        let mut s = settings_state(&[], &[]);
        s.sources.push(SourceStatus {
            path: PathBuf::from("/music"),
            enabled: true,
            available: true,
            track_count: 3,
            kind: SourceKind::Local {
                path: PathBuf::from("/music"),
            },
            remote: None,
        });
        assert!(enabled_remote_urls(&s).is_empty());
    }

    /// The leading enable toggle is a focusable control that disables its source:
    /// clicking it flips `enabled` off and the (now-disabled) source stops
    /// contributing tracks, emptying the aggregated library.
    #[test]
    fn toggling_a_source_disables_it() {
        // The toggle persists via apply_and_save — isolate config to a temp dir.
        let _config_guard = config::use_test_config_dir();
        let dir = testdata_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(std::slice::from_ref(&dir), &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Precondition: an enabled, available source contributes its tracks.
        assert!(rt.state.sources[0].enabled, "source starts enabled");
        assert!(
            !rt.state.library.is_empty(),
            "an enabled source contributes tracks"
        );
        let toggle = "enable-0".to_string();
        assert!(
            el_info(&rt, &toggle).focusable,
            "the enable toggle is keyboard-focusable"
        );
        click_key(&mut rt, &toggle);
        rt.settle_all();
        rt.frame(0.0);
        assert!(
            !rt.state.sources[0].enabled,
            "clicking the toggle disables the source"
        );
        assert!(
            rt.state.library.is_empty(),
            "a disabled source contributes no tracks"
        );
    }

    #[test]
    fn sidebar_has_library_and_settings_nav() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            State::new(PlayerHandle::offline()),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.frame(0.0);
        assert!(rt.find("nav-library").is_some());
        assert!(rt.find("nav-settings").is_some());
    }

    #[test]
    fn is_dark_respects_the_theme_preference() {
        assert!(is_dark(ThemePref::Dark, false));
        assert!(!is_dark(ThemePref::Light, true));
        assert!(is_dark(ThemePref::System, true));
        assert!(!is_dark(ThemePref::System, false));
    }

    #[test]
    fn settings_shows_appearance_theme_options() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("appearance").is_some(),
            "Settings has an Appearance section"
        );
        assert!(rt.find("theme-system").is_some());
        assert!(rt.find("theme-light").is_some());
        assert!(rt.find("theme-dark").is_some());
        assert!(rt.find("album-year-original").is_some());
        assert!(rt.find("album-year-release").is_some());
    }

    #[test]
    fn settings_shows_replaygain_section_with_modes_and_preamp() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::Audio;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("sp-stage-replaygain").is_some(),
            "Settings → Audio has a ReplayGain stage card"
        );
        for key in ["rg-off", "rg-track", "rg-album", "rg-preamp"] {
            assert!(rt.find(key).is_some(), "ReplayGain control {key} present");
        }
    }

    /// Settings toggle buttons match the old app's control metrics: ~36 pt tall
    /// (py 8 + a 14 pt label), the same button height the original's settings
    /// controls use.
    #[test]
    fn settings_toggle_buttons_are_36pt_tall() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let ([_, _, _, h], _) = rt
            .presented("theme-system")
            .expect("the Appearance section renders a theme toggle");
        assert!(
            (h - 36.0).abs() <= 4.0,
            "a settings toggle is ~36 pt tall (old app's control height), got {h}"
        );
    }

    /// The slider thumb is hidden at rest and fades in on hover — the old app's
    /// ProgressBar `opacity: isHovering ? 1 : 0`, driven here by `group_opacity`.
    /// Rendered as a lone slider so the `knob` key is unambiguous.
    #[test]
    fn slider_thumb_is_hidden_until_hover() {
        fn solo_slider(_s: &State, cx: &mut Cx<State>) -> El {
            let p = theme::palette(false);
            col().padding(40.0).child(crate::views::slider::slider(
                "solo",
                0.5,
                false,
                cx,
                &p,
                |_s: &mut State, _v: f32| {},
                |_s: &mut State, _v: f32| {},
            ))
        }
        let env = Env {
            system_dark: false,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            State::new(PlayerHandle::offline()),
            solo_slider,
            (400.0, 200.0),
            env,
        );
        for _ in 0..10 {
            rt.frame(0.05);
        }
        let (_, rest) = rt.presented("solo-knob").expect("the slider thumb renders");
        assert!(rest < 0.2, "thumb is hidden at rest (opacity {rest})");

        let (cx_pt, cy_pt) = rt.find("solo").expect("the slider is present");
        rt.pointer_move(cx_pt, cy_pt);
        for _ in 0..16 {
            rt.frame(0.05);
        }
        let (_, hov) = rt.presented("solo-knob").expect("the slider thumb renders");
        assert!(hov > 0.8, "thumb fades in on hover (opacity {hov})");
    }

    /// Transport controls are disabled — dimmed to 0.5 and inert — when there is
    /// no current track, as the old app's `disabled={!hasCurrentTrack}` →
    /// `opacity-0.5`; full opacity once a track is loaded.
    #[test]
    fn transport_is_disabled_with_no_current_track() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        // Nothing loaded → the play/pause button is dimmed.
        let mut idle = State::new(PlayerHandle::offline());
        idle.sidebar_collapsed = false; // the transport lives in the sidebar
        let mut rt = Runtime::new(idle, view, (1100.0, 720.0), env);
        for _ in 0..6 {
            rt.frame(0.05);
        }
        let (_, off) = rt.presented("play-pause").expect("transport renders");
        assert!(
            (off - 0.5).abs() < 0.12,
            "disabled transport is dimmed to ~0.5 (opacity {off})"
        );

        // A loaded track → full opacity.
        let mut live = State::new(PlayerHandle::offline());
        live.sidebar_collapsed = false;
        live.now_playing_id = Some(42);
        let mut rt = Runtime::new(live, view, (1100.0, 720.0), env);
        for _ in 0..6 {
            rt.frame(0.05);
        }
        let (_, on) = rt.presented("play-pause").expect("transport renders");
        assert!(on > 0.9, "enabled transport is full opacity (opacity {on})");
    }

    /// The play button of an album that is already playing toggles pause/resume
    /// (the old app's MediaCard), instead of restarting it from the top —
    /// end-to-end through the real button press, not just the decision fn.
    #[test]
    fn the_play_button_of_the_active_album_toggles_instead_of_restarting() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")]; // album "Alb", ids 1,2
        s.route = Route::Albums;
        s.now_playing_id = Some(1); // a track of this album is the current one
        s.playing = true;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "play-btn");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::TogglePlay],
            "an already-playing album toggles rather than restarting"
        );
    }

    fn album_track(id: u32, title: &str) -> soul_player_host::TrackView {
        soul_player_host::TrackView {
            id,
            title: Some(title.to_string()),
            artist: Some("A".into()),
            album: Some("Alb".into()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
        }
    }

    fn artist_track(id: u32, artist: &str) -> soul_player_host::TrackView {
        soul_player_host::TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(artist.to_string()),
            album: Some("Alb".into()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
        }
    }

    fn genre_track(id: u32, genre: &str) -> soul_player_host::TrackView {
        soul_player_host::TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some("A".into()),
            album: Some("Alb".into()),
            genre: Some(genre.to_string()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
        }
    }

    /// The sidebar offers an Artists nav entry alongside Tracks and Albums.
    #[test]
    fn the_sidebar_offers_an_artists_nav_item() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("nav-artists").is_some(),
            "the Artists nav item renders"
        );
    }

    /// Clicking an artist card opens that artist's detail page (and issues no
    /// playback) — the library grouped by artist, first-seen order.
    #[test]
    fn clicking_an_artist_card_opens_the_detail_page() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![artist_track(1, "Bach"), artist_track(2, "Satie")];
        s.route = Route::Artists;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        click(&mut rt, "card-text"); // the first grouped artist's title (Bach)
        assert!(
            matches!(rt.state.route, Route::Artist),
            "the card opens the artist detail page"
        );
        assert_eq!(rt.state.open_artist.as_deref(), Some("Bach"));
        assert!(
            rt.state.host.recorded_commands().is_empty(),
            "opening an artist does not start playback"
        );
    }

    /// The artist detail page lists the artist's tracks, with a Play All that
    /// plays them from the top and a back link.
    #[test]
    fn the_artist_detail_lists_tracks_with_play_all_and_back() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            artist_track(1, "Bach"),
            artist_track(2, "Bach"),
            artist_track(3, "Satie"),
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("artist-header").is_some(),
            "the detail header renders"
        );
        assert!(rt.find("detail-back").is_some(), "a back link renders");
        assert!(
            rt.find("album-track-0").is_some(),
            "the artist's tracks are listed"
        );
        click(&mut rt, "artist-play-all");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 0
            }],
            "Play All plays only this artist's tracks, from the top"
        );
    }

    /// The artist detail's Top-Songs rows share `albums::detail_track_row`
    /// with the main track list and the album detail page, so a remote
    /// track there shows the same cloud badge + hover tooltip too.
    #[test]
    fn the_artist_detail_page_shows_the_remote_badge_for_a_remote_track() {
        let _locale = config::use_test_locale("en");
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.library = vec![
            soul_player_host::TrackView {
                id: 1,
                title: Some("Streamed".into()),
                artist: Some("Bach".into()),
                album: Some("Alb".into()),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: Some("listen.home.lan".into()),
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
            soul_player_host::TrackView {
                id: 2,
                title: Some("OnDisk".into()),
                artist: Some("Bach".into()),
                album: Some("Alb".into()),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let badge_visible = |rt: &Runtime<State>, key: &str| {
            rt.elements()
                .iter()
                .any(|e| e.key.as_deref() == Some(key) && e.opacity > 0.01)
        };
        assert!(
            badge_visible(&rt, "track-remote-1"),
            "a remote track's artist-detail row should show the remote badge"
        );
        assert!(
            !badge_visible(&rt, "track-remote-2"),
            "a local track's artist-detail row should not show a remote badge"
        );
        let help = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("track-remote-1"))
            .and_then(|e| e.help);
        assert_eq!(
            help.as_deref(),
            Some("Streams from listen.home.lan"),
            "the artist-detail remote badge's tooltip should name the source host"
        );
    }

    /// The artist detail shows both a Top-Songs section and a Discography grid.
    #[test]
    fn the_artist_detail_has_top_songs_and_discography() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        // Two albums for Bach so the discography has cards to show.
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Cello Suites"),
            artist_album_track(3, "Bach", "Goldberg"),
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0); // second frame: discography grid reads its committed width
        assert!(
            rt.find("artist-header").is_some(),
            "the circular hero renders"
        );
        assert!(
            rt.find("artist-top-songs").is_some(),
            "Top Songs section renders"
        );
        assert!(
            rt.find("artist-discography").is_some(),
            "Discography section renders"
        );
        assert!(
            rt.find("artist-disc-0").is_some(),
            "a discography album card renders"
        );
    }

    #[test]
    fn discography_grid_list_toggle_switches_and_lists_navigate() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Goldberg"),
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        // Default is grid: cards present, no rows.
        assert!(rt.find("artist-disc-0").is_some(), "grid cards by default");
        assert!(rt.find("artist-disc-row-0").is_none());
        // Switch to list.
        click(&mut rt, "disc-toggle-list");
        assert!(rt.state.discography_list, "toggle flips to list");
        // The list is a brand-new subtree (replacing the grid), so it enter-animates
        // from opacity 0; settle instantly so its rows are pickable (matches the
        // settle_all pattern used elsewhere in this module).
        rt.settle_all();
        rt.frame(0.0);
        assert!(rt.find("artist-disc-row-0").is_some(), "list rows render");
        assert!(rt.find("artist-disc-0").is_none(), "grid cards gone");
        // A list row opens the album (through the back-stack).
        click(&mut rt, "artist-disc-row-0");
        assert!(
            matches!(rt.state.route, Route::Album),
            "a list row opens the album"
        );
        assert_eq!(rt.state.open_album.as_deref(), Some("Cello Suites"));
    }

    /// A discography album card opens that album's detail page.
    #[test]
    fn a_discography_card_opens_the_album() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Goldberg"),
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        click(&mut rt, "card-text"); // first disc card's title area (Cello Suites)
        assert!(
            matches!(rt.state.route, Route::Album),
            "a disc card opens the album"
        );
        assert_eq!(rt.state.open_album.as_deref(), Some("Cello Suites"));
    }

    #[test]
    fn back_returns_to_where_you_came_from() {
        // Artist → (Discography) → Album → Back must return to the ARTIST, not the
        // hardcoded Albums parent.
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Goldberg"),
        ];
        s.route = Route::Artists;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        rt.frame(0.0);
        click(&mut rt, "card-text"); // open the artist (Bach) — pushes Artists
        assert!(matches!(rt.state.route, Route::Artist));
        // The artist detail page is a brand-new subtree this frame, so its cards
        // enter-animate from opacity 0; settle instantly so the disc card below is
        // pickable (matches the settle_all pattern used elsewhere in this module).
        rt.settle_all();
        rt.frame(0.0);
        rt.frame(0.0);
        click(&mut rt, "card-text"); // first Discography album — pushes the artist
        assert!(
            matches!(rt.state.route, Route::Album),
            "a disc card opens the album"
        );
        // Same reason: the album detail page just mounted this frame.
        rt.settle_all();
        rt.frame(0.0);
        click(&mut rt, "detail-back"); // Back
        assert!(
            matches!(rt.state.route, Route::Artist),
            "back returns to the artist"
        );
        assert_eq!(rt.state.open_artist.as_deref(), Some("Bach"));
        // The artist detail page is remounted on the way back too.
        rt.settle_all();
        rt.frame(0.0);
        click(&mut rt, "detail-back"); // Back again
        assert!(
            matches!(rt.state.route, Route::Artists),
            "and then to the Artists list"
        );
    }

    #[test]
    fn back_falls_back_to_the_parent_with_an_empty_stack() {
        let mut s = State::new(PlayerHandle::offline());
        s.route = Route::Album; // reached without history (e.g. deep link)
        go_back(&mut s);
        assert!(matches!(s.route, Route::Albums), "album → albums");
        s.route = Route::Genre;
        go_back(&mut s);
        assert!(matches!(s.route, Route::Genres), "genre → genres");
        s.route = Route::Playlist;
        go_back(&mut s);
        assert!(matches!(s.route, Route::Playlists), "playlist → playlists");
    }

    /// The sidebar offers a Playlists destination.
    #[test]
    fn sidebar_has_a_playlists_destination() {
        assert!(views::sidebar::NAV_DEFS.iter().any(|d| d.id == "playlists"));
    }

    /// Opening a playlist's detail page sets `route` + `open_playlist`; `go_back`
    /// returns to the Playlists list. The playlist comes from a real on-disk
    /// store (via `State::new`'s `PlaylistStore::load`), guarded by the shared
    /// per-process test config dir so this doesn't race another config test.
    #[test]
    fn opening_a_playlist_sets_open_playlist_and_back_returns() {
        let _guard = config::use_test_config_dir();
        let mut store = playlists::PlaylistStore::load();
        let id = store.create("P");
        let mut s = State::new(PlayerHandle::offline());
        s.route = Route::Playlists; // as if opened from the Playlists list
        navigate_to(&mut s, Route::Playlist, None, None, None, Some(id));
        assert!(matches!(s.route, Route::Playlist));
        assert_eq!(s.open_playlist, Some(id));
        go_back(&mut s);
        assert!(matches!(s.route, Route::Playlists));
    }

    /// Partially-typed New-playlist text does not survive leaving the page
    /// (via navigate_to, go_back, or a sidebar tab).
    #[test]
    fn new_playlist_field_text_is_cleared_on_navigate_away() {
        let _guard = config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        s.route = Route::Playlists;
        s.playlist_new_buf = rune::TextEdit::new("half typed");
        s.playlist_search = rune::TextEdit::new("road");
        s.playlists_adding = true;
        navigate_to(&mut s, Route::Albums, None, None, None, None);
        assert_eq!(s.playlist_new_buf.text, "");
        assert_eq!(s.playlist_search.text, "", "the grid search clears too");
        assert!(!s.playlists_adding, "the New-playlist toggle resets");
        s.playlist_new_buf = rune::TextEdit::new("again");
        s.playlist_search = rune::TextEdit::new("focus");
        go_back(&mut s);
        assert_eq!(s.playlist_new_buf.text, "");
        assert_eq!(s.playlist_search.text, "");
    }

    /// The shared browse-page search is BOUNDED, not full-width: on a wide
    /// window the search box caps at `SEARCH_MAX_W` rather than stretching edge
    /// to edge (the user-reported "search shouldn't be full width"). Checked on
    /// two pages that route through the shared `page_search_bar`.
    #[test]
    fn browse_search_is_bounded_not_full_width() {
        let _locale = config::use_test_locale("en");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let cap = views::library::SEARCH_MAX_W;
        for (route, bar_key, box_key) in [
            (Route::Albums, "albums-search-bar", "albums-search-box"),
            (Route::Artists, "artists-search-bar", "artists-search-box"),
        ] {
            let mut s = populated_state();
            s.route = route;
            // A wide window so an unbounded search would stretch far past the cap.
            let mut rt = Runtime::new(s, view, (1280.0, 800.0), env);
            rt.frame(0.0);
            assert!(rt.find(bar_key).is_some(), "{bar_key} renders");
            let w = el_info(&rt, box_key).rect[2];
            assert!(
                w <= cap + 1.0,
                "{box_key} width {w} should be bounded to <= {cap} on a wide window"
            );
            assert!(
                w > 200.0,
                "{box_key} width {w} should still grow to a usable width"
            );
        }
    }

    /// The Albums/Artists grid search boxes are ephemeral, same convention as
    /// `genre_search`/`playlist_new_buf`: partially-typed text in either one
    /// does not survive leaving the page, whether via `navigate_to`,
    /// `go_back`, or a sidebar tab switch.
    #[test]
    fn albums_and_artists_search_are_cleared_on_navigate_away() {
        let _guard = config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        s.route = Route::Albums;
        s.album_search = rune::TextEdit::new("crooked");
        s.artist_search = rune::TextEdit::new("pavement");
        navigate_to(&mut s, Route::Artists, None, None, None, None);
        assert_eq!(s.album_search.text, "");
        assert_eq!(s.artist_search.text, "");

        s.album_search = rune::TextEdit::new("again");
        s.artist_search = rune::TextEdit::new("again");
        go_back(&mut s);
        assert_eq!(s.album_search.text, "");
        assert_eq!(s.artist_search.text, "");

        // A sidebar tab switch clears both too (the same nav-item `on_press`
        // that already clears `genre_search`/`playlist_new_buf`).
        s.album_search = rune::TextEdit::new("once more");
        s.artist_search = rune::TextEdit::new("once more");
        s.sidebar_collapsed = false;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "nav-genres");
        assert_eq!(rt.state.album_search.text, "");
        assert_eq!(rt.state.artist_search.text, "");
    }

    /// With more than five tracks, Show more expands the Top-Songs list.
    #[test]
    fn show_more_expands_top_songs() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = (1..=7)
            .map(|i| artist_album_track(i, "Bach", "Cello Suites"))
            .collect();
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Only the first five tracks show; the sixth row is hidden.
        assert!(rt.find("album-track-4").is_some(), "the fifth track shows");
        assert!(rt.find("album-track-5").is_none(), "the sixth is collapsed");
        assert!(
            rt.find("artist-top-toggle").is_some(),
            "a Show more toggle renders"
        );
        click(&mut rt, "artist-top-toggle");
        assert!(rt.state.artist_top_expanded, "the toggle expands Top Songs");
        assert!(
            rt.find("album-track-5").is_some(),
            "the sixth track shows once expanded"
        );
    }

    fn artist_album_track(id: u32, artist: &str, album: &str) -> soul_player_host::TrackView {
        soul_player_host::TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
        }
    }

    /// The sidebar offers a Genres nav entry, and pressing it switches the route
    /// to the Genres page.
    #[test]
    fn the_sidebar_offers_a_genres_nav_item_that_switches_route() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("nav-genres").is_some(),
            "the Genres nav item renders"
        );
        click(&mut rt, "nav-genres");
        assert!(
            matches!(rt.state.route, Route::Genres),
            "pressing the nav item switches to the Genres page"
        );
    }

    /// Clicking a genre tile opens that genre's detail page (and issues no
    /// playback), the library grouped by genre, first-seen order.
    #[test]
    fn clicking_a_genre_opens_the_detail_page() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![genre_track(1, "Rock"), genre_track(2, "Jazz")];
        s.route = Route::Genres;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        click(&mut rt, "genre-tile-0"); // the first grouped genre (Rock)
        assert!(
            matches!(rt.state.route, Route::Genre),
            "the row opens the genre detail page"
        );
        assert_eq!(rt.state.open_genre.as_deref(), Some("Rock"));
        assert!(
            rt.state.host.recorded_commands().is_empty(),
            "opening a genre does not start playback"
        );
    }

    /// The genre detail page lists the genre's tracks, with a Play All that plays
    /// them from the top and a back link.
    #[test]
    fn the_genre_detail_lists_tracks_with_play_all_and_back() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            genre_track(1, "Rock"),
            genre_track(2, "Rock"),
            genre_track(3, "Jazz"),
        ];
        s.route = Route::Genre;
        s.open_genre = Some("Rock".into());
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("genre-header").is_some(),
            "the detail header renders"
        );
        assert!(rt.find("detail-back").is_some(), "a back link renders");
        assert!(
            rt.find("album-track-0").is_some(),
            "the genre's tracks are listed"
        );
        click(&mut rt, "genre-play-all");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 0
            }],
            "Play All plays only this genre's tracks, from the top"
        );
    }

    #[test]
    fn genre_in_page_search_filters_the_track_list() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        // Two tracks with distinct titles in the one genre.
        s.library = vec![
            soul_player_host::TrackView {
                id: 1,
                title: Some("Blue".into()),
                artist: Some("A".into()),
                album: Some("Alb".into()),
                genre: Some("Jazz".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
            soul_player_host::TrackView {
                id: 2,
                title: Some("Green".into()),
                artist: Some("A".into()),
                album: Some("Alb".into()),
                genre: Some("Jazz".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
        ];
        s.route = Route::Genre;
        s.open_genre = Some("Jazz".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("genre-search").is_some(), "the search box renders");
        assert!(
            rt.find("album-track-0").is_some() && rt.find("album-track-1").is_some(),
            "both tracks show"
        );
        // Type a query that matches only "Blue".
        rt.update(|st: &mut State, _tx: &mut Tx| st.genre_search = rune::TextEdit::new("blue"));
        rt.frame(0.0);
        assert!(rt.find("album-track-0").is_some(), "the match stays");
        assert!(
            rt.find("album-track-1").is_none(),
            "the non-match is filtered out"
        );
    }

    // ── Configurable navigation + Home ───────────────────────────────────────

    #[test]
    fn the_sidebar_offers_a_home_nav_item_by_default() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.nav_hidden = Vec::new();
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("nav-home").is_some(),
            "Home is a nav item, shown by default"
        );
    }

    /// At rest a hidden nav item is fully collapsed away (reveal progress 0) — not
    /// rendered at all; shown items remain.
    #[test]
    fn a_hidden_nav_item_is_absent_at_rest() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.nav_hidden = vec!["artists".into(), "home".into()];
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        // A hidden item stays mounted (so toggling it never runs Rune's removal
        // animation) but collapsed to 0×0 and fully transparent, so nothing of it
        // shows. It is still in the element tree, so test visibility by opacity.
        let visible = |rt: &Runtime<State>, key: &str| {
            rt.elements()
                .iter()
                .any(|e| e.key.as_deref() == Some(key) && e.opacity > 0.01)
        };
        assert!(!visible(&rt, "nav-artists"), "a hidden item is invisible");
        assert!(!visible(&rt, "nav-home"));
        assert!(visible(&rt, "nav-albums"), "shown items remain");
        assert!(visible(&rt, "nav-library"));
    }

    #[test]
    fn toggling_navigation_in_settings_hides_the_item_and_persists() {
        // Redirect config to a scratch dir so the real user config is untouched.
        let _config_guard = config::use_test_config_dir();

        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.route = Route::Settings;
        // Start from a known nav state — the shared test config may carry a leaked
        // `nav_hidden` from another test (we assert the default, then the toggle).
        s.nav_hidden.clear();
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        let visible = |rt: &Runtime<State>, key: &str| {
            rt.elements()
                .iter()
                .any(|e| e.key.as_deref() == Some(key) && e.opacity > 0.01)
        };
        assert!(rt.state.nav_visible("albums"), "albums shown by default");
        assert!(visible(&rt, "nav-albums"));
        click(&mut rt, "nav-toggle-albums");
        assert!(!rt.state.nav_visible("albums"), "the toggle hides albums");
        assert!(
            config::load_nav_hidden().iter().any(|h| h == "albums"),
            "the choice persists to (the test) config"
        );
        // Once hidden, the row collapses and fades to transparent (the accessor
        // reports 0 for a hidden item, so it reads invisible even without the poll).
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
        }
        assert!(
            !visible(&rt, "nav-albums"),
            "the hidden item is no longer drawn once the reveal finishes"
        );
    }

    /// Hiding a nav item plays a *sequential* reveal driven by the poll: first the
    /// label fades while the row keeps full height (nothing below reflows), THEN —
    /// once it's invisible — the height collapses, and finally the row leaves.
    #[test]
    fn hiding_a_nav_item_fades_then_collapses_then_leaves() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.nav_hidden.clear();
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        // The reveal is driven by the app poll (it advances `nav_reveal` each frame);
        // `Runtime::new` doesn't wire it, so attach it here to exercise the real,
        // frame-by-frame fade+collapse rather than an instant visibility flip.
        rt.poll = Some(spectrum_poll);
        rt.frame(0.0);
        rt.settle_all();
        rt.frame(0.0);
        // The row stays mounted throughout (stable structure), so read its height and
        // opacity straight off `nav-albums`: full height + opacity while fading, then
        // 0 height + transparent once collapsed. The queue below is the witness that
        // the collapsed space actually closes without overlapping it.
        let albums = |rt: &Runtime<State>| {
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some("nav-albums"))
                .map(|e| (e.rect[3], e.opacity))
        };
        let queue_y = |rt: &Runtime<State>| {
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some("queue"))
                .map(|e| e.rect[1])
                .expect("the queue section renders")
        };
        let (open_h, _) = albums(&rt).expect("albums starts open");
        assert!(open_h > 20.0, "open to full height, got {open_h}");
        let y_open = queue_y(&rt);

        rt.update(|s, _tx| s.nav_hidden = vec!["albums".into()]);
        // Phase 1 (a couple frames in): the label is FADING — opacity down, but the
        // row keeps its full height, so the queue has barely moved (the collapse
        // that would pull it up hasn't started — the fade goes first).
        rt.frame(1.0 / 60.0);
        rt.frame(1.0 / 60.0);
        let (h, op) = albums(&rt).expect("still present while fading");
        assert!(op < 1.0, "label is fading, opacity {op}");
        assert!(
            (h - open_h).abs() < 1.0,
            "row keeps full height while fading, got {h} (open {open_h})"
        );
        assert!(
            (y_open - queue_y(&rt)).abs() < views::sidebar::NAV_ITEM_H * 0.5,
            "queue hasn't yet glided up a full row — the fade precedes the collapse"
        );
        // Phases 2+3: let it finish — the label leaves, and only now has the queue
        // glided up a whole row into the collapsed space.
        for _ in 0..60 {
            rt.frame(1.0 / 60.0);
        }
        let (_, end_op) = albums(&rt).expect("row stays mounted (stable structure)");
        assert!(
            end_op < 0.01,
            "fully hidden → transparent, opacity {end_op}"
        );
        // The collapse lives on the reveal wrapper (the button keeps its intrinsic
        // height, clipped); the wrapper is what drives to 0.
        let reveal_h = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("nav-albums-reveal"))
            .map(|e| e.rect[3])
            .expect("reveal wrapper stays mounted");
        assert!(
            reveal_h < 2.0,
            "fully hidden → collapsed, wrapper height {reveal_h}"
        );
        assert!(
            queue_y(&rt) < y_open - views::sidebar::NAV_ITEM_H * 0.5,
            "queue glided up into the collapsed space ({} → {})",
            y_open,
            queue_y(&rt)
        );
    }

    #[test]
    fn the_home_page_previews_albums_plays_all_and_opens_one() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.route = Route::Home;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(rt.find("home-play-all").is_some(), "Home offers Play all");
        assert!(
            rt.find("home-album-0").is_some(),
            "Home previews the library's albums"
        );
        click(&mut rt, "home-play-all");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 0
            }],
            "Play all plays the whole library from the top"
        );
        click(&mut rt, "home-album-0");
        assert!(
            matches!(rt.state.route, Route::Album),
            "an album tile opens its detail page"
        );
        assert_eq!(rt.state.open_album.as_deref(), Some("Alb"));
    }

    /// Clicking the card body (its title/artist area, not the play button) opens
    /// the album's detail page and issues no playback command — the old app's
    /// MediaCard navigate-on-body-click.
    #[test]
    fn clicking_the_album_card_body_opens_the_detail_page() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.route = Route::Albums;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "card-text"); // the title/artist area — not the play button
        assert!(
            matches!(rt.state.route, Route::Album),
            "the card body opens the album detail page"
        );
        assert_eq!(rt.state.open_album.as_deref(), Some("Alb"));
        assert!(
            rt.state.host.recorded_commands().is_empty(),
            "navigating does not start playback"
        );
    }

    /// Clicking the small play button plays the album and does NOT navigate — the
    /// counterpart to the body click.
    #[test]
    fn clicking_the_card_play_button_plays_without_navigating() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.route = Route::Albums;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "play-btn");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 0
            }],
            "the play button plays the album"
        );
        assert!(
            matches!(rt.state.route, Route::Albums),
            "the play button does not navigate"
        );
    }

    /// The album detail page lists the album's tracks (click plays from that
    /// position), a Play button, and a back link to the grid.
    #[test]
    fn the_album_detail_page_lists_tracks_with_play_and_back() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.route = Route::Album;
        s.open_album = Some("Alb".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("album-header").is_some(),
            "the detail header renders"
        );
        assert!(rt.find("album-play-all").is_some(), "a Play-all button");
        assert!(rt.find("detail-back").is_some(), "a back link");
        assert!(
            rt.find("album-track-0").is_some() && rt.find("album-track-1").is_some(),
            "both album tracks are listed"
        );
        click(&mut rt, "album-track-1");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 1
            }],
            "clicking a track plays the album from that position"
        );
        click(&mut rt, "detail-back");
        assert!(
            matches!(rt.state.route, Route::Albums),
            "back returns to the albums grid"
        );
    }

    /// The album detail page's track rows share `albums::detail_track_row`
    /// with the main track list, so a track carrying a remote source
    /// (`TrackView::remote_source == Some(host)`) must show the SAME subtle
    /// cloud badge and hover tooltip ([`El::help`]) there too, not just in
    /// the flat library view and the now-playing line; a local track in the
    /// same album shows none.
    #[test]
    fn the_album_detail_page_shows_the_remote_badge_for_a_remote_track() {
        let _locale = config::use_test_locale("en");
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.library = vec![
            soul_player_host::TrackView {
                id: 1,
                title: Some("Streamed".into()),
                artist: Some("A".into()),
                album: Some("Alb".into()),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: Some("listen.home.lan".into()),
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
            soul_player_host::TrackView {
                id: 2,
                title: Some("OnDisk".into()),
                artist: Some("A".into()),
                album: Some("Alb".into()),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
        ];
        s.route = Route::Album;
        s.open_album = Some("Alb".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let badge_visible = |rt: &Runtime<State>, key: &str| {
            rt.elements()
                .iter()
                .any(|e| e.key.as_deref() == Some(key) && e.opacity > 0.01)
        };
        assert!(
            badge_visible(&rt, "track-remote-1"),
            "a remote track's album-detail row should show the remote badge"
        );
        assert!(
            !badge_visible(&rt, "track-remote-2"),
            "a local track's album-detail row should not show a remote badge"
        );
        let help = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("track-remote-1"))
            .and_then(|e| e.help);
        assert_eq!(
            help.as_deref(),
            Some("Streams from listen.home.lan"),
            "the album-detail remote badge's tooltip should name the source host"
        );
    }

    /// The album detail page's "Play All" pill plays the album from the top.
    #[test]
    fn the_album_detail_play_all_plays_from_the_top() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.route = Route::Album;
        s.open_album = Some("Alb".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "album-play-all");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 0
            }],
            "Play All plays the album from the top"
        );
    }

    /// The album detail header's Play-All pill plays the album from the top.
    #[test]
    fn the_album_detail_play_all_plays_the_album() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.route = Route::Album;
        s.open_album = Some("Alb".into());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("album-header").is_some(), "the shared hero renders");
        assert!(rt.find("album-cover").is_some(), "the hero cover renders");
        click(&mut rt, "album-play-all");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 0
            }],
            "Play All plays the album from the top"
        );
    }

    /// Clicking the row of the track that is already playing toggles pause/resume
    /// (the old app's TrackList current-row control), rather than restarting it.
    #[test]
    fn clicking_the_now_playing_track_row_toggles_pause() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.now_playing_id = Some(1);
        s.playing = true;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "track-1"); // the current track's row
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::TogglePlay],
            "re-clicking the playing row pauses, not restarts"
        );
    }

    /// Clicking a different track's row plays the library from that track.
    #[test]
    fn clicking_a_non_playing_track_row_plays_it() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.now_playing_id = Some(1);
        s.playing = true;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "track-2"); // not the current track
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext {
                ids: vec![1, 2],
                start: 1
            }],
            "clicking another row plays the library from that track"
        );
    }

    /// The speaker icon sends `ToggleMute`; the backend owns the mute state and
    /// the pre-mute level (the mute *rules* are unit-tested in the backend's
    /// `VolumeState`). The UI just dispatches the command and reflects `Tick.muted`.
    #[test]
    fn the_speaker_icon_sends_toggle_mute() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "volume-mute");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::ToggleMute],
            "the speaker icon toggles mute via the backend"
        );
    }

    /// While muted the volume bar reads 0 (the old app's `displayVolume`), even
    /// though the backend preserves the real level. Measured off the volume
    /// slider's own thumb position (now uniquely keyed `volume-knob`).
    #[test]
    fn a_muted_volume_bar_reads_zero_while_an_unmuted_one_reads_the_level() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let knob_fraction = |muted: bool| -> f32 {
            let mut s = State::new(PlayerHandle::recording());
            s.sidebar_collapsed = false;
            s.volume = 0.8;
            s.muted = muted;
            let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
            for _ in 0..3 {
                rt.frame(0.0); // let the slider read back its own laid-out width
            }
            let ([kx, _, _, _], _) = rt.presented("volume-knob").expect("volume knob");
            let ([tx, _, tw, _], _) = rt.presented("volume-track").expect("volume track");
            (kx - tx) / tw
        };
        assert!(
            knob_fraction(true) < 0.15,
            "a muted volume bar reads ~0 (got {:.2})",
            knob_fraction(true)
        );
        assert!(
            knob_fraction(false) > 0.6,
            "an unmuted 0.8 bar reads high (got {:.2})",
            knob_fraction(false)
        );
    }

    /// Clicking the now-playing title/artist opens that track's album detail page
    /// (the old app's TrackInfo navigates to the album).
    #[test]
    fn clicking_the_now_playing_track_opens_its_album() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")]; // album "Alb"
        s.now_playing_id = Some(1);
        s.playing = true;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click(&mut rt, "now-playing-text");
        assert!(
            matches!(rt.state.route, Route::Album),
            "the now-playing title opens its album"
        );
        assert_eq!(rt.state.open_album.as_deref(), Some("Alb"));
    }

    /// The now-playing title line shows the remote badge when the current track
    /// streams from a remote source (`TrackView::remote_source`) — the one spot
    /// outside the track lists where the badge cleanly fits, since
    /// `now_playing_row` already carries the real `TrackView` the Inc-3 marker
    /// lives on. The badge's hover tooltip ([`El::help`]) names that source.
    #[test]
    fn now_playing_shows_the_remote_badge_for_a_remote_track() {
        let _locale = config::use_test_locale("en");
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.library = vec![soul_player_host::TrackView {
            id: 1,
            title: Some("Streamed".into()),
            artist: Some("A".into()),
            album: None,
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: Some("listen.home.lan".into()),
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
        }];
        s.now_playing_id = Some(1);
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let badge = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("track-remote-1") && e.opacity > 0.01);
        assert!(
            badge.is_some(),
            "the now-playing line should show the remote badge for a remote track"
        );
        assert_eq!(
            badge.unwrap().help.as_deref(),
            Some("Streams from listen.home.lan"),
            "the badge's hover tooltip should name the remote source"
        );
    }

    #[test]
    fn settings_shows_updates_section_without_install_by_default() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::About;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("updates").is_some(),
            "Settings has an About/Updates section"
        );
        assert!(
            rt.find("install-update").is_none(),
            "no install button until the launch check flags an update"
        );
    }

    #[test]
    fn settings_offers_install_when_update_available() {
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::About;
        s.update_available = Some("9.9.9".to_string());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("install-update").is_some(),
            "a flagged update offers Install & Restart"
        );
    }

    #[test]
    fn settings_nav_lists_categories_and_defaults_to_appearance() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        for key in ["cat-appearance", "cat-music-data", "cat-audio", "cat-about"] {
            assert!(rt.find(key).is_some(), "settings category rail has {key}");
        }
        // The default category is Appearance (theme + language cards); the Audio
        // category's ReplayGain card isn't rendered until it's selected.
        assert!(
            rt.find("appearance").is_some(),
            "default category shows the Appearance card"
        );
        assert!(
            rt.find("language").is_some(),
            "Appearance also hosts the Language card"
        );
        assert!(
            rt.find("sp-stage-replaygain").is_none(),
            "ReplayGain lives under the Audio category, not the default view"
        );
    }

    #[test]
    fn clicking_settings_categories_switches_the_content() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Default category is Appearance.
        assert!(rt.find("appearance").is_some());
        assert!(rt.find("sp-stage-replaygain").is_none());
        // Click Audio → the Output + ReplayGain cards appear, Appearance hides.
        click_key(&mut rt, "cat-audio");
        assert!(
            rt.find("sp-stage-output").is_some(),
            "Audio shows the Output card"
        );
        assert!(
            rt.find("sp-stage-replaygain").is_some(),
            "Audio shows the ReplayGain card"
        );
        assert!(
            rt.find("appearance").is_none(),
            "the Appearance card is hidden under Audio"
        );
        // Click Music Data → the sources card (keyed "sources").
        click_key(&mut rt, "cat-music-data");
        assert!(
            rt.find("sources").is_some(),
            "Music Data shows the sources card"
        );
        assert!(rt.find("sp-stage-replaygain").is_none());
        // Click About → the version / updater card (keyed "updates").
        click_key(&mut rt, "cat-about");
        assert!(rt.find("updates").is_some(), "About shows the version card");
        assert!(rt.find("sources").is_none());
        // Back to Appearance.
        click_key(&mut rt, "cat-appearance");
        assert!(rt.find("appearance").is_some(), "back to Appearance");
    }

    #[test]
    fn settings_controls_apply_their_effect_on_click() {
        // These clicks persist (theme/ReplayGain `save_*`) — isolate config to a
        // temp dir so the test never clobbers the user's real settings.
        let _config_guard = config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        // Appearance (the default tab): clicking a theme updates the preference.
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert_eq!(rt.state.settings_tab, SettingsTab::Appearance);
        click_key(&mut rt, "theme-dark");
        assert_eq!(rt.state.theme_pref, ThemePref::Dark);
        click_key(&mut rt, "theme-light");
        assert_eq!(rt.state.theme_pref, ThemePref::Light);
        // Don't assert the INITIAL loaded album_year here — it reads the shared
        // per-process test config file, which a concurrently-running config test
        // may have just written (see `config::use_test_config_dir`'s doc). Only
        // assert the click-driven transition, like the theme buttons above.
        click_key(&mut rt, "album-year-release");
        assert_eq!(rt.state.album_year, config::AlbumYearSource::Release);
        click_key(&mut rt, "album-year-original");
        assert_eq!(rt.state.album_year, config::AlbumYearSource::Original);

        // Audio controls: start on the Audio tab so the ReplayGain buttons are
        // laid out, then clicking a mode flows through to the state.
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::Audio;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "rg-track");
        assert_eq!(rt.state.replaygain_mode, ReplayGainMode::Track);
        click_key(&mut rt, "rg-album");
        assert_eq!(rt.state.replaygain_mode, ReplayGainMode::Album);
        click_key(&mut rt, "rg-off");
        assert_eq!(rt.state.replaygain_mode, ReplayGainMode::Off);
    }

    #[test]
    fn library_shows_the_sort_bar_when_populated() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(State::new(host), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("sort-bar").is_some(),
            "populated library shows a sort bar"
        );
        for key in ["sort-artist", "sort-album", "sort-title", "sort-folder"] {
            assert!(rt.find(key).is_some(), "sort option {key} present");
        }
    }

    /// The sort bar also offers the two client-side loudness pills (DR, Loudness): they
    /// render, clicking one records the loudness sort, and clicking a baked-axis pill
    /// clears it again (the two are mutually exclusive).
    #[test]
    fn sort_bar_loudness_pills_set_and_clear_the_loudness_sort() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(State::new(host), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("sort-dr").is_some(), "the DR sort pill renders");
        assert!(
            rt.find("sort-loudness").is_some(),
            "the Loudness sort pill renders"
        );

        click_key(&mut rt, "sort-dr");
        assert_eq!(
            rt.state.loudness_sort,
            Some(LoudnessKey::Dr),
            "clicking DR records the loudness sort"
        );

        click_key(&mut rt, "sort-title");
        assert_eq!(
            rt.state.loudness_sort, None,
            "clicking a baked-axis pill clears the loudness sort"
        );
    }

    /// DR is the headline loudness indicator and renders at every width; Integrated LUFS
    /// is secondary and joins only on a wide pane (where there is room beside artist/album).
    /// This matches the design and guards the overflow regression that first appeared when
    /// BOTH columns were added to every non-compact row.
    #[test]
    fn dr_shows_everywhere_but_lufs_only_on_a_wide_pane() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let populated = || {
            let host = PlayerHandle::offline();
            host.open_folder(&testdata_dir()).expect("scan testdata");
            State::new(host)
        };
        let s = populated();
        let tid = s.library.first().map(|t| t.id).expect("a scanned track");

        // Wide window: the second frame reads the first's committed pane width, so the
        // wide tier is in play and the DR + LUFS cells both render for the first row.
        let mut wide = Runtime::new(s, view, (1400.0, 720.0), env);
        wide.frame(0.0);
        wide.frame(0.0);
        assert!(
            wide.find(&format!("dr-{tid}")).is_some(),
            "the DR column shows on a wide pane"
        );
        assert!(
            wide.find(&format!("lufs-{tid}")).is_some(),
            "the LUFS column shows on a wide pane"
        );

        // Narrow but still a desktop window (>= the 640 mobile breakpoint, so the library
        // renders rather than the mobile menu): the ~440pt pane is the compact tier, where
        // DR still shows (the headline) but LUFS drops so the folded title/artist keep their
        // room. A fresh scan, so read its own first id (scan-assigned ids need not match the
        // wide state's).
        let narrow_state = populated();
        let narrow_tid = narrow_state
            .library
            .first()
            .map(|t| t.id)
            .expect("a scanned track");
        let mut narrow = Runtime::new(narrow_state, view, (760.0, 720.0), env);
        narrow.frame(0.0);
        narrow.frame(0.0);
        assert!(
            narrow.find(&format!("dr-{narrow_tid}")).is_some(),
            "the DR column still shows on a narrow pane"
        );
        assert!(
            narrow.find(&format!("lufs-{narrow_tid}")).is_none(),
            "the LUFS column is hidden on a narrow pane"
        );
    }

    /// The library refreshes itself when background loudness analysis completes: a
    /// `TrackLoudnessAnalyzed` event flags a pending refresh, and the next `Tick`
    /// consumes it (re-projecting the library so the new readouts appear) and clears
    /// the flag. This keeps a full-library sweep down to at most one re-project per
    /// tick instead of per analyzed track.
    #[test]
    fn loudness_analysis_refreshes_the_library_on_the_next_tick() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        assert!(
            !s.loudness_dirty,
            "a fresh state has no pending loudness refresh"
        );

        s.apply(&Event::TrackLoudnessAnalyzed {
            path: std::path::PathBuf::from("somewhere/track.flac"),
            loudness: TrackLoudness {
                integrated_lufs: -14.0,
                lra: 6.0,
                true_peak_dbtp: -1.0,
                plr: 9.0,
                dr: 12.0,
            },
        });
        assert!(
            s.loudness_dirty,
            "a completed analysis flags the library for a refresh"
        );

        s.apply(&Event::Tick(soul_player_host::Tick {
            now_playing_id: None,
            position_secs: 0.0,
            duration_secs: None,
            state: PlayState::Stopped,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume: 1.0,
            muted: false,
            analysis_pending: 0,
        }));
        assert!(!s.loudness_dirty, "the next tick consumes the refresh flag");
    }

    /// Build a track with (or without) a loudness reading `(dr, integrated_lufs)`, for the
    /// client-side loudness-sort tests.
    fn tv_loud(id: u32, metrics: Option<(f32, f32)>) -> soul_player_host::TrackView {
        soul_player_host::TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some("A".into()),
            album: Some("Alb".into()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: metrics.map(|(dr, integrated_lufs)| TrackLoudness {
                integrated_lufs,
                lra: 5.0,
                true_peak_dbtp: -1.0,
                plr: 8.0,
                dr,
            }),
            format: None,
        }
    }

    /// The client-side DR sort orders analyzed tracks by descending DR (most dynamic
    /// first) and keeps un-analyzed tracks in their original order at the end.
    #[test]
    fn sort_by_loudness_orders_dr_desc_with_unanalyzed_last() {
        let mut lib = vec![
            tv_loud(1, Some((6.0, -14.0))),
            tv_loud(2, None),
            tv_loud(3, Some((14.0, -14.0))),
            tv_loud(4, Some((10.0, -14.0))),
            tv_loud(5, None),
        ];
        sort_by_loudness(&mut lib, LoudnessKey::Dr);
        let ids: Vec<u32> = lib.iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            vec![3, 4, 1, 2, 5],
            "analyzed by descending DR, then un-analyzed in original order"
        );
    }

    /// The Loudness sort keys on Integrated LUFS, loudest (highest value, closest to 0)
    /// first; un-analyzed last.
    #[test]
    fn sort_by_loudness_orders_lufs_desc_with_unanalyzed_last() {
        let mut lib = vec![
            tv_loud(1, Some((10.0, -20.0))),
            tv_loud(2, Some((10.0, -9.0))),
            tv_loud(3, None),
            tv_loud(4, Some((10.0, -14.0))),
        ];
        sort_by_loudness(&mut lib, LoudnessKey::Loudness);
        let ids: Vec<u32> = lib.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![2, 4, 1, 3], "loudest first, un-analyzed last");
    }

    /// Picking a loudness sort records it (so the pill lights and the next tick re-applies
    /// it); picking a baked axis sort then clears it.
    #[test]
    fn loudness_and_axis_sorts_are_mutually_exclusive() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        assert_eq!(s.loudness_sort, None, "no loudness sort by default");

        set_loudness_sort(&mut s, LoudnessKey::Dr);
        assert_eq!(
            s.loudness_sort,
            Some(LoudnessKey::Dr),
            "the loudness sort is recorded"
        );

        set_sort(&mut s, BrowseAxis::Title);
        assert_eq!(
            s.loudness_sort, None,
            "an axis sort clears the loudness sort"
        );
        assert_eq!(s.sort_axis, BrowseAxis::Title);
    }

    /// A populated library shows the search bar, and a matching query keeps the
    /// matching track visible.
    #[test]
    fn search_keeps_matching_tracks_and_shows_the_bar() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        assert!(!s.library.is_empty());
        let first = s.library[0].clone();
        let needle = first
            .title
            .clone()
            .or_else(|| first.artist.clone())
            .expect("first track has a title or artist to search for");
        s.search = needle;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("tracks-search-bar").is_some(),
            "the search bar renders"
        );
        assert!(
            rt.find(&format!("track-{}", first.id)).is_some(),
            "the matching track stays visible"
        );
    }

    /// A query matching nothing filters every row out and shows the empty hint.
    #[test]
    fn search_with_no_matches_hides_rows() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        let first_id = s.library[0].id;
        s.search = "zzz-no-such-track-zzz".to_string();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("no-matches").is_some(),
            "a non-matching query shows the empty message"
        );
        assert!(
            rt.find(&format!("track-{first_id}")).is_none(),
            "non-matching rows are filtered out"
        );
    }

    /// Each track row offers an add-to-queue button (`Command::Enqueue`).
    #[test]
    fn track_rows_offer_add_to_queue() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let s = State::new(host);
        let first_id = s.library[0].id;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find(&format!("enqueue-{first_id}")).is_some(),
            "a track row offers an add-to-queue button"
        );
    }

    /// Track rows show a 32 pt cover thumbnail hugging the title — the old app's
    /// `w-8 h-8 rounded-lg` TrackList cover column (ours previously had none).
    #[test]
    fn track_rows_show_a_32pt_cover_thumbnail() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let s = State::new(host);
        let first_id = s.library[0].id;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let ([_, _, w, h], _) = rt
            .presented(&format!("track-cover-{first_id}"))
            .expect("the track row renders a cover thumbnail");
        assert!(
            (w - 32.0).abs() <= 0.5 && (h - 32.0).abs() <= 0.5,
            "the cover is 32x32 pt (old app's w-8 h-8), got {w}x{h}"
        );
    }

    /// A track row for a track carrying a remote source (`TrackView::remote_source
    /// == Some(host)`, the per-track Local/Remote marker) shows the subtle cloud
    /// badge, whose hover tooltip ([`El::help`]) names that host; a local one
    /// (`remote_source == None`, the unremarked default) shows none.
    #[test]
    fn a_remote_track_row_shows_the_remote_badge_a_local_one_does_not() {
        let _locale = config::use_test_locale("en");
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.library = vec![
            soul_player_host::TrackView {
                id: 1,
                title: Some("Streamed".into()),
                artist: Some("A".into()),
                album: Some("Alb".into()),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: Some("listen.home.lan".into()),
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
            soul_player_host::TrackView {
                id: 2,
                title: Some("OnDisk".into()),
                artist: Some("A".into()),
                album: Some("Alb".into()),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            },
        ];
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let badge_visible = |rt: &Runtime<State>, key: &str| {
            rt.elements()
                .iter()
                .any(|e| e.key.as_deref() == Some(key) && e.opacity > 0.01)
        };
        assert!(
            badge_visible(&rt, "track-remote-1"),
            "a remote track's row should show the remote badge"
        );
        assert!(
            !badge_visible(&rt, "track-remote-2"),
            "a local track's row should not show a remote badge"
        );
        let help = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("track-remote-1"))
            .and_then(|e| e.help);
        assert_eq!(
            help.as_deref(),
            Some("Streams from listen.home.lan"),
            "the remote badge's hover tooltip should name the source host"
        );
    }

    /// The Tracks page genuinely reflows with its content-pane width (the old
    /// app's TrackList hides the artist/album columns below `sm`): on a narrow
    /// pane the artist moves under the title into a `track-titleblock` that the
    /// wide layout never builds. It is width-driven via `cx.layout_size`, which
    /// reads the *prior* frame — hence two frames before asserting. The sidebar is
    /// collapsed so the content pane tracks the window width deterministically.
    #[test]
    fn tracks_reflow_to_a_compact_subtitle_layout_when_narrow() {
        let seed = || {
            let host = PlayerHandle::offline();
            host.open_folder(&testdata_dir()).expect("scan testdata");
            let mut s = State::new(host);
            s.sidebar_collapsed = true; // pane ≈ window width, no sidebar variance
            // Show the content pane directly: a narrow (< 640) window is mobile
            // single-pane, where the menu is otherwise home — the reflow we assert
            // is the content pane's, which on mobile spans the whole window.
            s.mobile_show_content = true;
            s
        };
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };

        // Narrow window → compact rows (artist as subtitle).
        let mut rt = Runtime::new(seed(), view, (600.0, 720.0), env);
        rt.frame(0.0); // establish layout
        rt.frame(0.0); // second frame reads last frame's pane width → compact
        assert!(
            rt.find("track-titleblock").is_some(),
            "a narrow pane uses the compact (artist-as-subtitle) track rows"
        );

        // Wide window → full columns, no subtitle block.
        let mut rt = Runtime::new(seed(), view, (1200.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("track-titleblock").is_none(),
            "a wide pane keeps the artist/album columns (no subtitle block)"
        );
    }

    /// A populated queue renders both bands — the manual "Next in queue" (with a
    /// drag grip, remove ✕, and Clear) and the "Next from <context>" remainder.
    #[test]
    fn populated_queue_renders_both_bands_with_controls() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        s.sidebar_collapsed = false; // deterministic — don't depend on persisted config
        let ids: Vec<u32> = s.library.iter().take(5).map(|t| t.id).collect();
        assert!(ids.len() >= 5, "testdata has enough tracks for the bands");
        s.now_playing_id = Some(ids[0]);
        s.up_next = ids[1..3].to_vec(); // two manual rows
        s.context_ids = ids[3..].to_vec(); // context rows
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("queue-next-in-queue").is_some(),
            "the manual band header renders"
        );
        assert!(
            rt.find("queue-next-from").is_some(),
            "the context band header renders"
        );
        assert!(rt.find("clear-queue").is_some(), "Clear is offered");
        // Rows are keyed by position: two manual rows at indices 0,1; two context
        // rows at upcoming slots 3,4 (the 2 manual rows precede them).
        assert!(
            rt.find("queue-manual-0").is_some(),
            "a manual queue row renders"
        );
        assert!(
            rt.find("grip-0").is_some(),
            "the manual row has a drag grip"
        );
        assert!(
            rt.find("remove-0").is_some(),
            "the manual row has a remove control"
        );
        assert!(
            rt.find("queue-context-3").is_some(),
            "a context row renders"
        );
        assert!(
            rt.find("grip-2").is_none(),
            "context rows are not reorderable (only the 2 manual rows have grips)"
        );
    }

    /// A track that appears more than once in the context — e.g. the same
    /// recording in several formats, as the test library has — must still produce
    /// two independently-addressable rows. Queue rows are keyed by position, not
    /// track id, so duplicate ids never collide into one key.
    #[test]
    fn a_repeated_track_in_the_context_yields_distinct_rows() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        s.sidebar_collapsed = false;
        let id = s.library[0].id;
        s.now_playing_id = Some(id);
        s.context_ids = vec![id, id]; // the same track twice
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // No manual band → the two context rows are upcoming slots 1 and 2.
        assert!(
            rt.find("queue-context-1").is_some(),
            "first context row is addressable by its position"
        );
        assert!(
            rt.find("queue-context-2").is_some(),
            "the second (duplicate-id) context row is addressable by its position"
        );
    }

    /// Nothing queued and nothing upcoming shows the empty state, not stale rows.
    #[test]
    fn empty_queue_shows_the_empty_state() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("queue-empty").is_some(),
            "an empty queue shows the empty state"
        );
    }

    /// A `State` with a populated queue backed by a RECORDING host, so the
    /// `Command` a click/drag dispatches can be read back with
    /// `host.recorded_commands()`. The library is empty — rows render as "Unknown"
    /// but stay clickable, and the command carries the row index, not the title.
    fn recording_queue_state(up_next: &[u32], context_ids: &[u32]) -> State {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.up_next = up_next.to_vec();
        s.context_ids = context_ids.to_vec();
        s
    }

    /// Click (press + release, no travel) the element keyed `key`, then advance a
    /// frame so its `on_press` dispatches.
    fn click(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt
            .find(key)
            .unwrap_or_else(|| panic!("element {key} not found"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    // ── Mobile (narrow-window) single-pane layout ────────────────────────────
    // The original's `useCollapsibleSidebar`: under a 640 px window it is
    // `isMobile` — the sidebar is the full-width "menu" home view, a nav tap
    // flips to full-screen content (`mobileShowContent`), and a back bar returns.
    // Collapse and the resize handle are desktop-only. See the original's
    // `MainLayout` / `LeftSidebar` / `NavBar`.

    /// Boot `view` at a logical window size, settling two frames so the root's
    /// committed width drives the mobile branch (the first frame reads no prior
    /// layout and falls back to desktop).
    fn booted(w: f32, h: f32, mutate: impl FnOnce(&mut State)) -> Runtime<State> {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        // Pin the nav visible explicitly — `State::new` loads `nav_hidden` from
        // the (process-shared) test config, which another test's toggle may have
        // written, so don't depend on it here.
        s.nav_hidden = Vec::new();
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        mutate(&mut s);
        let mut rt = Runtime::new(
            s,
            view,
            (w, h),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        rt.frame(0.0);
        rt
    }

    #[test]
    fn a_narrow_window_shows_only_the_sidebar_menu() {
        let rt = booted(500.0, 720.0, |_| {});
        assert!(
            rt.find("sidebar").is_some(),
            "the menu (sidebar) is the mobile home view"
        );
        assert!(
            rt.find("content").is_none(),
            "the content pane is hidden until a nav tap"
        );
        assert!(
            rt.find("sidebar-resize").is_none(),
            "no resize handle on mobile"
        );
        assert!(rt.find("grabber").is_none(), "no collapse rail on mobile");
    }

    #[test]
    fn a_mobile_nav_tap_switches_to_full_width_content() {
        let mut rt = booted(500.0, 720.0, |_| {});
        click(&mut rt, "nav-albums");
        rt.frame(0.0);
        assert!(
            rt.state.mobile_show_content,
            "a nav tap reveals the content"
        );
        assert!(matches!(rt.state.route, Route::Albums), "and routes to it");
        assert!(
            rt.find("sidebar").is_none(),
            "the sidebar yields to the content"
        );
        let content = rt.presented("content").expect("content renders").0;
        assert!(
            content[2] > 480.0,
            "content fills the window width, got {}",
            content[2]
        );
    }

    #[test]
    fn the_mobile_back_bar_returns_to_the_menu() {
        let mut rt = booted(500.0, 720.0, |s| {
            s.route = Route::Albums;
            s.mobile_show_content = true;
        });
        assert!(
            rt.find("mobile-back").is_some(),
            "the content view offers a back control"
        );
        click(&mut rt, "mobile-back");
        rt.frame(0.0);
        assert!(!rt.state.mobile_show_content, "back returns to the menu");
        assert!(
            rt.find("sidebar").is_some(),
            "the sidebar menu is shown again"
        );
    }

    /// The sidebar "Up Next" queue rows must left-align their covers with the
    /// now-playing panel's cover (both at the 16pt pane inset), not the old
    /// grip-pushed ~36pt. Mirrors the original's `QueueSection` (`px-4` list) +
    /// `NowPlayingPanel` (`p-4`) sharing one left edge.
    #[test]
    fn queue_row_covers_left_align_with_the_now_playing_cover() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            album_track(1, "A"),
            album_track(2, "B"),
            album_track(3, "C"),
            album_track(4, "D"),
        ];
        s.now_playing_id = Some(1);
        s.playing = true;
        s.up_next = vec![2];
        s.context_ids = vec![3, 4];
        s.context_len = 4;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        let np = rt
            .presented("np-cover")
            .expect("now-playing cover renders")
            .0;
        let man = rt
            .presented("qcover-m-0")
            .expect("manual queue cover renders")
            .0;
        // First context row: k = manual_len(1) + 0 + 1 = 2.
        let ctx = rt
            .presented("qcover-c-2")
            .expect("context queue cover renders")
            .0;
        assert!(
            (np[0] - man[0]).abs() < 0.5,
            "manual queue cover (x={}) left-aligns with the now-playing cover (x={})",
            man[0],
            np[0]
        );
        assert!(
            (np[0] - ctx[0]).abs() < 0.5,
            "context queue cover (x={}) left-aligns with the now-playing cover (x={})",
            ctx[0],
            np[0]
        );
    }

    /// The hover highlight is an inset rounded "pill" on the row body, not an
    /// edge-to-edge bar, and the drag grip stays hidden until the row is hovered
    /// — so the resting queue reads as a clean, aligned list.
    #[test]
    fn a_resting_queue_row_hides_its_grip_and_the_pill_is_inset() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            album_track(1, "A"),
            album_track(2, "B"),
            album_track(3, "C"),
        ];
        s.up_next = vec![1];
        s.context_ids = vec![2, 3];
        s.context_len = 3;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        rt.frame(0.0);
        // The grip is revealed only on hover: hidden (opacity ~0) at rest.
        let (_, grip_rest) = rt.presented("grip-0").expect("grip renders");
        assert!(
            grip_rest < 0.05,
            "the drag grip is hidden at rest, got opacity {grip_rest}"
        );
        // The hover pill (row body) is inset on BOTH sides and starts a small
        // padding (8 pt) to the LEFT of the 16 pt cover — so the highlight has
        // breathing room — rather than spanning the full width or hugging the
        // cover's edge.
        let body = rt
            .presented("queue-manual-body-0")
            .expect("row body renders")
            .0;
        let sidebar = rt.presented("sidebar").expect("sidebar renders").0;
        assert!(
            (body[0] - 8.0).abs() < 0.5,
            "the pill starts 8 pt in (left of the 16 pt cover), got {}",
            body[0]
        );
        assert!(
            body[0] + body[2] <= sidebar[0] + sidebar[2] - 4.0,
            "the pill is inset from the sidebar's right edge (body ends at {}, sidebar at {})",
            body[0] + body[2],
            sidebar[0] + sidebar[2]
        );
    }

    // ── Keyboard & accessibility ─────────────────────────────────────────────

    /// The key of the currently-focused element, from the a11y element tree.
    fn focused_key(rt: &Runtime<State>) -> Option<String> {
        rt.elements()
            .into_iter()
            .find(|e| e.focused)
            .and_then(|e| e.key)
    }

    /// Find the rendered element keyed `key`.
    fn el_info(rt: &Runtime<State>, key: &str) -> rune::ElementInfo {
        rt.elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("no element keyed {key}"))
    }

    /// Press a key and advance a frame (keys dispatch on the next frame).
    fn key(rt: &mut Runtime<State>, k: Key) {
        rt.key(KeyEvent {
            key: k,
            mods: Mods::default(),
        });
        rt.frame(0.0);
    }

    /// How many album cards sit in the top row (same y as the highest card) — the
    /// grid's current column count, read from the drawn card centres.
    fn first_row_count(rt: &Runtime<State>, n: u32) -> usize {
        let pts: Vec<(f32, f32)> = (0..n)
            .filter_map(|i| rt.find(&format!("album-{i}")))
            .collect();
        let Some(top) = pts.iter().map(|p| p.1).reduce(f32::min) else {
            return 0;
        };
        pts.iter().filter(|p| (p.1 - top).abs() < 5.0).count()
    }

    /// End-to-end: a Ctrl/⌘ chord driven through the real `Runtime::key` dispatch
    /// (not a direct `handle_key` call) reaches the zoom handler and zooms. This is
    /// the live key path; the unit test above can only prove the handler's logic.
    #[test]
    fn e2e_ctrl_chord_zooms_reflows_and_resets_via_the_runtime_key_path() {
        let _g = config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        s.ui_zoom = 1.0;
        s.route = Route::Albums;
        s.sidebar_collapsed = false;
        // 18 distinct albums so the grid is multi-column and can reflow.
        s.library = (1..=18u32)
            .map(|a| soul_player_host::TrackView {
                id: a,
                title: Some(format!("T{a}")),
                artist: Some("A".into()),
                album: Some(format!("Album {a:02}")),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            })
            .collect();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        // `Runtime::new` doesn't wire `AppSpec::on_key`; the live app does. Set it so
        // the chord routes exactly as it does in production.
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        rt.frame(0.0);
        let base_cols = first_row_count(&rt, 18);
        assert!(
            base_cols >= 3,
            "a wide window starts multi-column, got {base_cols}"
        );

        // Drive the REAL key path (Ctrl/⌘ chords): five `=` steps → 150 %.
        let chord = |rt: &mut Runtime<State>, c: char| {
            rt.key(KeyEvent {
                key: Key::Command(c),
                mods: Mods {
                    cmd: true,
                    ctrl: !cfg!(target_os = "macos"),
                    ..Mods::default()
                },
            });
            rt.frame(0.0);
        };
        for _ in 0..5 {
            chord(&mut rt, '=');
        }
        rt.frame(0.0);
        assert!(
            (rt.state.ui_zoom - 1.5).abs() < 1e-4,
            "five Ctrl+= steps zoom to 150 %, got {}",
            rt.state.ui_zoom
        );
        assert!(
            rt.find("content-zoom").is_some(),
            "the zoom wrapper is active"
        );
        let zoomed_cols = first_row_count(&rt, 18);
        assert!(
            zoomed_cols < base_cols,
            "zooming reflows to fewer columns: {zoomed_cols} vs {base_cols}"
        );

        // Ctrl+0 resets the zoom and restores the original column count.
        chord(&mut rt, '0');
        rt.frame(0.0);
        assert_eq!(rt.state.ui_zoom, 1.0, "Ctrl+0 resets");
        assert!(rt.find("content-zoom").is_none(), "no wrapper at 100 %");
        assert_eq!(
            first_row_count(&rt, 18),
            base_cols,
            "the column count is restored"
        );
    }

    #[test]
    fn album_grid_rows_are_not_looser_than_columns() {
        // Regression: the card used to carry bottom padding that stacked on the
        // grid's row gap, so the space between rows read as larger than between
        // columns. The row gap must now be ≤ the column gap.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.route = Route::Albums;
        s.library = (1..=18u32)
            .map(|a| soul_player_host::TrackView {
                id: a,
                title: Some(format!("T{a}")),
                artist: Some("A".into()),
                album: Some(format!("Album {a:02}")),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            })
            .collect();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        let rect = |k: &str| rt.presented(k).map(|r| r.0);
        let cols = first_row_count(&rt, 18);
        assert!(
            cols >= 2,
            "need multiple columns to compare gaps, got {cols}"
        );
        let a = rect("album-0").expect("first card present");
        let b = rect("album-1").expect("second card present");
        let below = rect(&format!("album-{cols}")).expect("first card of row 2 present");
        // [x, y, w, h]: column gap between cards 0 and 1; row gap between card 0 and
        // the card directly beneath it (the first of the next row).
        let col_gap = b[0] - (a[0] + a[2]);
        let row_gap = below[1] - (a[1] + a[3]);
        assert!(row_gap > 0.0, "rows are separated (got {row_gap})");
        assert!(
            row_gap <= col_gap + 1.0,
            "row gap ({row_gap:.1}) must not exceed column gap ({col_gap:.1})"
        );
    }

    #[test]
    fn zoom_step_snaps_to_the_grid_and_clamps() {
        assert!((stepped_zoom(1.0, 1) - 1.1).abs() < 1e-4, "one step in");
        assert!(
            (stepped_zoom(1.5, -1) - 1.4).abs() < 1e-4,
            "one step out, above the floor"
        );
        assert_eq!(
            stepped_zoom(1.0, -1),
            MIN_ZOOM,
            "Ctrl- at 100 % holds at the floor (zoom-in only)"
        );
        assert_eq!(stepped_zoom(2.0, 1), MAX_ZOOM, "clamps at the ceiling");
        assert_eq!(
            stepped_zoom(0.5, -1),
            MIN_ZOOM,
            "a below-floor value clamps up"
        );
        assert_eq!(stepped_zoom(1.95, 1), MAX_ZOOM, "a near-max step clamps");
        assert_eq!(sane_zoom(f32::NAN), 1.0, "non-finite → 100 %");
        assert_eq!(sane_zoom(5.0), MAX_ZOOM, "out-of-range clamps up");
        assert_eq!(sane_zoom(0.1), MIN_ZOOM, "out-of-range clamps down");
    }

    /// A primary-modifier chord (Ctrl elsewhere, ⌘ on macOS) — the form a real
    /// `Key::Command(c)` carries, which `handle_key`'s zoom arms match.
    fn cmd_chord(c: char) -> KeyEvent {
        KeyEvent {
            key: Key::Command(c),
            mods: Mods {
                cmd: true,
                ctrl: !cfg!(target_os = "macos"),
                ..Mods::default()
            },
        }
    }

    #[test]
    fn ctrl_chords_zoom_the_content_and_reset() {
        // `set_zoom` persists, so isolate the config file from the real one.
        let _g = config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        s.ui_zoom = 1.0;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        // `Runtime::new` doesn't wire `AppSpec::on_key`; wire it so the chord routes
        // through `Runtime::key` exactly as it does in the live app.
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        let hit = |rt: &mut Runtime<State>, c: char| {
            rt.key(cmd_chord(c));
            rt.frame(0.0);
        };
        hit(&mut rt, '='); // Ctrl/⌘ +
        assert!((rt.state.ui_zoom - 1.1).abs() < 1e-4, "Ctrl+= zooms in");
        hit(&mut rt, '=');
        assert!((rt.state.ui_zoom - 1.2).abs() < 1e-4, "again → 120 %");
        hit(&mut rt, '-'); // Ctrl/⌘ -
        assert!((rt.state.ui_zoom - 1.1).abs() < 1e-4, "Ctrl+- zooms out");
        hit(&mut rt, '0'); // Ctrl/⌘ 0
        assert_eq!(rt.state.ui_zoom, 1.0, "Ctrl+0 resets to 100 %");
    }

    #[test]
    fn ctrl_zoom_reflows_the_album_grid_into_fewer_columns() {
        fn distinct_album(id: u32) -> soul_player_host::TrackView {
            soul_player_host::TrackView {
                id,
                title: Some(format!("T{id}")),
                artist: Some("A".into()),
                album: Some(format!("Album {id:02}")),
                genre: Some("Rock".into()),
                duration_secs: 1.0,
                remote_source: None,
                original_date: 0,
                release_date: 0,
                loudness: None,
                format: None,
            }
        }
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.route = Route::Albums;
        s.ui_zoom = 1.0; // don't depend on any persisted zoom
        s.library = (1..=24).map(distinct_album).collect();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        // Two frames so the grid reads its committed (post-zoom) width, not the fallback.
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("content-zoom").is_none(),
            "no zoom wrapper at 100 %"
        );
        let base_cols = first_row_count(&rt, 24);
        assert!(
            base_cols >= 3,
            "a wide window has several columns, got {base_cols}"
        );

        // Zoom to 150 %: the page lays out in pane/1.5 → the grid re-columns to fewer.
        rt.update(|st: &mut State, _tx: &mut Tx| st.ui_zoom = 1.5);
        rt.frame(0.0);
        rt.frame(0.0);
        assert!(
            rt.find("content-zoom").is_some(),
            "the zoom wrapper wraps the pane at 150 %"
        );
        let zoomed_cols = first_row_count(&rt, 24);
        assert!(
            zoomed_cols < base_cols,
            "zooming in reflows into fewer columns: {zoomed_cols} vs {base_cols}"
        );
    }

    /// A distinct album track for grid-layout tests — distinct artist AND album so
    /// the same library fills either the Albums or the Artists grid with N cards.
    fn grid_album(id: u32) -> soul_player_host::TrackView {
        soul_player_host::TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(format!("Artist {id:02}")),
            album: Some(format!("Album {id:02}")),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
        }
    }

    /// A few albums in a very tall window must sit at the top at natural height —
    /// each wrapped row a row-gap below the last — not be spread down the viewport.
    /// Regression: the browse grid used to BE the growing scroll element, so flexbox
    /// stretched its wrapped lines (and the cards on them) to fill the height,
    /// opening big empty bands between rows whenever the library didn't fill the
    /// window. The scroll now wraps a natural-height grid.
    #[test]
    fn the_album_grid_packs_rows_to_the_top_of_a_tall_window() {
        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.route = Route::Albums;
        s.ui_zoom = 1.0;
        s.library = (1..=8).map(grid_album).collect(); // ~2 rows, far shorter than the window
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1200.0, 1600.0), env); // very tall
        rt.frame(0.0);
        rt.frame(0.0);
        // Positions via `rt.find` (one consistent anchor); width via the element rect.
        let (_x0, row1_y) = rt.find("album-0").expect("album-0 is laid out");
        // The second row: the highest card that sits clearly below row 1.
        let row2_y = (1..8u32)
            .filter_map(|i| rt.find(&format!("album-{i}")))
            .map(|(_x, y)| y)
            .filter(|&y| y > row1_y + 10.0)
            .fold(f32::MAX, f32::min);
        assert!(row2_y < f32::MAX, "eight albums wrap to a second row");
        let card_w = el_info(&rt, "album-0").rect[2];
        // Row-to-row pitch = one natural card (square cover `w` + a two-line caption)
        // plus the row gap. A stretched line (or stretched card) balloons this far
        // past the card's own width.
        let pitch = row2_y - row1_y;
        assert!(
            pitch <= card_w + 120.0,
            "rows a natural pitch apart, not spread: pitch={pitch:.0}, card_w={card_w:.0}"
        );
    }

    /// After a zoom change the live app renders a SINGLE frame (it is event-driven,
    /// not continuously repainting), so the grid must fill the pane on that one
    /// frame. It used to read its width back from the laid-out page, which lagged a
    /// frame under the zoom wrapper — leaving the cards sized for the previous zoom
    /// and the grid short of the right edge until the next relayout (a window
    /// resize). The width is now threaded from the stable content size ÷ zoom.
    #[test]
    fn zooming_out_fills_the_album_grid_width_on_one_frame() {
        let _locale = crate::config::use_test_locale("en");
        let _g = config::use_test_config_dir(); // set_zoom would persist
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.route = Route::Albums;
        s.ui_zoom = 2.0; // start zoomed in: a small logical box → small cards
        s.library = (1..=24).map(grid_album).collect();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        rt.frame(0.0);
        rt.frame(0.0); // settle at 200 %
        // Reset to 100 % and render ONE frame, exactly as a Ctrl+0 keypress does.
        rt.update(|st: &mut State, _tx: &mut Tx| st.ui_zoom = 1.0);
        rt.frame(0.0);
        let grid = el_info(&rt, "albums-grid").rect; // [x, y, w, h]
        let inner_right = grid[0] + grid[2] - crate::ui::space::PANE;
        // First-row cards, measured from element rects (top-left + width) so the row
        // test and the right-edge span share one coordinate space.
        let r0_top = el_info(&rt, "album-0").rect[1];
        let mut max_right = f32::MIN;
        for e in rt.elements() {
            let is_card = e
                .key
                .as_deref()
                .and_then(|k| k.strip_prefix("album-"))
                .is_some_and(|rest| rest.parse::<u32>().is_ok());
            if is_card && (e.rect[1] - r0_top).abs() < 5.0 {
                max_right = max_right.max(e.rect[0] + e.rect[2]);
            }
        }
        let slack = inner_right - max_right;
        assert!(
            slack.abs() <= 3.0,
            "the grid fills to its right inner edge on one frame: slack={slack:.1} \
             (inner right {inner_right:.0}, cards reach {max_right:.0})"
        );
    }

    /// The gap between a browse grid's right inner edge and the furthest-right card
    /// on its TOP row — how many points of empty column the grid leaves on the right
    /// at this window size. ~0 means the cards fill the width. Anchors to the true
    /// top row (min y over the cards), not a specific card, so it is robust to draw
    /// order. `grid_key` is e.g. `"albums-grid"`, `card_prefix` e.g. `"album-"`.
    fn grid_right_slack(rt: &Runtime<State>, grid_key: &str, card_prefix: &str) -> f32 {
        let grid = el_info(rt, grid_key).rect; // [x, y, w, h]
        let inner_right = grid[0] + grid[2] - crate::ui::space::PANE;
        let cards: Vec<[f32; 4]> = rt
            .elements()
            .into_iter()
            .filter(|e| {
                e.key
                    .as_deref()
                    .and_then(|k| k.strip_prefix(card_prefix))
                    .is_some_and(|rest| rest.parse::<u32>().is_ok())
            })
            .map(|e| e.rect)
            .collect();
        let top = cards.iter().map(|r| r[1]).fold(f32::MAX, f32::min);
        let max_right = cards
            .iter()
            .filter(|r| (r[1] - top).abs() < 5.0)
            .map(|r| r[0] + r[2])
            .fold(f32::MIN, f32::max);
        inner_right - max_right
    }

    /// Sweep a browse grid over many window widths AND the common desktop DPI scales,
    /// returning the worst (largest) right-edge empty column found and where. The
    /// exact-fill card width is fractional, so at unlucky width/scale combinations a
    /// fixed-width card rounds a hair too wide and wraps the last column (a full empty
    /// gap) or too narrow (a thin one); the flex cards must fill at every combination.
    fn worst_grid_fill(
        route: Route,
        grid_key: &str,
        card_prefix: &str,
        scales: &[f32],
    ) -> (f32, f32, f32) {
        let mut worst = f32::MIN;
        let (mut worst_w, mut worst_scale) = (0.0f32, 0.0f32);
        for &scale in scales {
            // Irrational-ish step so the card width's fractional part varies across a
            // 1-to-8-column span (the 7-column 1316 case is where the bug first bit).
            for step in 0..12u32 {
                let win_w = 900.0 + step as f32 * 66.9;
                let mut s = State::new(PlayerHandle::offline());
                s.sidebar_collapsed = false;
                s.route = route;
                s.ui_zoom = 1.0;
                s.library = (1..=24).map(grid_album).collect();
                let env = Env {
                    system_dark: true,
                    scale,
                };
                let mut rt = Runtime::new(s, view, (win_w, 900.0), env);
                // Settle: the first frame sizes the grid from the fallback width (no
                // prior layout), so give the readback-driven width two more frames to
                // reach the committed pane width — the steady state the user sees.
                for _ in 0..3 {
                    rt.frame(0.0);
                }
                let slack = grid_right_slack(&rt, grid_key, card_prefix);
                if slack > worst {
                    worst = slack;
                    worst_w = win_w;
                    worst_scale = scale;
                }
            }
        }
        (worst, worst_w, worst_scale)
    }

    /// The Albums grid must fill its width at EVERY window size and DPI scale, not
    /// only the lucky ones. One physical pixel of residual rounding is unavoidable
    /// and invisible; a multi-point empty column (worst case a whole wrapped column)
    /// is the bug the flex cards fix.
    #[test]
    fn the_album_grid_fills_the_width_at_every_size_and_scale() {
        let _locale = crate::config::use_test_locale("en");
        // Scale 1.0 is the coarsest rounding (most exposure); 2.0 the finest — the
        // two extremes of the physical-pixel grid the cards must tile.
        let (worst, w, scale) =
            worst_grid_fill(Route::Albums, "albums-grid", "album-", &[1.0, 2.0]);
        assert!(
            worst <= 1.0,
            "albums grid fills within 1 pt everywhere; worst empty column {worst:.1} pt at window {w:.0}, scale {scale}"
        );
    }

    /// The Artists grid shares the same flex card (`media_card`), so one representative
    /// scale confirms its grid path fills identically.
    #[test]
    fn the_artists_grid_fills_the_width() {
        let _locale = crate::config::use_test_locale("en");
        let (worst, w, scale) = worst_grid_fill(Route::Artists, "artists-grid", "artist-", &[1.25]);
        assert!(
            worst <= 1.0,
            "artists grid fills within 1 pt; worst empty column {worst:.1} pt at window {w:.0}, scale {scale}"
        );
    }

    /// End-to-end through the real render: flipping the Album-year toggle relabels
    /// the displayed year on the Albums grid. A reissue album (original 1973,
    /// release 2015) shows the 1973 original year under Original and the 2015
    /// release year under Release, with no leftover from the other source. Drives
    /// the live `State.album_year` -> `view` -> `album_subtitle` path, the
    /// user-facing effect of the setting.
    #[test]
    fn album_year_toggle_relabels_the_displayed_year_on_the_albums_grid() {
        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.route = Route::Albums;
        s.album_year = config::AlbumYearSource::Original;
        s.library = vec![soul_player_host::TrackView {
            id: 1,
            title: Some("T".into()),
            artist: Some("Pink Floyd".into()),
            album: Some("Dark Side".into()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 1973u32 << 9,
            release_date: 2015u32 << 9,
            loudness: None,
            format: None,
        }];
        let env = Env {
            system_dark: true,
            scale: 1.0,
        };
        let mut rt = Runtime::new(s, view, (1200.0, 800.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        // The only years rendered are the album-card subtitle, so a bare-year
        // substring is an unambiguous probe for "which year is shown".
        let shows = |rt: &Runtime<State>, year: &str| {
            rt.elements()
                .into_iter()
                .any(|e| e.text.as_deref().is_some_and(|t| t.contains(year)))
        };
        assert!(
            shows(&rt, "1973"),
            "Original source shows the 1973 original year"
        );
        assert!(
            !shows(&rt, "2015"),
            "the release year is not shown under Original"
        );

        rt.update(|st: &mut State, _tx: &mut Tx| st.album_year = config::AlbumYearSource::Release);
        rt.frame(0.0);
        assert!(
            shows(&rt, "2015"),
            "flipping to Release relabels to the 2015 release year"
        );
        assert!(
            !shows(&rt, "1973"),
            "no leftover of the original year under Release"
        );
    }

    /// Tab forward until the element keyed `key` is focused, up to `max` presses.
    fn tab_to(rt: &mut Runtime<State>, target: &str, max: usize) -> bool {
        for _ in 0..max {
            if focused_key(rt).as_deref() == Some(target) {
                return true;
            }
            key(rt, Key::Tab);
        }
        focused_key(rt).as_deref() == Some(target)
    }

    /// Tabbing through a surface visits controls in reading order: top-to-bottom
    /// within a column, and columns left-to-right. The app is multi-pane (sidebar,
    /// then content — Settings adds its category rail), so finishing one column and
    /// dropping to the TOP of the next pane to the right is in-order; what must never
    /// happen is focus jumping UP within the same (or a leftward) column — a control
    /// wired far from its visual place.
    #[test]
    fn focus_order_follows_reading_order() {
        const ROW_TOL: f32 = 36.0; // ~one row; same-row jitter stays "not above".
        const COL_TOL: f32 = 120.0; // a step this far right is a new pane, which may reset y.
        for (name, scene) in [
            ("home", scene_home as fn(&mut State)),
            ("settings-audio", scene_settings_audio),
        ] {
            let mut rt = a11y_surface(scene);
            rt.on_key = Some(handle_key);
            let mut prev: Option<[f32; 4]> = None;
            let mut seen = std::collections::HashSet::new();
            for _ in 0..60 {
                key(&mut rt, Key::Tab);
                let Some(k) = focused_key(&rt) else { continue };
                if !seen.insert(k.clone()) {
                    break; // wrapped around the focus ring
                }
                let r = el_info(&rt, &k).rect;
                if let Some(p) = prev {
                    // In order if it is not above the previous control, or it has
                    // clearly moved into a pane to the right (where y may reset).
                    let in_reading_order = r[1] >= p[1] - ROW_TOL || r[0] >= p[0] + COL_TOL;
                    assert!(
                        in_reading_order,
                        "[{name}] focus jumped backward to {k} at {r:?} (prev {p:?})"
                    );
                }
                prev = Some(r);
            }
            assert!(prev.is_some(), "[{name}] Tab reached at least one control");
        }
    }

    /// Overlays are operable from the keyboard: Escape dismisses them, and while a
    /// modal is open the page behind it is inert (so Tab can't leak behind it). The
    /// language dropdown is deliberately non-modal, so for it we instead check its
    /// options are focusable + labelled and that Escape closes it.
    #[test]
    fn dialogs_are_keyboard_operable() {
        for (name, scene, dialog_key) in [
            (
                "organize-confirm",
                scene_organize_confirm as fn(&mut State),
                "organize-confirm",
            ),
            ("crash-dialog", scene_crash_dialog, "crash-dialog"),
            (
                "add-source-open",
                scene_add_source_open,
                "add-source-opt-local",
            ),
            (
                "add-to-playlist-open",
                scene_add_to_playlist_open,
                "add-to-playlist-done",
            ),
        ] {
            let mut rt = a11y_surface(scene);
            rt.on_key = Some(handle_key);
            assert!(rt.find(dialog_key).is_some(), "[{name}] dialog is open");
            // A background control is not actionable while the dialog is up.
            let bg_leaks = rt
                .elements()
                .into_iter()
                .any(|e| e.key.as_deref() == Some("play-pause") && actionable(&e));
            assert!(
                !bg_leaks,
                "[{name}] the background stays inert behind the dialog"
            );
            key(&mut rt, Key::Escape);
            assert!(
                rt.find(dialog_key).is_none(),
                "[{name}] Escape closes the dialog"
            );
        }

        // The language dropdown (non-modal, keyed "lang-picker").
        let mut rt = a11y_surface(scene_settings_lang_open);
        rt.on_key = Some(handle_key);
        let items: Vec<_> = rt
            .elements()
            .into_iter()
            .filter(|e| {
                e.role == "option"
                    && e.key
                        .as_deref()
                        .is_some_and(|k| k.starts_with("lang-picker-opt-"))
            })
            .collect();
        assert!(
            items.iter().any(|e| e.focusable),
            "the language dropdown has focusable options"
        );
        for e in &items {
            assert!(a11y_name(e).is_some(), "{:?} is labelled", e.key);
        }
        key(&mut rt, Key::Escape);
        assert!(
            rt.state.open_dropdown.is_none(),
            "Escape closes the dropdown"
        );
    }

    /// Keyboard reaches the primary nav and Space activates it: a nav item is a
    /// labeled, focusable button; Tab lands on it; Space routes to its page
    /// (without the Space=play shortcut swallowing the activation).
    #[test]
    fn keyboard_reaches_and_activates_the_primary_nav() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.nav_hidden = Vec::new();
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        let nav = el_info(&rt, "nav-albums");
        assert!(nav.focusable, "the nav item is keyboard-focusable");
        assert_eq!(nav.role, "button", "and exposes a button role");
        assert!(
            nav.label.as_deref().is_some_and(|l| !l.is_empty()),
            "and carries an accessible label"
        );
        assert!(
            tab_to(&mut rt, "nav-albums", 25),
            "Tab reaches the Albums nav item"
        );
        key(&mut rt, Key::Text(" ".into()));
        assert!(
            matches!(rt.state.route, Route::Albums),
            "Space activates the focused nav item (not the play shortcut)"
        );
    }

    /// The track list is an accessible list of focusable rows: the container
    /// carries the `list` role (so assistive tech announces it as a list), each
    /// row is a focusable button, Tab reaches a row, and Space plays it.
    #[test]
    fn the_track_list_is_an_accessible_list_of_focusable_rows() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.nav_hidden = Vec::new();
        s.library = vec![
            album_track(1, "A"),
            album_track(2, "B"),
            album_track(3, "C"),
        ];
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        assert_eq!(
            el_info(&rt, "tracks").role,
            "list",
            "the track list is a list group for assistive tech"
        );
        let first = el_info(&rt, "track-1");
        assert!(first.focusable, "each track row is keyboard-focusable");
        assert_eq!(first.role, "button");
        assert!(
            first.label.as_deref().is_some_and(|l| !l.is_empty()),
            "and carries an accessible name"
        );
        assert!(tab_to(&mut rt, "track-1", 30), "Tab reaches a track row");
        key(&mut rt, Key::Text(" ".into()));
        assert_eq!(
            rt.state.host.recorded_commands().last(),
            Some(&Command::PlayContext {
                ids: vec![1, 2, 3],
                start: 0
            }),
            "Space plays the focused track"
        );
    }

    /// Controls that are focusable/role-bearing but legitimately carry no accessible
    /// name. (surface, element key, why). This allow-list is the ONLY suppression —
    /// every entry must be justified. Start empty; add only with a documented reason.
    const A11Y_ALLOWLIST: &[(&str, &str, &str)] = &[];

    /// Roles denoting an interactive control, which therefore require an accessible name.
    fn interactive_role(role: &str) -> bool {
        // `option` is the themed dropdown's item role (see `views/dropdown.rs`).
        // `switch` is the ARIA toggle-switch role (see `shuffle_button` in
        // `views/player.rs`), correctly distinct from `checkbox`.
        matches!(
            role,
            "button"
                | "link"
                | "checkbox"
                | "slider"
                | "menuitem"
                | "tab"
                | "textbox"
                | "option"
                | "switch"
        )
    }

    /// An element the user can operate right now: focusable or interactive-roled, and
    /// actually present (not inert, not transparent). `elements()` includes opacity-0
    /// nodes and `inert` ones, so both are filtered here.
    fn actionable(e: &rune::ElementInfo) -> bool {
        (e.focusable || interactive_role(e.role)) && !e.inert && e.opacity > 0.01
    }

    /// The accessible name: the explicit label, else the visible text (a track row
    /// names itself by its text). Whitespace-only counts as empty.
    fn a11y_name(e: &rune::ElementInfo) -> Option<String> {
        e.label
            .clone()
            .or_else(|| e.text.clone())
            .filter(|s| !s.trim().is_empty())
    }

    /// A fully-populated base state so controls (now-playing, track rows, transport)
    /// actually render on every surface.
    fn a11y_base() -> State {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.nav_hidden = Vec::new();
        s.library = vec![album_track(1, "Aria"), album_track(2, "Bolero")];
        s.now_playing_id = Some(1);
        s.playing = true;
        s
    }

    /// The surfaces the a11y sweep audits: each a scene fn applied over `a11y_base`.
    #[allow(clippy::type_complexity)]
    const A11Y_SURFACES: &[(&str, fn(&mut State))] = &[
        ("home", scene_home),
        ("albums", scene_albums),
        ("album-detail", scene_album_detail),
        ("artists", scene_artists),
        ("artist-detail", scene_artist_detail),
        ("genres", scene_genres),
        ("queue", scene_queue),
        ("collapsed", scene_collapsed),
        ("mobile-content", scene_mobile_content),
        ("settings-music", scene_settings_music),
        ("settings-keyboard", scene_settings_keyboard),
        ("settings-audio", scene_settings_audio),
        ("settings-audio-eq", scene_settings_audio_eq),
        ("settings-appearance", scene_settings_appearance),
        ("settings-about", scene_settings_about),
        ("settings-lang-open", scene_settings_lang_open),
        ("add-source-open", scene_add_source_open),
        ("organize-confirm", scene_organize_confirm),
        ("signal-path-open", scene_signal_path_open),
        ("crash-dialog", scene_crash_dialog),
        ("playlists", scene_playlists),
        ("playlist-detail", scene_playlist_detail),
        ("add-to-playlist-open", scene_add_to_playlist_open),
        ("artwork-crop", scene_artwork_crop),
        ("artwork-edit", scene_artwork_edit),
        ("remote-health-open", scene_remote_health_open),
    ];

    /// Render one a11y surface headlessly.
    fn a11y_surface(scene: fn(&mut State)) -> Runtime<State> {
        let mut s = a11y_base();
        scene(&mut s);
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        rt
    }

    /// Every actionable control, on every surface, exposes an accessible name and a
    /// valid interactive role. A dropped `.label()`/`.role()`/`.focusable()` fails here.
    #[test]
    fn all_actionable_controls_are_labelled() {
        let mut violations: Vec<String> = Vec::new();
        for (name, scene) in A11Y_SURFACES {
            let rt = a11y_surface(*scene);
            for e in rt.elements() {
                if !actionable(&e) {
                    continue;
                }
                let key = e.key.as_deref().unwrap_or("<no key>");
                if A11Y_ALLOWLIST
                    .iter()
                    .any(|(s, k, _)| s == name && *k == key)
                {
                    continue;
                }
                if !interactive_role(e.role) {
                    violations.push(format!(
                        "[{name}] {key}: focusable but role={:?} is not interactive",
                        e.role
                    ));
                }
                if a11y_name(&e).is_none() {
                    violations.push(format!(
                        "[{name}] {key}: role={:?} has no accessible name",
                        e.role
                    ));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "a11y sweep found {} unlabelled/mis-roled controls:\n{}",
            violations.len(),
            violations.join("\n")
        );
    }

    #[test]
    fn the_new_a11y_scenes_render_their_surface() {
        // Appearance tab shows its category rail.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        scene_settings_appearance(&mut s);
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("cat-appearance").is_some(),
            "appearance scene shows the settings rail"
        );

        // Add-source menu open.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        scene_add_source_open(&mut s);
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("add-source-opt-local").is_some(),
            "add-source menu is open"
        );

        // Crash dialog.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        scene_crash_dialog(&mut s);
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(rt.find("crash-dialog").is_some(), "crash dialog is shown");

        // Playlists grid page.
        let mut rt = a11y_surface(scene_playlists);
        assert!(
            rt.find("playlist-card-0").is_some(),
            "playlists page shows playlist cards"
        );
        // The inline new-playlist field is revealed by the top bar's button.
        click_key(&mut rt, "playlists-new");
        rt.frame(0.0);
        assert!(
            rt.find("playlist-new-input").is_some(),
            "and the inline new-playlist field after pressing New"
        );

        // Playlist detail page.
        let rt = a11y_surface(scene_playlist_detail);
        assert!(
            rt.find("playlist-rename").is_some(),
            "playlist detail shows its hero controls"
        );
        assert!(
            rt.find("playlist-grip-0").is_some(),
            "and per-entry reorder grips"
        );

        // Add-to-playlist overlay.
        let rt = a11y_surface(scene_add_to_playlist_open);
        assert!(
            rt.find("add-to-playlist-pick-0").is_some(),
            "add-to-playlist overlay lists playlists to pick"
        );
        assert!(
            rt.find("add-to-playlist-done").is_some(),
            "and a done button"
        );

        // Remote-health popover.
        let rt = a11y_surface(scene_remote_health_open);
        assert!(
            rt.find("remote-health-popover").is_some(),
            "remote-health popover is open over a seeded remote"
        );
    }

    #[test]
    fn a_wide_window_shows_both_panes_side_by_side() {
        let rt = booted(1100.0, 720.0, |_| {});
        assert!(
            rt.find("sidebar").is_some(),
            "desktop keeps the fixed sidebar"
        );
        assert!(
            rt.find("content").is_some(),
            "and the content pane beside it"
        );
        assert!(
            rt.find("sidebar-resize").is_some(),
            "with the resize handle"
        );
        assert!(
            rt.find("mobile-top-bar").is_none(),
            "and no mobile back bar on desktop"
        );
    }

    #[test]
    fn clicking_a_manual_queue_row_plays_that_upcoming_slot() {
        let mut rt = Runtime::new(
            recording_queue_state(&[10, 20], &[30, 40]),
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        click(&mut rt, "queue-manual-0"); // first manual row (index 0)
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayUpcoming(1)],
            "clicking the first manual row plays upcoming slot 1"
        );
    }

    #[test]
    fn clicking_a_context_row_plays_its_merged_upcoming_slot() {
        let mut rt = Runtime::new(
            recording_queue_state(&[10, 20], &[30, 40]),
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        // The first context row is the 3rd upcoming (2 manual rows precede it):
        // k = 3, and rows are keyed by that position.
        click(&mut rt, "queue-context-3");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayUpcoming(3)],
            "a context row plays its merged upcoming slot, after the manual band"
        );
    }

    #[test]
    fn the_remove_button_removes_that_manual_index() {
        let mut rt = Runtime::new(
            recording_queue_state(&[10, 20, 30], &[]),
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        click(&mut rt, "remove-1"); // manual index 1
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::RemoveFromQueue(1)],
            "the ✕ removes that manual-queue index (not the clicked id)"
        );
    }

    #[test]
    fn clear_empties_the_manual_queue() {
        let mut rt = Runtime::new(
            recording_queue_state(&[10, 20], &[30]),
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        click(&mut rt, "clear-queue");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::ClearQueue],
            "Clear empties the manual queue"
        );
    }

    #[test]
    fn dragging_a_manual_row_dispatches_a_move_command_on_release() {
        use crate::views::queue::QUEUE_ROW_H;
        let mut rt = Runtime::new(
            recording_queue_state(&[10, 20, 30], &[]),
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        // Press row 0's grip, drag down exactly one row (→ target index 1), release.
        let (gx, gy) = rt.find("grip-0").expect("manual row grip");
        rt.pointer_move(gx, gy);
        rt.pointer_down(gx, gy);
        rt.pointer_move(gx, gy + QUEUE_ROW_H);
        rt.frame(0.0);
        rt.pointer_up(gx, gy + QUEUE_ROW_H);
        rt.frame(0.0);
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::MoveInQueue { from: 0, to: 1 }],
            "releasing the drag commits a MoveInQueue for the dragged row"
        );
    }

    /// Dragging a track that appears twice in the manual queue moves the row the
    /// user actually grabbed, not the first copy. Rows and grips are keyed by
    /// position, and the drag reads the row's own index — so grabbing the second
    /// copy (index 2) and nudging it up one row commits a move FROM index 2, not
    /// index 0 (where a search-by-track-id would land).
    #[test]
    fn dragging_a_repeated_manual_track_moves_the_grabbed_row() {
        use crate::views::queue::QUEUE_ROW_H;
        let mut rt = Runtime::new(
            recording_queue_state(&[10, 20, 10], &[]), // id 10 at index 0 AND index 2
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        // Grab the SECOND copy (index 2) by its position-keyed grip, drag up one row.
        let (gx, gy) = rt.find("grip-2").expect("the third manual row's grip");
        rt.pointer_move(gx, gy);
        rt.pointer_down(gx, gy);
        rt.pointer_move(gx, gy - QUEUE_ROW_H);
        rt.frame(0.0);
        rt.pointer_up(gx, gy - QUEUE_ROW_H);
        rt.frame(0.0);
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::MoveInQueue { from: 2, to: 1 }],
            "the grabbed row (index 2) moves, not the first copy of the same track"
        );
    }

    /// Drag-reorder, driven through the real pointer: pressing a manual row's grip
    /// picks it up, dragging down past a row height advances the drop target and
    /// shows the drop indicator, and releasing ends the gesture (committing the
    /// move). The same `Runtime` + `pointer_*` harness the sidebar resize uses.
    #[test]
    fn dragging_a_queue_row_shows_a_drop_target_then_commits_on_release() {
        use crate::views::queue::QUEUE_ROW_H;
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        s.sidebar_collapsed = false;
        let ids: Vec<u32> = s.library.iter().take(6).map(|t| t.id).collect();
        assert!(ids.len() >= 6, "testdata has enough tracks");
        s.now_playing_id = Some(ids[0]);
        s.up_next = ids[1..4].to_vec(); // 3 manual rows at indices 0, 1, 2
        s.context_ids = ids[4..].to_vec();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        // Press the first manual row's grip (index 0) and drag it down ~1.5 rows.
        let (gx, gy) = rt
            .find("grip-0")
            .expect("the first manual row has a drag grip");
        rt.pointer_move(gx, gy);
        rt.pointer_down(gx, gy);
        rt.pointer_move(gx, gy + QUEUE_ROW_H * 1.5);
        rt.frame(0.0);
        assert_eq!(
            rt.state.queue_drag.map(|(from, _)| from),
            Some(0),
            "pressing a grip picks up that row"
        );
        assert!(
            rt.state.queue_drag.map(|(_, to)| to).unwrap_or(0) >= 1,
            "dragging down advances the drop target"
        );
        assert!(
            rt.find("drop-line").is_some(),
            "a drop indicator shows while dragging"
        );

        // Release ends the gesture and clears the transient drag state.
        rt.pointer_up(gx, gy + QUEUE_ROW_H * 1.5);
        rt.frame(0.0);
        assert_eq!(rt.state.queue_drag, None, "releasing ends the drag");
        assert!(
            rt.find("drop-line").is_none(),
            "the drop indicator is gone once released"
        );
    }

    /// `/` on the Library route enters search mode; typing builds the query,
    /// Backspace edits it, Esc clears + exits.
    #[test]
    fn slash_enters_search_and_typing_builds_the_query() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            State::new(PlayerHandle::offline()),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        let press = |rt: &mut Runtime<State>, key: Key| {
            rt.key(KeyEvent {
                key,
                mods: Mods::default(),
            });
            rt.frame(0.0); // keys dispatch on the next frame (Rune's own on_key tests)
        };
        press(&mut rt, Key::Text("/".into()));
        assert!(rt.state.searching, "/ enters search mode");
        press(&mut rt, Key::Text("a".into()));
        press(&mut rt, Key::Text("b".into()));
        assert_eq!(rt.state.search, "ab", "typed characters build the query");
        press(&mut rt, Key::Backspace);
        assert_eq!(rt.state.search, "a", "Backspace edits the query");
        press(&mut rt, Key::Escape);
        assert!(
            !rt.state.searching && rt.state.search.is_empty(),
            "Esc clears the query and exits search mode"
        );
    }

    /// End to end: `AppSpec::on_key`'s Space shortcut (set on the `Runtime`
    /// directly here — `crates/rune/src/tooling.rs` copies
    /// `AppSpec::on_key` into exactly this field) reaches the real backend
    /// via `Command::TogglePlay`, and the resulting `Tick` flips
    /// `State::playing` — not merely that `handle_key` returns `true`.
    /// Since the actor's events arrive off a real pump thread
    /// (`soul_player_host::PlayerHandle::subscribe`), this uses the same
    /// bounded `subscribe` + `recv_timeout` pattern as that crate's own
    /// `subscribe_receives_now_playing_tick` test.
    // Ignored in CI: this drives the real actor over NullSink, whose
    // instant-drain timing is environment-dependent. On a slow CI runner the
    // Space press can land after the 38-track context has already drained, so
    // the wait for a paused Tick never resolves and the loop spins forever (it
    // hung CI for hours). Runs locally with `cargo test -- --ignored`. A
    // deterministic re-enable would pin playback with `Repeat::One` and drain
    // the event backlog before pressing Space.
    #[test]
    #[ignore = "flaky actor-drain timing; can hang on slow CI — run with --ignored"]
    fn space_key_toggles_playback_through_the_real_backend() {
        let host = PlayerHandle::offline();
        let (tx, rx) = mpsc::channel::<Event>();
        host.subscribe(move |ev| {
            let _ = tx.send(ev);
        });
        host.open_folder(&testdata_dir()).expect("scan testdata/");

        let mut state = State::new(host);
        let deadline = Duration::from_secs(5);
        while let Ok(ev) = rx.recv_timeout(deadline) {
            state.apply(&ev);
            if state.playing {
                break;
            }
        }
        assert!(state.playing, "open_folder should autoplay track 0");

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(state, view, (1100.0, 720.0), env);
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Text(" ".into()),
            mods: Mods::default(),
        });
        rt.frame(1.0 / 60.0);

        while let Ok(ev) = rx.recv_timeout(deadline) {
            rt.state.apply(&ev);
            if !rt.state.playing {
                break;
            }
        }
        assert!(!rt.state.playing, "Space should have paused playback");
    }
}

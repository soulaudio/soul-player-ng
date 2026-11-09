//! Rune UI head for Soul Player (spike). Wires `soul_player_host::PlayerHandle`
//! into a Rune `App`: builds the host, autoloads a library from
//! `SOUL_PLAYER_FOLDER`, and folds backend `Tick`/`Queue` events into `State`
//! via `AppHandle::update` (see docs/superpowers/plans/2026-11-09-rune-head.md).
//! The view renders the Day/Night-themed track list (`theme`, `views::library`)
//! and the now-playing panel (`views::player`) stacked under it for this
//! milestone; the sidebar assembly is a later task.
#![forbid(unsafe_code)]

use rune::{App, AppSpec, Cx, El, Tx, Waker, col};
use soul_player_host::{Event, PlayState, PlayerHandle, RepeatMode, TrackView};

mod theme;
mod views;

use views::player::ArtCache;

struct State {
    /// Read by the view's command handlers (click-to-play, transport).
    host: PlayerHandle,
    library: Vec<TrackView>,
    now_playing_id: Option<u32>,
    position_secs: f64,
    duration_secs: f64,
    playing: bool,
    volume: f32,
    shuffle: bool,
    repeat: RepeatMode,
    up_next: Vec<u32>,
    context_pos: u32,
    context_len: u32,
    /// Cover-art decode cache (`views::player::ArtCache`), wired up with the
    /// app's `Waker` in `setup_art` below.
    art: ArtCache,
}

impl State {
    fn new(host: PlayerHandle) -> Self {
        let library = host.views();
        Self {
            host,
            library,
            now_playing_id: None,
            position_secs: 0.0,
            duration_secs: 0.0,
            playing: false,
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            up_next: Vec::new(),
            context_pos: 0,
            context_len: 0,
            art: ArtCache::new(),
        }
    }

    /// Fold one backend event into the UI state. Mirrors the Svelte store's
    /// `bindPlayerEvents` (app/tauri/src/lib/store.svelte.ts).
    fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Tick(t) => {
                self.now_playing_id = t.now_playing_id;
                self.position_secs = t.position_secs;
                self.duration_secs = t.duration_secs.unwrap_or(0.0);
                self.playing = matches!(t.state, PlayState::Playing);
                self.volume = t.volume;
                self.shuffle = t.shuffle;
                self.repeat = t.repeat;
            }
            Event::Queue(q) => {
                self.up_next = q.up_next_ids.clone();
                self.context_pos = q.context_pos;
                self.context_len = q.context_len;
            }
        }
    }
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let p = theme::palette(cx.system_dark());
    let scale = cx.env().scale;
    // Temporary milestone layout (Task 5/6): the player panel previews at
    // the bottom under the list. The sidebar assembly is the next task.
    col()
        .w_full()
        .h_full()
        .fill(p.bg)
        .child(views::library::track_list(s, &p))
        .child(views::player::player_panel(s, &p, scale))
}

/// `AppSpec::setup`: wires the app's `Waker` into the cover-art cache
/// (cheat-sheet §5b/§6a) so decoded covers wake a redraw instead of needing
/// a poll loop.
fn setup_art(s: &mut State, waker: Waker) {
    s.art.wire(waker);
}

fn main() -> anyhow::Result<()> {
    let host = PlayerHandle::new();
    host.autoload_from_env()?; // SOUL_PLAYER_FOLDER=<dir> to load a library

    let init_host = host.clone();
    let spec = AppSpec::<State> {
        setup: Some(setup_art),
        ..AppSpec::new(
            "Soul Player (Rune)",
            (1100.0, 720.0),
            || unreachable!("init overridden"),
            view,
        )
    };
    let app = App::new(spec).init(move || State::new(init_host.clone()));
    let ah = app.handle();
    host.subscribe(move |ev: Event| {
        ah.update(move |s: &mut State, tx: &mut Tx| {
            s.apply(&ev);
            tx.instant(); // don't animate fast-moving numeric fields
        });
    });
    app.run();
    Ok(())
}

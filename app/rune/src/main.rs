//! Rune UI head for Soul Player (spike). Wires `soul_player_host::PlayerHandle`
//! into a Rune `App`: builds the host, autoloads a library from
//! `SOUL_PLAYER_FOLDER`, and folds backend `Tick`/`Queue` events into `State`
//! via `AppHandle::update` (see docs/superpowers/plans/2026-11-09-rune-head.md).
//! The view renders the Day/Night-themed track list (`theme`, `views::library`);
//! the now-playing panel and transport controls come later.
#![forbid(unsafe_code)]

use rune::{App, AppSpec, Cx, El, Tx, col};
use soul_player_host::{Event, PlayState, PlayerHandle, RepeatMode, TrackView};

mod theme;
mod views;

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
    col()
        .w_full()
        .h_full()
        .fill(p.bg)
        .child(views::library::track_list(s, &p))
}

fn main() -> anyhow::Result<()> {
    let host = PlayerHandle::new();
    host.autoload_from_env()?; // SOUL_PLAYER_FOLDER=<dir> to load a library

    let init_host = host.clone();
    let spec = AppSpec::<State> {
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

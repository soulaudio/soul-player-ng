//! Rune UI head for Soul Player (spike). Wires `soul_player_host::PlayerHandle`
//! into a Rune `App`: builds the host, autoloads a library from
//! `SOUL_PLAYER_FOLDER`, and folds backend `Tick`/`Queue` events into `State`
//! via `AppHandle::update` (see docs/superpowers/plans/2026-11-09-rune-head.md).
//! The view here is a placeholder proving the wiring; the real UI comes later.
#![forbid(unsafe_code)]

use rune::{App, AppSpec, Cx, El, Tx, col, text, tok};
use soul_player_host::{Event, PlayState, PlayerHandle, RepeatMode, TrackView};

struct State {
    /// Read by the view's command handlers (click-to-play, transport); those
    /// land in a later task, so it's briefly write-only here.
    #[allow(dead_code)]
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

    fn track(&self, id: u32) -> Option<&TrackView> {
        self.library.iter().find(|t| t.id == id)
    }
}

fn view(s: &State, _cx: &mut Cx<State>) -> El {
    let theme = tok::theme(true);
    let now = s
        .now_playing_id
        .and_then(|id| s.track(id))
        .and_then(|track| track.title.clone())
        .unwrap_or_else(|| "—".to_string());
    col()
        .w_full()
        .h_full()
        .padding(24.0)
        .gap(8.0)
        .fill(theme.bg)
        .child(text(format!("{} tracks", s.library.len())).color(theme.text))
        .child(text(format!("now playing: {now}")).color(theme.text))
        .child(text(if s.playing { "▶ playing" } else { "⏸ paused" }).color(theme.text))
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

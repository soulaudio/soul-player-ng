//! Rune UI head for Soul Player (spike). Minimal window to prove the build /
//! toolchain / GPU stack before the real UI is layered on (see
//! docs/superpowers/plans/2026-11-09-rune-head.md). Mirrors examples/hello.
#![forbid(unsafe_code)]

use rune::tok;
use rune::{AppSpec, Cx, El, col, text};

#[derive(Default)]
struct State {}

fn view(_s: &State, _cx: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    col()
        .w_full()
        .h_full()
        .center()
        .gap(8.0)
        .fill(t.bg)
        .child(text("Soul Player — Rune head").color(t.text))
        .child(text("build OK").color(t.text))
}

fn main() {
    rune::run(AppSpec::<State> {
        ..AppSpec::new("Soul Player (Rune)", (1100.0, 720.0), State::default, view)
    });
}

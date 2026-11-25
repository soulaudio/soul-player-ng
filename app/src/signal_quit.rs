//! Unix termination signals quit the app **the same way ⌘Q does**.
//!
//! Rune already turns ⌘Q, the Dock's Quit, logout and shutdown into its quit hooks
//! (`on_quit` → `flush_on_exit`: scan caches, window geometry, the clean-exit marker). A
//! signal, `kill`, Ctrl-C in the terminal that launched it, a closed terminal, launchd
//! stopping a job, used to end the process without them, so the next launch offered a
//! "the last session ended without closing normally" bug report for a perfectly ordinary
//! exit. Now the first `SIGTERM`/`SIGINT`/`SIGHUP` requests a normal quit; a second one
//! (the user insisting while the quit is somehow stuck) exits at once, and that run is then
//! *correctly* reported as unclean.
#![forbid(unsafe_code)]

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// The signals that mean "please quit".
pub(crate) const QUIT_SIGNALS: [i32; 3] = [SIGTERM, SIGINT, SIGHUP];

/// What the n-th (1-based) quit signal does.
#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    /// Ask the app to quit normally (runs the quit hooks).
    Quit,
    /// Exit immediately with the conventional `128 + signal` status.
    Exit(i32),
}

pub(crate) fn action(nth: usize, signal: i32) -> Action {
    if nth <= 1 {
        Action::Quit
    } else {
        Action::Exit(128 + signal)
    }
}

/// Listen for [`QUIT_SIGNALS`] on a background thread, calling `quit` on the first.
pub(crate) fn spawn(quit: impl Fn(i32) + Send + 'static) {
    let mut signals = match Signals::new(QUIT_SIGNALS) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("signal handling unavailable: {e}");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("quit-signals".into())
        .spawn(move || {
            for (i, sig) in signals.forever().enumerate() {
                match action(i + 1, sig) {
                    Action::Quit => {
                        tracing::info!(signal = sig, "quit requested by signal");
                        quit(sig);
                    }
                    Action::Exit(code) => {
                        tracing::warn!(signal = sig, "second quit signal; exiting now");
                        std::process::exit(code);
                    }
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("signal thread failed to start: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_signal_quits_normally_and_a_second_exits_at_once() {
        assert_eq!(action(1, SIGTERM), Action::Quit);
        assert_eq!(action(2, SIGTERM), Action::Exit(128 + SIGTERM));
        assert_eq!(
            action(3, SIGINT),
            Action::Exit(130),
            "Ctrl-C twice → status 130"
        );
    }

    #[test]
    fn a_real_signal_reaches_the_quit_callback() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn(move |sig| {
            let _ = tx.send(sig);
        });
        // SIGHUP: caught by our handler (registered above), so the test process survives.
        signal_hook::low_level::raise(SIGHUP).expect("raise");
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("quit callback ran");
        assert_eq!(got, SIGHUP);
    }
}

//! File logging + panic routing. A non-blocking, daily-rolling file appender
//! (`tracing-appender`) keeps disk I/O off the audio/GPU hot paths; panics are
//! routed into the same file via a hook so a shipped, console-less build still
//! leaves a trace. Logs live in the per-OS local/state location — never the
//! roaming profile — so they don't sync across machines.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Rolling-file name parts: `Soul.YYYY-MM-DD.log`.
const LOG_PREFIX: &str = "Soul";
const LOG_SUFFIX: &str = "log";

/// Default level filter when `RUST_LOG` isn't set: our crate verbose in debug
/// builds, INFO everywhere in release.
fn default_filter() -> &'static str {
    if cfg!(debug_assertions) {
        "soul_player_rune=debug,info"
    } else {
        "info"
    }
}

/// Initialise file logging and route panics into the log. Keep the returned guard
/// alive for the whole process — dropping it stops the writer thread and loses
/// buffered lines (bind it in `main`). `None` if no log dir resolves/creates, in
/// which case the app still runs, just without a log file.
#[must_use]
pub(crate) fn init() -> Option<WorkerGuard> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix(LOG_SUFFIX)
        .max_log_files(7)
        .build(&dir)
        .ok()?;
    // Non-blocking: the file write happens on a worker thread, so a `tracing`
    // call never blocks the UI/audio thread on disk I/O.
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter()));

    // `try_init` fails only if a global subscriber is already set (e.g. a test);
    // keep the guard regardless so logging still works where it did initialise.
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .try_init();

    install_panic_hook();
    Some(guard)
}

/// Route panics into the log (as a `tracing` ERROR event) in addition to the
/// default stderr print, so a shipped console-less build still records them.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing_panic::panic_hook(info);
        previous(info);
    }));
}

/// The newest `*.log` file in [`log_dir`] (the one currently being written), if any.
pub(crate) fn current_log_file() -> Option<PathBuf> {
    let dir = log_dir()?;
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| {
            let path = e.ok()?.path();
            if path.extension().is_some_and(|x| x == LOG_SUFFIX) {
                let modified = path.metadata().ok()?.modified().ok()?;
                Some((modified, path))
            } else {
                None
            }
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// The last `max_lines` lines of the current log file (empty if there is none).
pub(crate) fn read_recent_log(max_lines: usize) -> String {
    current_log_file()
        .map(|f| read_log_tail(&f, max_lines))
        .unwrap_or_default()
}

/// The per-OS directory for Soul Player's log files: `%LOCALAPPDATA%\soul-player\logs`
/// (Windows, Local not Roaming), `~/Library/Logs/Soul Player` (macOS), and
/// `$XDG_STATE_HOME/soul-player/logs` (Linux). `None` only if the base dir can't
/// be resolved (a very unusual environment).
pub(crate) fn log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(
            dirs::home_dir()?
                .join("Library")
                .join("Logs")
                .join("Soul Player"),
        )
    }
    #[cfg(target_os = "linux")]
    {
        let base = dirs::state_dir().or_else(dirs::data_local_dir)?;
        Some(base.join("soul-player").join("logs"))
    }
    // Windows (and anything else): the Local, non-roaming app-data dir.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Some(dirs::data_local_dir()?.join("soul-player").join("logs"))
    }
}

/// The last `max_lines` lines of the file at `path` (all of it if shorter), with
/// no trailing newline. Empty string if the file can't be read.
pub(crate) fn read_log_tail(path: &Path, max_lines: usize) -> String {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// Crash detection via a session sentinel: returns whether `sentinel` already
/// existed (i.e. the previous session never removed it → it crashed or was
/// killed), then ensures it exists for this session. [`begin_session`] /
/// [`end_session`] wrap this against the real path.
fn detect_and_mark(sentinel: &Path) -> bool {
    let existed = sentinel.exists();
    let _ = std::fs::write(sentinel, b"");
    existed
}

/// The session sentinel file, alongside the logs.
fn running_sentinel() -> Option<PathBuf> {
    Some(log_dir()?.join("running.lock"))
}

/// Begin a session: returns whether the *previous* session ended without a clean
/// exit (crash or kill), and marks this session as running. Call once at startup,
/// after [`init`]. Pair with [`end_session`] after the run loop returns.
pub(crate) fn begin_session() -> bool {
    running_sentinel()
        .map(|s| detect_and_mark(&s))
        .unwrap_or(false)
}

/// End a session cleanly: removes the sentinel, so the next launch knows this
/// session exited normally. Call after the run loop returns.
pub(crate) fn end_session() {
    if let Some(sentinel) = running_sentinel() {
        let _ = std::fs::remove_file(sentinel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_points_at_an_app_scoped_logs_location() {
        let dir = log_dir().expect("a log dir resolves on a normal machine");
        let s = dir.to_string_lossy().to_lowercase();
        assert!(s.contains("log"), "looks like a logs path: {s}");
        assert!(s.contains("soul"), "app-scoped: {s}");
    }

    #[test]
    fn read_log_tail_returns_the_last_n_lines() {
        let dir = std::env::temp_dir().join(format!("soul-logtail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.log");
        std::fs::write(&f, "a\nb\nc\nd\ne").unwrap();

        assert_eq!(read_log_tail(&f, 2), "d\ne");
        assert_eq!(read_log_tail(&f, 10), "a\nb\nc\nd\ne");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_log_tail_of_a_missing_file_is_empty() {
        let missing = std::env::temp_dir().join("soul-does-not-exist-xyz.log");
        assert_eq!(read_log_tail(&missing, 5), "");
    }

    #[test]
    fn detect_and_mark_reports_prior_sentinel_then_persists_it() {
        let dir = std::env::temp_dir().join(format!("soul-sentinel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = dir.join("running.lock");
        std::fs::remove_file(&s).ok(); // clean slate

        assert!(!detect_and_mark(&s), "first run: no prior sentinel");
        assert!(s.exists(), "sentinel is created for this session");
        assert!(
            detect_and_mark(&s),
            "second run: the prior sentinel is seen"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

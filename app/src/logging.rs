//! File logging + panic routing. A non-blocking, daily-rolling file appender
//! (`tracing-appender`) keeps disk I/O off the audio/GPU hot paths; panics are
//! routed into the same file via a hook so a shipped, console-less build still
//! leaves a trace. Logs live in the per-OS local/state location, never the
//! roaming profile, so they don't sync across machines.
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
///
/// The target is the BINARY target's name with hyphens underscored, not the
/// package's, Cargo passes `--crate-name soul_player` for `[[bin]] name =
/// "soul-player"` even though the package is still `soul-player-rune`. Getting
/// this wrong costs nothing visible: an `EnvFilter` directive that matches no
/// target is not an error, so every one of our own debug lines would simply
/// stop appearing. `the_debug_filter_targets_this_crate` below pins it.
fn default_filter() -> &'static str {
    if cfg!(debug_assertions) {
        "soul_player=debug,info"
    } else {
        "info"
    }
}

/// Initialise file logging and route panics into the log. Keep the returned guard
/// alive for the whole process, dropping it stops the writer thread and loses
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

/// The running session's hold on the sentinel: an open handle with an exclusive
/// OS lock, released by the OS if the process dies.
static SESSION: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

/// What [`begin_at`] found.
#[derive(Debug, PartialEq, Eq)]
enum Begin {
    /// We own the sentinel; `crashed` = it was left behind by a session that
    /// never ended cleanly.
    Owned { crashed: bool },
    /// Another instance is running and holds it: not a crash, and not ours to
    /// remove.
    OtherInstance,
}

/// Crash detection via a locked session sentinel. A clean exit removes the file;
/// a crash or kill leaves it behind (the OS drops the lock with the process), so
/// finding it unlocked means the previous session died. Finding it *locked*
/// means another instance is live (Linux/macOS allow several), which must
/// neither show the crash prompt nor delete that instance's sentinel.
fn begin_at(sentinel: &Path) -> (Begin, Option<std::fs::File>) {
    let existed = sentinel.exists();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sentinel);
    let Ok(file) = file else {
        return (Begin::Owned { crashed: existed }, None);
    };
    match file.try_lock() {
        Ok(()) => (Begin::Owned { crashed: existed }, Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => (Begin::OtherInstance, None),
        // Locking unsupported here (some network filesystems): plain sentinel.
        Err(std::fs::TryLockError::Error(_)) => (Begin::Owned { crashed: existed }, Some(file)),
    }
}

/// The session sentinel file, alongside the logs.
fn running_sentinel() -> Option<PathBuf> {
    Some(log_dir()?.join("running.lock"))
}

/// Begin a session: returns whether the *previous* session ended without a clean
/// exit (crash or kill), and marks this session as running. Call once at startup,
/// after [`init`]. Pair with [`end_session`] after the run loop returns.
pub(crate) fn begin_session() -> bool {
    let Some(path) = running_sentinel() else {
        return false;
    };
    let (begin, file) = begin_at(&path);
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = file;
    match begin {
        Begin::Owned { crashed } => crashed,
        Begin::OtherInstance => {
            tracing::info!("another Soul Player instance is running");
            false
        }
    }
}

/// End a session cleanly: removes the sentinel, so the next launch knows this
/// session exited normally. Call after the run loop returns.
pub(crate) fn end_session() {
    // Only the instance that owns the sentinel removes it.
    let owned = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(file) = owned {
        drop(file); // release the lock before removing (Windows)
        if let Some(sentinel) = running_sentinel() {
            let _ = std::fs::remove_file(sentinel);
        }
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
    fn a_left_behind_sentinel_is_a_crash_but_a_held_one_is_another_instance() {
        let dir = std::env::temp_dir().join(format!("soul-sentinel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = dir.join("running.lock");
        std::fs::remove_file(&s).ok(); // clean slate

        let (first, held) = begin_at(&s);
        assert_eq!(
            first,
            Begin::Owned { crashed: false },
            "first run: no prior sentinel"
        );
        assert!(s.exists() && held.is_some(), "sentinel created and held");

        let (second, none) = begin_at(&s);
        assert_eq!(
            second,
            Begin::OtherInstance,
            "a live instance is not a crash"
        );
        assert!(
            none.is_none(),
            "the second instance doesn't own the sentinel"
        );

        drop(held); // the first instance dies without a clean exit: file stays
        let (third, _h) = begin_at(&s);
        assert_eq!(
            third,
            Begin::Owned { crashed: true },
            "a dead session's sentinel is a crash"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The debug filter names a tracing target, and a directive that matches
    /// nothing is silently ignored rather than rejected, so if the binary is
    /// ever renamed again, our own debug logging would just go quiet with no
    /// error anywhere. `module_path!()` is the same string tracing uses as the
    /// target, so comparing against its first segment pins the two together.
    #[test]
    fn the_debug_filter_targets_this_crate() {
        let crate_name = module_path!()
            .split("::")
            .next()
            .expect("module_path! always has at least one segment");

        let filter = {
            // `default_filter` branches on `debug_assertions`, and a test run
            // with `--release` takes the "info" arm, where there is no target
            // to check.
            if !cfg!(debug_assertions) {
                return;
            }
            default_filter()
        };

        let target = filter
            .split(',')
            .find_map(|d| d.split_once('=').map(|(t, _)| t))
            .expect("the debug filter has a `target=level` directive");

        assert_eq!(
            target, crate_name,
            "the debug filter targets `{target}` but this crate's tracing \
             target is `{crate_name}`; an EnvFilter directive that matches no \
             target is not an error, so this would silently drop every debug \
             line we emit"
        );
    }
}

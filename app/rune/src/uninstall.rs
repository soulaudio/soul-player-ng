//! "Uninstall Soul Player": remove the app and everything it stored on this
//! computer — settings, playlists kept with them, the library index, caches,
//! logs, saved sign-ins (OS keychain) — and never the user's music.
//!
//! Started from Settings → About (with a confirmation) or `soul-player
//! --uninstall` (scripts, the release e2e). Runs once the app has let go of its
//! files: in the quit hook, or before any window for the CLI.
//!
//! What goes, per install:
//!
//! * **macOS** — the `.app` moves to the Trash (recoverable, as Finder does),
//!   plus the per-bundle-id leftovers macOS keeps (saved window state,
//!   preferences, caches).
//! * **Windows** — the installer's own uninstaller runs silently (Start-menu
//!   entry, program files, the Apps & features entry), after this process has
//!   exited; our data folders and the notification registration go first.
//! * **Linux AppImage** — the AppImage file.
//! * **.deb / .rpm / Flatpak** — the data goes; the package itself belongs to
//!   the package manager, which needs the user's password. The confirmation
//!   says which command removes it.
//!
//! Every path removed is checked to be one of ours by name first
//! ([`ours`]), so a misresolved directory can never take a home folder with it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What removes the app itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppRemoval {
    /// macOS: move the bundle to the Trash.
    Trash(PathBuf),
    /// Windows: run the installer's uninstaller silently.
    Uninstaller(PathBuf),
    /// Linux: delete the AppImage.
    AppImage(PathBuf),
    /// A package manager owns it: the command that removes it.
    PackageManager(String),
    /// A development build: nothing installed to remove.
    Nothing,
}

/// Everything an uninstall removes.
#[derive(Clone, Debug)]
pub struct Plan {
    pub data: Vec<PathBuf>,
    /// Remote servers whose saved sign-in (keychain) goes too.
    pub sign_ins: Vec<String>,
    pub app: AppRemoval,
}

/// The data folders and files Soul Player writes, on this platform.
pub fn data_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = crate::config::soul_player_dir() {
        v.push(d);
    }
    if let Some(c) = dirs::cache_dir() {
        v.push(c.join("soul-player"));
    }
    if let Some(logs) = crate::logging::log_dir() {
        // `<state>/soul-player/logs` (Linux, Windows): the app's whole folder.
        match logs.parent() {
            Some(p) if p.file_name().is_some_and(|n| n == "soul-player") => v.push(p.into()),
            _ => v.push(logs),
        }
    }
    if cfg!(target_os = "linux")
        && let Some(d) = dirs::data_local_dir()
    {
        v.push(d.join("soul-player"));
    }
    if cfg!(target_os = "macos")
        && let Some(lib) = dirs::home_dir().map(|h| h.join("Library"))
    {
        let id = "co.soulaudio.soulplayer";
        v.push(
            lib.join("Saved Application State")
                .join(format!("{id}.savedState")),
        );
        v.push(lib.join("Preferences").join(format!("{id}.plist")));
        v.push(lib.join("Caches").join(id));
        v.push(lib.join("HTTPStorages").join(id));
    }
    v.sort();
    v.dedup();
    v
}

/// Whether `p` is unmistakably one of Soul Player's own paths: its last
/// component names the app. Guards every removal.
pub fn ours(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    p.components().count() > 2
        && (name == "soul-player"
            || name == "soul player"
            || name == "soul player.app"
            || name.starts_with("co.soulaudio.soulplayer")
            || (name.starts_with("soul-player") && name.ends_with(".appimage"))
            || name == "soul-player.appimage")
}

/// How the app itself goes, for this install.
pub fn app_removal() -> AppRemoval {
    if cfg!(debug_assertions) {
        return AppRemoval::Nothing;
    }
    if crate::in_flatpak() {
        return AppRemoval::PackageManager("flatpak uninstall co.soulaudio.soulplayer".into());
    }
    if let Some(img) = std::env::var_os("APPIMAGE") {
        return AppRemoval::AppImage(img.into());
    }
    let Ok(exe) = std::env::current_exe() else {
        return AppRemoval::Nothing;
    };
    if cfg!(target_os = "macos") {
        return crate::app_location::bundle_of(&exe).map_or(AppRemoval::Nothing, AppRemoval::Trash);
    }
    if cfg!(windows) {
        let u = exe.with_file_name("uninstall.exe");
        return if u.is_file() {
            AppRemoval::Uninstaller(u)
        } else {
            AppRemoval::Nothing
        };
    }
    if exe.starts_with("/usr") || exe.starts_with("/opt") {
        let cmd = if Path::new("/usr/bin/dnf").exists() {
            "sudo dnf remove soul-player"
        } else {
            "sudo apt remove soul-player"
        };
        return AppRemoval::PackageManager(cmd.into());
    }
    AppRemoval::Nothing
}

pub fn plan() -> Plan {
    Plan {
        data: data_paths(),
        sign_ins: crate::config::load_remotes(),
        app: app_removal(),
    }
}

/// What an uninstall did.
#[derive(Debug, Default)]
pub struct Report {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Remove the data and sign-ins now, and the app itself (on Windows, the
/// uninstaller starts and waits for this process to exit).
pub fn run(plan: &Plan) -> Report {
    let mut r = Report::default();
    let store = soul_player_host::remote::KeyringStore;
    for url in &plan.sign_ins {
        use soul_player_host::remote::TokenStore;
        let _ = store.delete_refresh(url);
    }
    #[cfg(windows)]
    remove_notification_registration();
    for p in &plan.data {
        remove(p, &mut r);
    }
    match &plan.app {
        AppRemoval::Trash(bundle) => match trash(bundle) {
            Ok(to) => {
                eprintln!("moved {} to {}", bundle.display(), to.display());
                r.removed.push(bundle.clone());
            }
            Err(e) => r.failed.push((bundle.clone(), e)),
        },
        AppRemoval::AppImage(img) => remove(img, &mut r),
        AppRemoval::Uninstaller(u) => {
            // The uninstaller can't delete a running program's folder, and
            // Windows won't delete our still-open log file: a detached shell
            // waits for this process to exit, clears what's left of our data,
            // then runs the uninstaller.
            match spawn_cmd(&windows_script(&plan.data, u)) {
                Ok(()) => r.removed.push(u.clone()),
                Err(e) => r.failed.push((u.clone(), e.to_string())),
            }
        }
        AppRemoval::PackageManager(_) | AppRemoval::Nothing => {}
    }
    r
}

/// The detached shell's script (Windows): wait for this process to exit, clear
/// what's left of our data, then run the uninstaller silently.
fn windows_script(data: &[PathBuf], uninstaller: &Path) -> String {
    let mut script = String::from("ping -n 3 127.0.0.1 >nul");
    for p in data.iter().filter(|p| ours(p)) {
        script.push_str(&format!(" & rmdir /s /q \"{}\" 2>nul", p.display()));
    }
    script.push_str(&format!(" & \"{}\" /S", uninstaller.display()));
    script
}

/// `cmd /C "<script>"` with the command line passed verbatim. The usual
/// argument quoting escapes inner quotes as `\"`, which cmd doesn't
/// understand: the quoted uninstaller path came through mangled and never ran
/// (the release e2e caught it). With /C, cmd strips the outer pair of quotes
/// and keeps the inner ones. Detached, so it outlives this process.
#[cfg(windows)]
fn spawn_cmd(script: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new("cmd")
        .raw_arg(format!("/C \"{script}\""))
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

#[cfg(not(windows))]
fn spawn_cmd(_script: &str) -> std::io::Result<()> {
    Err(std::io::Error::other("Windows only"))
}

fn remove(p: &Path, r: &mut Report) {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return; // never created
    };
    if !ours(p) {
        r.failed
            .push((p.into(), "not one of Soul Player's paths".into()));
        return;
    }
    let res = if meta.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
    match res {
        Ok(()) => r.removed.push(p.into()),
        Err(e) => r.failed.push((p.into(), e.to_string())),
    }
}

/// macOS: move `bundle` into the user's Trash, as Finder does (a name already
/// there gets a numbered suffix). Falls back to deleting it when the Trash is
/// on another volume.
fn trash(bundle: &Path) -> Result<PathBuf, String> {
    if !ours(bundle) {
        return Err("not Soul Player's bundle".into());
    }
    let bin = dirs::home_dir().ok_or("no home folder")?.join(".Trash");
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    let name = bundle
        .file_name()
        .ok_or("no bundle name")?
        .to_string_lossy();
    let stem = name.trim_end_matches(".app");
    let mut to = bin.join(&*name);
    let mut n = 2;
    while to.exists() {
        to = bin.join(format!("{stem} {n}.app"));
        n += 1;
    }
    match std::fs::rename(bundle, &to) {
        Ok(()) => Ok(to),
        Err(_) => std::fs::remove_dir_all(bundle)
            .map(|()| bundle.to_path_buf())
            .map_err(|e| e.to_string()),
    }
}

#[cfg(windows)]
fn remove_notification_registration() {
    let _ = windows_registry::CURRENT_USER.remove_tree(format!(
        r"Software\Classes\AppUserModelId\{}",
        crate::notify::AUMID
    ));
}

static PENDING: Mutex<Option<Plan>> = Mutex::new(None);

/// Uninstall when the app quits (Settings → About → Uninstall confirmed).
pub fn stage(plan: Plan) {
    // A downloaded update would otherwise install itself right back.
    crate::update::discard_staged();
    if let Ok(mut p) = PENDING.lock() {
        *p = Some(plan);
    }
}

/// Run a staged uninstall. Call after the session's last write (the quit
/// hook's flush), so nothing recreates the folders afterwards.
pub fn finish() {
    let Some(plan) = PENDING.lock().ok().and_then(|mut p| p.take()) else {
        return;
    };
    let r = run(&plan);
    for (p, e) in &r.failed {
        eprintln!("could not remove {}: {e}", p.display());
    }
}

/// `--uninstall`: remove everything now, report on stdout, exit status 0 when
/// everything that existed is gone.
pub fn cli() -> i32 {
    let plan = plan();
    let r = run(&plan);
    for p in &r.removed {
        println!("removed {}", p.display());
    }
    for (p, e) in &r.failed {
        eprintln!("could not remove {}: {e}", p.display());
    }
    if let AppRemoval::PackageManager(cmd) = &plan.app {
        println!("Soul Player's data is gone. Remove the package with: {cmd}");
    }
    i32::from(!r.failed.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_soul_player_paths_are_ever_removed() {
        for p in [
            "/Users/me/Library/Application Support/soul-player",
            "/Users/me/Library/Logs/Soul Player",
            "/Users/me/Library/Caches/co.soulaudio.soulplayer",
            "/Users/me/Library/Preferences/co.soulaudio.soulplayer.plist",
            "/Applications/Soul Player.app",
            "/home/me/Applications/Soul-Player.AppImage",
            "/home/me/Downloads/soul-player-rune_0.4.3_x86_64.AppImage",
        ] {
            assert!(ours(Path::new(p)), "{p} is ours");
        }
        for p in [
            "/",
            "/Users/me",
            "/Users/me/Library",
            "/Users/me/Music",
            "/Applications",
            "/soul-player",
            "/home/me/Applications/Other.AppImage",
            "/Users/me/Library/Caches",
        ] {
            assert!(!ours(Path::new(p)), "{p} must never be removed");
        }
    }

    #[test]
    fn the_windows_script_waits_clears_our_data_then_uninstalls() {
        let s = windows_script(
            &[
                PathBuf::from(r"C:/Users/me/AppData/Roaming/soul-player"),
                PathBuf::from(r"C:/Users/me/Music"),
            ],
            Path::new(r"C:/Users/me/AppData/Local/Soul Player/uninstall.exe"),
        );
        assert_eq!(
            s,
            r#"ping -n 3 127.0.0.1 >nul & rmdir /s /q "C:/Users/me/AppData/Roaming/soul-player" 2>nul & "C:/Users/me/AppData/Local/Soul Player/uninstall.exe" /S"#,
            "waits, removes only our folder, then runs the quoted uninstaller"
        );
    }

    #[test]
    fn every_data_path_is_ours() {
        // Under test the config folder is a per-process temp dir; the real
        // one is `<config>/soul-player`.
        let test_dir = crate::config::soul_player_dir();
        for p in data_paths()
            .into_iter()
            .filter(|p| Some(p) != test_dir.as_ref())
        {
            assert!(ours(&p), "{} would be refused", p.display());
        }
    }

    /// The real removal on this machine's temp dir: our folder goes, anything
    /// that isn't ours is refused even when asked for, and a path that never
    /// existed is neither removed nor a failure.
    #[test]
    fn run_removes_our_folders_and_refuses_anything_else() {
        let root = std::env::temp_dir().join(format!("soul_rune_uninstall_{}", std::process::id()));
        let ours_dir = root.join("data").join("soul-player");
        std::fs::create_dir_all(ours_dir.join("playlists")).unwrap();
        std::fs::write(ours_dir.join("config.toml"), "x").unwrap();
        let music = root.join("Music");
        std::fs::create_dir_all(&music).unwrap();
        std::fs::write(music.join("song.flac"), "x").unwrap();
        let plan = Plan {
            data: vec![
                ours_dir.clone(),
                music.clone(),
                root.join("data").join("soul-player-missing"),
            ],
            sign_ins: vec![],
            app: AppRemoval::Nothing,
        };
        let r = run(&plan);
        assert!(!ours_dir.exists(), "our folder is gone");
        assert!(music.join("song.flac").exists(), "music is never touched");
        assert_eq!(r.removed, vec![ours_dir]);
        assert_eq!(
            r.failed.len(),
            1,
            "the music folder is refused: {:?}",
            r.failed
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

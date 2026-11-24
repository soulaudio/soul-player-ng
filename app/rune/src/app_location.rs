//! macOS: where the app bundle runs from, and moving it into Applications.
//!
//! A Mac app opened straight from its disk image, from Downloads, or from a
//! quarantined copy that Gatekeeper translocated to a random read-only path runs
//! fine, but can't update itself: the updater replaces the bundle in place, and
//! there's either no place to write or the next launch runs somewhere else again.
//! So the app offers to move itself into Applications on launch, as LetsMove and
//! Sparkle-based apps do:
//!
//! 1. Copy the bundle with `ditto`, which keeps the code signature and resources,
//!    to `/Applications` (or `~/Applications` when that isn't writable).
//! 2. Remove the quarantine flag from the copy. The user already opened these
//!    exact bytes, and a quarantined copy that wasn't moved by Finder would be
//!    translocated again on every launch.
//! 3. Quit, start the moved copy, and eject the disk image it came from.
//!
//! "Keep Here" remembers that bundle path, so a deliberate custom location
//! isn't asked about again. A disk image or a translocated copy is always asked
//! about, because neither can be kept.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where the running bundle sits, as far as installing and updating go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// In an Applications folder: nothing to do.
    Installed,
    /// On a mounted disk image (`mount`): runs, but can't be updated in place.
    DiskImage { mount: PathBuf },
    /// Gatekeeper translocated a quarantined copy to a read-only path.
    Translocated,
    /// Somewhere else the user put it (Downloads, the Desktop): updatable, but
    /// not where a Mac app belongs. Offered once.
    Elsewhere,
}

impl Place {
    /// Whether the updater can replace the bundle where it is.
    pub fn updatable(&self) -> bool {
        matches!(self, Place::Installed | Place::Elsewhere)
    }
}

/// The `.app` bundle containing `exe`, if it is in one.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// Classify `bundle`. `mounts` are the mount points of the attached disk images
/// (`hdiutil info`), and `apps_dirs` the folders that count as installed.
pub fn classify(bundle: &Path, mounts: &[PathBuf], apps_dirs: &[PathBuf]) -> Place {
    if bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return Place::Translocated;
    }
    if let Some(mount) = mounts.iter().find(|m| bundle.starts_with(m)) {
        return Place::DiskImage {
            mount: mount.clone(),
        };
    }
    let parent = bundle.parent();
    let in_apps = apps_dirs.iter().any(|d| parent == Some(d.as_path()))
        // Any folder named Applications (a subfolder like /Applications/Audio,
        // another user's ~/Applications, a second volume's Applications).
        || bundle
            .ancestors()
            .skip(1)
            .any(|a| a.file_name().is_some_and(|n| n == "Applications"));
    if in_apps {
        Place::Installed
    } else {
        Place::Elsewhere
    }
}

/// Where a move puts the app: `SOUL_PLAYER_APPLICATIONS_DIR` (tests), else
/// `/Applications` when this user can write to it, else `~/Applications`.
pub fn applications_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SOUL_PLAYER_APPLICATIONS_DIR") {
        return PathBuf::from(d);
    }
    let system = PathBuf::from("/Applications");
    if writable(&system) {
        return system;
    }
    dirs::home_dir()
        .map(|h| h.join("Applications"))
        .unwrap_or(system)
}

/// The folders that count as installed.
fn apps_dirs() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/Applications")];
    if let Some(h) = dirs::home_dir() {
        v.push(h.join("Applications"));
    }
    if let Some(d) = std::env::var_os("SOUL_PLAYER_APPLICATIONS_DIR") {
        v.push(PathBuf::from(d));
    }
    // Compared with a resolved bundle path, so resolve these too.
    v.into_iter()
        .map(|d| std::fs::canonicalize(&d).unwrap_or(d))
        .collect()
}

/// Whether this user can create files in `dir` (std has no `access(2)`).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".soul-player-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Mount points of the attached disk images, from `hdiutil info -plist`.
fn disk_image_mounts() -> Vec<PathBuf> {
    let Ok(out) = std::process::Command::new("/usr/bin/hdiutil")
        .args(["info", "-plist"])
        .output()
    else {
        return Vec::new();
    };
    mount_points(&String::from_utf8_lossy(&out.stdout))
}

/// The `<key>mount-point</key><string>…</string>` values of a `hdiutil info
/// -plist` document. A plain scan: the keys and values are one per line, and a
/// full plist parser isn't worth a dependency for one field.
fn mount_points(plist: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut lines = plist.lines().map(str::trim);
    while let Some(l) = lines.next() {
        if l == "<key>mount-point</key>"
            && let Some(v) = lines.next()
            && let Some(s) = v
                .strip_prefix("<string>")
                .and_then(|v| v.strip_suffix("</string>"))
        {
            out.push(PathBuf::from(unescape(s)));
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The running bundle and where it is. `None` off macOS and outside a bundle
/// (a `cargo run` build).
pub fn current() -> Option<(PathBuf, Place)> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let bundle = bundle_of(&std::env::current_exe().ok()?)?;
    // Resolve symlinks (`/var` → `/private/var`, a symlinked Applications):
    // `hdiutil` reports mount points by their real path.
    let bundle = std::fs::canonicalize(&bundle).unwrap_or(bundle);
    // `hdiutil` only runs when the cheap checks can't decide.
    let quick = classify(&bundle, &[], &apps_dirs());
    let place = if quick == Place::Elsewhere {
        classify(&bundle, &disk_image_mounts(), &apps_dirs())
    } else {
        quick
    };
    Some((bundle, place))
}

/// Whether to offer the move at launch: anything but Installed, except an
/// Elsewhere bundle the user already chose to keep.
pub fn should_offer(bundle: &Path, place: &Place, kept: Option<&str>) -> bool {
    match place {
        Place::Installed => false,
        Place::Elsewhere => kept != Some(&*bundle.to_string_lossy()),
        Place::DiskImage { .. } | Place::Translocated => true,
    }
}

/// What to do once the app has quit after a move: start the moved copy, and
/// eject the disk image the old one ran from.
struct Pending {
    exe: PathBuf,
    eject: Option<PathBuf>,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// Copy `bundle` into [`applications_dir`], replacing an older copy there, and
/// arrange for the moved copy to start when this one quits. Returns the new
/// bundle path. The caller quits the app next.
pub fn move_to_applications(bundle: &Path, place: &Place) -> Result<PathBuf, String> {
    let dir = applications_dir();
    // Never "move" a bundle onto itself: that would relaunch it into the same
    // place, which would offer the move again, forever.
    let resolved_dir = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    if bundle.parent() == Some(resolved_dir.as_path()) {
        return Err(format!("it is already in {}", dir.display()));
    }
    // The executable's place inside the bundle, worked out before anything is
    // copied. Both sides resolved: `bundle` came through `canonicalize`.
    let rel = std::env::current_exe()
        .ok()
        .and_then(|e| std::fs::canonicalize(e).ok())
        .and_then(|e| e.strip_prefix(bundle).ok().map(Path::to_path_buf))
        .ok_or("the app's executable isn't inside its bundle")?;
    let dest = move_into(bundle, &dir)?;
    let exe = dest.join(rel);
    let eject = match place {
        Place::DiskImage { mount } => Some(mount.clone()),
        _ => None,
    };
    tracing::info!(from = %bundle.display(), to = %dest.display(), "moved to Applications");
    if let Ok(mut p) = PENDING.lock() {
        *p = Some(Pending { exe, eject });
    }
    Ok(dest)
}

/// Copy `bundle` into `dir` (unquarantined), replacing a copy already there.
fn move_into(bundle: &Path, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = bundle.file_name().ok_or("the app has no bundle name")?;
    let dest = dir.join(name);
    let pid = std::process::id();
    let staging = dir.join(format!(".{}.moving-{pid}", name.to_string_lossy()));
    let _ = std::fs::remove_dir_all(&staging);
    run("/usr/bin/ditto", &[bundle.as_os_str(), staging.as_os_str()])?;
    // A missing attribute is fine; the copy only needs to end up without it.
    let _ = std::process::Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&staging)
        .output();
    if dest.exists() {
        // Swap the older copy out, then the new one in, so a failure never
        // leaves the user with no app at all.
        let old = dir.join(format!(".{}.old-{pid}", name.to_string_lossy()));
        std::fs::rename(&dest, &old).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            format!("{}: {e}", dest.display())
        })?;
        if let Err(e) = std::fs::rename(&staging, &dest) {
            let _ = std::fs::rename(&old, &dest);
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("{}: {e}", dest.display()));
        }
        let _ = std::fs::remove_dir_all(&old);
    } else {
        std::fs::rename(&staging, &dest).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            format!("{}: {e}", dest.display())
        })?;
    }
    Ok(dest)
}

fn run(cmd: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// After a move, as the app quits: start the moved copy, and eject the disk
/// image once this process has let go of it. A no-op without a move.
pub fn finish_move() {
    let Some(Pending { exe, eject }) = PENDING.lock().ok().and_then(|mut p| p.take()) else {
        return;
    };
    // The moved copy is installed: it must not act on the request again.
    match std::process::Command::new(&exe)
        .env_remove("SOUL_PLAYER_MOVE_TO_APPLICATIONS")
        .spawn()
    {
        Ok(child) => tracing::info!(pid = child.id(), "started the moved app"),
        Err(e) => tracing::error!("moved, but starting {} failed: {e}", exe.display()),
    }
    if let Some(mount) = eject {
        // A detached shell outlives us; the image is busy until this process
        // has exited, so it waits a moment and retries.
        let spawned = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                r#"for i in 1 2 3 4 5 6 7 8 9 10; do sleep 1; /usr/sbin/diskutil eject "$0" >/dev/null 2>&1 && exit 0; done"#,
            ])
            .arg(&mount)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Err(e) = spawned {
            tracing::warn!("could not eject {}: {e}", mount.display());
        }
    }
}

/// `SOUL_PLAYER_MOVE_TO_APPLICATIONS`: accept the move at launch without asking
/// (the release e2e drives the move with it).
pub fn move_unattended() -> bool {
    std::env::var_os("SOUL_PLAYER_MOVE_TO_APPLICATIONS").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn the_bundle_is_found_from_its_executable() {
        assert_eq!(
            bundle_of(&p(
                "/Applications/Soul Player.app/Contents/MacOS/soul-player-rune"
            )),
            Some(p("/Applications/Soul Player.app"))
        );
        assert_eq!(bundle_of(&p("/usr/local/bin/soul-player-rune")), None);
    }

    #[test]
    fn places_are_told_apart() {
        let apps = [p("/Applications"), p("/Users/me/Applications")];
        let mounts = [p("/Volumes/Soul Player"), p("/private/tmp/e2e/mnt")];
        let at = |b: &str| classify(&p(b), &mounts, &apps);
        assert_eq!(at("/Applications/Soul Player.app"), Place::Installed);
        assert_eq!(
            at("/Users/me/Applications/Soul Player.app"),
            Place::Installed
        );
        assert_eq!(at("/Applications/Audio/Soul Player.app"), Place::Installed);
        assert_eq!(
            at("/Volumes/Soul Player/Soul Player.app"),
            Place::DiskImage {
                mount: p("/Volumes/Soul Player")
            }
        );
        // A disk image mounted anywhere, not only under /Volumes.
        assert_eq!(
            at("/private/tmp/e2e/mnt/Soul Player.app"),
            Place::DiskImage {
                mount: p("/private/tmp/e2e/mnt")
            }
        );
        assert_eq!(
            at("/private/var/folders/x/T/AppTranslocation/ABC-123/d/Soul Player.app"),
            Place::Translocated
        );
        assert_eq!(at("/Users/me/Downloads/Soul Player.app"), Place::Elsewhere);
        // A volume that merely starts with a mount's name isn't on it.
        assert_eq!(
            at("/Volumes/Soul Player 2/Soul Player.app"),
            Place::Elsewhere
        );
        assert!(!Place::Translocated.updatable());
        assert!(Place::Elsewhere.updatable());
    }

    #[test]
    fn keep_here_silences_a_custom_folder_but_never_a_disk_image() {
        let b = p("/Users/me/Downloads/Soul Player.app");
        let kept = Some("/Users/me/Downloads/Soul Player.app");
        assert!(should_offer(&b, &Place::Elsewhere, None));
        assert!(!should_offer(&b, &Place::Elsewhere, kept));
        assert!(should_offer(
            &b,
            &Place::Elsewhere,
            Some("/Users/me/Desktop/Soul Player.app")
        ));
        let dmg = Place::DiskImage {
            mount: p("/Volumes/Soul Player"),
        };
        assert!(should_offer(
            &p("/Volumes/Soul Player/Soul Player.app"),
            &dmg,
            kept
        ));
        assert!(!should_offer(&b, &Place::Installed, None));
    }

    #[test]
    fn hdiutil_mount_points_are_read_from_its_plist() {
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>images</key>
	<array>
		<dict>
			<key>system-entities</key>
			<array>
				<dict>
					<key>dev-entry</key>
					<string>/dev/disk4s1</string>
					<key>mount-point</key>
					<string>/Volumes/Soul Player &amp; Co</string>
				</dict>
				<dict>
					<key>dev-entry</key>
					<string>/dev/disk4</string>
				</dict>
			</array>
		</dict>
	</array>
</dict>
</plist>"#;
        assert_eq!(mount_points(plist), vec![p("/Volumes/Soul Player & Co")]);
        assert!(mount_points("").is_empty());
    }

    /// The real move on this Mac: a stand-in bundle on a real disk image is
    /// copied into an Applications folder, keeps its contents, loses the
    /// quarantine flag, and replaces an older copy already there.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bundle_moves_off_a_disk_image_into_applications() {
        let root = std::env::temp_dir().join(format!("soul_rune_move_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let work = Cleanup(root);
        let work = &work;
        let src = work.0.as_path().join("src/Soul Player.app/Contents/MacOS");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("soul-player-rune"), b"new").unwrap();
        let apps = work.0.as_path().join("Applications");
        let old = apps.join("Soul Player.app/Contents/MacOS");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("soul-player-rune"), b"old").unwrap();
        let bundle = work.0.as_path().join("src/Soul Player.app");
        let _ = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", "0081;00000000;Safari;"])
            .arg(&bundle)
            .status();

        let dest = move_into(&bundle, &apps).unwrap();
        assert_eq!(dest, apps.join("Soul Player.app"));
        assert_eq!(
            std::fs::read(dest.join("Contents/MacOS/soul-player-rune")).unwrap(),
            b"new"
        );
        let attrs = std::process::Command::new("/usr/bin/xattr")
            .arg(&dest)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&attrs.stdout).contains("quarantine"),
            "the moved copy is still quarantined"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&apps)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("Soul Player.app")]);
    }
}

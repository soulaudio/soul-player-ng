//! The packaging metadata has to agree with itself across four files, and
//! nothing in the build enforces that — a mismatch produces packages that
//! install fine and then misbehave in ways no unit test can see.
//!
//! The identity chain is the sharp edge. `app_id`, the MPRIS bus-name suffix
//! and the installed desktop entry's basename must be ONE string, or the
//! desktop cannot resolve a window (or a media player) to this app's icon. The
//! app resolves that at runtime from `FLATPAK_ID`, falling back to the binary
//! name — so the Flatpak manifest's `app-id`, the AppStream `<id>`, and the
//! packager `identifier` all have to be the same value, written in three
//! different files by three different tools.
//!
//! These are string comparisons against files on disk rather than anything
//! clever, because the failure they guard against is exactly someone editing
//! one file and not the others.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Pull a `key = "value"` out of a TOML line, ignoring comments. Deliberately
/// not a TOML parser: adding a dependency to the test binary to read two
/// strings would cost more than it is worth.
fn toml_string(src: &str, key: &str) -> Option<String> {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix(key)?.trim_start();
            let rest = rest.strip_prefix('=')?.trim();
            rest.strip_prefix('"')?
                .split('"')
                .next()
                .map(str::to_string)
        })
}

/// The one string the whole desktop identity hangs off. If these three drift,
/// the Flatpak silently loses its MPRIS name (Flatpak's D-Bus policy grants
/// exactly `org.mpris.MediaPlayer2.$FLATPAK_ID` and nothing else), so media
/// keys and the desktop's media controls stop seeing the player while the app
/// otherwise looks healthy.
#[test]
fn the_app_id_is_the_same_string_everywhere_it_is_written() {
    let root = repo_root();
    let cargo = read(&root.join("app/rune/Cargo.toml"));
    let identifier = toml_string(&cargo, "identifier")
        .expect("[package.metadata.packager] identifier is set in app/rune/Cargo.toml");

    let manifest = read(&root.join(format!("packaging/flatpak/{identifier}.yml")));
    let app_id = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("app-id:"))
        .map(|v| v.trim().to_string())
        .expect("the Flatpak manifest declares an app-id");
    assert_eq!(
        app_id, identifier,
        "the Flatpak app-id must equal the packager identifier: Flatpak installs \
         the desktop entry as `$FLATPAK_ID.desktop` and grants only \
         `org.mpris.MediaPlayer2.$FLATPAK_ID`"
    );

    let metainfo = read(&root.join(format!("packaging/flatpak/{identifier}.metainfo.xml")));
    assert!(
        metainfo.contains(&format!("<id>{identifier}</id>")),
        "the AppStream <id> must match too — it is what links the metadata to \
         the installed app"
    );
    assert!(
        metainfo.contains(&format!(
            "<launchable type=\"desktop-id\">{identifier}.desktop</launchable>"
        )),
        "the AppStream launchable must name the desktop entry Flatpak installs"
    );

    // The manifest's `command` is what Flatpak execs, so it has to be the real
    // binary name — not the app id, which is everything else here.
    let command = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("command:"))
        .map(|v| v.trim().to_string())
        .expect("the Flatpak manifest declares a command");
    // `CARGO_BIN_NAME` is not set for an integration-test target, so this reads
    // the name off the binary path Cargo does provide — which is the same thing
    // the session e2e does, and for the same reason.
    let bin_name = Path::new(env!("CARGO_BIN_EXE_soul-player-rune"))
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("the built binary has a name");
    assert_eq!(
        command, bin_name,
        "the Flatpak `command` runs the binary, so it is the binary's name"
    );
}

/// AppStream rejects a double hyphen inside an XML comment — it is illegal XML
/// — and Flathub runs that validator as a submission gate. This caught a real
/// mistake (a `-`-prefixed flag name written inside a comment), and the fix is
/// invisible enough that it would come back.
#[test]
fn the_appstream_metadata_has_no_illegal_double_hyphens() {
    let root = repo_root();
    let path = root.join("packaging/flatpak/co.datadir.soulplayer.metainfo.xml");
    let src = read(&path);

    let mut rest = src.as_str();
    let mut offenders = Vec::new();
    while let Some(start) = rest.find("<!--") {
        let after = &rest[start + 4..];
        let end = after
            .find("-->")
            .unwrap_or_else(|| panic!("unterminated XML comment in {}", path.display()));
        let body = &after[..end];
        if body.contains("--") {
            offenders.push(body.trim().lines().next().unwrap_or("").to_string());
        }
        rest = &after[end + 3..];
    }
    assert!(
        offenders.is_empty(),
        "`--` inside an XML comment is illegal XML and `appstreamcli validate` \
         rejects the whole file, which blocks Flathub submission. Offending \
         comment(s) begin: {offenders:?}"
    );
}

/// The .deb ships files that have to exist in the tree, and a missing one is a
/// build failure deep inside cargo-packager rather than anything readable.
#[test]
fn the_deb_extra_files_exist() {
    let root = repo_root();
    assert!(
        root.join("packaging/deb/copyright").is_file(),
        "Debian policy requires a copyright file; cargo-packager copies this one"
    );
    // The changelog is GENERATED per build (so it cannot go stale against the
    // version), which is why only its directory is expected to be reachable.
    assert!(
        root.join("packaging/deb").is_dir(),
        "the deb metadata directory must exist"
    );
    assert!(
        root.join("packaging/build-linux.sh").is_file(),
        "the Linux packaging entry point must exist — release CI calls it"
    );
    assert!(
        root.join("packaging/stage-linux.sh").is_file(),
        "the RPM and Flatpak take their desktop entry and icon from here"
    );
}

/// The release workflow's `PLATFORMS` count and its `build` matrix have to
/// agree: `PLATFORMS` is what `latest.json` is checked against before a release
/// is published, and the matrix is what actually produces the entries.
///
/// A mismatch IS caught at release time — the manifest check fails — but a
/// release run takes half an hour and is rare, so catching it here instead
/// turns a late, expensive failure into an immediate one. The specific accident
/// this guards is adding a platform to the matrix and forgetting the count,
/// which would block the release at the very last gate.
#[test]
fn the_release_platform_count_matches_the_build_matrix() {
    let src = read(&repo_root().join(".github/workflows/release.yml"));

    let declared: usize = src
        .lines()
        .find_map(|l| l.trim().strip_prefix("PLATFORMS:"))
        .map(|v| v.trim().trim_matches('"').to_string())
        .expect("release.yml declares PLATFORMS")
        .parse()
        .expect("PLATFORMS is a number");

    // Count the `target_key:` entries inside the `build` job's matrix: each one
    // is a platform the release produces. Bounded by the next job so the `e2e`
    // matrix (which mirrors it) is not counted twice.
    let build = src
        .split_once("\n  build:")
        .expect("release.yml has a build job")
        .1;
    let build = build.split_once("\n  e2e:").map_or(build, |(b, _)| b);
    let legs = build
        .lines()
        .filter(|l| l.trim().starts_with("target_key:"))
        .count();

    assert_eq!(
        declared, legs,
        "release.yml declares PLATFORMS={declared} but the build matrix has \
         {legs} legs; `latest.json` is verified against PLATFORMS before a \
         release publishes, so these must agree"
    );
}

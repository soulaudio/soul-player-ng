//! The packaging metadata has to agree with itself across four files, and
//! nothing in the build enforces that, a mismatch produces packages that
//! install fine and then misbehave in ways no unit test can see.
//!
//! The identity chain is the sharp edge. `app_id`, the MPRIS bus-name suffix
//! and the installed desktop entry's basename must be ONE string, or the
//! desktop cannot resolve a window (or a media player) to this app's icon. The
//! app resolves that at runtime from `FLATPAK_ID`, falling back to the binary
//! name, so the Flatpak manifest's `app-id`, the AppStream `<id>`, and the
//! packager `identifier` all have to be the same value, written in three
//! different files by three different tools.
//!
//! These are string comparisons against files on disk rather than anything
//! clever, because the failure they guard against is exactly someone editing
//! one file and not the others.

use std::path::{Path, PathBuf};

/// The cargo-packager version every assertion in this file describes.
///
/// This tool decides the `.deb`'s filename and `Package:` field, which config
/// keys are accepted, and the NSIS template that carries our preinstall hook.
/// All of that was read out of one specific version's source, so a release that
/// installs a different one is building with behaviour nothing here has checked.
/// `the_pinned_packager_version_matches_what_the_tests_assume` ties the two
/// together; bump both at once, deliberately.
const PACKAGER_VERSION: &str = "0.11.8";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
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
    let cargo = read(&root.join("app/Cargo.toml"));
    let identifier = toml_string(&cargo, "identifier")
        .expect("[package.metadata.packager] identifier is set in app/Cargo.toml");

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
        "the AppStream <id> must match too, it is what links the metadata to \
         the installed app"
    );
    assert!(
        metainfo.contains(&format!(
            "<launchable type=\"desktop-id\">{identifier}.desktop</launchable>"
        )),
        "the AppStream launchable must name the desktop entry Flatpak installs"
    );

    // The manifest's `command` is what Flatpak execs, so it has to be the real
    // binary name, not the app id, which is everything else here.
    let command = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("command:"))
        .map(|v| v.trim().to_string())
        .expect("the Flatpak manifest declares a command");
    // `CARGO_BIN_NAME` is not set for an integration-test target, so this reads
    // the name off the binary path Cargo does provide, which is the same thing
    // the session e2e does, and for the same reason.
    let bin_name = Path::new(env!("CARGO_BIN_EXE_soul-player"))
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("the built binary has a name");
    assert_eq!(
        command, bin_name,
        "the Flatpak `command` runs the binary, so it is the binary's name"
    );
}

/// AppStream rejects a double hyphen inside an XML comment, it is illegal XML,
/// and Flathub runs that validator as a submission gate. This caught a real
/// mistake (a `-`-prefixed flag name written inside a comment), and the fix is
/// invisible enough that it would come back.
#[test]
fn the_appstream_metadata_has_no_illegal_double_hyphens() {
    let root = repo_root();
    let path = root.join("packaging/flatpak/co.soulaudio.soulplayer.metainfo.xml");
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

/// Top-level keys of a `[section]` in a TOML file, skipping comments and
/// anything nested inside an array or inline table (`depends = [ ... ]` spans
/// many lines, and its entries are not keys).
fn section_keys(src: &str, section: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut inside = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with('[') && depth == 0 {
            inside = t.starts_with(&format!("[{section}]"));
            continue;
        }
        if !inside || t.starts_with('#') {
            continue;
        }
        if depth == 0
            && let Some((k, _)) = t.split_once('=')
        {
            let k = k.trim();
            if !k.is_empty() && !k.starts_with('"') {
                keys.push(k.to_string());
            }
        }
        depth += (t.matches(['[', '{']).count() as i32) - (t.matches([']', '}']).count() as i32);
    }
    keys
}

/// cargo-packager deserializes `[package.metadata.packager]` with its OWN
/// types, and `cargo metadata` / `cargo build` never look at it, so an
/// unsupported key is not a manifest error, it is a packaging failure that only
/// appears once CI has already spent minutes on a release build.
///
/// This caught nothing in review and everything in CI: `conflicts` was added
/// here from the provides/conflicts/replaces trio in cargo-packager's config
/// module, which turns out to belong to its PACMAN config, `DebianConfig` has
/// no such fields, so the build died with ``unknown field `conflicts` `` after
/// a 5-minute compile. (Those three now live in `packaging/build-linux.sh`,
/// which patches them into the built package's control file.)
#[test]
fn the_deb_packager_config_uses_only_fields_cargo_packager_accepts() {
    let cargo = read(&repo_root().join("app/Cargo.toml"));

    // `DebianConfig` in cargo-packager 0.11.8. Serde accepts three spellings of
    // the multi-word ones, so all are listed. Update this when the dependency
    // is bumped, the error message names the full set.
    const ACCEPTED: &[&str] = &[
        "depends",
        "desktop-template",
        "desktopTemplate",
        "desktop_template",
        "section",
        "priority",
        "files",
        "package-name",
        "packageName",
        "package_name",
    ];

    let unsupported: Vec<_> = section_keys(&cargo, "package.metadata.packager.deb")
        .into_iter()
        .filter(|k| !ACCEPTED.contains(&k.as_str()))
        .collect();

    assert!(
        unsupported.is_empty(),
        "[package.metadata.packager.deb] has key(s) cargo-packager's DebianConfig \
         does not accept: {unsupported:?}. It rejects the whole config at package \
         time: AFTER the release build, so this never shows up in `cargo build`. \
         Accepted: {ACCEPTED:?}"
    );
}

/// Same trap, the Windows half. `preinstall-section` is what removes the
/// pre-rename `soul-player-rune.exe`, and a typo in that key would not fail the
/// build, cargo-packager would reject the config, or (worse, if it were simply
/// ignored) the installer would silently stop cleaning up and every Windows
/// upgrade would leave two executables behind.
#[test]
fn the_nsis_packager_config_uses_only_fields_cargo_packager_accepts() {
    let cargo = read(&repo_root().join("app/Cargo.toml"));

    // `NsisConfig` in cargo-packager 0.11.8, in the three spellings serde takes.
    const ACCEPTED: &[&str] = &[
        "compression",
        "template",
        "preinstall-section",
        "preinstallSection",
        "preinstall_section",
        "header-image",
        "headerImage",
        "header_image",
        "sidebar-image",
        "sidebarImage",
        "sidebar_image",
        "installer-icon",
        "installerIcon",
        "installer_icon",
        "install-mode",
        "installMode",
        "install_mode",
        "languages",
        "custom-language-files",
        "customLanguageFiles",
        "custom_language_files",
        "display-language-selector",
        "displayLanguageSelector",
        "display_language_selector",
        "appdata-paths",
        "appdataPaths",
        "appdata_paths",
    ];

    let unsupported: Vec<_> = section_keys(&cargo, "package.metadata.packager.nsis")
        .into_iter()
        .filter(|k| !ACCEPTED.contains(&k.as_str()))
        .collect();

    assert!(
        unsupported.is_empty(),
        "[package.metadata.packager.nsis] has key(s) cargo-packager's NsisConfig \
         does not accept: {unsupported:?}. Accepted: {ACCEPTED:?}"
    );
}

/// The pre-rename executable has to actually be named in the NSIS hook, or
/// Windows upgrades keep both binaries. Easy to lose in a later edit of that
/// embedded script, and nothing else would notice until a release e2e ran.
#[test]
fn the_nsis_hook_deletes_the_pre_rename_executable() {
    let cargo = read(&repo_root().join("app/Cargo.toml"));
    let hook = cargo
        .split_once("preinstall-section = \'\'\'")
        .expect("[package.metadata.packager.nsis] sets preinstall-section")
        .1
        .split_once("\'\'\'")
        .expect("the preinstall-section literal is terminated")
        .0;

    assert!(
        hook.contains("Delete") && hook.contains("soul-player-rune.exe"),
        "the NSIS preinstall section must delete the pre-rename \
         soul-player-rune.exe; a silent or passive install never runs the \
         previous uninstaller (that lives in a skipped page), so without this \
         the install directory keeps both executables. Hook body: {hook:?}"
    );
    assert!(
        hook.contains("$INSTDIR"),
        "the Delete must be anchored to $INSTDIR, not a bare filename: {hook:?}"
    );
}

/// `cargo install` without `--version` installs whatever is newest, and
/// `--locked` does not change that, it only respects the installed crate's own
/// lockfile. So the release workflow floated across cargo-packager versions
/// while this file asserted one version's behaviour, which is how a surprise
/// gets to be a surprise: the `Package:` field derivation, the accepted config
/// keys and the NSIS template are all version-dependent.
#[test]
fn the_pinned_packager_version_matches_what_the_tests_assume() {
    let src = read(&repo_root().join(".github/workflows/release.yml"));

    let pins: Vec<&str> = src
        .lines()
        .filter(|l| l.contains("cargo install cargo-packager"))
        .collect();
    assert!(
        !pins.is_empty(),
        "release.yml no longer installs cargo-packager, has packaging moved?"
    );

    for line in &pins {
        assert!(
            line.contains(&format!("--version {PACKAGER_VERSION}")),
            "release.yml installs cargo-packager without pinning {PACKAGER_VERSION}: \
             `{}`. Unpinned, a release builds with whatever version is newest, \
             while this file's assertions describe {PACKAGER_VERSION}, the .deb \
             filename and Package: derivation, the accepted config keys, and the \
             NSIS template our preinstall hook relies on. If you are bumping it \
             on purpose, update PACKAGER_VERSION and re-check those assumptions \
             against the new source.",
            line.trim()
        );
    }
}

/// The Flathub build installs the Rust toolchain from a dated archive, and
/// `app/rust-toolchain.toml` pins the nightly everything else uses. Those
/// are two separate strings naming the same compiler, and nothing connected
/// them: bumping the toolchain would leave the Flathub build quietly compiling
/// with the old one, a different compiler than every other package gets, which
/// either fails strangely or, worse, succeeds.
#[test]
fn the_flathub_toolchain_matches_the_pinned_nightly() {
    let root = repo_root();
    let toolchain = read(&root.join("app/rust-toolchain.toml"));
    let manifest = read(&root.join("packaging/flathub/co.soulaudio.soulplayer.yml"));

    let channel = toolchain
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel ="))
        .map(|v| v.trim().trim_matches('"').to_string())
        .expect("rust-toolchain.toml sets a channel");
    let date = channel
        .strip_prefix("nightly-")
        .unwrap_or_else(|| panic!("expected a dated nightly, got `{channel}`"));

    // Every toolchain URL in the manifest, one per architecture.
    let urls: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("static.rust-lang.org/dist/"))
        .filter(|l| l.starts_with("url:"))
        .collect();
    assert_eq!(
        urls.len(),
        2,
        "expected one toolchain archive per architecture, found {urls:?}"
    );

    for url in &urls {
        assert!(
            url.contains(&format!("/dist/{date}/")),
            "the Flathub manifest fetches a toolchain for a different date than \
             rust-toolchain.toml pins ({channel}): `{url}`. The Flathub build \
             would compile with a compiler no other package uses. Update the URL \
             AND its sha256: the checksums are published alongside each archive \
             as `<url>.sha256`."
        );
    }
}

/// Software centres (GNOME Software, KDE Discover) render the AppStream
/// `<releases>` list as the app's "What's new", and nothing regenerated it, so
/// it advertised 0.4.0 while 0.4.5 was published, showing every Flatpak user
/// release notes five versions old. Validation does not catch this: a metainfo
/// with stale releases is perfectly valid AppStream.
///
/// Tied to the shipped version rather than to "some entry exists", because the
/// failure mode is specifically *falling behind*. `packaging/flatpak/
/// metainfo-release.py --version X --date Y --notes-from-tag vX` writes the
/// entry, taking the text from the same generator that fills `latest.json`, so
/// the updater and the software centres cannot disagree.
#[test]
fn the_metainfo_lists_the_version_being_shipped() {
    let root = repo_root();
    let cargo = read(&root.join("app/Cargo.toml"));
    let version = cargo
        .lines()
        .find_map(|l| l.trim().strip_prefix("version = \""))
        .and_then(|v| v.split('"').next())
        .expect("app/Cargo.toml has a version");

    let metainfo = read(&root.join("packaging/flatpak/co.soulaudio.soulplayer.metainfo.xml"));
    let needle = format!("<release version=\"{version}\"");

    // What it currently claims is the newest, for a message that says what to fix.
    let newest = metainfo
        .split_once("<release version=\"")
        .and_then(|(_, rest)| rest.split_once('"').map(|(v, _)| v.to_string()))
        .unwrap_or_else(|| "none".to_string());

    assert!(
        metainfo.contains(&needle),
        "the metainfo has no <release> entry for {version} (its newest is \
         {newest}), so software centres would show {newest}'s notes for a \
         {version} build. Add one with:\n  packaging/flatpak/metainfo-release.py \
         --version {version} --date $(date -u +%Y-%m-%d) --notes-from-tag v{version}"
    );
}

/// Pull every `- --flag` line out of a manifest's `finish-args:` block.
fn finish_args(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("finish-args:") {
            inside = true;
            continue;
        }
        if inside {
            if let Some(flag) = t.strip_prefix("- --") {
                out.push(format!("--{flag}"));
            } else if !t.is_empty() && !t.starts_with('#') {
                break; // the next top-level key ends the block
            }
        }
    }
    out.sort();
    out
}

/// There are two Flatpak manifests and they must differ in exactly one respect.
///
/// `packaging/flatpak/` installs the already-tested binary and produces the
/// `.flatpak` bundle on each release. `packaging/flathub/` builds from source,
/// because Flathub requires it ("All source available submissions must be built
/// entirely from source code") and prebuilt `extra-data` is reserved for
/// non-redistributable content, which Apache-2.0 is not.
///
/// Everything a USER experiences has to match, or the bundle and the Flathub
/// build become two different applications wearing one name: the same app id
/// (which decides the MPRIS name and the desktop entry), the same runtime, and
/// above all the same sandbox permissions. A `--device=all` that silently
/// existed in one and not the other would mean bit-perfect output worked for
/// some users and not others, with nothing to explain why.
#[test]
fn the_two_flatpak_manifests_agree() {
    let root = repo_root();
    let bundle = read(&root.join("packaging/flatpak/co.soulaudio.soulplayer.yml"));
    let flathub = read(&root.join("packaging/flathub/co.soulaudio.soulplayer.yml"));

    for key in [
        "app-id:",
        "runtime:",
        "runtime-version:",
        "sdk:",
        "command:",
    ] {
        let pick = |src: &str| {
            src.lines()
                .find_map(|l| l.trim().strip_prefix(key))
                .map(|v| v.trim().to_string())
        };
        assert_eq!(
            pick(&bundle),
            pick(&flathub),
            "the bundle and Flathub manifests disagree about `{key}`. They build \
             the same application and must present the same identity."
        );
    }

    let bundle_args = finish_args(&bundle);
    let flathub_args = finish_args(&flathub);

    // A parser that found nothing would make the comparison below pass for the
    // wrong reason, which is the quiet way a guard like this stops working.
    assert!(
        bundle_args.len() >= 5 && bundle_args.contains(&"--device=all".to_string()),
        "finish-args parsing is broken: got {bundle_args:?} from the bundle manifest"
    );

    assert_eq!(
        bundle_args, flathub_args,
        "the two manifests grant different sandbox permissions. Whichever one a \
         user installed would then behave differently, and `--device=all` is \
         what bit-perfect exclusive output depends on."
    );
}

/// The Flathub build cannot run cargo-packager (not vendored, and its builders
/// have no network), so its desktop entry is committed instead of generated.
/// That makes it a second source of truth for the app's name and category, and
/// this keeps it from drifting from the one cargo-packager derives from
/// Cargo.toml.
#[test]
fn the_flathub_desktop_entry_matches_the_packaged_one() {
    let root = repo_root();
    let cargo = read(&root.join("app/Cargo.toml"));
    let desktop = read(&root.join("packaging/flathub/co.soulaudio.soulplayer.desktop"));

    let field = |src: &str, k: &str| {
        src.lines()
            .find_map(|l| l.trim().strip_prefix(k))
            .map(|v| v.trim().to_string())
    };

    let product = toml_string(&cargo, "product-name").expect("packager product-name");
    assert_eq!(
        field(&desktop, "Name="),
        Some(product.clone()),
        "the desktop entry's Name must be the packager's product-name ({product})"
    );

    // `description` under [package.metadata.packager] is the user-facing one,
    // cargo-packager puts it in `Comment=` for every other Linux package.
    let section = cargo
        .split_once("[package.metadata.packager]")
        .expect("packager section")
        .1;
    let description = toml_string(section, "description").expect("packager description");
    assert_eq!(
        field(&desktop, "Comment="),
        Some(description.clone()),
        "the desktop entry's Comment must be the packager's description"
    );

    // Icon and the installed filename are the app id, which is Flatpak's rule
    // rather than a choice: it installs the entry as `$FLATPAK_ID.desktop` and
    // resolves the icon under the same name.
    let app_id = toml_string(&cargo, "identifier").expect("packager identifier");
    assert_eq!(
        field(&desktop, "Icon="),
        Some(app_id.clone()),
        "inside Flatpak the icon must be named for the app id, not the binary"
    );
    assert_eq!(
        field(&desktop, "Exec="),
        Some("soul-player".to_string()),
        "Exec runs the binary, which is what the manifest's `command:` starts"
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
        "the Linux packaging entry point must exist, release CI calls it"
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
/// A mismatch IS caught at release time, the manifest check fails, but a
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

//! END-TO-END **format matrix**, across BOTH source kinds.
//!
//! Every container the decoders claim to support is driven twice over the same
//! bytes, once as a **local watched folder** and once as a **remote
//! `soul-server` source**, and the two are then required to agree sample for
//! sample. That cross-check is the point of the file: a per-format local test
//! can only prove "the decoder decodes it", and a remote test over one or two
//! formats can only prove "the wire protocol works for those". Together, per
//! format, they prove the whole path, scan, tag extraction, `.soulidx` bake,
//! HTTP sync, Range-streamed decode, is format-agnostic, and that nothing
//! about going over the wire changes a single decoded sample.
//!
//! The corpus is the **Aria** fixtures (`testdata/meta.*`): a 1.5 s excerpt of
//! Kimiko Ishizaka's recording of the Aria from Bach's Goldberg Variations
//! BWV 988, CC0 / public domain via the Open Goldberg project, muxed into every
//! supported container from one PCM master by `testdata/generate-meta.sh`. Real
//! music rather than a synthetic tone, because tags and musical structure are
//! what a library scan and a browse are actually judged on, and because one
//! shared master is what makes the local-vs-remote sample comparison meaningful.
//!
//! What this file deliberately does NOT do: measure signal fidelity. Whether a
//! decode is *correct* (post-seek cross-correlation, SNR vs a reference decode,
//! DSD decimation rates) is `soul-player-backend`'s
//! `signal_e2e` / `seek_audit` / `signal_path_matrix` job over the measurable
//! `tone.*` fixtures. Here the question is only coverage and agreement: does
//! every format reach the library, keep its tags, and decode identically from
//! either source.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use soul_audio_core::Decoder;
use soul_player_backend::library::{Library, TrackSource};
use soul_player_backend::types::TrackView;
use soul_player_host::PlayerHandle;
use soul_player_host::remote::{DownloadCache, MediaFetch, RemoteLibrary, RemoteServer};

mod support;
use support::{oauth_login, start_server_serving, testdata_dir};

/// The tags every Aria fixture carries, as written by `generate-meta.sh`.
const ARIA: Tags = Tags {
    title: "Aria",
    artist: "Kimiko Ishizaka",
    album: "Open Goldberg Variations",
};
/// The tags the synthetic `tagged.*` fixtures carry, used for MPEG Layer I,
/// the one format with no real-music variant (see `Case::fixture` below).
const SYNTHETIC: Tags = Tags {
    title: "Soul Test Track",
    artist: "Test Artist",
    album: "Test Album",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tags {
    title: &'static str,
    artist: &'static str,
    album: &'static str,
}

struct Case {
    /// The extension the file is given in the library, this, not the committed
    /// fixture name, is what the scanner's `SUPPORTED` check and the Tracks
    /// table's FORMAT badge key off.
    ext: &'static str,
    /// The committed `testdata/` fixture supplying the bytes.
    fixture: &'static str,
    /// The FORMAT badge the Tracks table should show. Usually the uppercased
    /// extension, but `format_label` deliberately ALIASES a few to the label
    /// the old app showed, `oga` -> `OGG`, `aif` -> `AIFF`, so it is spelled
    /// out per case rather than computed, which pins that aliasing as intended
    /// behaviour instead of re-deriving (and so agreeing with) whatever the
    /// code happens to do.
    badge: &'static str,
    /// What the scan is expected to read back, or `None` for a container with
    /// no tag carrier at all (the library falls back to the file name, so there
    /// is no fixed string to assert, the local-vs-remote equality check still
    /// applies).
    tags: Option<Tags>,
}

/// Declares the matrix once and emits two things from it: the `MATRIX` slice
/// the coverage tests audit, and **one `#[test]` per format**.
///
/// One test per format rather than a single loop, for two reasons: libtest runs
/// them in parallel (the serial loop took ~130 s, most of it DSD decimation and
/// twenty sequential OAuth logins), and a failure names the format in the test
/// name instead of surfacing as one red test that stops at the first bad
/// container, so a regression in, say, WebM doesn't hide whatever WavPack
/// would have said.
macro_rules! format_matrix {
    ($($name:ident: $ext:literal, $fixture:literal, $badge:literal, $tags:expr;)*) => {
        const MATRIX: &[Case] = &[
            $(Case { ext: $ext, fixture: $fixture, badge: $badge, tags: $tags },)*
        ];
        $(
            #[test]
            fn $name() {
                run_case(&Case { ext: $ext, fixture: $fixture, badge: $badge, tags: $tags });
            }
        )*
    };
}

// Every extension in `soul-audio-import`'s `SUPPORTED` set, plus the second
// codec flavour for the two extensions that carry more than one.
//
// Keep this list in lockstep with that set: an extension the importer accepts
// but that is missing here is an untested format, and
// `the_matrix_covers_every_supported_extension` fails if the two drift apart.
// (Plain comments, not doc comments, rustdoc can't attach docs to a macro
// invocation, and `#[warn(unused_doc_comments)]` rightly says so.)
format_matrix! {
    fmt_wav_meta: "wav", "meta.wav",            "WAV",  Some(ARIA);
    fmt_flac_meta: "flac", "meta.flac",           "FLAC", Some(ARIA);
    fmt_wv_meta: "wv",  "meta.wv",             "WV",   Some(ARIA);  // WavPack
    fmt_m4a_meta_alac: "m4a", "meta-alac.m4a",       "M4A",  Some(ARIA);  // ALAC in MP4
    fmt_caf_meta: "caf", "meta.caf",            "CAF",  Some(ARIA);  // ALAC in CAF
    fmt_oga_meta_oggflac: "oga", "meta-oggflac.oga",    "OGG",  Some(ARIA);  // FLAC in Ogg
    fmt_aiff_meta: "aiff", "meta.aiff",           "AIFF", Some(ARIA);
    fmt_aif_meta: "aif", "meta.aif",            "AIFF", Some(ARIA);  // same, short ext
    fmt_mp3_meta: "mp3", "meta.mp3",            "MP3",  Some(ARIA);
    fmt_mp2_meta: "mp2", "meta.mp2",            "MP2",  Some(ARIA);  // MPEG Layer II
    fmt_ogg_meta: "ogg", "meta.ogg",            "OGG",  Some(ARIA);  // Vorbis
    fmt_opus_meta: "opus", "meta.opus",           "OPUS", Some(ARIA);
    fmt_mka_meta: "mka", "meta.mka",            "MKA",  Some(ARIA);  // Opus in Matroska
    fmt_webm_meta: "webm", "meta.webm",           "WEBM", Some(ARIA);  // Opus in WebM
    fmt_dsf_meta: "dsf", "meta.dsf",            "DSF",  Some(ARIA);
    fmt_dff_meta: "dff", "meta.dff",            "DFF",  None;
    fmt_mp1_tagged: "mp1", "tagged.mp1",          "MP1",  Some(SYNTHETIC);
    // --- The DSD rate ladder ----------------------------------------------
    // `meta.dsf` above is DSD64. The higher rates matter here and not just in
    // the backend's signal tests because each one is a DIFFERENT decimation
    // ratio (64/128/256/512 -> 44.1 kHz) and a different byte volume per
    // second, which is exactly what a Range-streamed remote read is most
    // likely to get wrong. Synthetic tone fixtures: no CC0 native-DSD music
    // exists to derive an Aria from at these rates (see soul-audio-core's
    // testdata README), so they carry the canonical tone rather than the Aria.
    fmt_dsf_dsd128: "dsf", "tone-dsd128.dsf",     "DSF",  None;  // DSD128
    fmt_dsf_dsd256: "dsf", "tone-dsd256.dsf",     "DSF",  None;  // DSD256
    fmt_dsf_dsd512: "dsf", "tone-dsd512.dsf",     "DSF",  None;  // DSD512
}

/// Decode `id` from `source` to exhaustion and return every interleaved sample.
///
/// `OwnedDecoder` is large by value (the boxed `AnyDecoder` / multichannel
/// Vorbis variants), so this runs on a big-stack thread, the same precaution
/// `soul_audio_desktop::decoder`'s own `open_track` tests and `remote_e2e`
/// take.
fn decode_all(source: Arc<dyn TrackSource>, id: u32, what: &str) -> Vec<i32> {
    let in_thread = what.to_owned();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut decoder = source
                .open_decoder(id)
                .unwrap_or_else(|e| panic!("{in_thread}: open_decoder failed: {e:#}"));
            let mut out = Vec::new();
            let mut buf = [0i32; 4096];
            loop {
                let n = decoder
                    .decode_into(&mut buf)
                    .unwrap_or_else(|e| panic!("{in_thread}: decode_into failed: {e:#}"));
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            out
        })
        .expect("spawn decode thread")
        .join()
        .unwrap_or_else(|_| panic!("{what}: decode thread panicked"))
}

/// Assert a scanned view carries `tags` (when the container can hold them) and
/// a real duration. `where_` names the source for the failure message.
fn assert_view(view: &TrackView, case: &Case, where_: &str) {
    let what = format!("{} ({}, via {where_})", case.ext, case.fixture);
    if let Some(tags) = case.tags {
        assert_eq!(
            view.title.as_deref(),
            Some(tags.title),
            "{what}: title did not survive the scan"
        );
        assert_eq!(
            view.artist.as_deref(),
            Some(tags.artist),
            "{what}: artist did not survive the scan"
        );
        assert_eq!(
            view.album.as_deref(),
            Some(tags.album),
            "{what}: album did not survive the scan"
        );
    }
    assert!(
        view.duration_secs > 0.0,
        "{what}: expected a real duration, got {}",
        view.duration_secs
    );
    assert_eq!(
        view.format.as_deref(),
        Some(case.badge),
        "{what}: wrong FORMAT badge"
    );
}

/// A one-file library directory holding `case`'s bytes under `aria.<ext>`.
fn stage(case: &Case) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create a library temp dir");
    let dest = dir.path().join(format!("aria.{}", case.ext));
    fs::copy(testdata_dir().join(case.fixture), &dest)
        .unwrap_or_else(|e| panic!("stage fixture {}: {e}", case.fixture));
    dir
}

/// THE matrix test: for every supported container, scan it as a local folder
/// and as a remote server source, and require both to agree.
///
/// One server (and one real OAuth login) per case rather than one shared server
/// over the whole corpus, deliberately. Several extensions appear twice with
/// different codecs (`.m4a` as ALAC and as AAC, `.aac` with and without ID3),
/// and every fixture carries identical tags, so in a pooled library there would
/// be no stable way to say which browsed view came from which file. Isolating
/// each case keeps "exactly one track, and it is this one" assertable, and
/// points a failure at a single format.
fn run_case(case: &Case) {
    let dir = stage(case);
    let what = format!("{} ({})", case.ext, case.fixture);

    // --- Local source -------------------------------------------------
    // The backend `Library` a local watched folder is backed by: scan the
    // folder, project the browsable view, decode off local disk.
    let local =
        Library::scan(dir.path()).unwrap_or_else(|e| panic!("{what}: local scan failed: {e:#}"));
    let local_views = local.views();
    assert_eq!(
        local_views.len(),
        1,
        "{what}: a one-file folder should scan to exactly one local track, got {local_views:?}"
    );
    assert_view(&local_views[0], case, "local folder");
    let local_id = local_views[0].id;
    let local_pcm = decode_all(Arc::new(local) as Arc<dyn TrackSource>, local_id, &what);
    assert!(
        !local_pcm.is_empty(),
        "{what}: decoding the local track produced no samples"
    );

    // Same folder through the public host API every UI head actually
    // calls, proves the format reaches a real watched source, not just
    // the backend scanner underneath it.
    let handle = PlayerHandle::offline();
    handle
        .add_folder(dir.path())
        .unwrap_or_else(|e| panic!("{what}: add_folder failed: {e:#}"));
    let host_views = handle.views();
    assert_eq!(
        host_views.len(),
        1,
        "{what}: the watched folder should expose exactly one track, got {host_views:?}"
    );
    assert_view(&host_views[0], case, "PlayerHandle watched folder");
    assert!(
        host_views[0].remote_source.is_none(),
        "{what}: a local folder's track must not be labelled remote"
    );

    // --- Remote source ------------------------------------------------
    // A real soul-server over the SAME directory, a real OAuth 2.1
    // Authorization-Code+PKCE login, and the real `.soulidx` sync.
    let server = start_server_serving(dir.path());
    let tokens = oauth_login(&server.url, "e2e", "pw");
    let access_token = tokens.access.clone();

    let remote_handle = PlayerHandle::offline();
    remote_handle
        .add_remote_source_with_tokens(&server.url, tokens)
        .unwrap_or_else(|e| panic!("{what}: add_remote_source_with_tokens failed: {e:#}"));
    let remote_views = remote_handle.views();
    assert_eq!(
        remote_views.len(),
        1,
        "{what}: the synced remote should expose exactly one track, got {remote_views:?}"
    );
    assert_view(&remote_views[0], case, "remote source");
    assert!(
        remote_views[0].remote_source.is_some(),
        "{what}: a synced track must name the server it streams from"
    );

    // The browsable projections must agree on everything the user sees,
    // whichever side the track came from.
    let (l, r) = (&local_views[0], &remote_views[0]);
    assert_eq!(
        (l.title.as_deref(), l.artist.as_deref(), l.album.as_deref()),
        (r.title.as_deref(), r.artist.as_deref(), r.album.as_deref()),
        "{what}: tags differ between the local scan and the synced remote"
    );
    assert!(
        (l.duration_secs - r.duration_secs).abs() < 0.001,
        "{what}: duration differs local {} vs remote {}",
        l.duration_secs,
        r.duration_secs
    );

    // --- Decode the remote track over HTTP Range ----------------------
    // Rebuild the same aggregate `add_remote_source_with_tokens` swapped
    // in (one remote block, no local sources) so `open_decoder` streams.
    // Its `DownloadCache` gets its own temp dir and is only there as the
    // fallback; the check below proves streaming never needed it.
    let remote_server = Arc::new(RemoteServer::new(server.url.clone(), access_token));
    let blobs = soul_player_host::remote::fetch_index(&remote_server)
        .unwrap_or_else(|e| panic!("{what}: fetch_index failed: {e:#}"));
    let cache_dir = tempfile::tempdir().expect("create download cache dir");
    let fetch: Arc<dyn MediaFetch> = Arc::new(DownloadCache {
        dir: cache_dir.path().to_path_buf(),
    });
    let remote_lib = Arc::new(RemoteLibrary::build(
        Vec::new(),
        vec![(remote_server, blobs)],
        fetch,
    ));
    let remote_id = remote_views[0].id;
    let remote_pcm = decode_all(remote_lib as Arc<dyn TrackSource>, remote_id, &what);

    // --- The cross-check ----------------------------------------------
    // Identical bytes decoded by the identical decoder: streaming over
    // HTTP Range must not perturb a single sample. A mismatch here means a
    // truncated range read, a short final block, or a seek/offset bug in
    // the streaming source, none of which a "decoded something" assertion
    // would catch.
    assert_eq!(
        remote_pcm.len(),
        local_pcm.len(),
        "{what}: remote decode produced {} samples, local {}: \
         the streamed read did not cover the whole file",
        remote_pcm.len(),
        local_pcm.len()
    );
    assert!(
        remote_pcm == local_pcm,
        "{what}: remote and local decodes of the same bytes differ \
         (first mismatch at sample {:?})",
        remote_pcm.iter().zip(&local_pcm).position(|(a, b)| a != b)
    );

    assert!(
        fs::read_dir(cache_dir.path()).map_or(true, |mut e| e.next().is_none()),
        "{what}: streaming must not fall back to the download cache"
    );
}

/// What gating AAC means for a user's files.
///
/// `soul-audio-core` gates its AAC decoder behind an opt-in feature (patent,
/// Via LA) that this product does not enable, and the importer refuses to list
/// a file whose decoder was not compiled in. So a `.aac` file is simply not
/// imported, no library entry, no badge, nothing that fails on play.
///
/// `.m4a` cannot be handled that way and is the honest remaining gap: the
/// container carries ALAC as well as AAC, only a decode can tell them apart, and
/// filtering on the extension would drop every ALAC file too. An AAC-in-MP4 file
/// therefore still appears and fails when played.
///
/// This test exists to keep both halves visible, and it is what will fail if the
/// licence position changes, prompting the matrix cases to come back.
#[test]
fn aac_files_are_not_imported_while_the_decoder_is_gated_off() {
    // Raw ADTS: the extension can only be AAC, so it is not imported at all.
    for fixture in ["meta.aac", "meta-id3.aac"] {
        let dir = tempfile::tempdir().expect("create a library temp dir");
        std::fs::copy(testdata_dir().join(fixture), dir.path().join("a.aac"))
            .unwrap_or_else(|e| panic!("stage {fixture}: {e}"));
        let lib =
            Library::scan(dir.path()).unwrap_or_else(|e| panic!("{fixture}: scan failed: {e:#}"));
        assert_eq!(
            lib.views().len(),
            0,
            "{fixture}: a .aac file must not be imported when its decoder is gated \
             off: listing it would show the user a track that fails on play. If \
             the AAC licence was taken, restore the AAC cases in MATRIX and drop \
             \"aac\" from UNDECODABLE."
        );
    }

    // AAC inside MP4: indistinguishable from ALAC by name, so it IS imported and
    // then cannot be decoded. The gap is deliberate, the alternative is losing
    // every ALAC file with it.
    let dir = tempfile::tempdir().expect("create a library temp dir");
    std::fs::copy(
        testdata_dir().join("meta-aac.m4a"),
        dir.path().join("a.m4a"),
    )
    .expect("stage meta-aac.m4a");
    let lib = Library::scan(dir.path()).expect("scan");
    let views = lib.views();
    assert_eq!(
        views.len(),
        1,
        "an .m4a file is always imported (it may be ALAC)"
    );

    let id = views[0].id;
    let source = Arc::new(lib) as Arc<dyn TrackSource>;
    let opened = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || source.open_decoder(id).map(|_| ()))
        .expect("spawn decode thread")
        .join()
        .expect("decode thread panicked");
    assert!(
        opened.is_err(),
        "AAC-in-MP4 decoded, the AAC feature appears to be ON"
    );
}

/// The matrix must cover every extension the importer accepts, otherwise a
/// format can be added to `SUPPORTED` upstream and silently never be tested
/// from either source.
///
/// `SUPPORTED` lives in `soul-audio-import` and is private, so this asserts
/// against a copy. The copy is the point: if upstream grows an extension, the
/// list below has to be updated to match, and that edit is what forces a new
/// `MATRIX` case to be written alongside it.
#[test]
fn the_matrix_covers_every_supported_extension() {
    // Mirrors `soul_audio_import::SUPPORTED`.
    const SUPPORTED: &[&str] = &[
        "wav", "flac", "mp3", "mp2", "mp1", "m4a", "aac", "ogg", "oga", "opus", "aiff", "aif",
        "dsf", "dff", "mka", "webm", "wv", "caf",
    ];
    // Extensions the importer accepts but this build cannot DECODE, so the
    // matrix deliberately has no playing case for them. Keeping the exception
    // explicit (rather than quietly dropping the rows) is the point: if someone
    // takes the AAC licence and enables the feature, this list is what tells
    // them to put the matrix cases back.
    const UNDECODABLE: &[&str] = &["aac"];

    let missing: Vec<_> = SUPPORTED
        .iter()
        .filter(|ext| !UNDECODABLE.contains(*ext))
        .filter(|ext| !MATRIX.iter().any(|c| c.ext == **ext))
        .collect();
    assert!(
        missing.is_empty(),
        "these supported extensions have no format-matrix case: {missing:?}"
    );
    // And an exception must be a real exception, one still in `SUPPORTED`.
    for ext in UNDECODABLE {
        assert!(
            SUPPORTED.contains(ext),
            "{ext} is listed as undecodable but the importer no longer accepts it; \
             drop it from UNDECODABLE"
        );
    }

    // And nothing in the matrix may claim an extension the importer would skip,
    // such a case would scan to zero tracks and the real failure would read
    // as a confusing count mismatch.
    let unsupported: Vec<_> = MATRIX
        .iter()
        .map(|c| c.ext)
        .filter(|ext| !SUPPORTED.contains(ext))
        .collect();
    assert!(
        unsupported.is_empty(),
        "these matrix cases use extensions the importer does not accept: {unsupported:?}"
    );
}

/// Every fixture the matrix names must actually exist. A typo'd fixture name
/// would otherwise surface deep inside the big test as a copy failure.
#[test]
fn every_matrix_fixture_is_committed() {
    let missing: Vec<_> = MATRIX
        .iter()
        .map(|c| c.fixture)
        .filter(|f| !Path::new(&testdata_dir().join(f)).exists())
        .collect();
    assert!(
        missing.is_empty(),
        "these fixtures named by the matrix are not committed in testdata/: {missing:?}"
    );
}

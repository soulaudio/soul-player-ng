//! Acceptance E2E: add a remote source, sync its `.soulidx`, browse the
//! aggregated library, and play a real remote track end to end — against a
//! REAL in-process `soul-server`, not a faked HTTP boundary.
//!
//! This is the "add source and syncing .soulidx working, showing the albums
//! etc and being able to play tracks" deliverable, proven as one flow:
//! 1. stand up a real server over a small library of tagged fixtures,
//! 2. `PlayerHandle::add_remote_source` (the public API a head calls),
//! 3. assert the aggregate `views()` reflects the synced catalog,
//! 4. resolve a remote track id through the same client-side resolution
//!    (`soul_player_host::remote`) and decode real audio from the
//!    downloaded bytes.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use soul_audio_core::Decoder;
use soul_player_backend::library::TrackSource;
use soul_player_host::PlayerHandle;
use soul_player_host::remote::{DownloadCache, MediaFetch, RemoteLibrary, RemoteServer};
use soul_server::auth::signer::Signer;
use soul_server::domain::ports::UserStore;
use soul_server::domain::service::Services;
use soul_server::index::CoreIndex;
use soul_server::media::FsMedia;
use soul_server::proto::native::{self, AppState};
use soul_server::store::FileUsers;

/// Path to the repo's committed `testdata/` fixtures, three levels up from
/// this crate's manifest dir to the repo root (same convention as
/// `soul-player-host::remote`'s own tests).
fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// A real `soul-server` native router, bound to an ephemeral localhost port
/// and seeded with one user over a server-library temp dir containing
/// `fixtures` (copied out of `testdata/`). Mirrors
/// `soul_player_host::remote`'s own `start_server_over` test helper.
struct TestServer {
    url: String,
    /// Kept alive so the server's worker threads (and listening socket) keep
    /// running for the test's duration; never read directly.
    _runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

fn start_server(fixtures: &[&str]) -> TestServer {
    let dir = tempfile::tempdir().expect("create server temp dir");
    let library_root = dir.path().join("library");
    fs::create_dir_all(&library_root).expect("create server library dir");
    for name in fixtures {
        fs::copy(testdata_dir().join(name), library_root.join(name))
            .unwrap_or_else(|e| panic!("copy fixture {name}: {e}"));
    }

    let index = CoreIndex::scan(vec![library_root]).expect("scan the server library");
    let users = FileUsers::open(&dir.path().join("users.json")).expect("open user store");
    users.add_user("e2e", "pw").expect("seed the e2e user");
    let services = Arc::new(Services {
        index,
        media: FsMedia,
        users,
    });
    let signer_dir = dir.path().join("signer");
    let signer = Arc::new(
        Signer::load_or_create(&signer_dir, "http://127.0.0.1".to_string())
            .expect("load_or_create signer"),
    );
    let state = AppState::new(services, signer, "http://127.0.0.1".to_string(), 3600);

    let runtime = tokio::runtime::Runtime::new().expect("build tokio runtime");
    let (addr_tx, addr_rx) = mpsc::channel();
    runtime.spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        addr_tx
            .send(listener.local_addr().expect("bound socket has an addr"))
            .expect("send bound addr to the test thread");
        axum::serve(listener, native::router(state))
            .await
            .expect("serve the native router");
    });
    let addr = addr_rx
        .recv()
        .expect("server thread reports its bound address");

    TestServer {
        url: format!("http://{addr}"),
        _runtime: runtime,
        _dir: dir,
    }
}

#[test]
fn remote_add_sync_browse_play() {
    // Three real, differently-containered fixtures with readable tags — the
    // server's whole "library".
    const FIXTURES: &[&str] = &["meta.aiff", "meta.caf", "meta-id3.aac"];
    let server = start_server(FIXTURES);

    // --- 2. Add + sync, through the public API a head actually calls. ---
    let handle = PlayerHandle::offline();
    handle
        .add_remote_source(&server.url, "e2e", "pw")
        .expect("add remote source: authenticate, fetch the index, fold it into the library");

    // --- 3. Browse: the synced `.soulidx` populated the aggregate library. ---
    let views = handle.views();
    assert_eq!(
        views.len(),
        FIXTURES.len(),
        "expected one browsable view per fixture synced from the server, got {views:?}"
    );
    // Every fixture here is the same reference piece (Kimiko Ishizaka's "Aria"
    // from the Open Goldberg Variations, muxed into several containers) — a
    // known, stable title every synced view should carry, proving the real
    // `.soulidx` tags round-tripped through the wire protocol, not just a
    // track count.
    assert!(
        views.iter().all(|v| v.title.as_deref() == Some("Aria")),
        "expected every synced view titled \"Aria\", got {views:?}"
    );
    assert!(
        views
            .iter()
            .any(|v| v.album.as_deref() == Some("Open Goldberg Variations")),
        "expected at least one synced view to carry its album tag, got {views:?}"
    );

    // --- 4. Play: resolve a remote track id through the SAME client-side
    //        resolution `add_remote_source` uses (authenticate -> fetch_index
    //        -> RemoteLibrary), then decode real audio from the downloaded
    //        original bytes. A separate `DownloadCache` (own temp dir) so this
    //        doesn't share the host's real on-disk cache.
    let token =
        soul_player_host::remote::authenticate(&server.url, "e2e", "pw").expect("authenticate");
    let remote_server = Arc::new(RemoteServer {
        url: server.url.clone(),
        token,
    });
    let blobs = soul_player_host::remote::fetch_index(&remote_server).expect("fetch_index");
    let cache_dir = tempfile::tempdir().expect("create download cache dir");
    let fetch: Arc<dyn MediaFetch> = Arc::new(DownloadCache {
        dir: cache_dir.path().to_path_buf(),
    });
    // No local sources (the handle was never pointed at a local folder), so
    // this rebuilds the exact aggregate `add_remote_source` swapped in: one
    // remote block at source 0 — the same ids `views()` above returned.
    let remote_lib = RemoteLibrary::build(Vec::new(), vec![(remote_server, blobs)], fetch);

    let id = views[0].id;
    let path = remote_lib
        .path_of(id)
        .expect("remote track id should resolve to a downloaded local path");
    assert!(
        fs::metadata(&path).is_ok_and(|m| m.len() > 0),
        "downloaded original should be a non-empty file at {}",
        path.display()
    );

    // `OwnedDecoder` is large by value (the boxed `AnyDecoder`/Vorbis
    // variants) — a big-stack thread, same precaution
    // `soul_audio_desktop::decoder`'s own `open_track` tests take.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut decoder = soul_audio_desktop::open_track(&path)
                .expect("the downloaded original should be a valid, decodable file");
            let mut buf = [0i32; 4096];
            let samples = decoder
                .decode_into(&mut buf)
                .expect("decoding the first block of the remote track should succeed");
            assert!(
                samples > 0,
                "expected a non-empty decoded block from the remote track"
            );
        })
        .expect("spawn decode thread")
        .join()
        .expect("decode thread panicked");
}

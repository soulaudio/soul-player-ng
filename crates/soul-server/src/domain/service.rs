//! `Services`: the use-case layer the protocol adapter calls into.
//!
//! Composes the three ports ([`IndexPort`], [`MediaStore`], [`UserStore`])
//! behind a handful of methods that *are* the server's behavior — auth,
//! catalog, streaming — so the (forthcoming) HTTP adapter has no business
//! logic of its own, only wire framing.

use std::io;

use super::ports::{IndexPort, MediaStore, ReadSeek, UserStore};
use super::{DomainError, IndexVersion, Token, UserId};

/// The use-case layer: one instance per running server, generic over the
/// concrete port adapters so tests can swap in fakes.
pub struct Services<I, M, U> {
    pub index: I,
    pub media: M,
    pub users: U,
}

impl<I: IndexPort, M: MediaStore, U: UserStore> Services<I, M, U> {
    /// Verify a username/password pair and, on success, issue a bearer
    /// token. Wrong credentials (unknown user or bad password) map to
    /// [`DomainError::Unauthorized`] — the port doesn't distinguish the two,
    /// and neither should the response.
    pub fn authenticate(&self, user: &str, pass: &str) -> Result<Token, DomainError> {
        let id = self
            .users
            .verify(user, pass)
            .ok_or(DomainError::Unauthorized)?;
        Ok(self.users.issue_token(id)?)
    }

    /// Resolve a bearer token to the user it was issued for.
    pub fn check(&self, token: &str) -> Result<UserId, DomainError> {
        self.users.check(token).ok_or(DomainError::Unauthorized)
    }

    /// The current catalog bytes plus the version they were baked at.
    pub fn index(&self) -> Result<(Vec<u8>, IndexVersion), DomainError> {
        Ok((self.index.index_bytes()?, self.index.version()))
    }

    /// Open a track for streaming by catalog id: resolves the id to a path
    /// via the index, then opens it through the media store. An id the
    /// index doesn't recognize maps to [`DomainError::NotFound`].
    pub fn open_track(&self, id: u32) -> Result<(Box<dyn ReadSeek>, u64), DomainError> {
        let path = self.index.locate(id).ok_or(DomainError::NotFound)?;
        let reader = self.media.open(&path).map_err(io_err)?;
        let len = self.media.len(&path).map_err(io_err)?;
        Ok((reader, len))
    }
}

/// Lift an [`io::Error`] into [`DomainError::Other`] (via `anyhow`).
fn io_err(e: io::Error) -> DomainError {
    DomainError::Other(e.into())
}

/// OAuth-flow use cases: credential verification without the legacy `/v0/auth`
/// bearer-token issuance, plus the refresh-token lifecycle. Bounded on
/// [`RefreshStore`] in addition to [`UserStore`] because — unlike the
/// `/v0` native protocol — the `/authorize`/`/token`/`/revoke` endpoints
/// need durable, rotating refresh tokens, which only a store backing both
/// traits (`FileUsers`) can provide.
impl<I: IndexPort, M: MediaStore, U> Services<I, M, U>
where
    U: UserStore + crate::auth::refresh::RefreshStore,
{
    /// Verify a username/password pair, returning the matched user without
    /// issuing a `/v0`-style bearer token — the `/authorize` login form only
    /// needs a yes/no answer before minting an OAuth authorization code.
    pub fn verify_credentials(&self, user: &str, pass: &str) -> Option<UserId> {
        self.users.verify(user, pass)
    }

    /// Issue a fresh refresh token for `user`.
    pub fn issue_refresh(&self, user: UserId, now: u64) -> anyhow::Result<String> {
        self.users.issue(user, now)
    }

    /// Redeem-and-rotate a refresh token: verify `raw`, invalidate it, and
    /// mint its replacement in one atomic step.
    pub fn rotate_refresh(&self, raw: &str, now: u64) -> Option<(UserId, String)> {
        self.users.consume_rotate(raw, now)
    }

    /// Revoke a refresh token. The caller (RFC 7009's `/revoke`) always
    /// responds `200` regardless of this method's result — it exists so the
    /// handler doesn't need to know the store's internals, not so it can
    /// branch on whether the token existed.
    pub fn revoke_refresh(&self, raw: &str) -> bool {
        self.users.revoke(raw)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::*;

    /// In-memory [`IndexPort`] fake: a fixed map of track id -> path.
    struct FakeIndex {
        version: IndexVersion,
        bytes: Vec<u8>,
        tracks: HashMap<u32, PathBuf>,
    }

    impl IndexPort for FakeIndex {
        fn version(&self) -> IndexVersion {
            self.version
        }

        fn index_bytes(&self) -> anyhow::Result<Vec<u8>> {
            Ok(self.bytes.clone())
        }

        fn locate(&self, track_id: u32) -> Option<PathBuf> {
            self.tracks.get(&track_id).cloned()
        }
    }

    /// In-memory [`UserStore`] fake: one fixed user, tokens tracked in a map.
    struct FakeUsers {
        user: &'static str,
        pass: &'static str,
        user_id: UserId,
        tokens: Mutex<HashMap<String, UserId>>,
    }

    impl UserStore for FakeUsers {
        fn verify(&self, user: &str, pass: &str) -> Option<UserId> {
            (user == self.user && pass == self.pass).then_some(self.user_id)
        }

        fn issue_token(&self, user: UserId) -> anyhow::Result<Token> {
            let token = format!("token-for-{user}");
            self.tokens.lock().unwrap().insert(token.clone(), user);
            Ok(Token(token))
        }

        fn check(&self, token: &str) -> Option<UserId> {
            self.tokens.lock().unwrap().get(token).copied()
        }

        fn add_user(&self, _user: &str, _pass: &str) -> anyhow::Result<UserId> {
            anyhow::bail!("not needed by these tests")
        }
    }

    /// [`MediaStore`] fake over the real filesystem (a temp file) — reusing
    /// `std::fs` here is simplest and still exercises the real `Read`/`Seek`
    /// path the protocol adapter will stream from.
    struct FakeMedia;

    impl MediaStore for FakeMedia {
        fn open(&self, path: &Path) -> io::Result<Box<dyn ReadSeek>> {
            Ok(Box::new(std::io::BufReader::new(std::fs::File::open(
                path,
            )?)))
        }

        fn len(&self, path: &Path) -> io::Result<u64> {
            Ok(std::fs::metadata(path)?.len())
        }
    }

    const TRACK_BYTES: &[u8] = b"fake track bytes for streaming test";

    /// Describe an `open_track` result for a panic message, without requiring
    /// `Debug` on the success payload (`Box<dyn ReadSeek>` has none).
    fn describe(result: Result<(Box<dyn ReadSeek>, u64), DomainError>) -> String {
        match result {
            Ok(_) => "Ok(..)".to_string(),
            Err(e) => format!("Err({e})"),
        }
    }

    fn services() -> (Services<FakeIndex, FakeMedia, FakeUsers>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let track_path = dir.path().join("track-1.flac");
        std::fs::write(&track_path, TRACK_BYTES).unwrap();

        let mut tracks = HashMap::new();
        tracks.insert(1, track_path);

        let services = Services {
            index: FakeIndex {
                version: IndexVersion(7),
                bytes: b"index bytes".to_vec(),
                tracks,
            },
            media: FakeMedia,
            users: FakeUsers {
                user: "alice",
                pass: "hunter2",
                user_id: 42,
                tokens: Mutex::new(HashMap::new()),
            },
        };
        (services, dir)
    }

    #[test]
    fn authenticate_rejects_wrong_password() {
        let (services, _dir) = services();
        let err = services.authenticate("alice", "wrong").unwrap_err();
        assert!(matches!(err, DomainError::Unauthorized));
    }

    #[test]
    fn authenticate_rejects_unknown_user() {
        let (services, _dir) = services();
        let err = services.authenticate("nobody", "hunter2").unwrap_err();
        assert!(matches!(err, DomainError::Unauthorized));
    }

    #[test]
    fn authenticate_accepts_right_password_and_issues_a_token() {
        let (services, _dir) = services();
        let token = services.authenticate("alice", "hunter2").unwrap();
        assert_eq!(token, Token("token-for-42".to_string()));
    }

    #[test]
    fn check_rejects_unknown_token() {
        let (services, _dir) = services();
        let err = services.check("not-a-token").unwrap_err();
        assert!(matches!(err, DomainError::Unauthorized));
    }

    #[test]
    fn check_accepts_a_token_from_authenticate() {
        let (services, _dir) = services();
        let token = services.authenticate("alice", "hunter2").unwrap();
        let id = services.check(&token.0).unwrap();
        assert_eq!(id, 42);
    }

    #[test]
    fn index_returns_bytes_and_version() {
        let (services, _dir) = services();
        let (bytes, version) = services.index().unwrap();
        assert_eq!(bytes, b"index bytes");
        assert_eq!(version, IndexVersion(7));
    }

    #[test]
    fn open_track_rejects_unknown_id() {
        let (services, _dir) = services();
        match services.open_track(999) {
            Err(DomainError::NotFound) => {}
            other => panic!("expected NotFound, got {}", describe(other)),
        }
    }

    #[test]
    fn open_track_returns_matching_bytes_and_len_for_a_known_id() {
        let (services, _dir) = services();
        let (mut reader, len) = services.open_track(1).unwrap();

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();

        assert_eq!(buf, TRACK_BYTES);
        assert_eq!(len, TRACK_BYTES.len() as u64);
    }
}

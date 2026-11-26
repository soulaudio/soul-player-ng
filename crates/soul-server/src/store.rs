//! Flat-file-backed [`UserStore`](crate::domain::ports::UserStore) (and
//! [`RefreshStore`](crate::auth::refresh::RefreshStore)) adapter.
//!
//! Stores user credentials (Argon2id-hashed passwords) and rotating refresh
//! tokens (SHA-256 hashed, never the raw secret) as a small JSON file.
//! Access tokens are stateless, self-verifying JWTs minted by
//! [`Signer`](crate::auth::signer::Signer); this store never issues or
//! tracks one, so there's nothing bearer-token-shaped to persist or
//! invalidate here.
//!
//! This replaces a previous SQLite-backed store: the bundled C SQLite was
//! the subject of a security advisory with no patched `rusqlite` release
//! yet, and this project forbids `unsafe` and favors the smallest safe
//! dependency that does the job, for two users' worth of data, that's a
//! JSON file, not an embedded database.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{Context, Result};
use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use serde::{Deserialize, Serialize};

use crate::auth::refresh::{self, RefreshRecord, RefreshStore};
use crate::domain::UserId;
use crate::domain::ports::UserStore;

/// One persisted user record.
#[derive(Clone, Serialize, Deserialize)]
struct UserRec {
    id: UserId,
    name: String,
    pass_hash: String,
    /// Reserved for a future Subsonic-compatibility shim; unused today.
    #[allow(
        dead_code,
        reason = "reserved field kept for forward/backward JSON compatibility \
                  with a future Subsonic-API shim; nothing reads it yet"
    )]
    subsonic_secret: Option<String>,
}

/// The store's full persisted state: `users` and `refresh` tokens. `refresh`
/// carries only SHA-256 hashes (see [`RefreshRecord`]), so it's safe, and
/// required for OAuth 2.1 rotation to survive a restart, to persist
/// durably. `#[serde(default)]` lets a store file written before refresh
/// tokens existed still load, with an empty refresh list.
#[derive(Default, Serialize, Deserialize)]
struct Store {
    users: Vec<UserRec>,
    #[serde(default)]
    refresh: Vec<RefreshRecord>,
}

/// Flat-file user/credential store.
///
/// Concurrency note: the `Mutex` below only serializes access *within one
/// process*. Running the `soul-server user add` CLI against the same store
/// file while a server process has it open is an unsupported edge, add
/// users with the server stopped, then (re)start it to pick up the file.
pub struct FileUsers {
    path: PathBuf,
    state: Mutex<Store>,
}

impl FileUsers {
    /// Opens the JSON user store at `path`, loading it if present. Does NOT
    /// create the file, it's only written on the first `add_user`, so
    /// pointing a fresh config at a path with no users yet doesn't leave an
    /// empty file sitting there until there's something to persist.
    pub fn open(path: &Path) -> Result<Self> {
        let state = if path.exists() {
            let text = fs::read_to_string(path)
                .with_context(|| format!("reading user store {}", path.display()))?;
            serde_json::from_str(&text)
                .with_context(|| format!("parsing user store {}", path.display()))?
        } else {
            Store::default()
        };
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(state),
        })
    }

    fn hash_password(pass: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(pass.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("hashing password: {e}"))?;
        Ok(hash.to_string())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Store>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("user store state lock poisoned"))
    }

    /// Serialize `store` and atomically replace the file at `self.path`:
    /// write to a sibling `.tmp` file, then `rename` into place, so a crash
    /// mid-write can never leave a half-written/corrupt store file behind.
    fn persist(&self, store: &Store) -> Result<()> {
        let json = serde_json::to_string_pretty(store).context("serializing user store")?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("finalizing user store {}", self.path.display()))?;
        Ok(())
    }
}

/// Drop refresh records whose idle window has elapsed
/// (`now - created_at >= REFRESH_MAX_IDLE_SECS`). Returns whether anything was
/// removed, so the caller can decide to persist. `saturating_sub` keeps a
/// backwards clock jump harmless: it reads as zero idle, i.e. not expired.
fn prune_expired_refresh(store: &mut Store, now: u64) -> bool {
    let before = store.refresh.len();
    store
        .refresh
        .retain(|r| now.saturating_sub(r.created_at) < refresh::REFRESH_MAX_IDLE_SECS);
    store.refresh.len() != before
}

/// A fixed, valid Argon2id PHC hash used only to equalize [`FileUsers::verify`]'s
/// timing for an unknown username. Computed once on first use; the password it
/// hashes is irrelevant (a real login's verify against it always fails), only its
/// cost matters, which matches a genuine wrong-password verify.
fn dummy_password_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(b"unused-dummy-password", &salt)
            .expect("hashing a constant password with default params and a fresh salt never fails")
            .to_string()
    })
}

impl UserStore for FileUsers {
    fn verify(&self, user: &str, pass: &str) -> Option<UserId> {
        let store = self.lock().ok()?;
        match store.users.iter().find(|u| u.name == user) {
            Some(rec) => {
                let parsed = PasswordHash::new(&rec.pass_hash).ok()?;
                Argon2::default()
                    .verify_password(pass.as_bytes(), &parsed)
                    .ok()?;
                Some(rec.id)
            }
            None => {
                // Unknown user: still run a verify against a fixed dummy hash so
                // the response takes the same time as a wrong-password attempt
                // on a real user. Returning early here (no Argon2 work) would be
                // a timing oracle that reveals which usernames exist (CWE-204
                // username enumeration).
                if let Ok(parsed) = PasswordHash::new(dummy_password_hash()) {
                    let _ = Argon2::default().verify_password(pass.as_bytes(), &parsed);
                }
                None
            }
        }
    }

    fn add_user(&self, user: &str, pass: &str) -> Result<UserId> {
        let pass_hash = Self::hash_password(pass)?;
        let mut store = self.lock()?;
        if store.users.iter().any(|u| u.name == user) {
            anyhow::bail!("user '{user}' already exists");
        }
        let id = store.users.iter().map(|u| u.id).max().unwrap_or(0) + 1;
        store.users.push(UserRec {
            id,
            name: user.to_string(),
            pass_hash,
            subsonic_secret: None,
        });
        self.persist(&store)?;
        Ok(id)
    }
}

impl RefreshStore for FileUsers {
    fn issue(&self, user: UserId, now: u64) -> Result<String> {
        let raw = refresh::generate_raw_token()?;
        let rec = RefreshRecord {
            token_sha256: refresh::sha256_hex(&raw),
            user_id: user,
            created_at: now,
        };

        let mut store = self.lock()?;
        prune_expired_refresh(&mut store, now);
        store.refresh.push(rec);
        self.persist(&store)?;
        Ok(raw)
    }

    fn consume_rotate(&self, raw: &str, now: u64) -> Option<(UserId, String)> {
        let hash = refresh::sha256_hex(raw);

        // Hold the lock across verify + delete-old + issue-new + persist so
        // a token can never be redeemed by two concurrent callers (no
        // double-spend of a rotated refresh token).
        let mut store = self.lock().ok()?;
        // Drop idle-expired records first: this enforces the max lifetime (an
        // expired token is then simply not found below) and bounds the store's
        // growth. Persist the prune even when the token itself is a miss, so an
        // abandoned record can't linger on disk forever.
        if prune_expired_refresh(&mut store, now) {
            let _ = self.persist(&store);
        }
        let idx = store.refresh.iter().position(|r| r.token_sha256 == hash)?;
        let user_id = store.refresh.remove(idx).user_id;

        let new_raw = refresh::generate_raw_token().ok()?;
        store.refresh.push(RefreshRecord {
            token_sha256: refresh::sha256_hex(&new_raw),
            user_id,
            created_at: now,
        });
        self.persist(&store).ok()?;
        Some((user_id, new_raw))
    }

    fn revoke(&self, raw: &str) -> bool {
        let hash = refresh::sha256_hex(raw);
        let Ok(mut store) = self.lock() else {
            return false;
        };

        let before = store.refresh.len();
        store.refresh.retain(|r| r.token_sha256 != hash);
        let revoked = store.refresh.len() != before;
        if revoked {
            let _ = self.persist(&store);
        }
        revoked
    }
}

#[cfg(test)]
mod tests {
    use super::FileUsers;
    use crate::auth::refresh::RefreshStore;
    use crate::domain::ports::UserStore;

    #[test]
    fn refresh_tokens_rotate_revoke_and_never_persist_raw() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u.json");
        let now = 1_000;

        let s = FileUsers::open(&path).unwrap();
        let t1 = s.issue(5, now).unwrap();

        let (user, t2) = s.consume_rotate(&t1, now).expect("t1 rotates");
        assert_eq!(user, 5);
        assert_ne!(t2, t1, "rotation must mint a new token");

        assert!(
            s.consume_rotate(&t1, now).is_none(),
            "old token must be invalid after rotation"
        );

        let (user2, t3) = s.consume_rotate(&t2, now).expect("t2 rotates");
        assert_eq!(user2, 5);
        assert_ne!(t3, t2);

        assert!(s.revoke(&t3), "revoke must report the token existed");
        assert!(
            s.consume_rotate(&t3, now).is_none(),
            "revoked token must not rotate"
        );

        // Issue one more token that should survive a reopen, proving
        // persistence across process restarts.
        let t4 = s.issue(5, now).unwrap();
        drop(s);

        let s2 = FileUsers::open(&path).unwrap();
        let (user4, _t5) = s2
            .consume_rotate(&t4, now)
            .expect("surviving token rotates after reopen");
        assert_eq!(user4, 5);
        drop(s2);

        let raw_file = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw_file.contains(&t1),
            "raw refresh token must never appear in the on-disk store"
        );
    }

    #[test]
    fn a_refresh_token_idle_past_the_max_lifetime_is_rejected_and_pruned() {
        use crate::auth::refresh::{REFRESH_MAX_IDLE_SECS, sha256_hex};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u.json");
        let s = FileUsers::open(&path).unwrap();

        let issued_at = 1_000_000;
        let stale = s.issue(7, issued_at).unwrap();

        // A token still within its idle window rotates normally (sanity).
        let (_u, fresh) = s
            .consume_rotate(&stale, issued_at + 10)
            .expect("a token within the idle window rotates");

        // Long after the replacement was issued (idle past the max lifetime), it
        // no longer rotates...
        let way_later = issued_at + 10 + REFRESH_MAX_IDLE_SECS + 1;
        assert!(
            s.consume_rotate(&fresh, way_later).is_none(),
            "an idle-expired refresh token must not rotate"
        );
        // ...and the expired record is pruned from the store, not left to grow it.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains(&sha256_hex(&fresh)),
            "the expired record is pruned from the on-disk store"
        );
    }

    #[test]
    fn add_user_then_verify_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileUsers::open(&dir.path().join("u.json")).unwrap();
        let id = s.add_user("alice", "hunter2").unwrap();
        assert_eq!(s.verify("alice", "hunter2"), Some(id));
        assert_eq!(s.verify("alice", "wrong"), None);
        assert_eq!(s.verify("nobody", "x"), None);
    }

    #[test]
    fn an_unknown_user_verify_runs_the_timing_equalizing_dummy_hash() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileUsers::open(&dir.path().join("u.json")).unwrap();
        s.add_user("alice", "hunter2").unwrap();
        // Behaviour is unchanged: an unknown user is rejected.
        assert_eq!(s.verify("nobody", "whatever"), None);
        // The dummy hash the timing defense verifies against must be a valid,
        // parseable PHC string. A typo that made it unparseable would skip the
        // verify and silently reintroduce the username-enumeration timing oracle.
        assert!(
            super::PasswordHash::new(super::dummy_password_hash()).is_ok(),
            "the dummy hash must be a valid PHC string so the timing defense runs"
        );
    }

    #[test]
    fn add_user_rejects_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileUsers::open(&dir.path().join("u.json")).unwrap();
        s.add_user("alice", "hunter2").unwrap();
        assert!(s.add_user("alice", "different-pass").is_err());
    }
}

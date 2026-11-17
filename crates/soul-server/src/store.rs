//! Flat-file-backed [`UserStore`](crate::domain::ports::UserStore) adapter.
//!
//! Stores user credentials (Argon2id-hashed passwords) as a small JSON file;
//! bearer tokens are issued and tracked in memory only, never persisted —
//! restarting the server invalidates every outstanding token, which is fine
//! (clients just re-authenticate) and strictly more secure than a durable
//! token table.
//!
//! This replaces a previous SQLite-backed store: the bundled C SQLite was
//! the subject of a security advisory with no patched `rusqlite` release
//! yet, and this project forbids `unsafe` and favors the smallest safe
//! dependency that does the job — for two users' worth of data, that's a
//! JSON file, not an embedded database.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::domain::ports::UserStore;
use crate::domain::{Token, UserId};

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

/// The store's full in-memory state: persisted `users` plus in-memory-only
/// `tokens`. `#[serde(skip)]` keeps tokens out of the JSON file entirely —
/// a freshly loaded `Store` always starts with an empty token map.
#[derive(Default, Serialize, Deserialize)]
struct Store {
    users: Vec<UserRec>,
    #[serde(skip)]
    tokens: HashMap<String, UserId>,
}

/// Flat-file user/credential store. Bearer tokens live only in memory (per
/// process) — never written to disk.
///
/// Concurrency note: the `Mutex` below only serializes access *within one
/// process*. Running the `soul-server user add` CLI against the same store
/// file while a server process has it open is an unsupported edge — add
/// users with the server stopped, then (re)start it to pick up the file.
pub struct FileUsers {
    path: PathBuf,
    state: Mutex<Store>,
}

impl FileUsers {
    /// Opens the JSON user store at `path`, loading it if present. Does NOT
    /// create the file — it's only written on the first `add_user` — so
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

impl UserStore for FileUsers {
    fn verify(&self, user: &str, pass: &str) -> Option<UserId> {
        let store = self.lock().ok()?;
        let rec = store.users.iter().find(|u| u.name == user)?;
        let parsed = PasswordHash::new(&rec.pass_hash).ok()?;
        Argon2::default()
            .verify_password(pass.as_bytes(), &parsed)
            .ok()?;
        Some(rec.id)
    }

    fn issue_token(&self, user: UserId) -> Result<Token> {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();

        let mut store = self.lock()?;
        store.tokens.insert(token.clone(), user);
        Ok(Token(token))
    }

    fn check(&self, token: &str) -> Option<UserId> {
        let store = self.lock().ok()?;
        store.tokens.get(token).copied()
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

#[cfg(test)]
mod tests {
    use super::FileUsers;
    use crate::domain::ports::UserStore;

    #[test]
    fn add_verify_token_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileUsers::open(&dir.path().join("u.json")).unwrap();
        let id = s.add_user("alice", "hunter2").unwrap();
        assert_eq!(s.verify("alice", "hunter2"), Some(id));
        assert_eq!(s.verify("alice", "wrong"), None);
        assert_eq!(s.verify("nobody", "x"), None);
        let tok = s.issue_token(id).unwrap();
        assert_eq!(s.check(&tok.0), Some(id));
        assert_eq!(s.check("not-a-token"), None);
    }

    #[test]
    fn add_user_rejects_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileUsers::open(&dir.path().join("u.json")).unwrap();
        s.add_user("alice", "hunter2").unwrap();
        assert!(s.add_user("alice", "different-pass").is_err());
    }
}

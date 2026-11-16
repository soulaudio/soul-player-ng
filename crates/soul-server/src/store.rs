//! SQLite-backed [`UserStore`](crate::domain::ports::UserStore) adapter.
//!
//! Stores user credentials (Argon2id-hashed passwords) and bearer tokens in a
//! small two-table SQLite schema. The `rusqlite::Connection` is wrapped in a
//! `Mutex` so the store can satisfy the port's `Send + Sync` bound even though
//! `Connection` itself is not `Sync`.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::ports::UserStore;
use crate::domain::{Token, UserId};

/// SQLite-backed user/credential/token store.
pub struct SqliteUsers {
    conn: Mutex<Connection>,
}

impl SqliteUsers {
    /// Opens (creating if absent) the SQLite database at `path` and ensures
    /// the `users`/`tokens` schema exists.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening sqlite user store at {}", path.display()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                 id INTEGER PRIMARY KEY,
                 name TEXT UNIQUE NOT NULL,
                 pass_hash TEXT NOT NULL,
                 subsonic_secret TEXT NULL
             );
             CREATE TABLE IF NOT EXISTS tokens (
                 token TEXT PRIMARY KEY,
                 user_id INTEGER NOT NULL,
                 created_at INTEGER NOT NULL
             );",
        )
        .context("creating soul-server user store schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn hash_password(pass: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(pass.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("hashing password: {e}"))?;
        Ok(hash.to_string())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("user store connection lock poisoned"))
    }
}

impl UserStore for SqliteUsers {
    fn verify(&self, user: &str, pass: &str) -> Option<UserId> {
        let conn = self.lock().ok()?;
        let (id, pass_hash): (i64, String) = conn
            .query_row(
                "SELECT id, pass_hash FROM users WHERE name = ?1",
                params![user],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()??;
        let parsed = PasswordHash::new(&pass_hash).ok()?;
        Argon2::default()
            .verify_password(pass.as_bytes(), &parsed)
            .ok()?;
        Some(id)
    }

    fn issue_token(&self, user: UserId) -> Result<Token> {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tokens (token, user_id, created_at) VALUES (?1, ?2, ?3)",
            params![token, user, created_at],
        )
        .context("inserting issued token")?;
        Ok(Token(token))
    }

    fn check(&self, token: &str) -> Option<UserId> {
        let conn = self.lock().ok()?;
        conn.query_row(
            "SELECT user_id FROM tokens WHERE token = ?1",
            params![token],
            |r| r.get(0),
        )
        .optional()
        .ok()?
    }

    fn add_user(&self, user: &str, pass: &str) -> Result<UserId> {
        let pass_hash = Self::hash_password(pass)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO users (name, pass_hash) VALUES (?1, ?2)",
            params![user, pass_hash],
        )
        .with_context(|| format!("adding user '{user}' (name must be unique)"))?;
        Ok(conn.last_insert_rowid())
    }
}

#[cfg(test)]
mod tests {
    use super::SqliteUsers;
    use crate::domain::ports::UserStore;

    #[test]
    fn add_verify_token_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = SqliteUsers::open(&dir.path().join("u.db")).unwrap();
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
        let s = SqliteUsers::open(&dir.path().join("u.db")).unwrap();
        s.add_user("alice", "hunter2").unwrap();
        assert!(s.add_user("alice", "different-pass").is_err());
    }
}

//! Durable, hashed, rotating, revocable refresh-token model.
//!
//! OAuth 2.1 requires refresh-token rotation: every redemption of a refresh
//! token invalidates it and mints a fresh replacement, so a token that leaks
//! (logs, a compromised client, a replayed request) is only useful once
//! before the legitimate client's next refresh silently invalidates it for
//! an attacker. This module defines the storage contract; `FileUsers` (in
//! [`crate::store`]) implements it durably, on top of the same JSON file +
//! atomic-rename persistence it already uses for user credentials.
//!
//! Only a SHA-256 hex digest of each token is ever persisted, never the
//! raw token, so a stolen store file cannot be replayed as a bearer
//! credential (the same posture Argon2id gives user passwords).

use serde::{Deserialize, Serialize};

use crate::domain::UserId;

/// Maximum idle lifetime of a refresh token, in seconds. A token not redeemed
/// (rotated) within this window is treated as expired: it no longer rotates and
/// is pruned from the store. Rotation resets the clock (each rotation records the
/// replacement stamped with the current time), so an actively used session never
/// expires, only an abandoned token does. This both enforces a session timeout
/// and bounds the store's growth: abandoned tokens cannot accumulate forever.
pub const REFRESH_MAX_IDLE_SECS: u64 = 90 * 24 * 60 * 60; // 90 days

/// One persisted refresh-token record.
///
/// `token_sha256` is the SHA-256 hex digest of the raw token; the raw token
/// itself is never stored anywhere.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshRecord {
    /// The SHA-256 hex digest of the raw token (the raw token is never stored).
    pub token_sha256: String,
    /// The user this refresh token belongs to.
    pub user_id: UserId,
    /// When the record was created, unix seconds.
    pub created_at: u64,
}

/// Durable, hashed, rotating, revocable refresh-token store.
///
/// Implementors MUST perform `consume_rotate`'s verify + delete-old +
/// issue-new as a single atomic, lock-held step: two concurrent callers
/// presenting the same token must never both succeed (no double-spend of a
/// rotated token).
pub trait RefreshStore: Send + Sync {
    /// Issues a brand-new refresh token for `user`, recorded with
    /// `created_at = now` (unix seconds). Returns the raw token; only its
    /// SHA-256 hash is persisted.
    fn issue(&self, user: UserId, now: u64) -> anyhow::Result<String>;

    /// Verifies `raw` against the store, atomically deletes its record, and
    /// issues a fresh replacement token for the same user. Returns `None`
    /// if `raw` is unknown, never issued, already rotated away, or
    /// revoked.
    fn consume_rotate(&self, raw: &str, now: u64) -> Option<(UserId, String)>;

    /// Revokes `raw` by deleting its record. Returns `true` if a matching
    /// record existed (and was removed), `false` if `raw` was already
    /// unknown.
    fn revoke(&self, raw: &str) -> bool;
}

/// SHA-256 hex digest of `raw`, the only form of a refresh token that is
/// ever written to disk.
pub(crate) fn sha256_hex(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(raw.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Generates a fresh 32-byte CSPRNG raw refresh token, hex-encoded.
pub(crate) fn generate_raw_token() -> anyhow::Result<String> {
    use ring::rand::{SecureRandom, SystemRandom};

    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("failed to generate refresh-token randomness"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

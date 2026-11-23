//! Pure security logic for the Authorization Server: PKCE S256 verification,
//! a single-use authorization-code store, and loopback redirect-URI
//! validation (RFC 8252 §7.3, native-app clients).
//!
//! This module is intentionally HTTP-free — no `/authorize` or `/token`
//! wiring lives here (that is the next task). Everything here is pure logic
//! so it can be pinned against exact spec vectors without standing up a
//! server.

use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use crate::domain::UserId;

/// One issued, not-yet-redeemed authorization code.
pub struct AuthCode {
    /// The opaque authorization code string.
    pub code: String,
    /// The user the code was issued to.
    pub user: UserId,
    /// The PKCE code challenge the `/token` redemption's verifier must satisfy.
    pub code_challenge: String,
    /// The redirect URI the redemption must match.
    pub redirect_uri: String,
    /// Expiry time, unix seconds.
    pub expires_at: u64,
}

/// In-memory, single-use authorization-code store.
///
/// `issue` mints a fresh random code bound to a user, PKCE challenge,
/// redirect URI, and expiry. `consume` redeems a code exactly once: a
/// successful redemption deletes the record, so replaying the same code
/// (even with the correct verifier) returns `None` thereafter.
pub struct CodeStore {
    codes: Mutex<Vec<AuthCode>>,
}

impl CodeStore {
    /// An empty authorization-code store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            codes: Mutex::new(Vec::new()),
        }
    }

    /// Issues a fresh, random authorization code bound to `user`, the PKCE
    /// `code_challenge`, and the `redirect_uri` the eventual `/token`
    /// redemption must match. Expires at `now + ttl` (unix seconds).
    pub fn issue(
        &self,
        user: UserId,
        code_challenge: String,
        redirect_uri: String,
        now: u64,
        ttl: u64,
    ) -> String {
        let code = generate_code();
        let record = AuthCode {
            code: code.clone(),
            user,
            code_challenge,
            redirect_uri,
            expires_at: now.saturating_add(ttl),
        };
        self.codes
            .lock()
            .expect("auth-code store mutex poisoned")
            .push(record);
        code
    }

    /// Redeems `code`: verifies PKCE (`S256(code_verifier) ==
    /// code_challenge`), that `redirect_uri` matches the one the code was
    /// issued with, and that the code has not expired. On success, deletes
    /// the code (single-use) and returns the bound user. On any failure —
    /// unknown code, verifier mismatch, redirect mismatch, or
    /// expiry — returns `None` without granting access; the code is only
    /// ever removed on a successful redemption.
    pub fn consume(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
        now: u64,
    ) -> Option<UserId> {
        let mut codes = self.codes.lock().expect("auth-code store mutex poisoned");
        let idx = codes.iter().position(|c| c.code == code)?;

        let valid = {
            let entry = &codes[idx];
            now < entry.expires_at
                && entry.redirect_uri == redirect_uri
                && verify_pkce_s256(code_verifier, &entry.code_challenge)
        };

        if valid {
            Some(codes.remove(idx).user)
        } else {
            None
        }
    }
}

impl Default for CodeStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Generates a fresh 32-byte CSPRNG authorization code, hex-encoded.
fn generate_code() -> String {
    use ring::rand::{SecureRandom, SystemRandom};

    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .expect("failed to generate auth-code randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Verifies a PKCE S256 code-verifier/code-challenge pair per RFC 7636 §4.6:
/// `code_challenge == BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`, using
/// base64url with NO padding.
pub fn verify_pkce_s256(code_verifier: &str, code_challenge: &str) -> bool {
    let digest = Sha256::digest(code_verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(digest);
    computed == code_challenge
}

/// Validates a redirect URI against the loopback-interface rule of RFC 8252
/// §7.3 for native-app clients: the scheme MUST be `http`, and the host MUST
/// be exactly the IPv4 loopback literal `127.0.0.1` or the IPv6 loopback
/// literal `[::1]` — any port is allowed (including none, i.e. the implicit
/// default port).
///
/// This is deliberately strict and hand-parses the authority component
/// rather than doing any substring match, specifically to reject:
/// - `http://localhost/...` — RFC 8252 prefers the loopback literal over the
///   `localhost` name (which can be remapped by local DNS/hosts-file
///   configuration on some systems).
/// - `http://127.0.0.1.evil.com/...` — a suffix trick: the host is the full
///   label `127.0.0.1.evil.com`, not `127.0.0.1`.
/// - `http://127.0.0.1@evil.com/...` — a userinfo trick: everything before
///   the (last) `@` is userinfo, so the actual host is `evil.com`.
pub fn is_loopback_redirect(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return false;
    }

    // Strip userinfo: the host is whatever follows the LAST '@' in the
    // authority. Anything before it (including a string that merely looks
    // like a loopback host) is userinfo and must be ignored.
    let host_port = match authority.rfind('@') {
        Some(idx) => &authority[idx + 1..],
        None => authority,
    };
    if host_port.is_empty() {
        return false;
    }

    // IPv6 literal: "[::1]" or "[::1]:port".
    if let Some(rest_after_bracket) = host_port.strip_prefix('[') {
        let Some(end) = rest_after_bracket.find(']') else {
            return false;
        };
        let host = &rest_after_bracket[..end];
        let after = &rest_after_bracket[end + 1..];
        if !after.is_empty() && !is_valid_port_suffix(after) {
            return false;
        }
        return host == "::1";
    }

    // Plain host, optionally followed by ":port".
    let host = match host_port.find(':') {
        Some(idx) => {
            let port = &host_port[idx + 1..];
            if !port.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            &host_port[..idx]
        }
        None => host_port,
    };
    host == "127.0.0.1"
}

/// `true` if `s` is a valid `:port` suffix (a colon followed by one or more
/// ASCII digits).
fn is_valid_port_suffix(s: &str) -> bool {
    match s.strip_prefix(':') {
        Some(digits) => !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7636 Appendix B test vector.
    const RFC7636_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC7636_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn pkce_s256_matches_rfc7636_vector() {
        assert!(verify_pkce_s256(RFC7636_VERIFIER, RFC7636_CHALLENGE));
    }

    #[test]
    fn pkce_s256_rejects_wrong_verifier() {
        assert!(!verify_pkce_s256(
            "this-is-not-the-right-verifier-at-all",
            RFC7636_CHALLENGE
        ));
    }

    #[test]
    fn loopback_redirect_accepts_ipv4_literal_with_port() {
        assert!(is_loopback_redirect("http://127.0.0.1:49152/callback"));
    }

    #[test]
    fn loopback_redirect_accepts_ipv4_literal_without_port() {
        assert!(is_loopback_redirect("http://127.0.0.1/callback"));
    }

    #[test]
    fn loopback_redirect_accepts_ipv6_literal_with_port() {
        assert!(is_loopback_redirect("http://[::1]:49152/callback"));
    }

    #[test]
    fn loopback_redirect_accepts_ipv6_literal_without_port() {
        assert!(is_loopback_redirect("http://[::1]/callback"));
    }

    #[test]
    fn loopback_redirect_rejects_remote_host() {
        assert!(!is_loopback_redirect("http://evil.com/callback"));
    }

    #[test]
    fn loopback_redirect_rejects_localhost_name() {
        assert!(!is_loopback_redirect("http://localhost/callback"));
    }

    #[test]
    fn loopback_redirect_rejects_suffix_trick() {
        // Host is the full label "127.0.0.1.evil.com", not "127.0.0.1".
        assert!(!is_loopback_redirect("http://127.0.0.1.evil.com/callback"));
    }

    #[test]
    fn loopback_redirect_rejects_userinfo_trick() {
        // Everything before '@' is userinfo; the real host is "evil.com".
        assert!(!is_loopback_redirect("http://127.0.0.1@evil.com/"));
    }

    #[test]
    fn loopback_redirect_rejects_https_scheme() {
        // RFC 8252 loopback redirects are plain http; strict per spec.
        assert!(!is_loopback_redirect("https://127.0.0.1/callback"));
    }

    fn redirect() -> String {
        "http://127.0.0.1:49152/callback".to_string()
    }

    #[test]
    fn code_store_issue_then_consume_succeeds() {
        let store = CodeStore::new();
        let code = store.issue(42, RFC7636_CHALLENGE.to_string(), redirect(), 1_000, 60);

        assert_eq!(
            store.consume(&code, RFC7636_VERIFIER, &redirect(), 1_010),
            Some(42)
        );
    }

    #[test]
    fn code_store_consume_is_single_use() {
        let store = CodeStore::new();
        let code = store.issue(42, RFC7636_CHALLENGE.to_string(), redirect(), 1_000, 60);

        assert_eq!(
            store.consume(&code, RFC7636_VERIFIER, &redirect(), 1_010),
            Some(42)
        );
        // Replay of the same code must fail — it was deleted on success.
        assert_eq!(
            store.consume(&code, RFC7636_VERIFIER, &redirect(), 1_010),
            None
        );
    }

    #[test]
    fn code_store_consume_rejects_wrong_verifier() {
        let store = CodeStore::new();
        let code = store.issue(42, RFC7636_CHALLENGE.to_string(), redirect(), 1_000, 60);

        assert_eq!(
            store.consume(&code, "wrong-verifier", &redirect(), 1_010),
            None
        );
    }

    #[test]
    fn code_store_consume_rejects_mismatched_redirect_uri() {
        let store = CodeStore::new();
        let code = store.issue(42, RFC7636_CHALLENGE.to_string(), redirect(), 1_000, 60);

        assert_eq!(
            store.consume(
                &code,
                RFC7636_VERIFIER,
                "http://127.0.0.1:1/callback",
                1_010
            ),
            None
        );
    }

    #[test]
    fn code_store_consume_rejects_expired_code() {
        let store = CodeStore::new();
        let code = store.issue(42, RFC7636_CHALLENGE.to_string(), redirect(), 1_000, 10);

        // now == expires_at (1_010): expiry check is `now < expires_at`, so
        // this must already be considered expired.
        assert_eq!(
            store.consume(&code, RFC7636_VERIFIER, &redirect(), 1_010),
            None
        );
    }

    #[test]
    fn code_store_consume_rejects_unknown_code() {
        let store = CodeStore::new();
        assert_eq!(
            store.consume("no-such-code", RFC7636_VERIFIER, &redirect(), 1_000),
            None
        );
    }
}

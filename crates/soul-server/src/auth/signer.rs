use std::path::Path;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};

use super::AccessClaims;
use crate::domain::UserId;

/// The single audience every access token issued by this server is scoped to.
const AUDIENCE: &str = "soul-player";
/// The default scope minted into every access token (library browsing/streaming).
const SCOPE: &str = "library";
/// File name of the persisted Ed25519 keypair (PKCS#8 DER), relative to the
/// signer's data dir.
const KEY_FILE_NAME: &str = "jwt_key.der";

/// Mints and verifies EdDSA (Ed25519) JWT access tokens off a single, on-disk
/// Ed25519 keypair, and exposes the public half as a JWK Set for `/jwks`.
pub struct Signer {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    /// Key id: a stable, content-derived identifier for the current keypair.
    kid: String,
    issuer: String,
    /// Base64url (no padding) raw 32-byte Ed25519 public key, cached for
    /// `jwks()`.
    x_b64: String,
}

impl Signer {
    /// Loads the Ed25519 keypair at `dir/jwt_key.der`, generating and
    /// persisting a fresh Ed25519 PKCS#8 keypair if none exists yet. The key
    /// file is created with `0600` permissions on platforms that support
    /// Unix-style file modes.
    pub fn load_or_create(dir: &Path, issuer: String) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let key_path = dir.join(KEY_FILE_NAME);

        let pkcs8_bytes = if key_path.exists() {
            std::fs::read(&key_path)?
        } else {
            let rng = SystemRandom::new();
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
                .map_err(|_| anyhow::anyhow!("failed to generate Ed25519 keypair"))?;
            let bytes = pkcs8.as_ref().to_vec();
            create_key_file(&key_path, &bytes)?;
            bytes
        };

        let keypair = Ed25519KeyPair::from_pkcs8(&pkcs8_bytes)
            .map_err(|_| anyhow::anyhow!("invalid Ed25519 PKCS#8 keypair"))?;
        let public_key_bytes = keypair.public_key().as_ref();

        let encoding_key = EncodingKey::from_ed_der(&pkcs8_bytes);
        let decoding_key = DecodingKey::from_ed_der(public_key_bytes);

        let x_b64 = URL_SAFE_NO_PAD.encode(public_key_bytes);
        let kid = URL_SAFE_NO_PAD.encode(Sha256::digest(public_key_bytes));

        Ok(Self {
            encoding_key,
            decoding_key,
            kid,
            issuer,
            x_b64,
        })
    }

    /// Mints an EdDSA JWT access token for `user`, valid for `ttl_secs`
    /// seconds starting at `now_unix`.
    pub fn mint(&self, user: UserId, ttl_secs: u64, now_unix: u64) -> anyhow::Result<String> {
        let claims = AccessClaims {
            iss: self.issuer.clone(),
            sub: user.to_string(),
            aud: AUDIENCE.to_string(),
            exp: usize::try_from(now_unix.saturating_add(ttl_secs))?,
            iat: usize::try_from(now_unix)?,
            scope: SCOPE.to_string(),
        };

        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(self.kid.clone());

        Ok(encode(&header, &claims, &self.encoding_key)?)
    }

    /// Verifies `token`'s signature, issuer, and audience, and checks that it
    /// has not expired as of `now_unix`. Returns `None` on any failure.
    pub fn verify(&self, token: &str, now_unix: u64) -> Option<AccessClaims> {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_audience(&[AUDIENCE]);
        validation.set_issuer(&[self.issuer.as_str()]);
        // Expiry is checked manually below against the caller-supplied clock
        // rather than jsonwebtoken's internal wall-clock read, so callers can
        // verify deterministically against an arbitrary `now_unix`.
        validation.validate_exp = false;

        let claims = decode::<AccessClaims>(token, &self.decoding_key, &validation)
            .ok()?
            .claims;

        if claims.aud != AUDIENCE || claims.iss != self.issuer {
            return None;
        }
        if (claims.exp as u64) <= now_unix {
            return None;
        }

        Some(claims)
    }

    /// Test-only: signs `claims` verbatim with this signer's real EdDSA key,
    /// bypassing the normalization `mint` applies (issuer/audience/expiry).
    /// This lets adversarial tests forge specific claim values (a bad `aud`
    /// or `iss`) while still producing a token with a genuinely valid
    /// signature, so `verify` is exercised on the claims check rather than
    /// the signature check.
    #[cfg(test)]
    fn sign_claims_for_test(&self, claims: &AccessClaims) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(self.kid.clone());
        encode(&header, claims, &self.encoding_key).expect("encode forged claims")
    }

    /// Exports the public half of the keypair as a JWK Set, suitable for
    /// serving at `/jwks`.
    pub fn jwks(&self) -> serde_json::Value {
        serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "use": "sig",
                "alg": "EdDSA",
                "kid": self.kid,
                "x": self.x_b64,
            }]
        })
    }
}

/// Creates `path` and writes `bytes` to it as a single atomic operation. On
/// Unix, the file is opened with mode `0600` from the moment it is created,
/// so the private key is never briefly world-readable (unlike a separate
/// write-then-chmod, which leaves a window with the default, more permissive
/// mode). `create_new` ensures we never clobber a pre-existing key file.
#[cfg(unix)]
fn create_key_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_key_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs()
    }

    #[test]
    fn mints_and_verifies_eddsa_tokens_with_persisted_keypair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = now_unix();

        let signer =
            Signer::load_or_create(dir.path(), "http://h".to_string()).expect("load_or_create");
        let token = signer.mint(7, 3600, now).expect("mint");

        let claims = signer
            .verify(&token, now)
            .expect("fresh token should verify");
        assert_eq!(claims.sub, "7");
        assert_eq!(claims.aud, "soul-player");
        assert_eq!(claims.iss, "http://h");
        assert_eq!(claims.scope, "library");

        assert!(
            signer.verify(&token, now + 4000).is_none(),
            "expired token must not verify"
        );
        assert!(
            signer.verify("garbage", now).is_none(),
            "malformed token must not verify"
        );

        // Persistence: a second `load_or_create` on the same dir must reuse
        // the same key, so a token minted by the first signer still verifies.
        let signer2 =
            Signer::load_or_create(dir.path(), "http://h".to_string()).expect("reload signer");
        let claims2 = signer2
            .verify(&token, now)
            .expect("token minted by the first signer must verify under the second");
        assert_eq!(claims2.sub, "7");

        let jwks = signer.jwks();
        let kid = jwks["keys"][0]["kid"].as_str().expect("kid is a string");
        assert!(!kid.is_empty());
        assert_eq!(jwks["keys"][0]["kty"], "OKP");
        assert_eq!(jwks["keys"][0]["crv"], "Ed25519");
        assert_eq!(jwks["keys"][0]["alg"], "EdDSA");
        let x = jwks["keys"][0]["x"].as_str().expect("x is a string");
        assert!(!x.is_empty());
    }

    // --- Adversarial regression guards -------------------------------
    //
    // These pin down `verify`'s rejection behavior against the classic JWT
    // forgery attacks, so a future refactor can't silently start accepting
    // forged tokens. Each constructs a forged token and asserts `verify`
    // returns `None`.

    #[test]
    fn rejects_token_signed_by_a_different_keypair() {
        let dir1 = tempfile::tempdir().expect("tempdir 1");
        let dir2 = tempfile::tempdir().expect("tempdir 2");
        let now = now_unix();

        let signer1 =
            Signer::load_or_create(dir1.path(), "http://h".to_string()).expect("signer 1");
        // Independent keypair: a different temp dir, same issuer, so only
        // the signature differs.
        let signer2 =
            Signer::load_or_create(dir2.path(), "http://h".to_string()).expect("signer 2");

        let forged = signer2.mint(7, 3600, now).expect("mint with wrong key");

        assert!(
            signer1.verify(&forged, now).is_none(),
            "a token signed by a different keypair must not verify"
        );
    }

    #[test]
    fn rejects_token_with_forged_audience() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = now_unix();
        let signer =
            Signer::load_or_create(dir.path(), "http://h".to_string()).expect("load_or_create");

        let forged_claims = AccessClaims {
            iss: "http://h".to_string(),
            sub: "7".to_string(),
            aud: "not-soul-player".to_string(),
            exp: usize::try_from(now + 3600).expect("exp fits usize"),
            iat: usize::try_from(now).expect("iat fits usize"),
            scope: "library".to_string(),
        };
        let forged = signer.sign_claims_for_test(&forged_claims);

        assert!(
            signer.verify(&forged, now).is_none(),
            "a token with a forged `aud` must not verify, even genuinely signed"
        );
    }

    #[test]
    fn rejects_token_with_forged_issuer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = now_unix();
        let signer =
            Signer::load_or_create(dir.path(), "http://h".to_string()).expect("load_or_create");

        let forged_claims = AccessClaims {
            iss: "http://attacker".to_string(),
            sub: "7".to_string(),
            aud: "soul-player".to_string(),
            exp: usize::try_from(now + 3600).expect("exp fits usize"),
            iat: usize::try_from(now).expect("iat fits usize"),
            scope: "library".to_string(),
        };
        let forged = signer.sign_claims_for_test(&forged_claims);

        assert!(
            signer.verify(&forged, now).is_none(),
            "a token with a forged `iss` must not verify, even genuinely signed"
        );
    }

    #[test]
    fn rejects_hs256_alg_confusion_using_public_key_as_hmac_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = now_unix();
        let signer =
            Signer::load_or_create(dir.path(), "http://h".to_string()).expect("load_or_create");

        // The classic alg-confusion attack: take the *public* Ed25519 key
        // (published at /jwks) and (ab)use its raw bytes as an HMAC-SHA256
        // secret, hoping a verifier that doesn't pin the expected algorithm
        // will accept a token "signed" this way.
        let jwks = signer.jwks();
        let x_b64 = jwks["keys"][0]["x"].as_str().expect("x is a string");
        let public_key_bytes = URL_SAFE_NO_PAD.decode(x_b64).expect("valid base64url");

        let legit_claims = AccessClaims {
            iss: "http://h".to_string(),
            sub: "7".to_string(),
            aud: "soul-player".to_string(),
            exp: usize::try_from(now + 3600).expect("exp fits usize"),
            iat: usize::try_from(now).expect("iat fits usize"),
            scope: "library".to_string(),
        };
        let header = Header::new(Algorithm::HS256);
        let hmac_key = EncodingKey::from_secret(&public_key_bytes);
        let forged = encode(&header, &legit_claims, &hmac_key).expect("encode HS256 token");

        assert!(
            signer.verify(&forged, now).is_none(),
            "an HS256 token 'signed' with the public key as secret must not verify"
        );
    }

    #[test]
    fn rejects_alg_none_token_with_empty_signature() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = now_unix();
        let signer =
            Signer::load_or_create(dir.path(), "http://h".to_string()).expect("load_or_create");

        let header_json = r#"{"alg":"none","typ":"JWT"}"#;
        let claims = AccessClaims {
            iss: "http://h".to_string(),
            sub: "7".to_string(),
            aud: "soul-player".to_string(),
            exp: usize::try_from(now + 3600).expect("exp fits usize"),
            iat: usize::try_from(now).expect("iat fits usize"),
            scope: "library".to_string(),
        };
        let claims_json = serde_json::to_vec(&claims).expect("serialize claims");

        let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
        let claims_b64 = URL_SAFE_NO_PAD.encode(claims_json);
        // `alg: none` tokens carry an empty signature segment.
        let forged = format!("{header_b64}.{claims_b64}.");

        assert!(
            signer.verify(&forged, now).is_none(),
            "an `alg: none` token with an empty signature must not verify"
        );
    }
}

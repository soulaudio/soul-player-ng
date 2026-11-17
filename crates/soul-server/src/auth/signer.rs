use std::path::Path;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rsa::RsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::traits::PublicKeyParts;
use sha2::{Digest, Sha256};

use super::AccessClaims;
use crate::domain::UserId;

/// The single audience every access token issued by this server is scoped to.
const AUDIENCE: &str = "soul-player";
/// The default scope minted into every access token (library browsing/streaming).
const SCOPE: &str = "library";
/// File name of the persisted RSA keypair, relative to the signer's data dir.
const KEY_FILE_NAME: &str = "jwt_key.pem";

/// Mints and verifies RS256 JWT access tokens off a single, on-disk RSA
/// keypair, and exposes the public half as a JWK Set for `/jwks`.
pub struct Signer {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    /// Key id: a stable, content-derived identifier for the current keypair.
    kid: String,
    issuer: String,
    /// Base64url (no padding) RSA modulus, cached for `jwks()`.
    n_b64: String,
    /// Base64url (no padding) RSA public exponent, cached for `jwks()`.
    e_b64: String,
}

impl Signer {
    /// Loads the RSA keypair at `dir/jwt_key.pem`, generating and persisting
    /// a fresh RSA-2048 PKCS#8 keypair if none exists yet. The key file is
    /// created with `0600` permissions on platforms that support Unix-style
    /// file modes.
    pub fn load_or_create(dir: &Path, issuer: String) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let key_path = dir.join(KEY_FILE_NAME);

        let private_key = if key_path.exists() {
            let pem = std::fs::read_to_string(&key_path)?;
            RsaPrivateKey::from_pkcs8_pem(&pem)?
        } else {
            let mut rng = rand::rngs::OsRng;
            let key = RsaPrivateKey::new(&mut rng, 2048)?;
            let pem = key.to_pkcs8_pem(LineEnding::LF)?;
            std::fs::write(&key_path, pem.as_bytes())?;
            restrict_permissions(&key_path)?;
            key
        };

        let private_pem = private_key.to_pkcs8_pem(LineEnding::LF)?;
        let public_key = private_key.to_public_key();
        let public_pem = public_key.to_public_key_pem(LineEnding::LF)?;

        let encoding_key = EncodingKey::from_rsa_pem(private_pem.as_bytes())?;
        let decoding_key = DecodingKey::from_rsa_pem(public_pem.as_bytes())?;

        let n_b64 = URL_SAFE_NO_PAD.encode(public_key.n().to_bytes_be());
        let e_b64 = URL_SAFE_NO_PAD.encode(public_key.e().to_bytes_be());

        let public_der = public_key.to_public_key_der()?;
        let kid = URL_SAFE_NO_PAD.encode(Sha256::digest(public_der.as_bytes()));

        Ok(Self {
            encoding_key,
            decoding_key,
            kid,
            issuer,
            n_b64,
            e_b64,
        })
    }

    /// Mints an RS256 JWT access token for `user`, valid for `ttl_secs`
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

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());

        Ok(encode(&header, &claims, &self.encoding_key)?)
    }

    /// Verifies `token`'s signature, issuer, and audience, and checks that it
    /// has not expired as of `now_unix`. Returns `None` on any failure.
    pub fn verify(&self, token: &str, now_unix: u64) -> Option<AccessClaims> {
        let mut validation = Validation::new(Algorithm::RS256);
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

    /// Exports the public half of the keypair as a JWK Set, suitable for
    /// serving at `/jwks`.
    pub fn jwks(&self) -> serde_json::Value {
        serde_json::json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": self.kid,
                "n": self.n_b64,
                "e": self.e_b64,
            }]
        })
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> anyhow::Result<()> {
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
    fn mints_and_verifies_rs256_tokens_with_persisted_keypair() {
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
        assert_eq!(jwks["keys"][0]["alg"], "RS256");
    }
}

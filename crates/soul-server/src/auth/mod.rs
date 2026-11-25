/// The OAuth 2.1 authorization-code + PKCE flow (`/authorize`, `/token`).
pub mod oauth;
/// Durable, hashed, rotating refresh tokens.
pub mod refresh;
/// The EdDSA token signer/verifier.
pub mod signer;

/// Claims embedded in a minted EdDSA access token.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct AccessClaims {
    /// Issuer: this server's own base URL.
    pub iss: String,
    /// Subject: the authenticated user id (as a string).
    pub sub: String,
    /// Audience: the intended recipient of the token.
    pub aud: String,
    /// Expiry time, unix seconds.
    pub exp: usize,
    /// Issued-at time, unix seconds.
    pub iat: usize,
    /// The granted OAuth scope(s).
    pub scope: String,
}

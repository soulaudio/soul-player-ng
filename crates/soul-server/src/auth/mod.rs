pub mod signer;

/// Claims embedded in a minted RS256 access token.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct AccessClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: usize,
    pub iat: usize,
    pub scope: String,
}

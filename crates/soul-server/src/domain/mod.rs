//! The hexagonal domain core: the server's entities plus the [`ports`] traits its
//! adapters implement, independent of HTTP transport or storage backend.

/// The port traits (storage/index/user) the adapters implement.
pub mod ports;
/// The domain service that orchestrates the ports behind the `/v0` endpoints.
pub mod service;

/// A server-local user identifier.
pub type UserId = i64;

/// An opaque bearer token string (an access or refresh token).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token(pub String);

/// The catalog's monotonically-increasing version, bumped on each reindex so a client
/// can tell its cached index is stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexVersion(pub u64);

/// A domain-level failure, mapped to an HTTP status by the protocol layer.
#[derive(thiserror::Error, Debug)]
pub enum DomainError {
    /// The request was unauthenticated or the credential was rejected (→ 401).
    #[error("unauthorized")]
    Unauthorized,
    /// The requested resource does not exist (→ 404).
    #[error("not found")]
    NotFound,
    /// Any other internal failure (→ 500).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_object_safe() {
        fn _a(_: &dyn ports::UserStore) {}
        fn _b(_: &dyn ports::IndexPort) {}
        fn _c(_: &dyn ports::MediaStore) {}
    }
}

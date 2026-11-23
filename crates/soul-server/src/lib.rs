//! The self-hostable Soul remote-source server: a hexagonal `/v0` HTTP service that
//! streams a music library to the desktop player over authenticated, range-capable
//! requests. The [`domain`] core is transport- and storage-agnostic; [`proto`] speaks
//! the wire protocol, [`auth`] handles OAuth, and [`store`]/[`index`]/[`media`] are the
//! concrete adapters.
#![deny(missing_docs)]

/// OAuth 2.1 / OIDC authentication: the browser login, token refresh, and user store.
pub mod auth;
/// Server configuration (bind address, library roots, secrets).
pub mod config;
/// The transport- and storage-agnostic core: entities, port traits, and the service.
pub mod domain;
/// The baked `.soulidx` catalog adapter the server serves to clients.
pub mod index;
/// Filesystem media access (range reads over the library's track files).
pub mod media;
/// The wire protocols the server speaks over HTTP.
pub mod proto {
    /// The native `/v0` protocol the desktop player speaks.
    pub mod native;
}
/// Persistent stores (users and their credentials).
pub mod store;

/// This server's crate version (`CARGO_PKG_VERSION`), surfaced on the health endpoint.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn has_version() {
        assert!(!crate::version().is_empty());
    }
}

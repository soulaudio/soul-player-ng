pub mod config;
pub mod domain;
pub mod index;
pub mod media;
pub mod store;

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

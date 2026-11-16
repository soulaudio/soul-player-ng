pub mod ports;

pub type UserId = i64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexVersion(pub u64);

#[derive(thiserror::Error, Debug)]
pub enum DomainError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
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

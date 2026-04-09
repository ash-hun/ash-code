//! ash-core — session, storage, settings primitives.
//!
//! M0 scaffold only.

/// Crate identifier used for smoke tests.
pub const CRATE_NAME: &str = "ash-core";

/// Returns the crate version declared in Cargo.toml.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(CRATE_NAME, "ash-core");
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}

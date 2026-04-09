//! ash-api — axum + utoipa Swagger UI at :8080 (M4).
//!
//! M0 scaffold only.

pub const CRATE_NAME: &str = "ash-api";
pub const DEFAULT_PORT: u16 = 8080;

#[cfg(test)]
mod tests {
    #[test]
    fn default_port_is_8080() {
        assert_eq!(super::DEFAULT_PORT, 8080);
    }
}

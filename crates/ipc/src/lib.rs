//! ash-ipc — gRPC client to the Python sidecar (M1).
//!
//! M0 scaffold only. In M1 this crate will add tonic + build.rs codegen
//! from `proto/ash.proto`.

pub const CRATE_NAME: &str = "ash-ipc";
pub const DEFAULT_SIDECAR_ENDPOINT: &str = "http://127.0.0.1:50051";

#[cfg(test)]
mod tests {
    #[test]
    fn default_endpoint_loopback() {
        assert!(super::DEFAULT_SIDECAR_ENDPOINT.starts_with("http://127.0.0.1"));
    }
}

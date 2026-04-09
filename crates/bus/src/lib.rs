//! ash-bus — in-process event bus shared by TUI and API (M8).
//!
//! M0 scaffold only.

pub const CRATE_NAME: &str = "ash-bus";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_ok() {
        assert_eq!(super::CRATE_NAME, "ash-bus");
    }
}

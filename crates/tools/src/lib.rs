//! ash-tools — built-in tool implementations (bash/file/grep/...).
//!
//! M0 scaffold only.

pub const CRATE_NAME: &str = "ash-tools";

/// Names of tools planned for M3.
pub fn planned_tools() -> &'static [&'static str] {
    &["bash", "file_read", "file_write", "file_edit", "grep", "glob"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_tools_listed() {
        assert_eq!(planned_tools().len(), 6);
    }
}

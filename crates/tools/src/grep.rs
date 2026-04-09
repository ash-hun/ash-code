//! `grep` tool — pure-Rust regex search over a directory tree.
//!
//! We do not shell out to `rg` in M3 — keeping the dependency surface
//! minimal and avoiding environment coupling. Walks via `walkdir` and
//! matches lines against a compiled regex.

use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::{parse_args, Tool, ToolResult};

pub struct GrepTool;

pub const MAX_MATCHES: usize = 500;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default = "default_true")]
    line_numbers: bool,
    #[serde(default)]
    case_insensitive: bool,
}

fn default_true() -> bool {
    true
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search for a regex pattern across a directory tree. Returns matching file:line entries."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Root directory (default current)"},
                "line_numbers": {"type": "boolean", "default": true},
                "case_insensitive": {"type": "boolean", "default": false}
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolResult> {
        let args: Args = parse_args(self.name(), args)?;
        let root = args.path.clone().unwrap_or_else(|| ".".to_string());
        let mut builder = regex::RegexBuilder::new(&args.pattern);
        builder.case_insensitive(args.case_insensitive);
        let re: Regex = match builder.build() {
            Ok(r) => r,
            Err(err) => {
                return Ok(ToolResult::err_text(format!("grep: bad pattern: {err}")))
            }
        };

        // Walk the tree off the async runtime — walkdir + std::fs are blocking.
        let root_path = root.clone();
        let result = tokio::task::spawn_blocking(move || grep_tree(&root_path, &re, args.line_numbers))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("grep: join error: {e}")))?;
        Ok(result)
    }
}

fn grep_tree(root: &str, re: &Regex, with_line_numbers: bool) -> Result<ToolResult> {
    let mut out = String::new();
    let mut matches = 0usize;
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok());
    for entry in walker {
        if matches >= MAX_MATCHES {
            out.push_str(&format!("... (truncated at {MAX_MATCHES} matches)\n"));
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            if matches >= MAX_MATCHES {
                break;
            }
            if re.is_match(line) {
                if with_line_numbers {
                    out.push_str(&format!(
                        "{}:{}:{}\n",
                        entry.path().display(),
                        idx + 1,
                        line
                    ));
                } else {
                    out.push_str(&format!("{}:{}\n", entry.path().display(), line));
                }
                matches += 1;
            }
        }
    }
    if matches == 0 {
        return Ok(ToolResult::ok_text(String::new()));
    }
    Ok(ToolResult::ok_text(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn finds_pattern_in_tree() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, "hello alpha\nhello beta\n").unwrap();
        let b = dir.path().join("b.txt");
        std::fs::write(&b, "no match here\n").unwrap();
        let r = GrepTool
            .run(json!({"pattern": "hello \\w+", "path": dir.path().to_string_lossy()}))
            .await
            .unwrap();
        assert!(r.ok);
        assert!(r.stdout.contains("hello alpha"));
        assert!(r.stdout.contains("hello beta"));
        assert!(!r.stdout.contains("b.txt"));
    }

    #[tokio::test]
    async fn bad_pattern_errors_gracefully() {
        let r = GrepTool.run(json!({"pattern": "[unterminated"})).await.unwrap();
        assert!(!r.ok);
    }
}

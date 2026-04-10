//! `glob` tool — filesystem glob matching.

use anyhow::Result;
use async_trait::async_trait;
use globset::{Glob, GlobSetBuilder};
use serde::Deserialize;
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::{parse_args, Tool, ToolResult};

pub const MAX_RESULTS: usize = 1000;

pub struct GlobTool;

#[derive(Debug, Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "List files matching a glob pattern (e.g. `**/*.rs`) under a directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Root directory (default current)"}
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolResult> {
        let args: Args = parse_args(self.name(), args)?;
        let root = args.path.clone().unwrap_or_else(|| ".".to_string());
        let result = tokio::task::spawn_blocking(move || glob_tree(&root, &args.pattern))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("glob: join error: {e}")))?;
        Ok(result)
    }
}

fn glob_tree(root: &str, pattern: &str) -> Result<ToolResult> {
    let glob = match Glob::new(pattern) {
        Ok(g) => g,
        Err(err) => {
            return Ok(ToolResult::err_text(format!("glob: bad pattern: {err}")))
        }
    };
    let mut builder = GlobSetBuilder::new();
    builder.add(glob);
    let set = match builder.build() {
        Ok(s) => s,
        Err(err) => return Ok(ToolResult::err_text(format!("glob: build failed: {err}"))),
    };

    let root_path = std::path::Path::new(root);
    let mut out = String::new();
    let mut count = 0usize;
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(root_path).unwrap_or(entry.path());
        if set.is_match(rel) {
            out.push_str(&format!("{}\n", entry.path().display()));
            count += 1;
            if count >= MAX_RESULTS {
                out.push_str(&format!("... (truncated at {MAX_RESULTS} results)\n"));
                break;
            }
        }
    }
    Ok(ToolResult::ok_text(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn finds_matching_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.rs"), "").unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();
        let r = GlobTool
            .run(json!({"pattern": "*.rs", "path": dir.path().to_string_lossy()}))
            .await
            .unwrap();
        assert!(r.ok);
        assert!(r.stdout.contains("a.rs"));
        assert!(r.stdout.contains("b.rs"));
        assert!(!r.stdout.contains("c.txt"));
    }
}

//! `file_read` tool — read a text file with optional offset/limit.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{parse_args, Tool, ToolResult};

pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub struct FileReadTool;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &'static str {
        "file_read"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file. Returns content with optional line-range slicing."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "minimum": 0, "description": "Line offset (0-based)"},
                "limit": {"type": "integer", "minimum": 1, "description": "Max lines to return"}
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolResult> {
        let args: Args = parse_args(self.name(), args)?;
        let path = std::path::PathBuf::from(&args.path);
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(err) => {
                return Ok(ToolResult::err_text(format!(
                    "file_read: stat failed: {err}"
                )))
            }
        };
        if meta.len() > MAX_BYTES {
            return Ok(ToolResult::err_text(format!(
                "file_read: {} exceeds {MAX_BYTES}-byte limit",
                args.path
            )));
        }
        let raw = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(err) => {
                return Ok(ToolResult::err_text(format!(
                    "file_read: read failed: {err}"
                )))
            }
        };
        let text = match String::from_utf8(raw) {
            Ok(s) => s,
            Err(_) => return Ok(ToolResult::err_text("file_read: file is not valid UTF-8")),
        };
        if args.offset.is_none() && args.limit.is_none() {
            return Ok(ToolResult::ok_text(text));
        }
        let lines: Vec<&str> = text.lines().collect();
        let start = args.offset.unwrap_or(0).min(lines.len());
        let end = args
            .limit
            .map(|l| (start + l).min(lines.len()))
            .unwrap_or(lines.len());
        let slice = lines[start..end].join("\n");
        Ok(ToolResult::ok_text(slice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn reads_whole_file() {
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"alpha\nbeta\ngamma\n").unwrap();
        let path = f.path().to_string_lossy().into_owned();
        let r = FileReadTool
            .run(json!({"path": path}))
            .await
            .unwrap();
        assert!(r.ok);
        assert!(r.stdout.contains("alpha"));
        assert!(r.stdout.contains("gamma"));
    }

    #[tokio::test]
    async fn respects_offset_and_limit() {
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"a\nb\nc\nd\ne\n").unwrap();
        let path = f.path().to_string_lossy().into_owned();
        let r = FileReadTool
            .run(json!({"path": path, "offset": 1, "limit": 2}))
            .await
            .unwrap();
        assert!(r.ok);
        assert_eq!(r.stdout, "b\nc");
    }

    #[tokio::test]
    async fn missing_file_errors_gracefully() {
        let r = FileReadTool
            .run(json!({"path": "/nonexistent/path/xyz"}))
            .await
            .unwrap();
        assert!(!r.ok);
    }
}

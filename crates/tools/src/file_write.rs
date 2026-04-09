//! `file_write` tool — atomic write (tempfile + rename).

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{parse_args, Tool, ToolResult};

pub struct FileWriteTool;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &'static str {
        "file_write"
    }

    fn description(&self) -> &'static str {
        "Write UTF-8 content to a file, creating parent directories as needed. Atomic."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolResult> {
        let args: Args = parse_args(self.name(), args)?;
        let path = std::path::PathBuf::from(&args.path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(err) = tokio::fs::create_dir_all(parent).await {
                    return Ok(ToolResult::err_text(format!(
                        "file_write: mkdir -p failed: {err}"
                    )));
                }
            }
        }
        // Atomic: write to sibling tempfile, then rename.
        let tmp = path.with_extension(format!(
            "{}.ash.tmp",
            path.extension().and_then(|s| s.to_str()).unwrap_or("")
        ));
        if let Err(err) = tokio::fs::write(&tmp, args.content.as_bytes()).await {
            return Ok(ToolResult::err_text(format!(
                "file_write: temp write failed: {err}"
            )));
        }
        if let Err(err) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Ok(ToolResult::err_text(format!(
                "file_write: rename failed: {err}"
            )));
        }
        Ok(ToolResult::ok_text(format!(
            "wrote {} bytes to {}",
            args.content.len(),
            args.path
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn writes_and_creates_parents() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("nested/sub/file.txt");
        let r = FileWriteTool
            .run(json!({
                "path": target.to_string_lossy(),
                "content": "hello world"
            }))
            .await
            .unwrap();
        assert!(r.ok, "stderr={}", r.stderr);
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "hello world"
        );
    }
}

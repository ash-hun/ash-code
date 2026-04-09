//! `file_edit` tool — exact string replacement.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{parse_args, Tool, ToolResult};

pub struct FileEditTool;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &'static str {
        "file_edit"
    }

    fn description(&self) -> &'static str {
        "Replace an exact string in a file. Fails if the pattern is not found or (by default) occurs more than once."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "old", "new"],
            "properties": {
                "path": {"type": "string"},
                "old": {"type": "string", "description": "Exact text to replace"},
                "new": {"type": "string", "description": "Replacement text"},
                "replace_all": {"type": "boolean", "default": false}
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolResult> {
        let args: Args = parse_args(self.name(), args)?;
        let path = std::path::PathBuf::from(&args.path);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(err) => {
                return Ok(ToolResult::err_text(format!(
                    "file_edit: read failed: {err}"
                )))
            }
        };
        if !content.contains(&args.old) {
            return Ok(ToolResult::err_text("file_edit: `old` not found in file"));
        }
        let count = content.matches(&args.old).count();
        if count > 1 && !args.replace_all {
            return Ok(ToolResult::err_text(format!(
                "file_edit: `old` occurs {count} times; set replace_all=true or provide a unique context"
            )));
        }
        let new_content = if args.replace_all {
            content.replace(&args.old, &args.new)
        } else {
            content.replacen(&args.old, &args.new, 1)
        };
        if let Err(err) = tokio::fs::write(&path, new_content).await {
            return Ok(ToolResult::err_text(format!(
                "file_edit: write failed: {err}"
            )));
        }
        Ok(ToolResult::ok_text(format!(
            "replaced {} occurrence(s) in {}",
            if args.replace_all { count } else { 1 },
            args.path
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn single_replacement() {
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"hello world").unwrap();
        let path = f.path().to_string_lossy().into_owned();
        let r = FileEditTool
            .run(json!({"path": path, "old": "world", "new": "ash"}))
            .await
            .unwrap();
        assert!(r.ok);
        assert_eq!(
            tokio::fs::read_to_string(f.path()).await.unwrap(),
            "hello ash"
        );
    }

    #[tokio::test]
    async fn refuses_ambiguous_match() {
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"foo foo foo").unwrap();
        let path = f.path().to_string_lossy().into_owned();
        let r = FileEditTool
            .run(json!({"path": path, "old": "foo", "new": "bar"}))
            .await
            .unwrap();
        assert!(!r.ok);
        assert!(r.stderr.contains("occurs 3 times"));
    }

    #[tokio::test]
    async fn replace_all_works() {
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"foo foo foo").unwrap();
        let path = f.path().to_string_lossy().into_owned();
        let r = FileEditTool
            .run(json!({
                "path": path,
                "old": "foo",
                "new": "bar",
                "replace_all": true
            }))
            .await
            .unwrap();
        assert!(r.ok);
        assert_eq!(
            tokio::fs::read_to_string(f.path()).await.unwrap(),
            "bar bar bar"
        );
    }
}

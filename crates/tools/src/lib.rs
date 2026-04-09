//! ash-tools — built-in tool implementations and the `ToolRegistry`.
//!
//! All tools implement the [`Tool`] trait. Registration is centralized in
//! [`ToolRegistry::with_builtins`] so the `ash-query` turn loop has a single
//! entry point.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod bash;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod glob_tool;
pub mod grep;

/// Catastrophic bash patterns blocked at the Rust layer regardless of
/// middleware configuration. Flexible policy lives in the Python
/// `bash_guard` middleware; this list is the last-resort safety net.
pub const CATASTROPHIC_BASH_PATTERNS: &[&str] = &[
    r"rm\s+-rf\s+/($|\s)",
    r"rm\s+-rf\s+/\*",
    r":\(\)\s*\{\s*:\|:\s*&\s*\}\s*;",   // fork bomb
    r"mkfs\.",
    r"dd\s+if=/dev/(zero|random)\s+of=/dev/",
    r">\s*/dev/sda",
    r"chmod\s+-R\s+000\s+/",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i32,
}

impl ToolResult {
    pub fn ok_text(text: impl Into<String>) -> Self {
        Self {
            ok: true,
            stdout: text.into(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    pub fn err_text(text: impl Into<String>) -> Self {
        Self {
            ok: false,
            stdout: String::new(),
            stderr: text.into(),
            exit_code: 1,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    async fn run(&self, args: Value) -> Result<ToolResult>;
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(bash::BashTool::new()));
        reg.register(Arc::new(file_read::FileReadTool));
        reg.register(Arc::new(file_write::FileWriteTool));
        reg.register(Arc::new(file_edit::FileEditTool));
        reg.register(Arc::new(grep::GrepTool));
        reg.register(Arc::new(glob_tool::GlobTool));
        reg
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn list(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .values()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.tools.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub async fn invoke(&self, name: &str, args: Value) -> Result<ToolResult> {
        let tool = self
            .tools
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown tool: {name}"))?;
        tool.run(args).await
    }
}

/// Convenience: parse `args` into `T` with a helpful error context.
pub(crate) fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: Value,
) -> Result<T> {
    serde_json::from_value::<T>(args).with_context(|| format!("invalid args for tool '{tool}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lists_six_builtins() {
        let reg = ToolRegistry::with_builtins();
        let names = reg.names();
        assert_eq!(
            names,
            vec!["bash", "file_edit", "file_read", "file_write", "glob", "grep"]
        );
        assert_eq!(reg.list().len(), 6);
    }

    #[tokio::test]
    async fn invoke_unknown_tool_errors() {
        let reg = ToolRegistry::with_builtins();
        let err = reg.invoke("nope", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("unknown tool"));
    }
}

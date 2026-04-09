//! `bash` tool — execute a shell command with a timeout.
//!
//! The catastrophic-pattern blacklist in [`crate::CATASTROPHIC_BASH_PATTERNS`]
//! is enforced here as an unconditional safety net. Flexible policy lives
//! in the Python `bash_guard` middleware.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{parse_args, Tool, ToolResult, CATASTROPHIC_BASH_PATTERNS};

pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 600_000;

pub struct BashTool {
    denies: Vec<Regex>,
}

impl BashTool {
    pub fn new() -> Self {
        let denies = CATASTROPHIC_BASH_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("valid regex literal"))
            .collect();
        Self { denies }
    }

    pub fn is_catastrophic(&self, command: &str) -> Option<String> {
        for re in &self.denies {
            if re.is_match(command) {
                return Some(re.as_str().to_string());
            }
        }
        None
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Execute a shell command with a timeout. Returns stdout, stderr, and exit code."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute"},
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default 120000, max 600000)",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_MS
                }
            }
        })
    }

    async fn run(&self, args: Value) -> Result<ToolResult> {
        let args: BashArgs = parse_args(self.name(), args)?;
        if let Some(pat) = self.is_catastrophic(&args.command) {
            return Ok(ToolResult::err_text(format!(
                "bash: catastrophic pattern blocked by Rust safety net ({pat})"
            )));
        }

        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS);
        let dur = Duration::from_millis(timeout_ms);

        let mut cmd = Command::new("bash");
        cmd.arg("-lc").arg(&args.command);

        let exec = async {
            let output = cmd.output().await?;
            anyhow::Ok(output)
        };

        match timeout(dur, exec).await {
            Ok(Ok(output)) => Ok(ToolResult {
                ok: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
            }),
            Ok(Err(err)) => Ok(ToolResult::err_text(format!("bash: spawn failed: {err}"))),
            Err(_) => Ok(ToolResult::err_text(format!(
                "bash: timed out after {timeout_ms} ms"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_hello() {
        let t = BashTool::new();
        let r = t
            .run(json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(r.ok);
        assert_eq!(r.stdout.trim(), "hello");
        assert_eq!(r.exit_code, 0);
    }

    #[tokio::test]
    async fn nonzero_exit() {
        let t = BashTool::new();
        let r = t.run(json!({"command": "false"})).await.unwrap();
        assert!(!r.ok);
        assert_eq!(r.exit_code, 1);
    }

    #[tokio::test]
    async fn catastrophic_rm_rf_root_is_blocked() {
        let t = BashTool::new();
        let r = t
            .run(json!({"command": "rm -rf /"}))
            .await
            .unwrap();
        assert!(!r.ok);
        assert!(r.stderr.contains("catastrophic"));
    }

    #[tokio::test]
    async fn fork_bomb_is_blocked() {
        let t = BashTool::new();
        let r = t
            .run(json!({"command": ":(){ :|: & };:"}))
            .await
            .unwrap();
        assert!(!r.ok);
        assert!(r.stderr.contains("catastrophic"));
    }

    #[tokio::test]
    async fn timeout_kicks_in() {
        let t = BashTool::new();
        let r = t
            .run(json!({"command": "sleep 5", "timeout_ms": 200}))
            .await
            .unwrap();
        assert!(!r.ok);
        assert!(r.stderr.contains("timed out"));
    }
}

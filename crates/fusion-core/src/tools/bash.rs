//! The only execution primitive in v1: a bash shell, like the host harness.
use std::time::Duration;

use serde_json::{json, Value};

use super::{Tool, ToolCtx, ToolDef, ToolOutput};

const OUTPUT_LIMIT: usize = 30_000;

pub struct BashTool {
    pub default_timeout: Duration,
    pub max_timeout: Duration,
}

impl Default for BashTool {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
            max_timeout: Duration::from_secs(600),
        }
    }
}

fn truncate(s: &str) -> String {
    if s.len() <= OUTPUT_LIMIT {
        return s.to_string();
    }
    let keep = OUTPUT_LIMIT / 2;
    format!(
        "{}\n\n[... {} bytes truncated ...]\n\n{}",
        &s[..keep],
        s.len() - 2 * keep,
        &s[s.len() - keep..]
    )
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash".into(),
            description: "Run a bash command in the workspace. Returns stdout, stderr and \
                          the exit code. Takes an optional `timeout` in seconds."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout": {"type": "integer", "minimum": 1}
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        let Some(command) = input["command"].as_str() else {
            return ToolOutput::err("`command` is required");
        };
        let timeout = input["timeout"]
            .as_u64()
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout)
            .min(self.max_timeout);

        let fut = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.cwd)
            .output();
        match tokio::time::timeout(timeout, fut).await {
            Err(_) => ToolOutput::err(format!("timed out after {}s", timeout.as_secs())),
            Ok(Err(e)) => ToolOutput::err(format!("spawn failed: {e}")),
            Ok(Ok(out)) => {
                let mut s = String::new();
                s.push_str(&String::from_utf8_lossy(&out.stdout));
                let err = String::from_utf8_lossy(&out.stderr);
                if !err.is_empty() {
                    s.push_str("\n--- stderr ---\n");
                    s.push_str(&err);
                }
                s.push_str(&format!("\n[exit {}]", out.status.code().unwrap_or(-1)));
                ToolOutput {
                    content: truncate(&s),
                    is_error: !out.status.success(),
                }
            }
        }
    }
}

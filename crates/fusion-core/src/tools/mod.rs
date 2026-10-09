pub mod bash;

use std::path::PathBuf;

use serde_json::Value;

use crate::model::ToolDef;

pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[derive(Clone)]
pub struct ToolCtx {
    pub cwd: PathBuf,
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;
    async fn call(&self, input: Value, ctx: &ToolCtx) -> ToolOutput;
}

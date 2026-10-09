//! Scripted model client for tests and dry-runs.
use std::collections::VecDeque;
use std::sync::Mutex;

use crate::model::{Completion, Content, ModelClient, Request};

type Fallback = Box<dyn Fn(&Request) -> Completion + Send + Sync>;

/// Pops scripted completions in order; when the script runs out it replies
/// with a fixed text (or echoes the last user message).
pub struct ScriptedModel {
    script: Mutex<VecDeque<Completion>>,
    fallback: Mutex<Fallback>,
    name: String,
}

impl ScriptedModel {
    pub fn new(name: impl Into<String>, script: Vec<Completion>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            fallback: Mutex::new(Box::new(|_| Completion {
                content: vec![Content::Text("(script exhausted)".into())],
                stop_reason: None,
            })),
            name: name.into(),
        }
    }

    /// Fallback: echo the last user message's text.
    pub fn echo_fallback(self) -> Self {
        *self.fallback.lock().unwrap() = Box::new(|req: &Request| {
            let t = req
                .messages
                .last()
                .map(|m| m.text_content())
                .unwrap_or_default();
            Completion {
                content: vec![Content::Text(format!("echo: {t}"))],
                stop_reason: None,
            }
        });
        self
    }

    pub fn text(s: impl Into<String>) -> Completion {
        Completion {
            content: vec![Content::Text(s.into())],
            stop_reason: Some("end_turn".into()),
        }
    }

    pub fn tool_call(name: &str, input: serde_json::Value) -> Completion {
        Completion {
            content: vec![Content::ToolUse {
                id: format!("call_{name}"),
                name: name.into(),
                input,
            }],
            stop_reason: Some("tool_use".into()),
        }
    }
}

#[async_trait::async_trait]
impl ModelClient for ScriptedModel {
    async fn complete(&self, req: Request<'_>) -> anyhow::Result<Completion> {
        if let Some(c) = self.script.lock().unwrap().pop_front() {
            return Ok(c);
        }
        Ok((self.fallback.lock().unwrap())(&req))
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}

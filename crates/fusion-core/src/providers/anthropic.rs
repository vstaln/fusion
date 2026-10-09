//! Anthropic Messages API client.
use anyhow::{bail, Context};
use serde_json::{json, Value};

use crate::model::{Completion, Content, Message, ModelClient, Request, Role};

pub struct AnthropicClient {
    http: reqwest::Client,
    base: String,
    key: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(model: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into()),
            key: key.into(),
            model: model.into(),
        }
    }
}

fn msg_to_json(m: &Message) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let content: Vec<Value> = m
        .content
        .iter()
        .map(|c| match c {
            Content::Text(t) => json!({"type": "text", "text": t}),
            Content::ToolUse { id, name, input } => {
                json!({"type": "tool_use", "id": id, "name": name, "input": input})
            }
            Content::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            }),
        })
        .collect();
    json!({"role": role, "content": content})
}

#[async_trait::async_trait]
impl ModelClient for AnthropicClient {
    async fn complete(&self, req: Request<'_>) -> anyhow::Result<Completion> {
        let body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "system": req.system,
            "messages": req.messages.iter().map(msg_to_json).collect::<Vec<_>>(),
            "tools": req.tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })).collect::<Vec<_>>(),
        });
        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("anthropic request failed")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("anthropic {}: {}", status, &text[..text.len().min(500)]);
        }
        let v: Value = serde_json::from_str(&text)?;
        let mut content = Vec::new();
        for block in v["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => content.push(Content::Text(
                    block["text"].as_str().unwrap_or_default().to_string(),
                )),
                Some("tool_use") => content.push(Content::ToolUse {
                    id: block["id"].as_str().unwrap_or_default().to_string(),
                    name: block["name"].as_str().unwrap_or_default().to_string(),
                    input: block["input"].clone(),
                }),
                _ => {}
            }
        }
        Ok(Completion {
            content,
            stop_reason: v["stop_reason"].as_str().map(|s| s.to_string()),
        })
    }

    fn describe(&self) -> String {
        format!("anthropic:{}", self.model)
    }
}

//! OpenAI-compatible chat completions client (OpenAI, OpenRouter, Ollama, ...).
use anyhow::{bail, Context};
use serde_json::{json, Value};

use crate::model::{Completion, Content, Message, ModelClient, Request, Role};

pub struct OpenAiClient {
    http: reqwest::Client,
    base: String,
    key: String,
    model: String,
}

impl OpenAiClient {
    pub fn new(model: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            key: key.into(),
            model: model.into(),
        }
    }
}

fn msg_to_json(m: &Message) -> Vec<Value> {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut texts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();
    for c in &m.content {
        match c {
            Content::Text(t) => texts.push(t.clone()),
            Content::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()},
            })),
            Content::ToolResult {
                tool_use_id,
                content,
                ..
            } => tool_results.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            })),
        }
    }
    let mut out = Vec::new();
    if !texts.is_empty() || !tool_calls.is_empty() {
        let mut msg = json!({"role": role});
        if tool_calls.is_empty() {
            msg["content"] = json!(texts.join("\n"));
        } else {
            msg["content"] = if texts.is_empty() {
                Value::Null
            } else {
                json!(texts.join("\n"))
            };
            msg["tool_calls"] = json!(tool_calls);
        }
        out.push(msg);
    }
    out.extend(tool_results);
    out
}

#[async_trait::async_trait]
impl ModelClient for OpenAiClient {
    async fn complete(&self, req: Request<'_>) -> anyhow::Result<Completion> {
        let mut messages = vec![json!({"role": "system", "content": req.system})];
        for m in req.messages {
            messages.extend(msg_to_json(m));
        }
        let body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
            "tools": req.tools.iter().map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                },
            })).collect::<Vec<_>>(),
        });
        let mut rb = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .json(&body);
        if !self.key.is_empty() {
            rb = rb.bearer_auth(&self.key);
        }
        let resp = rb.send().await.context("openai request failed")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("openai {}: {}", status, &text[..text.len().min(500)]);
        }
        let v: Value = serde_json::from_str(&text)?;
        let msg = &v["choices"][0]["message"];
        let mut content = Vec::new();
        if let Some(t) = msg["content"].as_str() {
            if !t.is_empty() {
                content.push(Content::Text(t.to_string()));
            }
        }
        for tc in msg["tool_calls"].as_array().into_iter().flatten() {
            let input = tc["function"]["arguments"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(json!({}));
            content.push(Content::ToolUse {
                id: tc["id"].as_str().unwrap_or_default().to_string(),
                name: tc["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                input,
            });
        }
        Ok(Completion {
            content,
            stop_reason: v["choices"][0]["finish_reason"].as_str().map(String::from),
        })
    }

    fn describe(&self) -> String {
        format!("openai:{}", self.model)
    }
}

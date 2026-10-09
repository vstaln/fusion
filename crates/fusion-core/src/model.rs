//! Canonical message model shared by all providers.
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![Content::Text(text.into())],
        }
    }

    pub fn tool_results(results: Vec<(String, String, bool)>) -> Self {
        Self {
            role: Role::User,
            content: results
                .into_iter()
                .map(|(tool_use_id, content, is_error)| Content::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                })
                .collect(),
        }
    }

    /// All text blocks concatenated.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub content: Vec<Content>,
    pub stop_reason: Option<String>,
}

impl Completion {
    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
                _ => None,
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

pub struct Request<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDef],
    pub max_tokens: u32,
}

#[async_trait::async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(&self, req: Request<'_>) -> anyhow::Result<Completion>;
    fn describe(&self) -> String;
}

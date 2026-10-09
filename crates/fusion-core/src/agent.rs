//! One agent: a model client + a toolset + a persistent message history,
//! with an inbox for mid-run message injection.
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;
use tokio::sync::mpsc;

use crate::model::{Content, Message, ModelClient, Request, Role, ToolDef};
use crate::tools::{Tool, ToolCtx, ToolOutput};

/// Things an agent reports while it runs. Handed to a callback per event.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Text(String),
    ToolCall {
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        name: String,
        is_error: bool,
    },
    Injected(String),
    Note(String),
    Error(String),
}

pub type EventSink = Arc<dyn Fn(&str, AgentEvent) + Send + Sync>;

pub fn noop_sink() -> EventSink {
    Arc::new(|_, _| {})
}

pub struct Agent {
    pub name: String,
    client: Arc<dyn ModelClient>,
    system: String,
    tools: HashMap<String, Arc<dyn Tool>>,
    tool_defs: Vec<ToolDef>,
    history: Vec<Message>,
    inbox: mpsc::Receiver<String>,
    notes: Vec<String>,
    ctx: ToolCtx,
    events: EventSink,
    max_tokens: u32,
    max_iterations: usize,
}

pub struct AgentBuilder {
    name: String,
    client: Arc<dyn ModelClient>,
    system: String,
    ctx: ToolCtx,
    events: EventSink,
    max_tokens: u32,
    max_iterations: usize,
}

impl AgentBuilder {
    pub fn new(
        name: impl Into<String>,
        client: Arc<dyn ModelClient>,
        system: impl Into<String>,
        ctx: ToolCtx,
    ) -> Self {
        Self {
            name: name.into(),
            client,
            system: system.into(),
            ctx,
            events: noop_sink(),
            max_tokens: 16_000,
            max_iterations: 200,
        }
    }

    pub fn events(mut self, sink: EventSink) -> Self {
        self.events = sink;
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Fresh agent plus the sender half of its injection channel.
    pub fn build(self, tools: Vec<Arc<dyn Tool>>) -> (Agent, mpsc::Sender<String>) {
        let (tx, rx) = mpsc::channel(64);
        (self.build_with_inbox(tools, rx), tx)
    }

    /// Fresh agent driven by a caller-provided injection channel — lets the
    /// owner keep the sender before the agent exists.
    pub fn build_with_inbox(
        self,
        tools: Vec<Arc<dyn Tool>>,
        inbox: mpsc::Receiver<String>,
    ) -> Agent {
        let agent = Agent {
            name: self.name,
            client: self.client,
            system: self.system,
            tool_defs: tools.iter().map(|t| t.def()).collect(),
            tools: tools
                .into_iter()
                .map(|t| (t.def().name.clone(), t))
                .collect(),
            history: Vec::new(),
            inbox,
            notes: Vec::new(),
            ctx: self.ctx,
            events: self.events,
            max_tokens: self.max_tokens,
            max_iterations: self.max_iterations,
        };
        agent
    }
}

impl Agent {
    /// Queue a note appended as a user-role `<system_guidance>` message
    /// before the next model call — the harness's way of nudging the agent.
    pub fn inject_note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    fn drain_pending(&mut self) {
        while let Ok(msg) = self.inbox.try_recv() {
            (self.events)(&self.name, AgentEvent::Injected(msg.clone()));
            self.history.push(Message::text(Role::User, msg));
        }
        for note in std::mem::take(&mut self.notes) {
            (self.events)(&self.name, AgentEvent::Note(note.clone()));
            self.history.push(Message::text(
                Role::User,
                format!("<system_guidance>\n{note}\n</system_guidance>"),
            ));
        }
    }

    /// One turn: `input` goes in as a user message, then model<->tools until
    /// the model stops calling tools. Returns the final assistant text.
    /// Mid-run messages (interrupts, async reports, notes) are folded in
    /// between iterations.
    pub async fn run_turn(&mut self, input: impl Into<String>) -> anyhow::Result<String> {
        self.history.push(Message::text(Role::User, input.into()));
        self.drive().await
    }

    /// Re-enter the loop without new user input — for when an inbox
    /// notification (e.g. an async sidekick report) should re-wake the agent.
    pub async fn resume(&mut self) -> anyhow::Result<String> {
        self.drive().await
    }

    async fn drive(&mut self) -> anyhow::Result<String> {
        for _ in 0..self.max_iterations {
            self.drain_pending();
            let completion = self
                .client
                .complete(Request {
                    system: &self.system,
                    messages: &self.history,
                    tools: &self.tool_defs,
                    max_tokens: self.max_tokens,
                })
                .await?;
            self.history.push(Message {
                role: Role::Assistant,
                content: completion.content.clone(),
            });
            let tool_uses = completion.tool_uses();
            if tool_uses.is_empty() {
                let text = completion.text();
                (self.events)(&self.name, AgentEvent::Text(text.clone()));
                return Ok(text);
            }
            if !completion.text().is_empty() {
                (self.events)(&self.name, AgentEvent::Text(completion.text()));
            }
            let mut results = Vec::with_capacity(tool_uses.len());
            for (id, name, input) in tool_uses {
                (self.events)(
                    &self.name,
                    AgentEvent::ToolCall {
                        name: name.to_string(),
                        input: input.clone(),
                    },
                );
                let out = match self.tools.get(name) {
                    Some(t) => t.call(input.clone(), &self.ctx).await,
                    None => ToolOutput::err(format!("unknown tool `{name}`")),
                };
                (self.events)(
                    &self.name,
                    AgentEvent::ToolResult {
                        name: name.to_string(),
                        is_error: out.is_error,
                    },
                );
                results.push((id.to_string(), out.content, out.is_error));
            }
            self.history.push(Message::tool_results(results));
        }
        anyhow::bail!("exceeded {} iterations", self.max_iterations)
    }

    /// Serialized history, e.g. for a handoff-conversation transcript.
    pub fn transcript(&self) -> String {
        let mut s = String::new();
        for m in &self.history {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            for c in &m.content {
                match c {
                    Content::Text(t) => s.push_str(&format!("## {role}\n{t}\n\n")),
                    Content::ToolUse { name, input, .. } => {
                        s.push_str(&format!("## {role} tool:{name}\n{}\n\n", input))
                    }
                    Content::ToolResult { content, .. } => {
                        let head: String = content.chars().take(500).collect();
                        s.push_str(&format!("## tool_result\n{head}\n\n"))
                    }
                }
            }
        }
        s
    }
}

/// Convenience JSON-schema helper for object tools.
pub fn schema_object(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

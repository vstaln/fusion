//! ModelClient backed by a persistent `devin acp` child process — the
//! ToS-safe path to subscription models (SWE-2 etc.) per Agent Client
//! Protocol. The child runs with every native Devin tool denied, so the
//! model is a text-only responder; our tools ride as fenced
//! `tool_call` JSON blocks in its text.
//!
//! Layout cribbed from gray-devin-sub (vstaln/gray-devin-sub).
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::model::{Completion, Content, Message, ModelClient, Request, Role};

const DENY_RULES: &[&str] = &[
    "read",
    "edit",
    "grep",
    "glob",
    "exec",
    "Fetch(domain:*)",
    "mcp__*",
];

/// Instructions appended to the system prompt for ACP-backed models:
/// tools exist only as a text convention.
pub fn tool_spec_text(tools: &[crate::model::ToolDef]) -> String {
    let mut s = String::from(
        "\n\n## Tool calling convention\n\
         You have NO native tools — attempts to call them fail silently. Your ONLY way to \
         act is a fenced block at the END of your reply:\n\
         ```tool_call\n[{\"name\": \"<tool>\", \"arguments\": {...}}]\n```\n\
         (a single object is accepted too; you may batch several). Rules:\n\
         - To act, your reply MUST contain a tool_call block. Prose like \"I'll now \
         run X\" does nothing — emit the block instead.\n\
         - After emitting a block, STOP. Results return as `[tool_result <id>] ...`.\n\
         - When results arrive, continue emitting tool_call blocks until the task is \
         done. A reply with NO block is your FINAL report and ends the task — only \
         stop emitting calls when you are actually finished.\n\
         Available tools:\n",
    );
    for t in tools {
        s.push_str(&format!(
            "- `{}`: {}\n  schema: {}\n",
            t.name, t.description, t.input_schema
        ));
    }
    s
}

/// Parse fenced ```tool_call blocks out of model text.
fn parse_tool_calls(text: &str, next_id: &mut usize) -> (String, Vec<Content>) {
    let mut calls = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("```tool_call") {
        plain.push_str(&rest[..start]);
        rest = &rest[start + "```tool_call".len()..];
        match rest.find("```") {
            Some(end) => {
                let body = rest[..end].trim();
                rest = &rest[end + 3..];
                let v: Result<Value, _> = serde_json::from_str(body);
                match v {
                    Ok(Value::Array(items)) => {
                        for item in items {
                            push_call(item, &mut calls, next_id);
                        }
                    }
                    Ok(item @ Value::Object(_)) => push_call(item, &mut calls, next_id),
                    _ => {
                        plain.push_str("```tool_call\n");
                        plain.push_str(body);
                        plain.push_str("\n```");
                    }
                }
            }
            None => {
                plain.push_str("```tool_call");
                plain.push_str(rest);
                rest = "";
            }
        }
    }
    plain.push_str(rest);
    (plain.trim().to_string(), calls)
}

fn push_call(item: Value, calls: &mut Vec<Content>, next_id: &mut usize) {
    let name = item["name"].as_str().unwrap_or_default().to_string();
    if name.is_empty() {
        return;
    }
    *next_id += 1;
    calls.push(Content::ToolUse {
        id: format!("fcall_{next_id}"),
        name,
        input: item["arguments"].clone(),
    });
}

struct Child_ {
    // Kept alive by holding the handle; drop kills the process.
    #[allow(dead_code)]
    proc: Child,
    stdin: ChildStdin,
    rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
}

fn scratch_stage() -> anyhow::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("fusion-acp-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".devin"))?;
    std::fs::write(
        dir.join(".devin/config.json"),
        json!({"permissions": {"deny": DENY_RULES}}).to_string(),
    )?;
    std::fs::write(
        dir.join("user-config.json"),
        json!({
            "version": 1,
            "subagents_enabled": false,
            "auto_update": false,
            "notify": "never",
            "permissions": {"deny": DENY_RULES, "allow": [], "ask": []},
        })
        .to_string(),
    )?;
    Ok(dir)
}

impl Child_ {
    async fn spawn(model: &str) -> anyhow::Result<Self> {
        let binary = std::env::var("FUSION_ACP_COMMAND").unwrap_or_else(|_| "devin".into());
        let stage = scratch_stage()?;
        let mut proc = Command::new(&binary)
            .args([
                "--config",
                &stage.join("user-config.json").to_string_lossy(),
                "acp",
                "--model",
                model,
            ])
            .current_dir(&stage)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear()
            .envs(
                [
                    "PATH",
                    "HOME",
                    "USER",
                    "LANG",
                    "XDG_CONFIG_HOME",
                    "XDG_DATA_HOME",
                    "XDG_CACHE_HOME",
                ]
                .iter()
                .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v))),
            )
            .env("BROWSER", "/bin/true")
            .env("DISPLAY", "")
            .env("WAYLAND_DISPLAY", "")
            .spawn()
            .with_context(|| format!("spawn `{binary} acp`"))?;
        let stdin = proc.stdin.take().context("child stdin")?;
        let stdout = proc.stdout.take().context("child stdout")?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(_) => tracing::debug!(target: "fusion::acp", "non-json line: {line}"),
                }
            }
        });
        Ok(Self {
            proc,
            stdin,
            rx,
            next_id: 0,
        })
    }

    async fn send(&mut self, v: Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(&v)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Call a method; collect `agent_message_chunk` text into `text_out`
    /// while waiting. Server→client requests are answered cancelled.
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        mut text_out: Option<&mut String>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        self.next_id += 1;
        let my_id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":my_id,"method":method,"params":params}))
            .await?;
        let fut = async {
            loop {
                let v = self.rx.recv().await.context("acp child closed stdout")?;
                // Server→client request: answer and continue.
                if v.get("method").is_some() && v.get("id").is_some() {
                    let method = v["method"].as_str().unwrap_or("");
                    let rid = v["id"].clone();
                    let result = if method == "session/request_permission" {
                        json!({"outcome": {"outcome": "cancelled"}})
                    } else {
                        json!({"code": -32601, "message": "unsupported"})
                    };
                    let msg = if method == "session/request_permission" {
                        json!({"jsonrpc":"2.0","id":rid,"result":result})
                    } else {
                        json!({"jsonrpc":"2.0","id":rid,"error":result})
                    };
                    self.send(msg).await?;
                    continue;
                }
                // session/update notification → fold text chunks.
                if v["method"].as_str() == Some("session/update") {
                    let upd = &v["params"]["update"];
                    if upd["sessionUpdate"].as_str() == Some("agent_message_chunk") {
                        if let Some(t) = upd["content"]["text"].as_str() {
                            if let Some(out) = text_out.as_deref_mut() {
                                out.push_str(t);
                            }
                        }
                    }
                    continue;
                }
                // Response to our request?
                if v.get("id").and_then(Value::as_u64) == Some(my_id) {
                    if let Some(err) = v.get("error") {
                        bail!("{method} failed: {err}");
                    }
                    return Ok(v["result"].clone());
                }
            }
        };
        tokio::time::timeout(timeout, fut)
            .await
            .context(format!("{method} timed out"))?
    }
}

pub struct AcpClient {
    model: String,
    child: tokio::sync::Mutex<Option<Child_>>,
    call_counter: Mutex<usize>,
    /// Index into the history: user messages below this line were already
    /// sent upstream (assistant turns never are — the session generated
    /// them itself).
    sent_up_to: Mutex<usize>,
    system_sent: Mutex<bool>,
    session_id: Mutex<String>,
}

impl AcpClient {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            child: tokio::sync::Mutex::new(None),
            call_counter: Mutex::new(0),
            sent_up_to: Mutex::new(0),
            system_sent: Mutex::new(false),
            session_id: Mutex::new(String::new()),
        }
    }

    /// Render user-role messages as prompt text. Tool results become
    /// `[tool_result id]` blocks so the model can correlate them.
    fn render_delta(&self, req: &Request<'_>) -> (String, bool) {
        let mut sent = self.sent_up_to.lock().unwrap();
        let mut parts = Vec::new();
        let mut has_results = false;
        if !*self.system_sent.lock().unwrap() {
            parts.push(format!(
                "<system>\n{}{}\n</system>",
                req.system,
                tool_spec_text(req.tools)
            ));
            *self.system_sent.lock().unwrap() = true;
        }
        for m in &req.messages[*sent..] {
            if m.role != Role::User {
                continue;
            }
            for c in &m.content {
                match c {
                    Content::Text(t) => parts.push(t.clone()),
                    Content::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        has_results = true;
                        parts.push(format!(
                            "[tool_result {tool_use_id}{}] {content}",
                            if *is_error { " error" } else { "" }
                        ))
                    }
                    Content::ToolUse { .. } => {}
                }
            }
        }
        *sent = req.messages.len();
        (parts.join("\n\n"), has_results)
    }
}

#[async_trait::async_trait]
impl ModelClient for AcpClient {
    async fn complete(&self, req: Request<'_>) -> anyhow::Result<Completion> {
        // One prompt in flight per child; serialize via the child mutex.
        let mut guard = self.child.lock().await;
        if guard.is_none() {
            let mut c = Child_::spawn(&self.model).await?;
            c.call(
                "initialize",
                json!({"protocolVersion": 1,
                    "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false},
                        "terminal": false}}),
                None,
                Duration::from_secs(60),
            )
            .await?;
            let new = c
                .call(
                    "session/new",
                    json!({"cwd": std::env::temp_dir().join(format!("fusion-acp-{}", std::process::id())), "mcpServers": []}),
                    None,
                    Duration::from_secs(60),
                )
                .await?;
            if new.get("sessionId").is_none() {
                bail!("session/new returned no sessionId");
            }
            let sid = new["sessionId"].as_str().unwrap_or_default().to_string();
            *guard = Some(c);
            *self.session_id.lock().unwrap() = sid;
        }

        let (prompt_text, mut mid_task) = self.render_delta(&req);
        if prompt_text.trim().is_empty() {
            return Ok(Completion {
                content: vec![Content::Text(String::new())],
                stop_reason: None,
            });
        }

        let c = guard.as_mut().unwrap();
        let sid = self.session_id.lock().unwrap().clone();
        let mut text = String::new();
        let mut prompt = prompt_text;
        // Protocol recovery: a reply with no tool_call block mid-task is a
        // premature stop — nudge the model to continue (bounded).
        let mut result = json!(null);
        for attempt in 0..3 {
            text.clear();
            result = c
                .call(
                    "session/prompt",
                    json!({"sessionId": sid, "prompt": [{"type":"text","text": prompt}]}),
                    Some(&mut text),
                    Duration::from_secs(600),
                )
                .await?;
            let stop = result["stopReason"].as_str().unwrap_or_default();
            if stop == "refusal" {
                bail!("devin refused this request (usage policy)");
            }
            let has_calls = {
                let mut n = self.call_counter.lock().unwrap();
                let (_, calls) = parse_tool_calls(&text, &mut n);
                !calls.is_empty()
            };
            if has_calls || (!mid_task && !text.trim().is_empty()) {
                break;
            }
            mid_task = false;
            prompt = if text.trim().is_empty() {
                "Your reply was empty. To act, emit a fenced ```tool_call block at the                  end of your reply; to finish, write your final report."
                    .to_string()
            } else {
                "You stopped without a tool_call block but the task is not done. Either                  emit the next ```tool_call block now, or — only if the work is actually                  finished — write your final report."
                    .to_string()
            };
            let _ = attempt;
        }
        let stop = result["stopReason"].as_str().unwrap_or_default();
        let (plain, calls) = {
            let mut n = self.call_counter.lock().unwrap();
            parse_tool_calls(&text, &mut n)
        };
        let mut content = Vec::new();
        if !plain.is_empty() {
            content.push(Content::Text(plain));
        }
        content.extend(calls);
        if content.is_empty() {
            content.push(Content::Text("(empty response)".into()));
        }
        Ok(Completion {
            content,
            stop_reason: Some(stop.to_string()),
        })
    }

    fn describe(&self) -> String {
        format!("acp:{}", self.model)
    }
}

//! The Fusion orchestrator: a lead agent plus ONE persistent sidekick
//! worker, connected by a `handoff` tool with block / inject semantics.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use crate::agent::{noop_sink, schema_object, Agent, AgentBuilder, AgentEvent, EventSink};
use crate::model::{ModelClient, ToolDef};
use crate::prompts;
use crate::tools::bash::BashTool;
use crate::tools::{Tool, ToolCtx, ToolOutput};

struct SidekickSlot {
    agent: Option<Agent>,
    agent_tx: Option<mpsc::Sender<String>>,
    running: bool,
    ever_dispatched: bool,
    last_report: Option<String>,
    finished: Arc<Notify>,
}

impl Default for SidekickSlot {
    fn default() -> Self {
        Self {
            agent: None,
            agent_tx: None,
            running: false,
            ever_dispatched: false,
            last_report: None,
            finished: Arc::new(Notify::new()),
        }
    }
}

struct SidekickFactory {
    client: Arc<dyn ModelClient>,
    system: String,
    ctx: ToolCtx,
    events: EventSink,
    max_tokens: u32,
    tools: Vec<Arc<dyn Tool>>,
}

impl SidekickFactory {
    fn spawn(&self) -> (Agent, mpsc::Sender<String>) {
        let mut b = AgentBuilder::new(
            "sidekick",
            self.client.clone(),
            self.system.clone(),
            self.ctx.clone(),
        )
        .events(self.events.clone())
        .max_tokens(self.max_tokens);
        let _ = &mut b;
        b.build(self.tools.clone())
    }
}

struct NudgeState {
    mutations: AtomicUsize,
    ever_dispatched: AtomicBool,
    fired: AtomicUsize,
    lead_tx: mpsc::Sender<String>,
    enabled: bool,
}

fn looks_mutating(cmd: &str) -> bool {
    const PATTERNS: &[&str] = &[
        ">",
        "sed -i",
        "apply_patch",
        "patch ",
        "tee ",
        "mv ",
        "cp ",
        "rm ",
        "mkdir",
        "touch ",
        "chmod",
        "ln -s",
        "git apply",
        "cargo add",
        "npm i ",
        "npm install",
        "pnpm add",
        "pip install",
        "truncate",
    ];
    PATTERNS.iter().any(|p| cmd.contains(p))
}

/// Wraps the lead's bash tool to inject a delegation reminder when the
/// lead starts doing mutating work itself. At most two nudges per session,
/// disabled once any handoff has been dispatched.
struct LeadBash {
    inner: BashTool,
    state: Arc<NudgeState>,
}

#[async_trait::async_trait]
impl Tool for LeadBash {
    fn def(&self) -> ToolDef {
        self.inner.def()
    }
    async fn call(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        if self.state.enabled && !self.state.ever_dispatched.load(Ordering::Relaxed) {
            if let Some(cmd) = input["command"].as_str() {
                if looks_mutating(cmd) {
                    let n = self.state.mutations.fetch_add(1, Ordering::Relaxed) + 1;
                    let should_fire =
                        (n == 2 || n == 5) && self.state.fired.load(Ordering::Relaxed) < 2;
                    if should_fire {
                        self.state.fired.fetch_add(1, Ordering::Relaxed);
                        let _ = self.state.lead_tx.try_send(format!(
                            "<system_guidance>\n{}\n</system_guidance>",
                            prompts::DELEGATE_NUDGE
                        ));
                    }
                }
            }
        }
        self.inner.call(input, ctx).await
    }
}

/// The `handoff` tool: dispatch work to the persistent sidekick, inject an
/// interrupt into a running handoff, or — with `block: false` — run it in
/// the background with the report arriving later as a notification.
struct HandoffTool {
    slot: Arc<Mutex<SidekickSlot>>,
    factory: SidekickFactory,
    nudge: Arc<NudgeState>,
    lead_tx: mpsc::Sender<String>,
}

impl HandoffTool {
    fn spawn_handoff(
        &self,
        agent: Option<Agent>,
        first: bool,
        brief: String,
        deliver_async: bool,
    ) -> JoinHandle<(String, bool)> {
        let mut agent = match agent {
            Some(a) => a,
            None => {
                let (a, tx) = self.factory.spawn();
                self.slot.lock().unwrap().agent_tx = Some(tx);
                a
            }
        };
        let slot = Arc::clone(&self.slot);
        let lead_tx = self.lead_tx.clone();
        let wrapped = prompts::wrap_brief(first, &brief);
        let events = self.factory.events.clone();
        tokio::spawn(async move {
            let (report, failed) = match agent.run_turn(wrapped).await {
                Ok(r) => (r, false),
                Err(e) => {
                    (events)("sidekick", AgentEvent::Error(format!("{e:#}")));
                    (format!("Handoff failed: {e:#}"), true)
                }
            };
            {
                let mut slot = slot.lock().unwrap();
                slot.agent = Some(agent);
                slot.running = false;
                slot.last_report = Some(report.clone());
                slot.finished.notify_waiters();
            }
            if deliver_async {
                let _ = lead_tx.send(prompts::wrap_report(&report)).await;
            }
            (report, failed)
        })
    }
}

#[async_trait::async_trait]
impl Tool for HandoffTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "handoff".into(),
            description: "Hand off work to your persistent sidekick subagent. There is \
                exactly one sidekick for the whole session: its conversation context and \
                runtime persist across handoffs, and it runs on the same machine (shared \
                filesystem, separate shell sessions). With `block: true` (default) the call \
                waits and returns the sidekick's report directly. With `block: false` it \
                returns immediately and the report arrives later as a notification. Calling \
                this while the sidekick is running injects `brief` into the running handoff \
                as an interrupt rather than starting a second sidekick. The sidekick cannot \
                talk to the user and does not own commits, pushes, pull requests or CI; \
                user-facing and authority actions stay with you. Correctness-critical \
                authoring (queries against shared/production data systems, eval/grader/prompt \
                text, scoring/pipeline config) is NOT delegable: author it yourself first, \
                then hand off mechanical execution."
                .into(),
            input_schema: schema_object(
                json!({
                    "brief": {
                        "type": "string",
                        "description": "The task brief: goal, plan, constraints, literal \
                            commands/code where decided, and how to verify."
                    },
                    "block": {
                        "type": "boolean",
                        "description": "Wait for the handoff to finish (default true). \
                            Pass false only for genuine parallel lead work."
                    },
                    "label": {
                        "type": "string",
                        "description": "Optional short label for the handoff."
                    }
                }),
                &["brief"],
            ),
        }
    }

    async fn call(&self, input: Value, _ctx: &ToolCtx) -> ToolOutput {
        let brief = input["brief"]
            .as_str()
            .or_else(|| input["message"].as_str())
            .unwrap_or_default()
            .to_string();
        if brief.is_empty() {
            return ToolOutput::err("`brief` is required");
        }
        let block = input["block"].as_bool().unwrap_or(true);

        // Mid-handoff: inject as interrupt.
        let running_tx = {
            let slot = self.slot.lock().unwrap();
            if slot.running {
                slot.agent_tx.clone()
            } else {
                None
            }
        };
        if let Some(tx) = running_tx {
            return match tx.send(prompts::wrap_update(&brief)).await {
                Ok(()) => {
                    ToolOutput::ok("Brief injected into the running handoff as an interrupt.")
                }
                Err(_) => {
                    ToolOutput::err("Sidekick finished during injection; brief not delivered.")
                }
            };
        }

        // New handoff.
        let (agent, first) = {
            let mut slot = self.slot.lock().unwrap();
            slot.running = true;
            let first = !slot.ever_dispatched;
            slot.ever_dispatched = true;
            (slot.agent.take(), first)
        };
        self.nudge.ever_dispatched.store(true, Ordering::Relaxed);
        let handle = self.spawn_handoff(agent, first, brief, !block);

        if block {
            match handle.await {
                Ok((report, false)) => {
                    ToolOutput::ok(format!("Sidekick completed the handoff:\n\n{report}"))
                }
                Ok((report, true)) => ToolOutput::err(report),
                Err(e) => ToolOutput::err(format!("Handoff task failed: {e}")),
            }
        } else {
            ToolOutput::ok(
                "Handoff dispatched. The sidekick's report will arrive as a \
                 notification when it finishes; wait for it with `wait` rather \
                 than polling.",
            )
        }
    }
}

/// `wait`: block until the running handoff finishes (or a timeout), then
/// return its report — the lead's way of re-joining a background handoff.
struct WaitTool {
    slot: Arc<Mutex<SidekickSlot>>,
    default_secs: u64,
    max_secs: u64,
}

#[async_trait::async_trait]
impl Tool for WaitTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "wait".into(),
            description: "Wait for the running sidekick handoff to finish and return its \
                report. Use after a `handoff` with `block: false` once your parallel work \
                is done, or after a blocking call timed out. Do not call in a loop."
                .into(),
            input_schema: schema_object(
                json!({
                    "seconds": {
                        "type": "integer",
                        "description": "Max seconds to wait (default 300)."
                    }
                }),
                &[],
            ),
        }
    }

    async fn call(&self, input: Value, _ctx: &ToolCtx) -> ToolOutput {
        let secs = input["seconds"]
            .as_u64()
            .unwrap_or(self.default_secs)
            .min(self.max_secs);
        let finished = {
            let slot = self.slot.lock().unwrap();
            if !slot.running {
                return match &slot.last_report {
                    Some(r) => {
                        ToolOutput::ok(format!("No handoff is running. Most recent report:\n\n{r}"))
                    }
                    None => ToolOutput::ok("No handoff has run yet."),
                };
            }
            slot.finished.clone()
        };
        // Register before awaiting to avoid a missed notification.
        let notified = finished.notified();
        match tokio::time::timeout(Duration::from_secs(secs), notified).await {
            Ok(()) => {
                let slot = self.slot.lock().unwrap();
                match &slot.last_report {
                    Some(r) => ToolOutput::ok(format!("Sidekick completed the handoff:\n\n{r}")),
                    None => ToolOutput::ok("Handoff finished without a report."),
                }
            }
            Err(_) => ToolOutput::ok(format!(
                "Sidekick still running after {secs}s. Its report will arrive as a \
                 notification when it finishes."
            )),
        }
    }
}

pub struct Fusion {
    lead: Agent,
    slot: Arc<Mutex<SidekickSlot>>,
    lead_tx: mpsc::Sender<String>,
}

pub struct FusionBuilder {
    lead_client: Arc<dyn ModelClient>,
    sidekick_client: Arc<dyn ModelClient>,
    ctx: ToolCtx,
    events: EventSink,
    lead_identity: String,
    lead_prompt: Option<String>,
    sidekick_prompt: Option<String>,
    max_tokens: u32,
    nudge: bool,
}

impl FusionBuilder {
    pub fn new(
        lead_client: Arc<dyn ModelClient>,
        sidekick_client: Arc<dyn ModelClient>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            lead_client,
            sidekick_client,
            ctx: ToolCtx { cwd: cwd.into() },
            events: noop_sink(),
            lead_identity: "the lead agent".into(),
            lead_prompt: None,
            sidekick_prompt: None,
            max_tokens: 16_000,
            nudge: true,
        }
    }

    pub fn events(mut self, sink: EventSink) -> Self {
        self.events = sink;
        self
    }
    pub fn lead_identity(mut self, id: impl Into<String>) -> Self {
        self.lead_identity = id.into();
        self
    }
    pub fn lead_prompt(mut self, p: impl Into<String>) -> Self {
        self.lead_prompt = Some(p.into());
        self
    }
    pub fn sidekick_prompt(mut self, p: impl Into<String>) -> Self {
        self.sidekick_prompt = Some(p.into());
        self
    }
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
    pub fn nudge(mut self, on: bool) -> Self {
        self.nudge = on;
        self
    }

    pub fn build(self) -> Fusion {
        let slot = Arc::new(Mutex::new(SidekickSlot::default()));
        let (lead_tx, lead_rx) = mpsc::channel::<String>(64);
        let nudge = Arc::new(NudgeState {
            mutations: AtomicUsize::new(0),
            ever_dispatched: AtomicBool::new(false),
            fired: AtomicUsize::new(0),
            lead_tx: lead_tx.clone(),
            enabled: self.nudge,
        });

        let factory = SidekickFactory {
            client: self.sidekick_client,
            system: self
                .sidekick_prompt
                .unwrap_or_else(|| prompts::SIDEKICK_PROMPT.to_string()),
            ctx: self.ctx.clone(),
            events: self.events.clone(),
            max_tokens: self.max_tokens,
            tools: vec![Arc::new(BashTool::default())],
        };

        let lead_tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(LeadBash {
                inner: BashTool::default(),
                state: nudge.clone(),
            }),
            Arc::new(HandoffTool {
                slot: slot.clone(),
                factory,
                nudge,
                lead_tx: lead_tx.clone(),
            }),
            Arc::new(WaitTool {
                slot: slot.clone(),
                default_secs: 300,
                max_secs: 3600,
            }),
        ];

        let system = self
            .lead_prompt
            .unwrap_or_else(|| prompts::LEAD_PROMPT.to_string())
            .replace("{LEAD_IDENTITY}", &self.lead_identity);

        let lead = AgentBuilder::new("lead", self.lead_client, system, self.ctx)
            .events(self.events)
            .max_tokens(self.max_tokens)
            .build_with_inbox(lead_tools, lead_rx);

        Fusion {
            lead,
            slot,
            lead_tx,
        }
    }
}

impl Fusion {
    /// Run one user turn on the lead. Returns the lead's final text.
    ///
    /// A `block: false` handoff can still be running when the lead stops.
    /// Its report lands in the lead inbox on completion — wait for it and
    /// let the lead react rather than losing the work at end of turn.
    pub async fn run(&mut self, input: impl Into<String>) -> anyhow::Result<String> {
        let mut out = self.lead.run_turn(input).await?;
        for _ in 0..4 {
            let notify = {
                let slot = self.slot.lock().unwrap();
                if !slot.running {
                    return Ok(out);
                }
                slot.finished.clone()
            };
            let mut wait = Box::pin(notify.notified());
            wait.as_mut().enable();
            if tokio::time::timeout(Duration::from_secs(600), wait)
                .await
                .is_err()
            {
                return Ok(out); // sidekick wedged — don't hang the run
            }
            out = self.lead.resume().await?;
        }
        Ok(out)
    }

    /// Channel for external steering: anything sent here lands in the
    /// lead's context as a user message on its next iteration — how a host
    /// harness delivers user messages that arrive mid-run.
    pub fn lead_inbox(&self) -> mpsc::Sender<String> {
        self.lead_tx.clone()
    }

    pub fn sidekick_running(&self) -> bool {
        self.slot.lock().unwrap().running
    }

    pub fn lead_transcript(&self) -> String {
        self.lead.transcript()
    }

    pub fn sidekick_transcript(&self) -> Option<String> {
        self.slot
            .lock()
            .unwrap()
            .agent
            .as_ref()
            .map(|a| a.transcript())
    }
}

/// Emitted on the event sink with agent names "lead"/"sidekick".
pub use crate::agent::AgentEvent as FusionEvent;

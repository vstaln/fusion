//! fusion-core — dual-model agent orchestration.
//!
//! A **lead** model runs a normal tool loop with one extra tool, `handoff`,
//! that drives a **persistent sidekick** worker: exactly one per session,
//! keeping its conversation and shell runtime across handoffs. Handoffs can
//! block (report returned in-band), run in the background (report arrives as
//! an injected notification), or be steered mid-flight (a second call
//! injects an interrupt into the running handoff).
pub mod agent;
pub mod fusion;
pub mod mock;
pub mod model;
pub mod prompts;
pub mod providers;
pub mod tools;

pub use agent::{Agent, AgentBuilder, AgentEvent, EventSink};
pub use fusion::{Fusion, FusionBuilder};
pub use model::{Completion, Content, Message, ModelClient, Role, ToolDef};
pub use tools::{Tool, ToolCtx, ToolOutput};

use fusion_core::agent::AgentEvent;
use fusion_core::fusion::FusionBuilder;
use fusion_core::mock::ScriptedModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    eprintln!("build mocks");
    let lead = ScriptedModel::new(
        "lead",
        vec![
            ScriptedModel::tool_call("handoff", json!({"brief":"run echo hi","block":true})),
            ScriptedModel::text("done"),
        ],
    );
    let sk = ScriptedModel::new(
        "sidekick",
        vec![
            ScriptedModel::tool_call("bash", json!({"command":"echo hi"})),
            ScriptedModel::text("report"),
        ],
    );
    let sink: fusion_core::EventSink = Arc::new(|a: &str, e: AgentEvent| {
        eprintln!("  evt[{a}] {e:?}");
    });
    eprintln!("build fusion");
    let mut f = FusionBuilder::new(Arc::new(lead), Arc::new(sk), std::env::temp_dir())
        .nudge(false)
        .events(sink)
        .build();
    eprintln!("run");
    let r = tokio::time::timeout(std::time::Duration::from_secs(8), f.run("go")).await;
    eprintln!("result: {r:?}");
}

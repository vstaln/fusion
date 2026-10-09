use std::sync::Arc;

use fusion_core::fusion::FusionBuilder;
use fusion_core::mock::ScriptedModel;
use serde_json::json;

fn dir() -> std::path::PathBuf {
    std::env::temp_dir()
}

/// Lead delegates, sidekick runs a bash command, report returns in-band.
#[tokio::test]
async fn blocking_handoff_returns_report() {
    let lead = ScriptedModel::new(
        "lead",
        vec![
            ScriptedModel::tool_call(
                "handoff",
                json!({"brief": "run echo hi and report", "block": true}),
            ),
            ScriptedModel::text("done — sidekick verified it"),
        ],
    );
    let sidekick = ScriptedModel::new(
        "sidekick",
        vec![
            ScriptedModel::tool_call("bash", json!({"command": "echo hi"})),
            ScriptedModel::text("report: ran echo hi, saw hi"),
        ],
    );
    let mut f = FusionBuilder::new(Arc::new(lead), Arc::new(sidekick), dir())
        .nudge(false)
        .build();
    let out = f.run("test task").await.unwrap();
    assert_eq!(out, "done — sidekick verified it");
    let sk = f.sidekick_transcript().unwrap();
    assert!(sk.contains("first handoff"));
    assert!(sk.contains("hi"));
    assert!(!f.sidekick_running());
}

/// Second call while the sidekick works injects an interrupt, no respawn.
#[tokio::test]
async fn second_call_injects_into_running_handoff() {
    // sidekick: first call sleeps so the second handoff call can interleave
    // — but run_turn is sequential, so instead we verify the inject path by
    // calling the tool twice from the lead where the sidekick stays busy:
    // script sidekick to take a while via a slow bash command.
    let lead = ScriptedModel::new(
        "lead",
        vec![
            ScriptedModel::tool_call(
                "handoff",
                json!({"brief": "sleep then report", "block": false}),
            ),
            ScriptedModel::tool_call("handoff", json!({"brief": "also fold this in"})),
            ScriptedModel::text("wrapped"),
        ],
    );
    let sidekick = ScriptedModel::new(
        "sidekick",
        vec![
            ScriptedModel::tool_call("bash", json!({"command": "sleep 2; echo done"})),
            ScriptedModel::text("report: done"),
            // the injected <lead_update> arrives in the same turn's context
            ScriptedModel::text("report2: folded update in"),
        ],
    );
    let mut f = FusionBuilder::new(Arc::new(lead), Arc::new(sidekick), dir())
        .nudge(false)
        .build();
    let out = f.run("go").await.unwrap();
    assert_eq!(out, "wrapped");
    // sidekick is still mid-handoff (sleep 2); wait for it to finish,
    // then the injected update should be visible in its transcript.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let sk = f.sidekick_transcript().unwrap();
    assert!(sk.contains("<lead_update>") || sk.contains("fold this in"));
}

/// Lead bash calls that mutate files trigger a delegation nudge (once).
#[tokio::test]
async fn mutating_lead_commands_trigger_nudge() {
    let lead = ScriptedModel::new(
        "lead",
        vec![
            ScriptedModel::tool_call("bash", json!({"command": "echo a > /tmp/f1"})),
            ScriptedModel::tool_call("bash", json!({"command": "echo b > /tmp/f2"})),
            ScriptedModel::text("ok"),
        ],
    );
    let sidekick = ScriptedModel::new("sidekick", vec![]);
    let mut f = FusionBuilder::new(Arc::new(lead), Arc::new(sidekick), dir()).build();
    f.run("go").await.unwrap();
    assert!(f.lead_transcript().contains("<system_guidance>"));
}

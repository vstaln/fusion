use std::io::{BufRead, Write};
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use fusion_core::agent::AgentEvent;
use fusion_core::fusion::FusionBuilder;
use fusion_core::providers::client_from_spec;

#[derive(Parser)]
#[command(about = "fusion — a lead model + persistent sidekick, one session")]
struct Args {
    /// lead model spec, e.g. anthropic:claude-... or openai:gpt-...
    #[arg(long, env = "FUSION_LEAD")]
    lead: String,
    /// sidekick model spec
    #[arg(long, env = "FUSION_SIDEKICK")]
    sidekick: String,
    /// working directory for both agents
    #[arg(long, default_value = ".")]
    cwd: String,
    /// single prompt; omit for an interactive REPL
    prompt: Option<String>,
}

fn event_sink() -> fusion_core::EventSink {
    Arc::new(|agent: &str, ev: AgentEvent| {
        let tag = if agent == "lead" {
            "\x1b[36mlead\x1b[0m"
        } else {
            "\x1b[34msidekick\x1b[0m"
        };
        match ev {
            AgentEvent::ToolCall { name, input } => {
                let s = input.to_string();
                eprintln!("  {tag} → {name} {}", &s[..s.len().min(160)]);
            }
            AgentEvent::ToolResult { name, is_error } => {
                eprintln!("  {tag} ← {name}{}", if is_error { " (error)" } else { "" });
            }
            AgentEvent::Injected(m) => eprintln!("  {tag} ⇐ injected: {}", &m[..m.len().min(120)]),
            AgentEvent::Note(m) => eprintln!("  {tag} ⚠ nudge: {}", &m[..m.len().min(120)]),
            AgentEvent::Error(m) => eprintln!("  {tag} ✗ error: {}", &m[..m.len().min(400)]),
            AgentEvent::Text(_) => {}
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("fusion=warn")
        .init();
    let args = Args::parse();
    let cwd = std::fs::canonicalize(&args.cwd)?;
    let mut fusion = FusionBuilder::new(
        client_from_spec(&args.lead, &cwd)?,
        client_from_spec(&args.sidekick, &cwd)?,
        cwd.clone(),
    )
    .lead_identity("fusion")
    .events(event_sink())
    .build();

    if let Some(p) = args.prompt {
        println!("{}", fusion.run(p).await?);
        return Ok(());
    }

    eprintln!(
        "fusion — lead: {} · sidekick: {} · cwd: {}",
        args.lead,
        args.sidekick,
        cwd.display()
    );
    eprintln!("type a message; ctrl-d to quit");
    let stdin = std::io::stdin();
    loop {
        eprint!("\x1b[32myou>\x1b[0m ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match fusion.run(line).await {
            Ok(text) => println!("\n{text}\n"),
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
    Ok(())
}

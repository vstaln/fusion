# fusion

Open-source, Rust reimplementation of the dual-model orchestration in
Devin CLI's **Fusion** mode: a frontier **lead** model steers one
**persistent sidekick** worker through a `handoff` tool, so a session gets
frontier judgment at sidekick prices.

Clean-room: the mechanism was reverse-engineered; all prompt text is
written fresh for this project.

## How it works

```
user ──► lead ──(handoff {brief, block})──► sidekick (persistent agent)
          ▲            │                          │ same machine,
          │            │ block=true: report       │ shared fs, own shell
          │            │   returned in-band       │
          │            │ block=false: report      │ keeps context AND
          │            │   arrives as notify      │ runtime (bg shells,
          └────────────┴── call again mid-run =   │ servers) across
               <lead_update> interrupt           │ handoffs
```

- **Lead** keeps: user comms, planning/design, commits/PRs/CI,
  correctness-critical authoring (prod queries, eval/grader text, pipeline
  config), rendered-browser judgment, and reviews every sidekick diff.
- **Sidekick** gets: implementation, verification, env setup/repair, broad
  searches. Briefs carry settled decisions as literal code/commands.
- Second `handoff` while one runs injects a `<lead_update>` interrupt into
  the running agent — the sidekick folds it in, it does not restart.
- Non-blocking reports land in the lead's inbox automatically; `wait`
  re-joins a running handoff.
- The harness injects `<system_guidance>` nudges when the lead starts
  doing mutating work itself instead of delegating.

## Layout

- `crates/fusion-core` — the library: `FusionBuilder`/`Fusion`,
  `handoff` + `wait` + bash tools, agent loop with an injection inbox,
  Anthropic + OpenAI-compatible providers, scripted `mock` for tests.
- `crates/fusion-cli` — reference REPL/one-shot CLI.

## Usage

```console
$ export ANTHROPIC_API_KEY=...   OPENAI_API_KEY=...
$ cargo run -p fusion-cli -- \
    --lead anthropic:claude-sonnet-4-6 \
    --sidekick openai:gpt-5-mini \
    "add a --json flag to the stats command"
```

`OPENAI_BASE_URL` covers OpenRouter/Ollama/etc. Omit the prompt for a REPL.

Embed it:

```rust
let mut f = FusionBuilder::new(lead_client, sidekick_client, cwd)
    .events(sink).build();
let answer = f.run("fix the failing tests").await?;
// steer a running session: f.lead_inbox().send(msg).await
```

## Differences from the original

- Toolset is `bash` only (matching the host harness); read/edit/grep
  tools, streaming, and handoff-history compaction are TODO.
- Model routing is local (`provider:model` specs), not a server-side
  `AssignModel` RPC.
- Prompts are fresh text implementing the same policy.

## Tests

`cargo test` — covers blocking handoffs, mid-run interrupt injection, and
the delegation nudge, all against a scripted model (no API keys needed).

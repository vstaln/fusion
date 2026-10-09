pub mod acp;
pub mod anthropic;
pub mod openai;

use std::sync::Arc;

use anyhow::{bail, Context};

use crate::model::ModelClient;

/// Build a client from "provider:model".
///   anthropic:claude-...       (env ANTHROPIC_API_KEY, ANTHROPIC_BASE_URL)
///   openai:model               (env OPENAI_API_KEY, OPENAI_BASE_URL — works for OpenRouter/Ollama)
///   mock:                      scripted client for tests, see fusion_core::mock
pub fn client_from_spec(spec: &str) -> anyhow::Result<Arc<dyn ModelClient>> {
    let (provider, model) = spec
        .split_once(':')
        .context("model spec must be `provider:model`")?;
    match provider {
        "anthropic" => {
            let key = std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY not set")?;
            Ok(Arc::new(anthropic::AnthropicClient::new(model, key)))
        }
        "openai" => {
            let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
            Ok(Arc::new(openai::OpenAiClient::new(model, key)))
        }
        "acp" | "devin" => Ok(Arc::new(acp::AcpClient::new(model))),
        other => bail!("unknown provider `{other}` (expected anthropic|openai)"),
    }
}

use fusion_core::model::{Message, ModelClient, Request, Role};
use fusion_core::providers::acp::AcpClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = std::env::args().nth(1).unwrap_or_else(|| "swe-2".into());
    let client = AcpClient::new(&model, std::env::temp_dir());
    let msgs = vec![Message::text(Role::User, "Reply with exactly: pong")];
    let req = Request {
        system: "You are a test echo. Reply with exactly what is asked.",
        messages: &msgs,
        tools: &[],
        max_tokens: 500,
    };
    eprintln!("completing via devin acp (model={model})...");
    let t = std::time::Instant::now();
    let out = client.complete(req).await?;
    eprintln!("done in {:?}", t.elapsed());
    println!("{out:?}");
    Ok(())
}

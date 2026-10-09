use fusion_core::model::{Message, ModelClient, Request, Role};
use fusion_core::providers::acp::AcpClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = AcpClient::new("swe-2");
    let msgs = vec![Message::text(Role::User, "Reply with exactly: pong")];
    let req = Request {
        system: "You are a test echo. Reply with exactly what is asked.",
        messages: &msgs,
        tools: &[],
        max_tokens: 500,
    };
    eprintln!("completing via devin acp...");
    let t = std::time::Instant::now();
    let out = client.complete(req).await?;
    eprintln!("done in {:?}", t.elapsed());
    println!("{out:?}");
    Ok(())
}

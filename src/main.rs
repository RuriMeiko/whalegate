// Server: axum webapp — UI quản lý account + OpenAI-compatible API
mod chat;
mod client;
mod chrome_capture;
mod pow;
mod server;
mod usage;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()).unwrap_or("serve") {
        "whoami" => {
            let creds = server::load_creds()?;
            let c = client::DsClient::new(creds.token.clone(), creds.cookie_string())?;
            let v = c.users_current().await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        "chat" => {
            let prompt = args.get(2).cloned().unwrap_or_else(|| "Say HARNESS_OK".into());
            let creds = server::load_creds()?;
            let c = client::DsClient::new(creds.token.clone(), creds.cookie_string())?;
            let (sid, sse, _mid) = c.chat(&prompt, None, None, false, false).await?;
            println!("session: {}", sid);
            println!("{}", sse);
        }
        "chatraw" => {
            // chatraw <prompt> <thinking> <search> — dump raw SSE để debug extractor
            let prompt = args.get(2).cloned().unwrap_or_else(|| "What is 17*23?".into());
            let thinking = args.get(3).map(|s| s == "true" || s == "1").unwrap_or(false);
            let search = args.get(4).map(|s| s == "true" || s == "1").unwrap_or(false);
            let creds = server::load_creds()?;
            let c = client::DsClient::new(creds.token.clone(), creds.cookie_string())?;
            let (sid, sse, _mid) = c.chat(&prompt, None, None, thinking, search).await?;
            eprintln!("session: {}", sid);
            println!("{}", sse);
        }
        "serve" => {
            let port: u16 = std::env::var("DS_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8787);
            server::run(port).await?;
        }
        _ => {
            eprintln!("usage: deepseek-harness [serve|whoami|chat <prompt>]");
        }
    }
    Ok(())
}

use claude_code_rs::mobile_bridge::MobileBridgeServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8787").await?;
    println!("claude-code mobile bridge listening on http://127.0.0.1:8787");

    axum::serve(listener, MobileBridgeServer::new().router()).await?;
    Ok(())
}

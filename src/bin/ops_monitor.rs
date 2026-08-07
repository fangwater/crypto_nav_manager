#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "crypto_nav_manager::ops_monitor=info,tower_http=info",
                )
            }),
        )
        .init();

    crypto_nav_manager::ops_monitor::run().await
}

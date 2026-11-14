//! Explicit read-only export for offline financial and performance regression.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    crypto_nav_manager::pnl::audit::capture().await
}

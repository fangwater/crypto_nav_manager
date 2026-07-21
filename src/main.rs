#[tokio::main]
async fn main() -> anyhow::Result<()> {
    crypto_nav_manager::server::run().await
}

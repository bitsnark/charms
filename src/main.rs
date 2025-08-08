#[tokio::main]
async fn main() -> anyhow::Result<()> {
    unsafe {
        // We don't want to be in mock mode accidentally.
        std::env::remove_var("MOCK");
    }
    charms::cli::run().await
}

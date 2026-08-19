use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    served::cli::run().await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pieskieo_server::serve().await
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    server::run(server::ServerConfig::default()).await;
}
#[tokio::main]
async fn main() {
    if let Err(error) = served::cli::run().await {
        if let Some(signal) = error.downcast_ref::<served::attach::Interrupted>() {
            std::process::exit(128 + signal.0);
        }
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

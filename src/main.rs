/// Main entry point for the matching engine application
///
/// This serves as a thin wrapper that delegates to the interfaces layer.
/// The actual application logic is implemented in `interfaces::cli`.

use matching_engine::interfaces::cli;

#[tokio::main]
async fn main() {
    cli::run().await;
}

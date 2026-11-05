//! Main entry point for the matching engine application.
//!
//! There is no async runtime: the matching core, client connections, and the
//! observability server are plain OS threads. This removes future polling and
//! scheduler wakeups from the trading data path.
use matching_engine::interfaces::cli;

fn main() {
    cli::run();
}

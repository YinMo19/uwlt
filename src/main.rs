mod bridge;
mod cli;
mod core;
mod ethereum;
mod swap;
mod transaction;
mod tron;
mod vault;
mod wallet;

use clap::Parser;

fn main() -> std::process::ExitCode {
    match cli::Cli::parse().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("uwlt: {error:#}");
            if error
                .downcast_ref::<vault::VaultError>()
                .is_some_and(|error| error.cancelled)
            {
                std::process::ExitCode::from(130)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}

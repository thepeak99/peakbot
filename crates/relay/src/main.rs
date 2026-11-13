//! `peakbot-relay` CLI (design §2, §7 task 11): `serve` and `reset-account`.
//!
//! SKELETON: argument parsing is in place; behaviour is `todo!()`.
//! Exit codes for `reset-account`: 0 ok, 1 no such account / DB error,
//! 2 relay is running (design §4.8).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "peakbot-relay", version, about = "Shifu Cloud Mode relay")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the relay from a relay.yaml config file (plain HTTP; the proxy
    /// terminates TLS).
    Serve {
        /// Path to relay.yaml.
        #[arg(short, long)]
        config: PathBuf,
    },
    /// Reset one account: delete chain, pending devices (and their letters)
    /// and sessions. The relay must be stopped.
    ResetAccount {
        /// Path to relay.yaml (for the db path only).
        #[arg(short, long)]
        config: PathBuf,
        /// The configured issuer URL.
        issuer: String,
        /// The subject of the ID token.
        subject: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { .. } => {
            todo!("P2 task 11: tracing init (RUST_LOG, default info) + peakbot_relay::serve")
        }
        Cmd::ResetAccount { .. } => {
            todo!("P2 task 11: peakbot_relay::reset_account; exit 0/1/2, print counts")
        }
    }
}

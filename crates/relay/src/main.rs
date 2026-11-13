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
        Cmd::Serve { config } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .init();
            let cfg = peakbot_relay::config::Config::from_file(&config)
                .map_err(|e| anyhow::anyhow!("config: {e:?}"))?;
            let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
            let handle = peakbot_relay::serve(cfg, listener).await?;
            tracing::info!(addr = %handle.addr, "relay listening");
            tokio::signal::ctrl_c().await?;
            handle.shutdown().await;
            Ok(())
        }
        Cmd::ResetAccount {
            config,
            issuer,
            subject,
        } => {
            use peakbot_relay::store::ResetError;
            let cfg = peakbot_relay::config::Config::from_file(&config)
                .map_err(|e| anyhow::anyhow!("config: {e:?}"))?;
            match peakbot_relay::reset_account(&cfg.db, &issuer, &subject) {
                Ok(r) => {
                    println!(
                        "reset: {} chain entries, {} pending devices, {} letters, {} sessions deleted",
                        r.chain_entries, r.pending_devices, r.letters, r.sessions
                    );
                    Ok(())
                }
                Err(ResetError::Locked) => {
                    eprintln!("relay is running — stop it, then retry");
                    std::process::exit(2);
                }
                Err(ResetError::NoSuchAccount) => {
                    eprintln!("no such account");
                    std::process::exit(1);
                }
                Err(ResetError::Db(e)) => {
                    eprintln!("db error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

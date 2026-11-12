//! micro-dsql: a local Aurora DSQL database emulator for tests and CI.
//!
//! ```text
//! micro-dsql [--listen 127.0.0.1:5432] [--data <file> | --memory] [--log info]
//! ```

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use micro_dsql_server::{Server, Storage};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "micro-dsql",
    version,
    about = "Local Aurora DSQL database emulator"
)]
struct Args {
    /// Address to listen on; port 0 lets the OS choose (the chosen port is logged).
    #[arg(long, value_name = "ADDR:PORT", default_value = "127.0.0.1:5432")]
    listen: String,

    /// Keep the database in this file (created if missing).
    #[arg(long, value_name = "FILE", conflicts_with = "memory")]
    data: Option<PathBuf>,

    /// Use a fresh in-memory database (the default).
    #[arg(long)]
    memory: bool,

    /// Log filter: a level (error, warn, info, debug, trace) or a tracing filter
    /// such as `micro_dsql_server=debug,warn`.
    #[arg(long, value_name = "FILTER", default_value = "info")]
    log: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let filter = match EnvFilter::try_new(&args.log) {
        Ok(filter) => filter,
        Err(e) => {
            eprintln!("micro-dsql: invalid --log filter {:?}: {e}", args.log);
            return ExitCode::from(2);
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let storage = match args.data {
        Some(path) => Storage::File(path),
        None => Storage::Memory,
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            error!("cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        let server = Server::bind(&args.listen, &storage).await?;
        let addr = server.local_addr()?;
        info!("micro-dsql listening on {addr} (database: {storage})");
        server.serve(shutdown_signal()).await
    });
    // Do not wait for statements still running on the blocking pool.
    runtime.shutdown_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => {
            info!("stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Completes on SIGINT (Ctrl-C) or, on Unix, SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("cannot listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                error!("cannot listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => info!("received SIGINT"),
        _ = terminate => info!("received SIGTERM"),
    }
}

//! The per-connection message loop.
//!
//! Same as pgwire's `process_socket`, built from its public pieces, with one change: a
//! Terminate message closes the connection (pgwire waits for the client to hang up, which
//! makes clients such as asyncpg wait forever in `close()`; M0 blocker B4). TLS is never
//! offered: an SSLRequest is answered `N` and the client continues in plain text.

use std::io;
use std::time::Duration;

use futures::StreamExt;
use pgwire::api::{ClientInfo, PgWireConnectionState, PgWireServerHandlers};
use pgwire::messages::PgWireFrontendMessage;
use pgwire::tokio::server::{negotiate_tls, process_error, process_message};
use tokio::net::TcpStream;

/// A client must finish the startup phase within this time.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) async fn process_socket<H>(tcp_socket: TcpStream, handlers: H) -> io::Result<()>
where
    H: PgWireServerHandlers,
{
    let startup_timeout = tokio::time::sleep(STARTUP_TIMEOUT);
    tokio::pin!(startup_timeout);

    let socket = tokio::select! {
        _ = &mut startup_timeout => return Ok(()),
        socket = negotiate_tls(tcp_socket, None) => socket?,
    };
    // `None`: the client tried direct TLS, which is not offered.
    let Some(mut socket) = socket else {
        return Ok(());
    };

    let startup_handler = handlers.startup_handler();
    let simple_query_handler = handlers.simple_query_handler();
    let extended_query_handler = handlers.extended_query_handler();
    let copy_handler = handlers.copy_handler();
    let cancel_handler = handlers.cancel_handler();

    let socket = &mut socket;
    loop {
        let in_startup = matches!(
            socket.state(),
            PgWireConnectionState::AwaitingStartup
                | PgWireConnectionState::AuthenticationInProgress
        );
        let msg = if in_startup {
            tokio::select! {
                _ = &mut startup_timeout => None,
                msg = socket.next() => msg,
            }
        } else {
            socket.next().await
        };

        let Some(Ok(msg)) = msg else {
            break;
        };
        if matches!(msg, PgWireFrontendMessage::Terminate(_)) {
            break;
        }
        let is_extended_query = match socket.state() {
            PgWireConnectionState::CopyInProgress(is_extended_query) => is_extended_query,
            _ => msg.is_extended_query(),
        };
        if let Err(e) = process_message(
            msg,
            socket,
            startup_handler.clone(),
            simple_query_handler.clone(),
            extended_query_handler.clone(),
            copy_handler.clone(),
            cancel_handler.clone(),
        )
        .await
        {
            process_error(socket, e, is_extended_query).await?;
        }
    }
    Ok(())
}

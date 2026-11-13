//! One task per WebSocket (design §4.2). PASS A PLACEHOLDER: sends the
//! `Challenge`, enforces the Hello window and the decode errors, and answers
//! everything else `NOT_PERMITTED`. Pass B replaces the body with the full
//! state machine (Hello, ChainFetch, Pipe/Post/Ack, ChainAppend, writer task).

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use cloud::codec::{FromRelay, decode_to_relay, encode_from_relay};
use cloud::constants::{HELLO_WINDOW, relay_error};
use tokio::sync::watch;

use crate::hub::SocketPermit;

/// Run the per-socket state machine until the socket closes. The permit lives
/// as long as the socket, so a closed socket frees its slot (RAII).
pub async fn run_conn(
    mut socket: WebSocket,
    _permit: SocketPermit,
    _account: crate::store::AccountId,
    _session: [u8; 32],
    shutdown: watch::Receiver<bool>,
) {
    let Ok(nonce) = crate::random_32() else {
        return;
    };
    if send(&mut socket, &FromRelay::Challenge { nonce })
        .await
        .is_err()
    {
        return;
    }
    let window = tokio::time::sleep(std::time::Duration::from_secs(HELLO_WINDOW));
    tokio::pin!(window);
    loop {
        tokio::select! {
            _ = &mut window => return close(&mut socket, 1008).await,
            _ = crate::stopped(shutdown.clone()) => return close(&mut socket, 1001).await,
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(b))) => match decode_to_relay(&b) {
                    // Pass B: the real state machine goes here.
                    Ok(_) => {
                        let e = FromRelay::Error { code: relay_error::NOT_PERMITTED };
                        if send(&mut socket, &e).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => return protocol_close(&mut socket).await,
                },
                Some(Ok(Message::Text(_))) => return protocol_close(&mut socket).await,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                // tungstenite reports an over-cap frame as a read error; answer 1009.
                Some(Err(_)) => return close(&mut socket, 1009).await,
                Some(Ok(Message::Close(_))) | None => return,
            },
        }
    }
}

async fn send(socket: &mut WebSocket, msg: &FromRelay) -> Result<(), axum::Error> {
    socket
        .send(Message::Binary(encode_from_relay(msg).into()))
        .await
}

/// `Error{UNSUPPORTED_VERSION}` then close 1002 (design §4.2).
async fn protocol_close(socket: &mut WebSocket) {
    let e = FromRelay::Error {
        code: relay_error::UNSUPPORTED_VERSION,
    };
    let _ = send(socket, &e).await;
    close(socket, 1002).await;
}

/// Best-effort close frame with a 1 s bound (design §6.2).
async fn close(socket: &mut WebSocket, code: u16) {
    let frame = Message::Close(Some(CloseFrame {
        code,
        reason: "".into(),
    }));
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), socket.send(frame)).await;
}

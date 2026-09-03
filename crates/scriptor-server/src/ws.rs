//! The per-connection websocket session.
//!
//! On connect: join the room, send the current snapshot, then bridge the socket to the room - the
//! read half forwards inbound update bytes to the room actor, the write half forwards broadcast
//! frames to the socket (skipping the client's own echo). On broadcast lag, the client re-syncs
//! from a fresh snapshot (safe: convergence needs eventual merged state, not every delta).

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;

use crate::persistence::Persistence;
use crate::AppState;

/// The websocket subprotocol this relay speaks, matching `DEFAULT_SUBPROTOCOL` in
/// `@truespar/scriptor-core`. It must be echoed back: RFC 6455 requires a client that offered
/// subprotocols to FAIL the connection if the server selects none, so a browser peer cannot
/// connect at all unless we answer its offer.
pub const SUBPROTOCOL: &str = "scriptor-collab.v1";

pub async fn ws_handler<P: Persistence>(
    Path(doc): Path<String>,
    State(state): State<AppState<P>>,
    ws: WebSocketUpgrade,
) -> Response {
    // Selects `SUBPROTOCOL` when the client offers it and echoes it; a client that offers nothing
    // (a raw tool, a test harness) still upgrades cleanly with no protocol header.
    ws.protocols([SUBPROTOCOL]).on_upgrade(move |socket| session(socket, doc, state))
}

async fn session<P: Persistence>(socket: WebSocket, doc: String, state: AppState<P>) {
    let client = state.next_client();
    let room = match state.room(&doc).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(doc, error = %e, "could not open room");
            return;
        }
    };
    let reply = match room.join(client).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(doc, error = %e, "join failed");
            return;
        }
    };

    let (mut sink, mut source) = socket.split();
    if sink.send(Message::Binary(reply.snapshot.into())).await.is_err() {
        return;
    }

    // Write half: relay broadcast frames to this socket.
    let mut stream = reply.stream;
    let room_out = room.clone();
    let mut out = tokio::spawn(async move {
        loop {
            match stream.recv().await {
                Ok(frame) => {
                    if frame.from == client {
                        continue; // don't echo a client its own update
                    }
                    if sink.send(Message::Binary((*frame.bytes).clone().into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // Fell behind the ring: re-sync from a fresh snapshot.
                    if let Ok(snap) = room_out.snapshot().await
                        && sink.send(Message::Binary(snap.into())).await.is_err() {
                            break;
                        }
                }
                Err(RecvError::Closed) => break,
            }
        }
    });

    // Read half: forward inbound update bytes to the room.
    while let Some(Ok(msg)) = source.next().await {
        match msg {
            Message::Binary(bytes)
                if room.update(client, bytes.to_vec()).await.is_err() => {
                    break;
                }
            Message::Close(_) => break,
            _ => {}
        }
    }

    out.abort();
    let _ = (&mut out).await;
    tracing::debug!(doc, client, "session closed");
}

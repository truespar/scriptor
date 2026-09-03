//! End-to-end websocket test of the axum relay: two real clients connect; one edits, the other
//! receives the relayed update bytes and merges them to the same content. This is the only path
//! that exercises the live `ws` handler (the room actor is unit-tested separately).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use scriptor_crdt::{CollabDoc, Run};
use scriptor_server::persistence::Memory;
use scriptor_server::router;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Read frames until a binary one arrives (skip pings/pongs/text).
async fn next_binary(ws: &mut Ws) -> Result<Vec<u8>> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for a binary frame"))?
            .ok_or_else(|| anyhow::anyhow!("socket closed"))??;
        if let Message::Binary(bytes) = msg {
            return Ok(bytes.to_vec());
        }
    }
}

#[tokio::test]
async fn two_clients_relay_an_edit() -> Result<()> {
    // Start the server on an ephemeral port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(Arc::new(Memory::new()));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let url = format!("ws://{addr}/doc/shared");
    let (mut a, _) = connect_async(&url).await?;
    let (mut b, _) = connect_async(&url).await?;

    // Each client first receives the (initially empty) snapshot - this also guarantees the room
    // has processed the join (and therefore B is subscribed before A edits).
    let _ = next_binary(&mut a).await?;
    let _ = next_binary(&mut b).await?;

    // A produces an edit locally and ships the update bytes.
    let local = CollabDoc::new();
    local.append_paragraph(&[Run::plain("hello over the wire")], None)?;
    let update = local.snapshot()?;
    a.send(Message::Binary(update.clone().into())).await?;

    // B receives the relayed update and merges it.
    let relayed = next_binary(&mut b).await?;
    let view = CollabDoc::new();
    view.merge(&relayed)?;
    let texts: Vec<String> = view
        .paragraphs()?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert!(
        texts.iter().any(|t| t == "hello over the wire"),
        "client B did not receive A's edit; got {texts:?}"
    );
    Ok(())
}

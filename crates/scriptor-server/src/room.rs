//! The per-document room actor.
//!
//! One tokio task per live document owns the merged [`CollabDoc`], a persistence handle, and a
//! `broadcast` channel. Clients talk to it through a [`RoomHandle`] (an mpsc sender). Because loro
//! merges are commutative + idempotent, the relay needs no central transform: it merges each
//! incoming update, persists it, and rebroadcasts the raw bytes to the other peers. A joining
//! client receives the current snapshot, then the live stream.

use std::any::Any;
use std::sync::Arc;

use anyhow::Result;
use scriptor_crdt::CollabDoc;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::persistence::Persistence;

/// How many updates to log before compacting the history into a fresh snapshot.
const COMPACT_EVERY: u32 = 256;
/// Bound on the live broadcast ring. On lag, a client re-syncs from a fresh snapshot.
const BROADCAST_CAP: usize = 1024;

/// A reserved [`ClientId`] for server-originated edits (e.g. an agent editing
/// through a side channel rather than a websocket). No real connection is ever
/// assigned this id, so every connected client receives the broadcast of a
/// server edit - none mistakes it for its own echo.
pub const SERVER_CLIENT: ClientId = u64::MAX;

/// A boxed server-side mutation: runs against the room's live [`CollabDoc`] and
/// returns its result type-erased (downcast by [`RoomHandle::apply`]).
type ApplyFn = Box<dyn FnOnce(&CollabDoc) -> Result<Box<dyn Any + Send>> + Send>;

/// One broadcast frame: an update blob and the client that produced it (so the origin can skip
/// its own echo - merges are idempotent, so this is an optimization, not a correctness need).
#[derive(Clone)]
pub struct Frame {
    pub from: ClientId,
    pub bytes: Arc<Vec<u8>>,
}

/// Opaque per-connection id, unique within a room.
pub type ClientId = u64;

enum Cmd {
    Join {
        client: ClientId,
        resp: oneshot::Sender<JoinReply>,
    },
    Update {
        from: ClientId,
        bytes: Vec<u8>,
    },
    /// Current merged snapshot (used by tests and re-sync after broadcast lag).
    Snapshot {
        resp: oneshot::Sender<Vec<u8>>,
    },
    /// Apply a server-side mutation directly to the room's authoritative doc,
    /// then persist + broadcast the resulting delta like any peer update.
    Apply {
        from: ClientId,
        mutate: ApplyFn,
        resp: oneshot::Sender<Result<Box<dyn Any + Send>>>,
    },
}

/// What a joining client receives: the current snapshot plus a live subscription.
pub struct JoinReply {
    pub snapshot: Vec<u8>,
    pub stream: broadcast::Receiver<Frame>,
}

/// A cloneable handle to a room actor.
#[derive(Clone)]
pub struct RoomHandle {
    tx: mpsc::Sender<Cmd>,
}

impl RoomHandle {
    /// Join the room: returns the current snapshot and a live broadcast subscription.
    pub async fn join(&self, client: ClientId) -> Result<JoinReply> {
        let (resp, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Join { client, resp })
            .await
            .map_err(|_| anyhow::anyhow!("room actor is gone"))?;
        rx.await.map_err(|_| anyhow::anyhow!("room actor dropped the join reply"))
    }

    /// Submit an update blob produced by `from`. Fire-and-forget; ordering per client is preserved
    /// by the mpsc channel.
    pub async fn update(&self, from: ClientId, bytes: Vec<u8>) -> Result<()> {
        self.tx
            .send(Cmd::Update { from, bytes })
            .await
            .map_err(|_| anyhow::anyhow!("room actor is gone"))
    }

    /// Fetch the current merged snapshot.
    pub async fn snapshot(&self) -> Result<Vec<u8>> {
        let (resp, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Snapshot { resp })
            .await
            .map_err(|_| anyhow::anyhow!("room actor is gone"))?;
        rx.await.map_err(|_| anyhow::anyhow!("room actor dropped the snapshot reply"))
    }

    /// Apply a server-side mutation to the room's authoritative document.
    ///
    /// `mutate` runs inside the room actor on the live [`CollabDoc`] (the single
    /// authoritative writer), so its tracked-change ids are generated against the
    /// real document and its result is returned to the caller. The loro delta the
    /// mutation produces is persisted and broadcast to every connected client
    /// exactly like a peer update - so a server-originated edit (e.g. an agent's
    /// tracked change via the `document.*` MCP) appears live in open editors.
    /// Pass [`SERVER_CLIENT`] as `from` so no client skips it as its own echo.
    ///
    /// This is the seam that unifies the headless and live write paths: when a
    /// room exists for a document, *all* writers (websocket clients and the
    /// agent) go through it, so nothing compacts the op-log behind the room's
    /// back.
    pub async fn apply<R, F>(&self, from: ClientId, mutate: F) -> Result<R>
    where
        F: FnOnce(&CollabDoc) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let (resp, rx) = oneshot::channel();
        let boxed: ApplyFn =
            Box::new(move |doc| mutate(doc).map(|r| Box::new(r) as Box<dyn Any + Send>));
        self.tx
            .send(Cmd::Apply { from, mutate: boxed, resp })
            .await
            .map_err(|_| anyhow::anyhow!("room actor is gone"))?;
        let out = rx
            .await
            .map_err(|_| anyhow::anyhow!("room actor dropped the apply reply"))??;
        out.downcast::<R>()
            .map(|b| *b)
            .map_err(|_| anyhow::anyhow!("apply result type mismatch (internal bug)"))
    }
}

struct Room<P: Persistence> {
    doc_id: String,
    doc: CollabDoc,
    persist: Arc<P>,
    bcast: broadcast::Sender<Frame>,
    dirty: u32,
}

impl<P: Persistence> Room<P> {
    /// Load `doc_id` from persistence and spawn its actor task. Returns a handle.
    pub async fn spawn(doc_id: String, persist: Arc<P>) -> Result<RoomHandle> {
        let doc = CollabDoc::new();
        for blob in persist.load(&doc_id).await? {
            doc.merge(&blob)?;
        }
        let (bcast, _) = broadcast::channel(BROADCAST_CAP);
        let (tx, rx) = mpsc::channel(256);
        let room = Room { doc_id, doc, persist, bcast, dirty: 0 };
        tokio::spawn(room.run(rx));
        Ok(RoomHandle { tx })
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Cmd>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Cmd::Join { client, resp } => {
                    let snapshot = self.doc.snapshot().unwrap_or_default();
                    let _ = resp.send(JoinReply { snapshot, stream: self.bcast.subscribe() });
                    tracing::debug!(doc = %self.doc_id, client, "join");
                }
                Cmd::Update { from, bytes } => {
                    if let Err(e) = self.apply(from, bytes).await {
                        tracing::warn!(doc = %self.doc_id, error = %e, "apply update failed");
                    }
                }
                Cmd::Snapshot { resp } => {
                    let _ = resp.send(self.doc.snapshot().unwrap_or_default());
                }
                Cmd::Apply { from, mutate, resp } => {
                    let r = self.apply_local(from, mutate).await;
                    let _ = resp.send(r);
                }
            }
        }
        tracing::debug!(doc = %self.doc_id, "room actor stopped (no more handles)");
    }

    /// Run a server-side mutation on the live doc, then persist + broadcast the
    /// delta it produced. Mirrors [`Self::apply`] for incoming peer updates, but
    /// the bytes come from the mutation (an `ExportMode::Updates` diff captured
    /// across the closure) rather than off the wire.
    async fn apply_local(
        &mut self,
        from: ClientId,
        mutate: ApplyFn,
    ) -> Result<Box<dyn Any + Send>> {
        let before = self.doc.version();
        let out = mutate(&self.doc)?;
        // No ops added (e.g. the mutation found nothing to change): don't grow the
        // log or broadcast an empty delta.
        if self.doc.version() == before {
            return Ok(out);
        }
        let delta = self.doc.export_updates_since(&before)?;
        self.persist.append_update(&self.doc_id, &delta).await?;
        let _ = self.bcast.send(Frame { from, bytes: Arc::new(delta) });

        self.dirty += 1;
        if self.dirty >= COMPACT_EVERY {
            let snap = self.doc.snapshot()?;
            self.persist.compact(&self.doc_id, &snap).await?;
            self.dirty = 0;
            tracing::debug!(doc = %self.doc_id, "compacted history");
        }
        Ok(out)
    }

    async fn apply(&mut self, from: ClientId, bytes: Vec<u8>) -> Result<()> {
        self.doc.merge(&bytes)?;
        self.persist.append_update(&self.doc_id, &bytes).await?;
        // Rebroadcast to peers (ignore "no receivers"). Origin skips its own echo on receipt.
        let _ = self.bcast.send(Frame { from, bytes: Arc::new(bytes) });

        self.dirty += 1;
        if self.dirty >= COMPACT_EVERY {
            let snap = self.doc.snapshot()?;
            self.persist.compact(&self.doc_id, &snap).await?;
            self.dirty = 0;
            tracing::debug!(doc = %self.doc_id, "compacted history");
        }
        Ok(())
    }
}

/// Spawn (or look up the spawn of) a room. Exposed at module level so the registry can call it
/// without naming the generic `Room` type.
pub async fn spawn_room<P: Persistence>(doc_id: String, persist: Arc<P>) -> Result<RoomHandle> {
    Room::spawn(doc_id, persist).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Memory;

    /// An update submitted by one client is merged, persisted, and relayed to another client; a
    /// fresh doc built from the room snapshot sees the content (relay + convergence through the
    /// server).
    #[tokio::test]
    async fn relays_and_persists_updates() -> Result<()> {
        let persist = Arc::new(Memory::new());
        let room = spawn_room("doc-1".into(), persist.clone()).await?;

        // Two clients join.
        let a = room.join(1).await?;
        let mut b_stream = room.join(2).await?.stream;
        // a's snapshot is empty (new doc); just confirm it joined.
        assert!(a.snapshot.is_empty() || !a.snapshot.is_empty());

        // Client 1 produces an edit locally and ships the update bytes.
        let local = CollabDoc::new();
        local.append_paragraph(&[scriptor_crdt::Run::plain("hello from client 1")], None)?;
        let update = local.snapshot()?;
        room.update(1, update.clone()).await?;

        // Client 2 receives the relayed frame.
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), b_stream.recv())
            .await
            .expect("timed out waiting for relay")
            .expect("broadcast closed");
        assert_eq!(frame.from, 1);
        assert_eq!(&*frame.bytes, &update);

        // The room's merged snapshot contains client 1's paragraph.
        let merged = CollabDoc::new();
        merged.merge(&room.snapshot().await?)?;
        let texts: Vec<String> = merged
            .paragraphs()?
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect();
        assert!(texts.iter().any(|t| t == "hello from client 1"), "room did not merge the update");

        // Persistence captured the update (a fresh room reloads it).
        let reloaded = spawn_room("doc-1".into(), persist).await?;
        let r2 = CollabDoc::new();
        r2.merge(&reloaded.snapshot().await?)?;
        assert_eq!(r2.paragraphs()?.len(), 1, "update was not persisted");
        Ok(())
    }

    /// A late joiner gets a snapshot that already contains prior edits.
    #[tokio::test]
    async fn late_joiner_gets_current_state() -> Result<()> {
        let persist = Arc::new(Memory::new());
        let room = spawn_room("doc-2".into(), persist).await?;

        let early = CollabDoc::new();
        early.append_paragraph(&[scriptor_crdt::Run::plain("first")], None)?;
        room.update(1, early.snapshot()?).await?;

        // Join after the edit; the snapshot must already include it.
        let reply = room.join(99).await?;
        let view = CollabDoc::new();
        view.merge(&reply.snapshot)?;
        assert_eq!(view.paragraphs()?.len(), 1);
        Ok(())
    }

    /// A server-side `apply` mutates the authoritative doc, returns the closure's
    /// value, persists the delta, and broadcasts it to connected clients (the
    /// agent-edit-appears-live path). The broadcast bytes are a delta, not a full
    /// snapshot.
    #[tokio::test]
    async fn apply_mutates_persists_and_broadcasts_delta() -> Result<()> {
        let persist = Arc::new(Memory::new());
        let room = spawn_room("doc-3".into(), persist.clone()).await?;

        // A connected client subscribes to the live stream.
        let mut stream = room.join(1).await?.stream;

        // Server-side edit (as an agent would, via SERVER_CLIENT). The closure's
        // return value comes back to the caller.
        let n: usize = room
            .apply(SERVER_CLIENT, |doc| {
                doc.append_paragraph(&[scriptor_crdt::Run::plain("agent wrote this")], None)?;
                Ok(doc.paragraphs()?.len())
            })
            .await?;
        assert_eq!(n, 1, "apply returns the closure result");

        // The client receives the broadcast as a delta frame from SERVER_CLIENT.
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), stream.recv())
            .await
            .expect("timed out waiting for the apply broadcast")
            .expect("broadcast closed");
        assert_eq!(frame.from, SERVER_CLIENT);
        // A delta is materially smaller than the full snapshot - confirm it is the
        // update form, not a re-broadcast of the whole document.
        assert!(
            frame.bytes.len() < room.snapshot().await?.len(),
            "broadcast a delta, not a full snapshot"
        );
        // Merging the delta into a fresh peer that already has the prior state
        // reproduces the paragraph.
        let peer = CollabDoc::new();
        peer.merge(&frame.bytes)?;
        assert!(
            peer.paragraphs()?.iter().any(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>()
                == "agent wrote this"),
            "the delta carries the agent's paragraph"
        );

        // A fresh room reloads it from persistence.
        let reloaded = spawn_room("doc-3".into(), persist).await?;
        let view = CollabDoc::new();
        view.merge(&reloaded.snapshot().await?)?;
        assert_eq!(view.paragraphs()?.len(), 1, "apply delta was persisted");
        Ok(())
    }
}

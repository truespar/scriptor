//! Collaboration relay + persistence server (axum).
//!
//! A y-sweet-shaped relay over loro: one room actor per live document owns the merged CRDT, a
//! broadcast channel, and a persistence handle (see [`room`] and [`persistence`]). Clients connect
//! over a websocket at `/doc/{id}`, receive the current snapshot, then exchange opaque loro update
//! bytes - the server never interprets the document schema. Because loro merges are commutative +
//! idempotent, the relay needs no central transform.
//!
//! The relay today: snapshot-on-join + live broadcast + append-only persistence with compaction.
//! Version-vector catch-up (`ExportMode::Updates { from }`), `EphemeralStore` presence, and
//! multi-node scale-out are not yet implemented.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use axum::routing::get;
use axum::Router;
use dashmap::DashMap;

pub mod persistence;
pub mod room;
mod ws;

use persistence::Persistence;
use room::{spawn_room, RoomHandle};

/// Shared server state: the live room registry + the persistence backend + a connection counter.
/// Cheap to clone (one `Arc`).
pub struct AppState<P: Persistence>(Arc<Inner<P>>);

struct Inner<P: Persistence> {
    rooms: DashMap<String, RoomHandle>,
    persist: Arc<P>,
    next_client: AtomicU64,
}

impl<P: Persistence> Clone for AppState<P> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<P: Persistence> AppState<P> {
    pub fn new(persist: Arc<P>) -> Self {
        Self(Arc::new(Inner { rooms: DashMap::new(), persist, next_client: AtomicU64::new(1) }))
    }

    fn next_client(&self) -> u64 {
        self.0.next_client.fetch_add(1, Ordering::Relaxed)
    }

    /// Get the room for `doc`, spawning (and loading) it on first use.
    async fn room(&self, doc: &str) -> Result<RoomHandle> {
        if let Some(h) = self.0.rooms.get(doc) {
            return Ok(h.clone());
        }
        // Spawn outside the map lock (spawn is async). A concurrent first-join may spawn a second
        // actor; `or_insert` keeps the first and the loser idles until its handle drops.
        let handle = spawn_room(doc.to_string(), self.0.persist.clone()).await?;
        Ok(self.0.rooms.entry(doc.to_string()).or_insert(handle).clone())
    }
}

/// Build the axum router: a health check + the per-document websocket endpoint.
pub fn router<P: Persistence>(persist: Arc<P>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/doc/{id}", get(ws::ws_handler::<P>))
        .with_state(AppState::new(persist))
}

//! Durable storage for a room's CRDT history.
//!
//! A room's state is reconstructed by merging a sequence of opaque loro blobs: a base snapshot
//! (if compacted) followed by the append-only update log. When the log grows large relative to the
//! snapshot, the room compacts - one fresh snapshot replaces the log. This is the y-sweet shape;
//! loro merges are commutative + idempotent, so no central transform is needed.
//!
//! [`Memory`] is the default (dev / tests). [`Postgres`] (feature `postgres`) is the EU-residency
//! system-of-record: `doc_snapshots` + append-only `doc_updates`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use anyhow::Result;

/// Persistent storage for room histories. Methods return `Send` futures so a room loop can be
/// spawned on the multi-threaded runtime (edition-2024 RPITIT; no `async-trait`).
pub trait Persistence: Send + Sync + 'static {
    /// Ordered blobs to merge into a fresh doc to reconstruct `doc`: the base snapshot (if any)
    /// followed by every logged update. Empty for a never-seen document.
    fn load(&self, doc: &str) -> impl Future<Output = Result<Vec<Vec<u8>>>> + Send;

    /// Append one update blob to `doc`'s log.
    fn append_update(&self, doc: &str, update: &[u8]) -> impl Future<Output = Result<()>> + Send;

    /// Replace `doc`'s snapshot with `snapshot` and clear its update log (compaction).
    fn compact(&self, doc: &str, snapshot: &[u8]) -> impl Future<Output = Result<()>> + Send;
}

/// In-memory persistence: a room's history lives only for the process lifetime. Correct for
/// convergence and relay; not durable. The default when no database is configured.
#[derive(Default)]
pub struct Memory {
    docs: Mutex<HashMap<String, DocLog>>,
}

#[derive(Default)]
struct DocLog {
    snapshot: Option<Vec<u8>>,
    updates: Vec<Vec<u8>>,
}

impl Memory {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Persistence for Memory {
    async fn load(&self, doc: &str) -> Result<Vec<Vec<u8>>> {
        let guard = self.docs.lock().unwrap();
        let Some(log) = guard.get(doc) else { return Ok(Vec::new()) };
        let mut blobs = Vec::with_capacity(log.updates.len() + 1);
        if let Some(snap) = &log.snapshot {
            blobs.push(snap.clone());
        }
        blobs.extend(log.updates.iter().cloned());
        Ok(blobs)
    }

    async fn append_update(&self, doc: &str, update: &[u8]) -> Result<()> {
        let mut guard = self.docs.lock().unwrap();
        guard.entry(doc.to_string()).or_default().updates.push(update.to_vec());
        Ok(())
    }

    async fn compact(&self, doc: &str, snapshot: &[u8]) -> Result<()> {
        let mut guard = self.docs.lock().unwrap();
        let log = guard.entry(doc.to_string()).or_default();
        log.snapshot = Some(snapshot.to_vec());
        log.updates.clear();
        Ok(())
    }
}

/// Postgres-backed persistence (feature `postgres`): `doc_snapshots(doc_id, snapshot, created_at)`
/// + append-only `doc_updates(doc_id, seq, update, created_at)`. Load = latest snapshot + replay;
/// compact = write a snapshot and delete the superseded updates, in one transaction.
#[cfg(feature = "postgres")]
pub struct Postgres {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl Postgres {
    /// Connect and ensure the schema exists.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(8).connect(url).await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS doc_snapshots (
                 doc_id TEXT PRIMARY KEY,
                 snapshot BYTEA NOT NULL,
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now()
             )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS doc_updates (
                 doc_id TEXT NOT NULL,
                 seq BIGSERIAL PRIMARY KEY,
                 update BYTEA NOT NULL,
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now()
             )",
        )
        .execute(&pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS doc_updates_doc_seq ON doc_updates (doc_id, seq)")
            .execute(&pool)
            .await?;
        Ok(Self { pool })
    }
}

#[cfg(feature = "postgres")]
impl Persistence for Postgres {
    async fn load(&self, doc: &str) -> Result<Vec<Vec<u8>>> {
        let mut blobs = Vec::new();
        if let Some(row) =
            sqlx::query_as::<_, (Vec<u8>,)>("SELECT snapshot FROM doc_snapshots WHERE doc_id = $1")
                .bind(doc)
                .fetch_optional(&self.pool)
                .await?
        {
            blobs.push(row.0);
        }
        let updates = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT update FROM doc_updates WHERE doc_id = $1 ORDER BY seq",
        )
        .bind(doc)
        .fetch_all(&self.pool)
        .await?;
        blobs.extend(updates.into_iter().map(|r| r.0));
        Ok(blobs)
    }

    async fn append_update(&self, doc: &str, update: &[u8]) -> Result<()> {
        sqlx::query("INSERT INTO doc_updates (doc_id, update) VALUES ($1, $2)")
            .bind(doc)
            .bind(update)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn compact(&self, doc: &str, snapshot: &[u8]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO doc_snapshots (doc_id, snapshot) VALUES ($1, $2)
             ON CONFLICT (doc_id) DO UPDATE SET snapshot = EXCLUDED.snapshot, created_at = now()",
        )
        .bind(doc)
        .bind(snapshot)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM doc_updates WHERE doc_id = $1").bind(doc).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

//! [`Store`] over Durable Object SQLite (`ctx.storage.sql`), wasm only.
//!
//! The statement text lives in [`crate::session::sql`] (also exercised against
//! real SQLite by `tests/unit/studio-session-sql.test.mjs`). Every value is a
//! bound parameter. DO SQL is synchronous, so all methods are synchronous. Writes
//! issued in one turn are NOT atomic by themselves (a failing statement leaves
//! the earlier ones applied): `object::SiteSession::with` wraps each engine call
//! in `storage.transactionSync`, and any failed statement is recorded here
//! ([`Store::take_failure`]) so the object also drops its cached engine.

use std::cell::Cell;

use serde::Deserialize;
use worker::SqlStorage;
use worker::SqlStorageValue as V;

use super::sql;
use super::store::{
    EntryKind, LogEntry, Meta, OpRecord, PresenceRow, SnapshotRow, Store, StoreError, StoreResult,
};

pub struct DoStore {
    sql: SqlStorage,
    failed: Cell<bool>,
}

fn err(e: worker::Error) -> StoreError {
    StoreError(e.to_string())
}

#[derive(Deserialize)]
struct LogRow {
    seq: i64,
    op_id: String,
    user_id: i64,
    client_id: String,
    kind: String,
    target_seq: Option<i64>,
    base_seq: i64,
    rebased: i64,
    touched: String,
    tx: String,
    ts_ms: i64,
}

impl LogRow {
    fn into_entry(self) -> StoreResult<LogEntry> {
        Ok(LogEntry {
            seq: self.seq,
            op_id: self.op_id,
            user_id: self.user_id,
            client_id: self.client_id,
            kind: EntryKind::parse(&self.kind)
                .ok_or_else(|| StoreError(format!("unknown log kind {}", self.kind)))?,
            target_seq: self.target_seq,
            base_seq: self.base_seq,
            rebased: self.rebased != 0,
            touched: serde_json::from_str(&self.touched)
                .map_err(|e| StoreError(format!("touched: {e}")))?,
            tx_json: self.tx,
            ts_ms: self.ts_ms,
        })
    }
}

#[derive(Deserialize)]
struct SeqTouched {
    seq: i64,
    touched: String,
}

#[derive(Deserialize)]
struct OpRow {
    op_id: String,
    user_id: i64,
    seq: i64,
    rebased: i64,
    expires_ms: i64,
    payload: String,
}

#[derive(Deserialize)]
struct PresenceDbRow {
    client_id: String,
    user_id: i64,
    state: String,
    updated_ms: i64,
    expires_ms: i64,
}

#[derive(Deserialize)]
struct At {
    at: Option<i64>,
}

#[derive(Deserialize)]
struct Seq {
    seq: i64,
}

#[derive(Deserialize)]
struct Value1 {
    value: String,
}

impl From<PresenceDbRow> for PresenceRow {
    fn from(r: PresenceDbRow) -> Self {
        PresenceRow {
            client_id: r.client_id,
            user_id: r.user_id,
            state: r.state,
            updated_ms: r.updated_ms,
            expires_ms: r.expires_ms,
        }
    }
}

impl DoStore {
    /// Create the tables if needed (idempotent; runs on every wake-up).
    pub fn new(sql_storage: SqlStorage) -> StoreResult<Self> {
        for stmt in sql::SCHEMA {
            sql_storage.exec(stmt, None).map_err(err)?;
        }
        Ok(Self {
            sql: sql_storage,
            failed: Cell::new(false),
        })
    }

    fn note<T>(&self, r: StoreResult<T>) -> StoreResult<T> {
        if r.is_err() {
            self.failed.set(true);
        }
        r
    }

    fn run(&self, q: &str, params: Vec<V>) -> StoreResult<()> {
        let r = self.sql.exec(q, Some(params)).map(|_| ()).map_err(err);
        self.note(r)
    }

    fn rows<T: serde::de::DeserializeOwned>(&self, q: &str, params: Vec<V>) -> StoreResult<Vec<T>> {
        let r = self
            .sql
            .exec(q, Some(params))
            .map_err(err)
            .and_then(|c| c.to_array::<T>().map_err(err));
        self.note(r)
    }
}

impl Store for DoStore {
    fn load_meta(&self) -> StoreResult<Meta> {
        let rows: Vec<Value1> = self.rows(sql::META_GET, vec![V::String(sql::META_KEY.into())])?;
        match rows.into_iter().next() {
            None => Ok(Meta::default()),
            Some(r) => serde_json::from_str(&r.value).map_err(|e| StoreError(format!("meta: {e}"))),
        }
    }

    fn save_meta(&mut self, meta: &Meta) -> StoreResult<()> {
        let json = serde_json::to_string(meta).map_err(|e| StoreError(e.to_string()))?;
        self.run(
            sql::META_PUT,
            vec![V::String(sql::META_KEY.into()), V::String(json)],
        )
    }

    fn latest_snapshot(&self) -> StoreResult<Option<SnapshotRow>> {
        #[derive(Deserialize)]
        struct Row {
            seq: i64,
            doc: String,
            sha256: String,
        }
        let rows: Vec<Row> = self.rows(sql::SNAPSHOT_LATEST, vec![])?;
        Ok(rows.into_iter().next().map(|r| SnapshotRow {
            seq: r.seq,
            doc: r.doc,
            sha256: r.sha256,
        }))
    }

    fn snapshot_get(&self, seq: i64) -> StoreResult<Option<SnapshotRow>> {
        #[derive(Deserialize)]
        struct Row {
            seq: i64,
            doc: String,
            sha256: String,
        }
        let rows: Vec<Row> = self.rows(sql::SNAPSHOT_GET, vec![V::Integer(seq)])?;
        Ok(rows.into_iter().next().map(|r| SnapshotRow {
            seq: r.seq,
            doc: r.doc,
            sha256: r.sha256,
        }))
    }

    fn put_snapshot(&mut self, snap: &SnapshotRow, now_ms: i64) -> StoreResult<()> {
        self.run(
            sql::SNAPSHOT_PUT,
            vec![
                V::Integer(snap.seq),
                V::String(snap.doc.clone()),
                V::String(snap.sha256.clone()),
                V::Integer(now_ms),
            ],
        )
    }

    fn prune_snapshots(&mut self, latest_seq: i64, keep_seq: Option<i64>) -> StoreResult<()> {
        self.run(
            sql::SNAPSHOT_PRUNE,
            vec![V::Integer(latest_seq), V::Integer(keep_seq.unwrap_or(-1))],
        )
    }

    fn take_failure(&mut self) -> bool {
        self.failed.replace(false)
    }

    fn log_max_seq(&self) -> StoreResult<i64> {
        let rows: Vec<Seq> = self.rows(sql::LOG_MAX_SEQ, vec![])?;
        Ok(rows.first().map_or(0, |r| r.seq))
    }

    fn log_after(&self, after_seq: i64, limit: usize) -> StoreResult<Vec<LogEntry>> {
        let rows: Vec<LogRow> = self.rows(
            sql::LOG_AFTER,
            vec![V::Integer(after_seq), V::Integer(limit as i64)],
        )?;
        rows.into_iter().map(LogRow::into_entry).collect()
    }

    fn log_get(&self, seq: i64) -> StoreResult<Option<LogEntry>> {
        let rows: Vec<LogRow> = self.rows(sql::LOG_GET, vec![V::Integer(seq)])?;
        rows.into_iter().next().map(LogRow::into_entry).transpose()
    }

    fn touched_after(&self, after_seq: i64) -> StoreResult<Vec<(i64, Vec<String>)>> {
        let rows: Vec<SeqTouched> =
            self.rows(sql::LOG_TOUCHED_AFTER, vec![V::Integer(after_seq)])?;
        rows.into_iter()
            .map(|r| {
                serde_json::from_str::<Vec<String>>(&r.touched)
                    .map(|t| (r.seq, t))
                    .map_err(|e| StoreError(format!("touched: {e}")))
            })
            .collect()
    }

    fn append(&mut self, entry: &LogEntry, op: &OpRecord) -> StoreResult<()> {
        let touched =
            serde_json::to_string(&entry.touched).map_err(|e| StoreError(e.to_string()))?;
        self.run(
            sql::LOG_APPEND,
            vec![
                V::Integer(entry.seq),
                V::String(entry.op_id.clone()),
                V::Integer(entry.user_id),
                V::String(entry.client_id.clone()),
                V::String(entry.kind.as_str().into()),
                entry.target_seq.map_or(V::Null, V::Integer),
                V::Integer(entry.base_seq),
                V::Integer(i64::from(entry.rebased)),
                V::String(touched),
                V::String(entry.tx_json.clone()),
                V::Integer(entry.ts_ms),
            ],
        )?;
        self.run(
            sql::OP_PUT,
            vec![
                V::String(op.op_id.clone()),
                V::Integer(op.user_id),
                V::Integer(op.seq),
                V::Integer(i64::from(op.rebased)),
                V::Integer(op.expires_ms),
                V::String(op.payload.clone()),
            ],
        )
    }

    fn compact_log(&mut self, through_seq: i64) -> StoreResult<()> {
        self.run(sql::LOG_COMPACT, vec![V::Integer(through_seq)])
    }

    fn op_get(&self, op_id: &str, now_ms: i64) -> StoreResult<Option<OpRecord>> {
        let rows: Vec<OpRow> = self.rows(
            sql::OP_GET,
            vec![V::String(op_id.into()), V::Integer(now_ms)],
        )?;
        Ok(rows.into_iter().next().map(|r| OpRecord {
            op_id: r.op_id,
            user_id: r.user_id,
            seq: r.seq,
            rebased: r.rebased != 0,
            expires_ms: r.expires_ms,
            payload: r.payload,
        }))
    }

    fn prune_ops(&mut self, now_ms: i64, keep: i64) -> StoreResult<()> {
        self.run(sql::OP_PRUNE_EXPIRED, vec![V::Integer(now_ms)])?;
        self.run(sql::OP_PRUNE_EXCESS, vec![V::Integer(keep.max(0))])
    }

    fn next_op_expiry(&self) -> StoreResult<Option<i64>> {
        let rows: Vec<At> = self.rows(sql::OP_NEXT_EXPIRY, vec![])?;
        Ok(rows.first().and_then(|r| r.at))
    }

    fn presence_get(&self, client_id: &str) -> StoreResult<Option<PresenceRow>> {
        let rows: Vec<PresenceDbRow> =
            self.rows(sql::PRESENCE_GET, vec![V::String(client_id.into())])?;
        Ok(rows.into_iter().next().map(Into::into))
    }

    fn presence_put(&mut self, row: &PresenceRow) -> StoreResult<()> {
        self.run(
            sql::PRESENCE_PUT,
            vec![
                V::String(row.client_id.clone()),
                V::Integer(row.user_id),
                V::String(row.state.clone()),
                V::Integer(row.updated_ms),
                V::Integer(row.expires_ms),
            ],
        )
    }

    fn presence_delete(&mut self, client_id: &str) -> StoreResult<()> {
        self.run(sql::PRESENCE_DELETE, vec![V::String(client_id.into())])
    }

    fn presence_list(&self, now_ms: i64) -> StoreResult<Vec<PresenceRow>> {
        let rows: Vec<PresenceDbRow> = self.rows(sql::PRESENCE_LIST, vec![V::Integer(now_ms)])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    fn presence_prune(&mut self, now_ms: i64) -> StoreResult<usize> {
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let n: Vec<N> = self.rows(sql::PRESENCE_COUNT_EXPIRED, vec![V::Integer(now_ms)])?;
        self.run(sql::PRESENCE_PRUNE, vec![V::Integer(now_ms)])?;
        Ok(n.first().map_or(0, |r| r.n.max(0) as usize))
    }

    fn next_presence_expiry(&self) -> StoreResult<Option<i64>> {
        let rows: Vec<At> = self.rows(sql::PRESENCE_NEXT_EXPIRY, vec![])?;
        Ok(rows.first().and_then(|r| r.at))
    }
}

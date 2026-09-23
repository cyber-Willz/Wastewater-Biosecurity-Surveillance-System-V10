//! Audit-trail helper for the PostgreSQL-backed API.
//!
//! Action strings mirror `ww_audit::AuditAction::as_str` so NDJSON exports
//! stay compatible with the in-memory log's format.
//!
//! ## v0.6 — hash-chained rows
//!
//! The `audit_log` table now carries `prev_hash` and `entry_hash` columns
//! (see migration `0002_audit_chain.sql`).  Each call to [`record`] reads
//! the current chain tip inside the same transaction and derives:
//!
//! ```text
//! entry_hash = SHA-256(audit_id || "\n" || ts || "\n" || actor || "\n"
//!                   || action || "\n" || target_id || "\n" || details || "\n"
//!                   || prev_hash)
//! ```
//!
//! The DB-side `audit_log_no_update_delete` trigger still enforces
//! append-only.  The `audit_chain_prev_hash_check` trigger (added in
//! migration `0002`) additionally rejects any INSERT whose `prev_hash` does
//! not match the stored `entry_hash` of the row with the largest `audit_id`,
//! so the chain cannot be forked even by a direct DML statement.

use sha2::{Digest, Sha256};
use tokio_postgres::Transaction;

use crate::error::ApiResult;

pub const NETWORK_UPDATED:  &str = "NETWORK_UPDATED";
pub const SITE_ADDED:       &str = "SITE_ADDED";
pub const ROUND_INGESTED:   &str = "SAMPLE_INGESTED";
pub const ALERT_RAISED:     &str = "ALERT_RAISED";
pub const ALERT_CONFIRMED:  &str = "ALERT_CONFIRMED";
pub const ALERT_DISMISSED:  &str = "ALERT_DISMISSED";
pub const ALERT_ESCALATED:  &str = "ALERT_ESCALATED";

const GENESIS_HASH: &str = "GENESIS";

/// Compute the entry hash using the same formula as `ww_audit::log`.
fn compute_entry_hash(
    audit_id:  &str,
    timestamp: &str,
    actor:     &str,
    action:    &str,
    target_id: &str,
    details:   &str,
    prev_hash: &str,
) -> String {
    let mut h = Sha256::new();
    for field in [audit_id, timestamp, actor, action, target_id, details, prev_hash] {
        h.update(field.as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

/// Append one audit record to the `audit_log` table, computing and storing
/// the hash-chain fields inside `tx`.
///
/// ## Concurrency
///
/// Reading "the current tip" and inserting the next entry is a
/// read-then-write sequence, so it must be serialised against every other
/// writer or two concurrent callers can each read the same tip and produce
/// two rows that both claim to extend it (a fork the DB trigger cannot
/// always catch under READ COMMITTED, since a concurrent transaction's
/// uncommitted insert is invisible to the trigger's own tip query).
///
/// `record` therefore takes `crate::db::ADVISORY_KEY` itself, unconditionally,
/// as its first statement. This is the same key `rounds.rs` and `network.rs`
/// already hold for their own writes — re-acquiring it within the same
/// transaction is a fast no-op for those callers, and for callers that do
/// not otherwise take it (e.g. `handlers::review_alert`) it serialises them
/// against every other audit writer, which is exactly what's required for
/// the chain to stay linear. Audit writes are infrequent enough that this
/// serialisation point has no meaningful throughput cost.
pub async fn record(
    tx:        &Transaction<'_>,
    actor:     &str,
    action:    &str,
    target_id: &str,
    details:   &str,
) -> ApiResult<()> {
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&crate::db::ADVISORY_KEY]).await?;

    // Read chain tip inside this transaction for consistency.
    let prev_hash: String = tx
        .query_one(
            "SELECT COALESCE(\
                (SELECT entry_hash FROM audit_log ORDER BY audit_id DESC LIMIT 1), \
                $1\
             )",
            &[&GENESIS_HASH],
        )
        .await?
        .get(0);

    // The auto-generated audit_id (bigserial) is needed before the INSERT so
    // it can be included in the hash input. Peek the sequence's next value
    // directly rather than inserting-then-updating: this keeps the row
    // write-once (no UPDATE ever touches audit_log, matching the append-only
    // trigger's intent) and avoids a second round-trip inside the transaction.
    let next_id: i64 = tx
        .query_one("SELECT nextval('audit_log_audit_id_seq')", &[])
        .await?
        .get(0);

    // `clock_timestamp()` (not `now()`) gives wall-clock time even mid-txn.
    let ts_row = tx
        .query_one("SELECT to_char(clock_timestamp(), 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')", &[])
        .await?;
    let timestamp: String = ts_row.get(0);

    let audit_id_str = format!("{next_id}");
    let entry_hash = compute_entry_hash(
        &audit_id_str, &timestamp, actor, action, target_id, details, &prev_hash,
    );

    tx.execute(
        "INSERT INTO audit_log \
             (audit_id, ts, actor, action, target_id, details, prev_hash, entry_hash) \
         VALUES ($1, $2::timestamptz, $3, $4, $5, $6, $7, $8)",
        &[
            &next_id, &timestamp.as_str(), &actor, &action,
            &target_id, &details, &prev_hash.as_str(), &entry_hash.as_str(),
        ],
    )
    .await?;
    Ok(())
}

/// Verify the entire hash chain stored in the database, returning the number
/// of intact rows and the first broken row's `audit_id` if any.
///
/// This is an expensive operation (full table scan); intended for scheduled
/// integrity checks or on-demand operator queries, not the hot path.
pub async fn verify_chain(
    conn: &impl tokio_postgres::GenericClient,
) -> ApiResult<(usize, Option<i64>)> {
    let rows = conn
        .query(
            "SELECT audit_id, ts, actor, action, target_id, details, prev_hash, entry_hash \
             FROM audit_log ORDER BY audit_id",
            &[],
        )
        .await?;

    let mut expected_prev = GENESIS_HASH.to_string();
    for (i, row) in rows.iter().enumerate() {
        let audit_id: i64    = row.get(0);
        let ts: String       = row.get(1);
        let actor: String    = row.get(2);
        let action: String   = row.get(3);
        let target: String   = row.get(4);
        let details: String  = row.get(5);
        let prev: String     = row.get(6);
        let stored: String   = row.get(7);

        if prev != expected_prev {
            return Ok((i, Some(audit_id)));
        }
        let recomputed = compute_entry_hash(
            &audit_id.to_string(), &ts, &actor, &action, &target, &details, &prev,
        );
        if recomputed != stored {
            return Ok((i, Some(audit_id)));
        }
        expected_prev = stored;
    }
    Ok((rows.len(), None))
}

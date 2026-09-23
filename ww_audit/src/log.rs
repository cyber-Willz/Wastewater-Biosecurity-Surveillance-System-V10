//! Thread-safe, append-only, hash-chained audit log.
//!
//! Every auditable event in the system goes through [`AuditLog::record`].
//! The log lives entirely in memory during a session; call
//! [`AuditLog::export_ndjson`] to materialise the full trail as
//! newline-delimited JSON suitable for archival or regulatory submission.
//!
//! ## v0.6 — Hash-chained entries
//!
//! The in-memory log and the PostgreSQL `audit_log` table are now hash-chained:
//! each entry records `prev_hash` (the `entry_hash` of the immediately preceding
//! entry, or `"GENESIS"` for the first) and `entry_hash` — a SHA-256 digest of
//! the canonical fields of **this** entry concatenated with `prev_hash`.
//!
//! ### What the chain guarantees
//!
//! * **Tamper-evidence** — deleting, inserting, or silently editing any single
//!   entry breaks the chain from that point forward; any reader can detect the
//!   break by calling [`AuditLog::verify_chain`].
//! * **Ordering** — the chain encodes a total ordering; reordering entries also
//!   breaks it.
//!
//! ### What it does not guarantee
//!
//! * **Non-repudiation** — the hash chain is integrity-only, not
//!   non-repudiation.  A party who can rewrite the whole sequence can also
//!   re-hash it.  Signing the final hash with an HSM or submitting it to an
//!   external timestamping authority closes this gap; this is left as a
//!   deployment concern.
//! * **Confidentiality** — entries are stored in plain JSON.
//!
//! ### Hash input format
//!
//! ```text
//! HASH_INPUT = audit_id || "\n" || timestamp || "\n" || actor || "\n"
//!           || action || "\n" || target_id || "\n" || details || "\n"
//!           || prev_hash
//! ```
//!
//! Fields are included in serialised form (the same strings that appear in the
//! NDJSON export).  The delimiter is a newline; none of the fields contain
//! newlines by API contract.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

static AUDIT_CTR: AtomicU64 = AtomicU64::new(1);

fn next_audit_id() -> String {
    let n = AUDIT_CTR.fetch_add(1, Ordering::Relaxed);
    format!("aud_{n:06}")
}

// ── Hash helpers ─────────────────────────────────────────────────────────────

const GENESIS_HASH: &str = "GENESIS";

/// Compute SHA-256 of the canonical entry fields.
///
/// Input: `audit_id\ntimestamp\nactor\naction\ntarget_id\ndetails\nprev_hash`
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

// ── Action enum ──────────────────────────────────────────────────────────────

/// Every auditable event category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditAction {
    SchemaRegistered,
    SiteAdded,
    SampleIngested,
    SignalDetected,
    AlertRaised,
    AlertConfirmed,
    AlertDismissed,
    AlertEscalated,
    ManualAnnotation,
    ReportGenerated,
}

impl AuditAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuditAction::SchemaRegistered => "SCHEMA_REGISTERED",
            AuditAction::SiteAdded        => "SITE_ADDED",
            AuditAction::SampleIngested   => "SAMPLE_INGESTED",
            AuditAction::SignalDetected   => "SIGNAL_DETECTED",
            AuditAction::AlertRaised      => "ALERT_RAISED",
            AuditAction::AlertConfirmed   => "ALERT_CONFIRMED",
            AuditAction::AlertDismissed   => "ALERT_DISMISSED",
            AuditAction::AlertEscalated   => "ALERT_ESCALATED",
            AuditAction::ManualAnnotation => "MANUAL_ANNOTATION",
            AuditAction::ReportGenerated  => "REPORT_GENERATED",
        }
    }
}

// ── Entry ────────────────────────────────────────────────────────────────────

/// One record in the append-only, hash-chained audit trail.
///
/// `prev_hash` is the `entry_hash` of the immediately preceding entry, or
/// `"GENESIS"` for the first entry.  `entry_hash` is the SHA-256 of this
/// entry's canonical fields concatenated with `prev_hash`; see the module
/// doc for the exact format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub audit_id:   String,
    /// ISO-8601 UTC timestamp with millisecond precision.
    pub timestamp:  String,
    /// Actor that triggered the event (`"system"`, `"Dr. Martinez"`, …).
    pub actor:      String,
    /// Action category (uppercase snake-case string).
    pub action:     String,
    /// ID of the primary object this event relates to.
    pub target_id:  String,
    /// Free-form detail string (human-readable or JSON fragment).
    pub details:    String,
    /// `entry_hash` of the previous record, `"GENESIS"` for the first.
    pub prev_hash:  String,
    /// SHA-256(`audit_id\ntimestamp\nactor\naction\ntarget_id\ndetails\nprev_hash`).
    pub entry_hash: String,
}

// ── Chain verification ────────────────────────────────────────────────────────

/// Result of [`AuditLog::verify_chain`].
#[derive(Debug)]
pub struct ChainVerifyResult {
    /// `true` if every entry's `entry_hash` matches its recomputed value and
    /// every `prev_hash` links to the previous entry correctly.
    pub intact:          bool,
    /// Number of entries checked.
    pub entries_checked: usize,
    /// Index (0-based) of the first broken link, if any.
    pub first_break_at:  Option<usize>,
    /// Human-readable description of the break.
    pub break_reason:    Option<String>,
}

// ── Log ──────────────────────────────────────────────────────────────────────

/// Thread-safe, append-only, hash-chained audit log.
///
/// Cloning an [`AuditLog`] handle gives a second handle to the *same*
/// underlying storage — all handles share the same entry list.  This is
/// intentional: pass `AuditLog` by value to subsystems without losing
/// visibility into their events.
#[derive(Clone, Default)]
pub struct AuditLog {
    entries: Arc<Mutex<Vec<AuditEntry>>>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one entry, compute its hash, and return its generated `audit_id`.
    pub fn record(
        &self,
        actor:     &str,
        action:    AuditAction,
        target_id: &str,
        details:   impl Into<String>,
    ) -> String {
        let id        = next_audit_id();
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let details   = details.into();
        let action_s  = action.as_str();

        let mut guard    = self.entries.lock();
        let prev_hash = guard
            .last()
            .map(|e| e.entry_hash.as_str())
            .unwrap_or(GENESIS_HASH)
            .to_string();

        let entry_hash = compute_entry_hash(
            &id, &timestamp, actor, action_s, target_id, &details, &prev_hash,
        );

        guard.push(AuditEntry {
            audit_id:  id.clone(),
            timestamp,
            actor:     actor.to_string(),
            action:    action_s.to_string(),
            target_id: target_id.to_string(),
            details,
            prev_hash,
            entry_hash,
        });
        id
    }

    /// Verify the entire hash chain from first to last entry.
    ///
    /// Recomputes each entry's `entry_hash` from its fields and checks that
    /// `prev_hash` matches the preceding entry's `entry_hash` (or `"GENESIS"`
    /// for the first).  Returns a [`ChainVerifyResult`] describing any break.
    pub fn verify_chain(&self) -> ChainVerifyResult {
        let guard = self.entries.lock();
        let mut expected_prev = GENESIS_HASH.to_string();

        for (i, e) in guard.iter().enumerate() {
            // Check prev_hash links correctly.
            if e.prev_hash != expected_prev {
                return ChainVerifyResult {
                    intact:          false,
                    entries_checked: i + 1,
                    first_break_at:  Some(i),
                    break_reason:    Some(format!(
                        "entry {i} ({}) prev_hash mismatch: expected '{}', got '{}'",
                        e.audit_id, expected_prev, e.prev_hash
                    )),
                };
            }

            // Recompute this entry's hash.
            let recomputed = compute_entry_hash(
                &e.audit_id, &e.timestamp, &e.actor,
                &e.action, &e.target_id, &e.details, &e.prev_hash,
            );
            if recomputed != e.entry_hash {
                return ChainVerifyResult {
                    intact:          false,
                    entries_checked: i + 1,
                    first_break_at:  Some(i),
                    break_reason:    Some(format!(
                        "entry {i} ({}) entry_hash mismatch: recomputed '{}', stored '{}'",
                        e.audit_id, recomputed, e.entry_hash
                    )),
                };
            }

            expected_prev = e.entry_hash.clone();
        }

        ChainVerifyResult {
            intact:          true,
            entries_checked: guard.len(),
            first_break_at:  None,
            break_reason:    None,
        }
    }

    /// The `entry_hash` of the most recent entry, or `"GENESIS"` if empty.
    pub fn chain_tip(&self) -> String {
        self.entries
            .lock()
            .last()
            .map(|e| e.entry_hash.clone())
            .unwrap_or_else(|| GENESIS_HASH.to_string())
    }

    /// All entries for a specific `target_id`, in insertion order.
    pub fn entries_for(&self, target_id: &str) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .iter()
            .filter(|e| e.target_id == target_id)
            .cloned()
            .collect()
    }

    /// All entries, newest-first.
    pub fn recent(&self, n: usize) -> Vec<AuditEntry> {
        let guard = self.entries.lock();
        let start = guard.len().saturating_sub(n);
        guard[start..].iter().rev().cloned().collect()
    }

    /// Export as newline-delimited JSON (one JSON object per line), including
    /// `prev_hash` and `entry_hash` fields so the chain can be verified
    /// independently by any reader.
    pub fn export_ndjson(&self) -> String {
        self.entries
            .lock()
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_entry_links_to_genesis() {
        let log = AuditLog::new();
        log.record("system", AuditAction::SiteAdded, "site:1", "test");
        let entries = log.entries_for("site:1");
        assert_eq!(entries[0].prev_hash, GENESIS_HASH);
        assert_eq!(entries[0].entry_hash.len(), 64); // 32 bytes hex
    }

    #[test]
    fn chain_links_sequentially() {
        let log = AuditLog::new();
        for i in 0..5 {
            log.record("system", AuditAction::SampleIngested, "round:1", format!("obs {i}"));
        }
        let r = log.verify_chain();
        assert!(r.intact, "chain broke: {:?}", r.break_reason);
        assert_eq!(r.entries_checked, 5);
    }

    #[test]
    fn verify_detects_hash_tampering() {
        let log = AuditLog::new();
        log.record("a", AuditAction::AlertRaised, "alert:1", "d1");
        log.record("b", AuditAction::AlertConfirmed, "alert:1", "d2");
        // Tamper with the first entry's details (simulate database edit).
        {
            let mut guard = log.entries.lock();
            guard[0].details = "TAMPERED".to_string();
        }
        let r = log.verify_chain();
        assert!(!r.intact);
        assert_eq!(r.first_break_at, Some(0));
    }

    #[test]
    fn verify_detects_prev_hash_break() {
        let log = AuditLog::new();
        log.record("a", AuditAction::AlertRaised, "alert:1", "d1");
        log.record("b", AuditAction::AlertConfirmed, "alert:1", "d2");
        {
            let mut guard = log.entries.lock();
            guard[1].prev_hash = "bad_hash".to_string();
        }
        let r = log.verify_chain();
        assert!(!r.intact);
        assert_eq!(r.first_break_at, Some(1));
    }

    #[test]
    fn chain_tip_matches_last_entry() {
        let log = AuditLog::new();
        log.record("x", AuditAction::ReportGenerated, "report:1", "");
        log.record("x", AuditAction::ManualAnnotation, "report:1", "note");
        let tip = log.chain_tip();
        let guard = log.entries.lock();
        assert_eq!(tip, guard.last().unwrap().entry_hash);
    }

    #[test]
    fn empty_log_chain_is_intact() {
        let log = AuditLog::new();
        let r = log.verify_chain();
        assert!(r.intact);
        assert_eq!(r.entries_checked, 0);
        assert_eq!(log.chain_tip(), GENESIS_HASH);
    }

    #[test]
    fn ndjson_export_includes_hash_fields() {
        let log = AuditLog::new();
        log.record("sys", AuditAction::AlertRaised, "alert:99", "detail");
        let ndjson = log.export_ndjson();
        assert!(ndjson.contains("prev_hash"));
        assert!(ndjson.contains("entry_hash"));
        assert!(ndjson.contains("GENESIS"));
    }
}

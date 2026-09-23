-- Migration 0002: add hash-chain columns to audit_log and enforce chain integrity.
--
-- Run AFTER 0001_init.sql.  Safe to apply to an existing deployment:
-- existing rows will have empty strings for both columns, and the
-- `verify_chain` API call will report the first un-hashed row so operators
-- can backfill or start fresh.

ALTER TABLE audit_log
    ADD COLUMN IF NOT EXISTS prev_hash   text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS entry_hash  text NOT NULL DEFAULT '';

-- Rename the old timestamp column to 'ts' if it was named 'ts' in the
-- original schema (0001 uses 'ts' via DEFAULT clock_timestamp(), so no-op).
-- The audit.rs helper uses the 'ts' column; verify alignment here.
-- (No-op if already named 'ts'.)

-- Integrity trigger: reject any INSERT whose prev_hash does not match the
-- current chain tip.  This prevents forking the chain even via direct DML.
-- The trigger fires BEFORE INSERT so the offending row is never written.
CREATE OR REPLACE FUNCTION audit_chain_check() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    tip text;
BEGIN
    -- Chain tip = entry_hash of the row with the largest audit_id.
    SELECT entry_hash INTO tip
    FROM   audit_log
    ORDER  BY audit_id DESC
    LIMIT  1;

    -- First row: tip is NULL → expect GENESIS.
    IF tip IS NULL THEN
        tip := 'GENESIS';
    END IF;

    IF NEW.prev_hash <> tip THEN
        RAISE EXCEPTION
            'audit chain fork rejected: expected prev_hash=% got %',
            tip, NEW.prev_hash
            USING ERRCODE = 'restrict_violation';
    END IF;

    RETURN NEW;
END;
$$;

-- Drop the old trigger first (idempotent re-run safety).
DROP TRIGGER IF EXISTS audit_log_chain_check ON audit_log;

CREATE TRIGGER audit_log_chain_check
    BEFORE INSERT ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_chain_check();

-- Add an index on the most-recent-row lookup used by the chain-tip query.
CREATE INDEX IF NOT EXISTS audit_log_audit_id_desc_idx ON audit_log (audit_id DESC);

COMMENT ON COLUMN audit_log.prev_hash IS
    'SHA-256 entry_hash of the preceding row, or ''GENESIS'' for the first row.';
COMMENT ON COLUMN audit_log.entry_hash IS
    'SHA-256(audit_id||LF||ts||LF||actor||LF||action||LF||target_id||LF||details||LF||prev_hash).';

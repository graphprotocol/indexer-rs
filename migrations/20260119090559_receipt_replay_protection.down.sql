-- Rollback: Remove unique constraints on receipt signatures
-- Note: This does NOT restore deleted duplicate receipts

DROP INDEX IF EXISTS scalar_tap_receipts_signature_idx;
DROP INDEX IF EXISTS tap_horizon_receipts_signature_idx;

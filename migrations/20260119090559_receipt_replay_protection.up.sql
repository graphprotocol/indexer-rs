-- Migration: Add unique constraint on receipt signatures to prevent replay attacks
-- Audit: TRST-M-3 - Replayable TAP receipts inflate accounting and cause DB growth
--
-- This migration:
-- 1. Removes duplicate receipts (keeps the oldest by id)
-- 2. Adds unique index on signature to prevent future duplicates
--
-- Note: Duplicate receipts were already filtered by tap-agent during aggregation,
-- so removing them does not affect accounting or RAV creation.

-- V1: Remove duplicate signatures from scalar_tap_receipts (keep lowest id)
DELETE FROM scalar_tap_receipts a
USING scalar_tap_receipts b
WHERE a.id > b.id AND a.signature = b.signature;

-- V1: Add unique constraint on signature
CREATE UNIQUE INDEX IF NOT EXISTS scalar_tap_receipts_signature_idx 
    ON scalar_tap_receipts (signature);

-- V2: Remove duplicate signatures from tap_horizon_receipts (keep lowest id)
DELETE FROM tap_horizon_receipts a
USING tap_horizon_receipts b
WHERE a.id > b.id AND a.signature = b.signature;

-- V2: Add unique constraint on signature
CREATE UNIQUE INDEX IF NOT EXISTS tap_horizon_receipts_signature_idx 
    ON tap_horizon_receipts (signature);

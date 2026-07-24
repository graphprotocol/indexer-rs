-- Add down migration script here
DROP INDEX IF EXISTS idx_tap_horizon_ravs_last_redeemed;

DROP TRIGGER IF EXISTS rav_redeemed_notify ON tap_horizon_ravs CASCADE;

DROP FUNCTION IF EXISTS tap_horizon_rav_redeemed_notify() CASCADE;

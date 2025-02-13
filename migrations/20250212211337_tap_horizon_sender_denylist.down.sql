-- Add down migration script here
DROP TRIGGER IF EXISTS deny_update ON tap_horizon_deny CASCADE;

DROP FUNCTION IF EXISTS tap_horizon_deny_notify() CASCADE;

DROP TABLE IF EXISTS tap_horizon_denylist CASCADE;

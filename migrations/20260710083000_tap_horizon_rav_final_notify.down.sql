-- Add down migration script here
DROP TRIGGER IF EXISTS rav_final_notify ON tap_horizon_ravs CASCADE;

DROP FUNCTION IF EXISTS tap_horizon_rav_final_notify() CASCADE;

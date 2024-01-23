DROP TRIGGER IF EXISTS deny_update ON scalar_tap_deny CASCADE;

DROP FUNCTION IF EXISTS scalar_tap_deny_notify() CASCADE;

DROP TABLE IF EXISTS scalar_tap_denylist CASCADE;

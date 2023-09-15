DROP TRIGGER IF EXISTS receipt_update ON scalar_tap_receipts CASCADE;

DROP FUNCTION IF EXISTS scalar_tap_receipt_notify() CASCADE;

DROP TABLE IF EXISTS scalar_tap_receipts CASCADE;

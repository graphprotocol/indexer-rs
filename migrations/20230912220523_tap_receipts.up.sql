CREATE TABLE IF NOT EXISTS scalar_tap_receipts (
    id BIGSERIAL PRIMARY KEY,
    allocation_id CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    receipt JSON NOT NULL
);

CREATE FUNCTION scalar_tap_receipt_notify()
RETURNS trigger AS
$$
BEGIN
    PERFORM pg_notify('scalar_tap_receipt_notification', format('{"id": %s, "allocation_id": "%s", "timestamp_ns": %s}', NEW.id, NEW.allocation_id, NEW.timestamp_ns));
    RETURN NEW;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER receipt_update AFTER INSERT OR UPDATE
    ON scalar_tap_receipts
    FOR EACH ROW EXECUTE PROCEDURE scalar_tap_receipt_notify();

CREATE INDEX IF NOT EXISTS scalar_tap_receipts_allocation_id_idx ON scalar_tap_receipts (allocation_id);
CREATE INDEX IF NOT EXISTS scalar_tap_receipts_timestamp_ns_idx ON scalar_tap_receipts (timestamp_ns);

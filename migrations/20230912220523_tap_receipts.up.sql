CREATE TABLE IF NOT EXISTS scalar_tap_receipts (
    id BIGSERIAL PRIMARY KEY,
    allocation_id CHAR(40) NOT NULL,
    sender_address CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    -- signature CHAR(130) NOT NULL,
    value NUMERIC(39) NOT NULL,
    receipt JSON NOT NULL
);

CREATE FUNCTION scalar_tap_receipt_notify()
RETURNS trigger AS
$$
BEGIN
    PERFORM pg_notify('scalar_tap_receipt_notification', format('{"id": %s, "allocation_id": "%s", "sender_address": "%s", "timestamp_ns": %s, "value": %s}', NEW.id, NEW.allocation_id, NEW.sender_address, NEW.timestamp_ns, NEW.value));
    RETURN NEW;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER receipt_update AFTER INSERT OR UPDATE
    ON scalar_tap_receipts
    FOR EACH ROW EXECUTE PROCEDURE scalar_tap_receipt_notify();

CREATE INDEX IF NOT EXISTS scalar_tap_receipts_allocation_id_idx ON scalar_tap_receipts (allocation_id);
CREATE INDEX IF NOT EXISTS scalar_tap_receipts_timestamp_ns_idx ON scalar_tap_receipts (timestamp_ns);

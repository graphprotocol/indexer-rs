CREATE TABLE IF NOT EXISTS scalar_tap_receipts (
    id BIGSERIAL PRIMARY KEY, -- id being SERIAL is important for the function of tap-agent
    signer_address CHAR(40) NOT NULL,

    -- Values below are the individual fields of the EIP-712 receipt
    signature BYTEA NOT NULL,
    allocation_id CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    nonce NUMERIC(20) NOT NULL,
    value NUMERIC(39) NOT NULL
);

CREATE FUNCTION scalar_tap_receipt_notify()
RETURNS trigger AS
$$
BEGIN
    PERFORM pg_notify('scalar_tap_receipt_notification', format('{"id": %s, "allocation_id": "%s", "signer_address": "%s", "timestamp_ns": %s, "value": %s}', NEW.id, NEW.allocation_id, NEW.signer_address, NEW.timestamp_ns, NEW.value));
    RETURN NEW;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER receipt_update AFTER INSERT OR UPDATE
    ON scalar_tap_receipts
    FOR EACH ROW EXECUTE PROCEDURE scalar_tap_receipt_notify();

CREATE INDEX IF NOT EXISTS scalar_tap_receipts_allocation_id_idx ON scalar_tap_receipts (allocation_id);
CREATE INDEX IF NOT EXISTS scalar_tap_receipts_timestamp_ns_idx ON scalar_tap_receipts (timestamp_ns);

-- This table is used to store invalid receipts (receipts that fail at least one of the checks in the tap-agent).
-- Used for logging and debugging purposes.
CREATE TABLE IF NOT EXISTS scalar_tap_receipts_invalid (
    id BIGSERIAL PRIMARY KEY,
    allocation_id CHAR(40) NOT NULL,
    signer_address CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    value NUMERIC(39) NOT NULL,
    received_receipt JSON NOT NULL
);

-- Add up migration script here
CREATE TABLE IF NOT EXISTS tap_horizon_receipts (
    id BIGSERIAL PRIMARY KEY, -- id being SERIAL is important for the function of tap-agent
    signer_address CHAR(40) NOT NULL,

    -- Values below are the individual fields of the EIP-712 receipt
    signature BYTEA NOT NULL,
    allocation_id CHAR(40) NOT NULL,
    payer CHAR(40) NOT NULL,
    data_service CHAR(40) NOT NULL,
    service_provider CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    nonce NUMERIC(20) NOT NULL,
    value NUMERIC(39) NOT NULL
);

CREATE INDEX IF NOT EXISTS scalar_tap_receipts_allocation_id_idx ON scalar_tap_receipts (allocation_id);
CREATE INDEX IF NOT EXISTS scalar_tap_receipts_timestamp_ns_idx ON scalar_tap_receipts (timestamp_ns);


-- This table is used to store invalid receipts (receipts that fail at least one of the checks in the tap-agent).
-- Used for logging and debugging purposes.
CREATE TABLE IF NOT EXISTS tap_horizon_receipts_invalid (
    id BIGSERIAL PRIMARY KEY, -- id being SERIAL is important for the function of tap-agent
    signer_address CHAR(40) NOT NULL,

    -- Values below are the individual fields of the EIP-712 receipt
    signature BYTEA NOT NULL,
    allocation_id CHAR(40) NOT NULL,
    payer CHAR(40) NOT NULL,
    data_service CHAR(40) NOT NULL,
    service_provider CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    nonce NUMERIC(20) NOT NULL,
    value NUMERIC(39) NOT NULL,
    error_log TEXT NOT NULL DEFAULT ''
);

-- Add up migration script here

CREATE TABLE IF NOT EXISTS scalar_tap_receipts_v2 (
    id BIGSERIAL PRIMARY KEY, -- id being SERIAL is important for the function of tap-agent
    signer_address CHAR(40) NOT NULL,

    -- Values below are the individual fields of the EIP-712 receipt
    signature BYTEA NOT NULL,
    payer CHAR(40) NOT NULL,
    data_service CHAR(40) NOT NULL,
    service_provider CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    nonce NUMERIC(20) NOT NULL,
    value NUMERIC(39) NOT NULL
);

-- Add up migration script here
CREATE TABLE IF NOT EXISTS tap_v2_ravs (
    -- Values below are the individual fields of the EIP-712 RAV
    signature BYTEA NOT NULL,

    payer CHAR(40) NOT NULL,
    data_service CHAR(40) NOT NULL,
    service_provider CHAR(40) NOT NULL,

    timestamp_ns NUMERIC(20) NOT NULL,
    value_aggregate NUMERIC(39) NOT NULL,

    PRIMARY KEY (payer, data_service),

    -- To make indexer-agent's sequelize happy
    created_at TIMESTAMP WITH TIME ZONE,
    updated_at TIMESTAMP WITH TIME ZONE
);

-- This table is used to store failed RAV requests.
-- Used for logging and debugging purposes.
CREATE TABLE IF NOT EXISTS tap_v2_rav_requests_failed (
    id BIGSERIAL PRIMARY KEY,

    payer CHAR(40) NOT NULL,
    data_service CHAR(40) NOT NULL,
    service_provider CHAR(40) NOT NULL,

    expected_rav JSON NOT NULL,
    rav_response JSON NOT NULL,
    reason TEXT NOT NULL
);

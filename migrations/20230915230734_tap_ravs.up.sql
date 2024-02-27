CREATE TABLE IF NOT EXISTS scalar_tap_ravs (
    sender_address CHAR(40) NOT NULL,

    -- Values below are the individual fields of the EIP-712 RAV
    signature BYTEA NOT NULL, -- RLP encoded (v, r, s)
    allocation_id CHAR(40) NOT NULL,
    timestamp_ns NUMERIC(20) NOT NULL,
    value_aggregate NUMERIC(39) NOT NULL,

    final BOOLEAN DEFAULT FALSE NOT NULL,
    PRIMARY KEY (allocation_id, sender_address),

    -- To make indexer-agent's sequelize happy
    "createdAt" TIMESTAMP WITH TIME ZONE,
    "updatedAt" TIMESTAMP WITH TIME ZONE
);

-- This table is used to store failed RAV requests.
-- Used for logging and debugging purposes.
CREATE TABLE IF NOT EXISTS scalar_tap_rav_requests_failed (
    id BIGSERIAL PRIMARY KEY,
    allocation_id CHAR(40) NOT NULL,
    sender_address CHAR(40) NOT NULL,
    expected_rav JSON NOT NULL,
    rav_response JSON NOT NULL,
    reason TEXT NOT NULL
);

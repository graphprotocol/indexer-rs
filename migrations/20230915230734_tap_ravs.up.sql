CREATE TABLE IF NOT EXISTS scalar_tap_ravs (
    allocation_id CHAR(40) NOT NULL,
    sender_address CHAR(40) NOT NULL,
    rav JSON NOT NULL,
    final BOOLEAN DEFAULT FALSE NOT NULL,
    PRIMARY KEY (allocation_id, sender_address)
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

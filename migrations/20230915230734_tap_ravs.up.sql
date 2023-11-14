CREATE TABLE IF NOT EXISTS scalar_tap_ravs (
    allocation_id CHAR(40) NOT NULL,
    sender_address CHAR(40) NOT NULL,
    rav JSON NOT NULL,
    final BOOLEAN DEFAULT FALSE NOT NULL,
    PRIMARY KEY (allocation_id, sender_address)
);

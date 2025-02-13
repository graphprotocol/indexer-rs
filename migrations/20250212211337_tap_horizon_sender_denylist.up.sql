-- Add up migration script here
CREATE TABLE IF NOT EXISTS tap_horizon_denylist (
     sender_address CHAR(40) PRIMARY KEY
);

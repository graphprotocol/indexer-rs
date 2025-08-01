-- Add up migration script here

CREATE TABLE IF NOT EXISTS dips_receipts (
    id VARCHAR(255) PRIMARY KEY,
    agreement_id UUID NOT NULL REFERENCES indexing_agreements(id),
    amount NUMERIC(39) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'PENDING',
    transaction_hash CHAR(66),
    error_message TEXT,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    CONSTRAINT valid_status CHECK (status IN ('PENDING', 'SUBMITTED', 'FAILED'))
);

CREATE INDEX idx_dips_receipts_agreement_id ON dips_receipts(agreement_id);
CREATE INDEX idx_dips_receipts_status ON dips_receipts(status);
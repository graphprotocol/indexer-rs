-- Add up migration script here

CREATE TABLE IF NOT EXISTS dips_agreements (
    id UUID PRIMARY KEY,
    signature BYTEA NOT NULL, 
    signed_payload BYTEA NOT NULL,

    protocol CHAR(40) NOT NULL,
    service BYTEA NOT NULL,
    payee BYTEA NOT NULL,
    payer BYTEA NOT NULL,

    created_at TIMESTAMP WITH TIME ZONE NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

    cancelled_at TIMESTAMP WITH TIME ZONE,
    signed_cancellation_payload BYTEA
);

CREATE UNIQUE INDEX IX_UNIQ_SIGNATURE_AGREEMENT on dips_agreements(signature); 

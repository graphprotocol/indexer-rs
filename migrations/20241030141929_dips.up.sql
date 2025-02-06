-- Add up migration script here

CREATE TABLE IF NOT EXISTS indexing_agreements (
    id UUID PRIMARY KEY,

    signature BYTEA NOT NULL, 
    signed_payload BYTEA NOT NULL,

    protocol_network VARCHAR(255) NOT NULL,
    chain_id VARCHAR(255) NOT NULL,
    base_price_per_epoch NUMERIC(39) NOT NULL,
    price_per_entity NUMERIC(39) NOT NULL,
    subgraph_deployment_id VARCHAR(255) NOT NULL,

    service CHAR(40) NOT NULL,
    payee CHAR(40) NOT NULL,
    payer CHAR(40) NOT NULL,
    deadline TIMESTAMP WITH TIME ZONE NOT NULL,
    duration_epochs BIGINT NOT NULL,
    max_initial_amount NUMERIC(39) NOT NULL,
    max_ongoing_amount_per_epoch NUMERIC(39) NOT NULL,
    min_epochs_per_collection BIGINT NOT NULL,
    max_epochs_per_collection BIGINT NOT NULL,

    created_at TIMESTAMP WITH TIME ZONE NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

    cancelled_at TIMESTAMP WITH TIME ZONE,
    signed_cancellation_payload BYTEA,

    current_allocation_id CHAR(40)
);

CREATE UNIQUE INDEX IX_UNIQ_SIGNATURE_AGREEMENT on indexing_agreements(signature); 

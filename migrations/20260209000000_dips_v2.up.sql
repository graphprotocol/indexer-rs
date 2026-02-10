-- Drop legacy table if exists
DROP TABLE IF EXISTS indexing_agreements;

-- Table for validated RCA proposals
-- indexer-rs validates proposals before storing; the agent queries and accepts on-chain
CREATE TABLE IF NOT EXISTS pending_rca_proposals (
    id UUID PRIMARY KEY,
    signed_payload BYTEA NOT NULL,
    version SMALLINT NOT NULL DEFAULT 2,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- Index for agent queries: "give me all pending proposals, newest first"
CREATE INDEX idx_pending_rca_status ON pending_rca_proposals(status, created_at);

-- Index for time-ordered retrieval
CREATE INDEX idx_pending_rca_created ON pending_rca_proposals(created_at DESC);

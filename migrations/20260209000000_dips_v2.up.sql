-- Drop legacy table if exists
DROP TABLE IF EXISTS indexing_agreements;

-- Table for validated RCA proposals
--
-- Design rationale: This table is intentionally minimal (6 columns vs 24 in the old schema).
-- The RecurringCollector contract is the source of truth for agreement state. This table
-- serves only as a temporary queue between indexer-rs (validates) and indexer-agent (accepts on-chain).
--
-- We store the raw signed payload rather than denormalizing fields (network, payer, etc.) because:
-- 1. The signed payload IS the agreement - no risk of columns drifting out of sync
-- 2. Schema stability - RCA format changes don't require migrations
-- 3. Agent decodes the blob anyway to verify signature and submit on-chain
-- 4. Once accepted on-chain, all state queries go to the contract/subgraph, not here
--
-- If operational needs arise (e.g., "show pending proposals by network"), fields can be
-- extracted into columns. But start minimal - you can always add columns, removing is harder.
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

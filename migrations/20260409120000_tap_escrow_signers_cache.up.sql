-- Local cache of signer-to-sender mappings from the escrow accounts subgraph.
-- Used as a fallback when the subgraph query truncates signers (TRST-H-4).
CREATE TABLE IF NOT EXISTS tap_escrow_signers (
    signer_address CHAR(40) NOT NULL PRIMARY KEY,
    sender_address CHAR(40) NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS tap_escrow_signers_sender_idx
    ON tap_escrow_signers (sender_address);

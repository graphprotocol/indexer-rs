-- Constrain pending_rca_proposals.status to the vocabulary shared with the
-- indexer-agent (it writes 'accepted'/'completed'/'rejected'); a value outside
-- this set means the two services have drifted and should fail at write time.
ALTER TABLE pending_rca_proposals
    ADD CONSTRAINT pending_rca_proposals_status_check
    CHECK (status IN ('pending', 'accepted', 'completed', 'rejected'));

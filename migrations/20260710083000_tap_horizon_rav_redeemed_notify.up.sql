-- Add up migration script here

-- Notify listeners when the redemption state of a closing RAV changes: redeemed_at is set when
-- its redemption is submitted or observed on-chain, and cleared if that transaction reorgs out.
-- indexer-service listens to reject further receipts for a redeemed (payer, collection) pair.
CREATE FUNCTION tap_horizon_rav_redeemed_notify()
RETURNS trigger AS
$$
BEGIN
    PERFORM pg_notify('tap_horizon_rav_redeemed_notification', format('{"redeemed": %s, "payer": "%s", "data_service": "%s", "service_provider": "%s", "collection_id": "%s"}', CASE WHEN NEW.last AND NEW.redeemed_at IS NOT NULL THEN 'true' ELSE 'false' END, NEW.payer, NEW.data_service, NEW.service_provider, NEW.collection_id));
    RETURN NEW;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER rav_redeemed_notify AFTER INSERT OR UPDATE OF last, redeemed_at
    ON tap_horizon_ravs
    FOR EACH ROW
    EXECUTE PROCEDURE tap_horizon_rav_redeemed_notify();

-- The redeemed-receipt check loads redeemed closing RAVs at startup; the table itself grows
-- with every RAV ever produced, so give that predicate its own small index.
CREATE INDEX idx_tap_horizon_ravs_last_redeemed
    ON tap_horizon_ravs (service_provider)
    WHERE last = TRUE AND redeemed_at IS NOT NULL;

-- Add up migration script here

-- Notify listeners when a RAV is marked final, i.e. the last RAV for a
-- (payer, collection) was redeemed on-chain and passed the finality window.
-- indexer-service uses this to reject further receipts for that pair.
CREATE FUNCTION tap_horizon_rav_final_notify()
RETURNS trigger AS
$$
BEGIN
    PERFORM pg_notify('tap_horizon_rav_final_notification', format('{"payer": "%s", "collection_id": "%s"}', NEW.payer, NEW.collection_id));
    RETURN NEW;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER rav_final_notify AFTER INSERT OR UPDATE OF final
    ON tap_horizon_ravs
    FOR EACH ROW
    WHEN (NEW.final = TRUE)
    EXECUTE PROCEDURE tap_horizon_rav_final_notify();

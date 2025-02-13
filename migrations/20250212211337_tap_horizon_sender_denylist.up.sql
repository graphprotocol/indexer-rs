-- Add up migration script here
CREATE TABLE IF NOT EXISTS tap_horizon_denylist (
     sender_address CHAR(40) PRIMARY KEY
);


CREATE FUNCTION tap_horizon_deny_notify()
RETURNS trigger AS
$$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_notify('tap_horizon_deny_notification', format('{"tg_op": "DELETE", "sender_address": "%s"}', OLD.sender_address));
        RETURN OLD;
    ELSIF TG_OP = 'INSERT' THEN
        PERFORM pg_notify('tap_horizon_deny_notification', format('{"tg_op": "INSERT", "sender_address": "%s"}', NEW.sender_address));
        RETURN NEW;
    ELSE -- UPDATE OR TRUNCATE, should never happen
        PERFORM pg_notify('tap_horizon_deny_notification', format('{"tg_op": "%s", "sender_address": null}', TG_OP, NEW.sender_address));
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER deny_update AFTER INSERT OR UPDATE OR DELETE
    ON tap_horizon_denylist
    FOR EACH ROW EXECUTE PROCEDURE tap_horizon_deny_notify();

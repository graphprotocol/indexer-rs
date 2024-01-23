CREATE TABLE IF NOT EXISTS scalar_tap_denylist (
     sender_address CHAR(40) PRIMARY KEY
);

CREATE FUNCTION scalar_tap_deny_notify()
RETURNS trigger AS
$$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_notify('scalar_tap_deny_notification', format('{"tg_op": "DELETE", "sender_address": "%s"}', OLD.sender_address));
        RETURN OLD;
    ELSIF TG_OP = 'INSERT' THEN
        PERFORM pg_notify('scalar_tap_deny_notification', format('{"tg_op": "INSERT", "sender_address": "%s"}', NEW.sender_address));
        RETURN NEW;
    ELSE -- UPDATE OR TRUNCATE, should never happen
        PERFORM pg_notify('scalar_tap_deny_notification', format('{"tg_op": "%s", "sender_address": null}', TG_OP, NEW.sender_address));
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER deny_update AFTER INSERT OR UPDATE OR DELETE
    ON scalar_tap_denylist
    FOR EACH ROW EXECUTE PROCEDURE scalar_tap_deny_notify();

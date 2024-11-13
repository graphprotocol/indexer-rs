-- Add up migration script here
CREATE FUNCTION cost_models_update_notify()
RETURNS trigger AS
$$
BEGIN
  IF TG_OP = 'DELETE' THEN
    PERFORM pg_notify('cost_models_update_notification', format('{"tg_op": "DELETE", "deployment": "%s"}', OLD.deployment));
    RETURN OLD;
  ELSIF TG_OP = 'INSERT' THEN
    PERFORM pg_notify('cost_models_update_notification', format('{"tg_op": "INSERT", "deployment": "%s", "model": "%s", "variables": "%s"}', NEW.deployment, NEW.model, NEW.variables));
    RETURN NEW;
  ELSE
    PERFORM pg_notify('cost_models_update_notification', format('{"tg_op": "%s", "deployment": "%s", "model": "%s", "variables": "%s" }', NEW.deployment, NEW.model, NEW.variables));
    RETURN NEW;
  END IF;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER cost_models_update AFTER INSERT OR UPDATE OR DELETE
        ON "CostModels"
        FOR EACH ROW EXECUTE PROCEDURE cost_models_update_notify();

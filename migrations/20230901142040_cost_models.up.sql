CREATE TABLE IF NOT EXISTS "CostModelsHistory"
(
    id SERIAL PRIMARY KEY,
    deployment VARCHAR NOT NULL,
    model TEXT,
    variables JSONB,
    "createdAt" TIMESTAMP WITH TIME ZONE,
    "updatedAt" TIMESTAMP WITH TIME ZONE
);

CREATE VIEW "CostModels" AS SELECT id,
       deployment,
       model,
       variables,
       "createdAt",
       "updatedAt"
      FROM "CostModelsHistory" t1
      JOIN
      (
          SELECT MAX(id)
          FROM "CostModelsHistory"
          GROUP BY deployment
      ) t2
        ON t1.id = t2.MAX;

CREATE FUNCTION cost_models_update_notify()
RETURNS trigger AS
$$
BEGIN
  IF TG_OP = 'DELETE' THEN
    PERFORM pg_notify('cost_models_update_notification', format('{"tg_op": "DELETE", "deployment": "%s"}', OLD.deployment));
    RETURN OLD;
  ELSIF TG_OP = 'INSERT' THEN
    PERFORM pg_notify('cost_models_update_notification', format('{"tg_op": "INSERT", "deployment": "%s", "model": "%s"}', NEW.deployment, NEW.model));
    RETURN NEW;
  ELSE
    PERFORM pg_notify('cost_models_update_notification', format('{"tg_op": "%s", "deployment": "%s", "model": "%s"}', NEW.deployment, NEW.model));
    RETURN NEW;
  END IF;
END;
$$ LANGUAGE 'plpgsql';

CREATE TRIGGER cost_models_update AFTER INSERT OR UPDATE OR DELETE
        ON "CostModelsHistory"
        FOR EACH ROW EXECUTE PROCEDURE cost_models_update_notify();

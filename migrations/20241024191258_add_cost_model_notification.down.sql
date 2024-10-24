-- Add down migration script here
DROP TRIGGER IF EXISTS cost_models_update ON "CostModels" CASCADE;

DROP FUNCTION IF EXISTS cost_models_update_notify() CASCADE;

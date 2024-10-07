-- Add down migration script here
DROP TRIGGER IF EXISTS cost_models_update ON "CostModelsHistory" CASCADE;

DROP FUNCTION IF EXISTS cost_models_update_notify() CASCADE;

DROP VIEW "CostModels";

DROP TABLE "CostModelsHistory";

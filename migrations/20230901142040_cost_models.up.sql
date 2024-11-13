CREATE TABLE IF NOT EXISTS "CostModels"
(
    id INT,
    deployment VARCHAR NOT NULL,
    model TEXT,
    variables JSONB,
    PRIMARY KEY( deployment )
);

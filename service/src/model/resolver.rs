use sqlx::PgPool;
use tracing::info;

use super::schema::CostModel;
use crate::common::types::SubgraphDeploymentID;

/// Query postgres indexer management server's cost models
/// Filter on deployments if it is not empty, otherwise return all cost models
//TODO: If there is global cost model, merge with all the specific cost models
pub async fn cost_models(
    pool: &PgPool,
    deployments: Vec<String>,
) -> Result<Vec<CostModel>, anyhow::Error> {
    let deployment_ids = deployments
        .iter()
        .map(|d| SubgraphDeploymentID::new(d).unwrap().to_string())
        .collect::<Vec<String>>();
    let models = if deployment_ids.is_empty() {
        sqlx::query_as!(
            CostModel,
            r#"
        SELECT deployment, model, variables
        FROM "CostModels"
        ORDER BY deployment ASC
    "#
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            CostModel,
            r#"
            SELECT deployment, model, variables
            FROM "CostModels"
            WHERE deployment = ANY($1)
            ORDER BY deployment ASC
        "#,
            &deployment_ids
        )
        .fetch_all(pool)
        .await?
    };
    Ok(models)
}

/// Make database query for a cost model indexed by deployment id
//TODO: Add global fallback if cost model doesn't exist
pub async fn cost_model(
    pool: &PgPool,
    deployment: &str,
) -> Result<Option<CostModel>, anyhow::Error> {
    let deployment_id = SubgraphDeploymentID::new(deployment).unwrap().to_string();
    let model = sqlx::query_as!(
        CostModel,
        r#"
        SELECT deployment, model, variables
        FROM "CostModels"
        WHERE deployment = $1
    "#,
        deployment_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(model)
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use sqlx::{PgPool, types::time::OffsetDateTime};

    use super::*;


    async fn mock_cost_models_table(pool: &PgPool) {
        let test_models = test_cost_models();
        let datetime = OffsetDateTime::now_utc();
        for model in test_models {
            sqlx::query!(
                r#"
                    INSERT INTO "CostModels" (deployment, model, "createdAt", "updatedAt")
                    VALUES ($1, $2, $3, $4);
                    "#,
                    model.deployment,
                    model.model,
                    datetime, 
                    datetime
            )
            .execute(pool)
            .await.expect("Create test instance in db");

        }
    }

    fn test_cost_models() -> Vec<CostModel> {
        vec![CostModel { deployment: "0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea".to_string(), 
            model: Some("default => 0.00025;".to_string()), variables: None }]
    }

    #[sqlx::test]
    async fn cost_model_handler(pool: PgPool) {
        let out_dir = env::var("DATABASE_URL").unwrap();
        let deployment_id = "0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea";
        let deployment_hash = "Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss".to_string();
        let timestamp_ns = u64::MAX - 10;
        
        let res = cost_models(
            &pool,
            vec![deployment_hash],
        ).await.expect("Cost models query");

        assert_eq!(res.len(), 1);
    }
}
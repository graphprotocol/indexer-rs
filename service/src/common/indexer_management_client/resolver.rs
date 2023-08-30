// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use sqlx::PgPool;

use super::schema::CostModel;
use crate::common::types::SubgraphDeploymentID;

/// Query postgres indexer management server's cost models
/// Filter on deployments if it is not empty, otherwise return all cost models
pub async fn cost_models(
    pool: &PgPool,
    deployments: &[String],
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

    // Merge deployment cost models with global cost model
    let models = match (deployment_ids.is_empty(), global_cost_model(pool).await?) {
        (false, Some(global)) => {
            let m = deployment_ids
                .iter()
                .map(|d| {
                    let m = models.iter().find(|&m| &m.deployment == d).map_or_else(
                        || {
                            merge_global(
                                CostModel {
                                    deployment: d.clone(),
                                    model: None,
                                    variables: None,
                                },
                                &global,
                            )
                        },
                        |m| merge_global(m.clone(), &global),
                    );
                    m
                })
                .collect::<Vec<CostModel>>();

            m
        }
        _ => models,
    };

    Ok(models)
}

/// Make database query for a cost model indexed by deployment id
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

    // Fallback with global cost model
    let model = if let Some(global) = global_cost_model(pool).await? {
        model.map_or_else(
            || {
                Some(merge_global(
                    CostModel {
                        deployment: deployment.to_string(),
                        model: None,
                        variables: None,
                    },
                    &global,
                ))
            },
            |m| Some(merge_global(m, &global)),
        )
    } else {
        model
    };

    Ok(model)
}

/// Query global cost model
pub async fn global_cost_model(pool: &PgPool) -> Result<Option<CostModel>, anyhow::Error> {
    let model = sqlx::query_as!(
        CostModel,
        r#"
        SELECT deployment, model, variables
        FROM "CostModels"
        WHERE deployment = $1
    "#,
        "global"
    )
    .fetch_optional(pool)
    .await?;
    Ok(model)
}

fn merge_global(model: CostModel, global_model: &CostModel) -> CostModel {
    CostModel {
        deployment: model.deployment,
        model: model.model.clone().or(global_model.model.clone()),
        variables: model.variables.clone().or(global_model.variables.clone()),
    }
}

#[cfg(test)]
mod test {

    use sqlx::PgPool;

    use super::*;

    async fn setup_cost_models_table(pool: &PgPool) {
        sqlx::query!(
            r#"
            CREATE TABLE "CostModels"(
                id INT,
                deployment VARCHAR NOT NULL,
                model TEXT,
                variables JSONB,
                PRIMARY KEY( deployment )
            );
            "#,
        )
        .execute(pool)
        .await
        .expect("Create test instance in db");
    }

    async fn add_cost_models(pool: &PgPool, models: Vec<CostModel>) {
        for model in models {
            sqlx::query!(
                r#"
                    INSERT INTO "CostModels" (deployment, model)
                    VALUES ($1, $2);
                    "#,
                model.deployment,
                model.model,
            )
            .execute(pool)
            .await
            .expect("Create test instance in db");
        }
    }

    fn global_cost_model() -> CostModel {
        CostModel {
            deployment: "global".to_string(),
            model: Some("default => 0.00001;".to_string()),
            variables: None,
        }
    }

    fn simple_cost_models() -> Vec<CostModel> {
        vec![
            CostModel {
                deployment: "0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea"
                    .to_string(),
                model: Some("default => 0.00025;".to_string()),
                variables: None,
            },
            CostModel {
                deployment: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
                model: None,
                variables: None,
            },
        ]
    }

    #[sqlx::test]
    #[ignore]
    async fn success_cost_models(pool: PgPool) {
        _ = setup_cost_models_table(&pool).await;
        let expected_models = simple_cost_models();
        _ = add_cost_models(&pool, expected_models.clone()).await;
        let res = cost_models(
            &pool,
            &["Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss".to_string()],
        )
        .await
        .expect("Cost models query");

        assert_eq!(res.len(), 1);
        assert_eq!(
            &res.first().unwrap().deployment,
            &expected_models.first().unwrap().deployment
        );
        assert_eq!(
            &res.first().unwrap().model,
            &expected_models.first().unwrap().model
        );

        let res = cost_models(
            &pool,
            &["0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea".to_string()],
        )
        .await
        .expect("Cost models query");

        assert_eq!(res.len(), 1);
        assert_eq!(
            &res.first().unwrap().deployment,
            &expected_models.first().unwrap().deployment
        );
        assert_eq!(
            &res.first().unwrap().model,
            &expected_models.first().unwrap().model
        );
    }

    #[sqlx::test]
    #[ignore]
    async fn global_fallback_cost_models(pool: PgPool) {
        let deployment_id =
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        _ = setup_cost_models_table(&pool).await;
        _ = add_cost_models(&pool, simple_cost_models()).await;
        let global = global_cost_model();
        _ = add_cost_models(&pool, vec![global.clone()]).await;
        let res = cost_models(&pool, &[])
            .await
            .expect("Cost models query without deployments filter");

        assert_eq!(res.len(), 3);
        let incomplete_model = res.iter().find(|m| m.deployment == deployment_id.clone());
        assert!(incomplete_model.is_some());
        assert_ne!(incomplete_model.unwrap().model, global.model);

        let res = cost_models(
            &pool,
            &[
                deployment_id.clone(),
                "0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea".to_string(),
            ],
        )
        .await
        .expect("Cost models query without deployments filter");

        assert_eq!(res.len(), 2);
        let incomplete_model = res.iter().find(|m| m.deployment == deployment_id.clone());
        assert!(incomplete_model.is_some());
        assert_eq!(incomplete_model.unwrap().model, global.model);

        let complete_model = res.iter().find(|m| {
            m.deployment == *"0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea"
        });
        assert!(complete_model.is_some());
        assert_ne!(complete_model.unwrap().model, global.model);

        let missing_deployment = "Qmaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let res = cost_models(&pool, &[missing_deployment.to_string()])
            .await
            .expect("Cost models query without deployments filter");

        let missing_model = res.iter().find(|m| {
            m.deployment == *"0xb5ddb473e202a7abba81803ad153fd93a9b18d07ab38a711f7c2bd79435e50d7"
        });
        assert!(missing_model.is_some());
        assert_eq!(missing_model.unwrap().model, global.model);
    }

    #[sqlx::test]
    #[ignore]
    async fn success_cost_model(pool: PgPool) {
        let deployment_id = "0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea";
        let deployment_hash = "Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss".to_string();
        _ = setup_cost_models_table(&pool).await;
        _ = add_cost_models(&pool, simple_cost_models()).await;
        let res = cost_model(&pool, &deployment_hash)
            .await
            .expect("Cost model query")
            .expect("Cost model match deployment");

        assert_eq!(res.deployment, deployment_id.to_string());
    }

    #[sqlx::test]
    #[ignore]
    async fn global_fallback_cost_model(pool: PgPool) {
        let deployment_hash = "Qmaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        _ = setup_cost_models_table(&pool).await;
        _ = add_cost_models(&pool, simple_cost_models()).await;

        let res = cost_model(&pool, deployment_hash)
            .await
            .expect("Cost model query");

        assert!(res.is_none());

        let global = global_cost_model();
        _ = add_cost_models(&pool, vec![global.clone()]).await;

        let res = cost_model(&pool, deployment_hash)
            .await
            .expect("Cost model query")
            .expect("Global cost model fallback");

        assert_eq!(res.model, global.model);
        assert_eq!(&res.deployment, deployment_hash);
    }
}

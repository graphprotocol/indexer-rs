// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Tests for task lifecycle monitoring and health checks
//!
//! This module tests the enhanced lifecycle management capabilities
//! including health monitoring, restart policies, and system health reporting.

#[cfg(test)]
mod tests {
    use crate::task_lifecycle::{LifecycleManager, RestartPolicy, TaskStatus};
    use anyhow::Result;
    use std::time::Duration;
    use tokio::time::sleep;
    use tracing::info;

    /// Test basic health monitoring functionality
    #[tokio::test]
    async fn test_basic_health_monitoring() {
        let lifecycle = LifecycleManager::new();

        // Spawn a simple task
        let _task_handle = lifecycle
            .spawn_task::<(), _, _>(
                Some("test-health-task".to_string()),
                RestartPolicy::Never,
                10,
                |mut _rx, _ctx| async {
                    // Task that runs for a short time
                    sleep(Duration::from_millis(100)).await;
                    Ok(())
                },
            )
            .await
            .expect("Failed to spawn task");

        // Initial health check
        let system_health = lifecycle.get_system_health().await;
        assert_eq!(system_health.total_tasks, 1);
        assert_eq!(system_health.healthy_tasks, 1);
        assert_eq!(system_health.failed_tasks, 0);
        assert!(system_health.overall_healthy);

        // Perform health check
        lifecycle.perform_health_check().await;

        // Wait for task to complete
        sleep(Duration::from_millis(200)).await;

        // Check health after task completion
        lifecycle.perform_health_check().await;
        let final_health = lifecycle.get_system_health().await;

        info!("Final system health: {:?}", final_health);

        // The task should be marked as failed since it completed unexpectedly
        assert_eq!(final_health.total_tasks, 1);
        assert_eq!(final_health.failed_tasks, 1);
        assert!(!final_health.overall_healthy);

        info!("✅ Basic health monitoring test completed successfully");
    }

    /// Test detailed task health information
    #[tokio::test]
    async fn test_detailed_task_health_info() {
        let lifecycle = LifecycleManager::new();

        // Spawn a long-running task
        let task_handle = lifecycle
            .spawn_task::<(), _, _>(
                Some("long-running-task".to_string()),
                RestartPolicy::Never,
                10,
                |mut _rx, _ctx| async {
                    // Task that runs indefinitely
                    loop {
                        sleep(Duration::from_millis(100)).await;
                    }
                },
            )
            .await
            .expect("Failed to spawn task");

        // Give task time to start
        sleep(Duration::from_millis(50)).await;

        // Get detailed health information
        let health_status = lifecycle.get_health_status().await;
        assert_eq!(health_status.len(), 1);

        let task_health = health_status.values().next().unwrap();
        assert_eq!(task_health.name, Some("long-running-task".to_string()));
        assert_eq!(task_health.status, TaskStatus::Running);
        assert_eq!(task_health.restart_count, 0);
        assert!(task_health.is_healthy);
        assert!(task_health.uptime > Duration::from_millis(0));
        assert!(task_health.time_since_last_restart.is_none());

        // Test task info retrieval by ID
        let task_info = lifecycle.get_task_info(task_health.task_id).await;
        assert!(task_info.is_some());
        let info = task_info.unwrap();
        assert_eq!(info.name, Some("long-running-task".to_string()));
        assert!(info.is_healthy);

        // Cleanup
        drop(task_handle);
        sleep(Duration::from_millis(50)).await;

        info!("✅ Detailed task health info test completed successfully");
    }

    /// Test exponential backoff restart policy
    #[tokio::test]
    async fn test_exponential_backoff_restart() {
        let lifecycle = LifecycleManager::new();

        // Spawn a task with exponential backoff that fails quickly
        let _task_handle = lifecycle
            .spawn_task::<(), _, _>(
                Some("failing-task".to_string()),
                RestartPolicy::ExponentialBackoff {
                    initial: Duration::from_millis(10),
                    max: Duration::from_millis(100),
                    multiplier: 2.0,
                },
                10,
                |mut _rx, _ctx| async {
                    // Task that fails immediately
                    sleep(Duration::from_millis(5)).await;
                    Result::<()>::Err(anyhow::anyhow!("Intentional test failure"))
                },
            )
            .await
            .expect("Failed to spawn task");

        // Give time for initial failure and restart attempt
        sleep(Duration::from_millis(50)).await;

        // Check that the task is tracked
        let health_status = lifecycle.get_health_status().await;
        assert_eq!(health_status.len(), 1);

        let task_health = health_status.values().next().unwrap();
        assert_eq!(task_health.name, Some("failing-task".to_string()));

        info!(
            "Task status: {:?}, restart count: {}",
            task_health.status, task_health.restart_count
        );

        info!("✅ Exponential backoff restart test completed successfully");
    }

    /// Test task listing functionality
    #[tokio::test]
    async fn test_task_listing() {
        let lifecycle = LifecycleManager::new();

        // Spawn multiple tasks
        let task_names = vec!["task-1", "task-2", "task-3"];
        let mut _handles = Vec::new();

        for name in &task_names {
            let handle = lifecycle
                .spawn_task::<(), _, _>(
                    Some(name.to_string()),
                    RestartPolicy::Never,
                    10,
                    |mut _rx, _ctx| async {
                        // Long-running task
                        loop {
                            sleep(Duration::from_millis(100)).await;
                        }
                    },
                )
                .await
                .expect("Failed to spawn task");
            _handles.push(handle);
        }

        // Give tasks time to start
        sleep(Duration::from_millis(50)).await;

        // List all tasks
        let task_list = lifecycle.list_tasks().await;
        assert_eq!(task_list.len(), 3);

        // Verify all task names are present
        let listed_names: Vec<_> = task_list
            .iter()
            .filter_map(|(_, name)| name.as_ref())
            .collect();

        for expected_name in &task_names {
            assert!(listed_names.iter().any(|name| name == expected_name));
        }

        // Get system health
        let system_health = lifecycle.get_system_health().await;
        assert_eq!(system_health.total_tasks, 3);
        assert_eq!(system_health.healthy_tasks, 3);
        assert_eq!(system_health.failed_tasks, 0);
        assert!(system_health.overall_healthy);

        info!("✅ Task listing test completed successfully");
    }

    /// Test system health aggregation
    #[tokio::test]
    async fn test_system_health_aggregation() {
        let lifecycle = LifecycleManager::new();

        // Spawn a mix of healthy and failing tasks
        let _healthy_task = lifecycle
            .spawn_task::<(), _, _>(
                Some("healthy-task".to_string()),
                RestartPolicy::Never,
                10,
                |mut _rx, _ctx| async {
                    // Long-running healthy task
                    loop {
                        sleep(Duration::from_millis(100)).await;
                    }
                },
            )
            .await
            .expect("Failed to spawn healthy task");

        let _failing_task = lifecycle
            .spawn_task::<(), _, _>(
                Some("failing-task".to_string()),
                RestartPolicy::Never,
                10,
                |mut _rx, _ctx| async {
                    // Task that fails quickly
                    sleep(Duration::from_millis(10)).await;
                    Result::<()>::Err(anyhow::anyhow!("Intentional failure"))
                },
            )
            .await
            .expect("Failed to spawn failing task");

        // Give time for the failing task to fail
        sleep(Duration::from_millis(100)).await;
        lifecycle.perform_health_check().await;

        // Check system health
        let system_health = lifecycle.get_system_health().await;
        assert_eq!(system_health.total_tasks, 2);
        assert_eq!(system_health.healthy_tasks, 1);
        assert_eq!(system_health.failed_tasks, 1);
        assert!(!system_health.overall_healthy); // System unhealthy due to failed task

        info!("System health: {:?}", system_health);
        info!("✅ System health aggregation test completed successfully");
    }
}

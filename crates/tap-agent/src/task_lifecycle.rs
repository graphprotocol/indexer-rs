// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Task management and lifecycle abstractions for the tokio-based TAP agent
//!
//! This module provides task spawning, lifecycle management, and communication
//! abstractions for the tokio-based TAP agent architecture.

use std::{collections::HashMap, fmt::Debug, future::Future, sync::Arc, time::Duration};

use anyhow::{anyhow, Result};
use tokio::{
    sync::{mpsc, RwLock},
    task::JoinHandle,
};

/// Unique identifier for a task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(u64);

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskId {
    /// Create a new unique task identifier
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        TaskId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

/// Task status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// Task is currently running
    Running,
    /// Task has been stopped
    Stopped,
    /// Task failed and cannot continue
    Failed,
    /// Task is restarting after failure
    Restarting,
}

/// Restart policy for tasks
#[derive(Debug, Clone)]
pub enum RestartPolicy {
    /// Never restart the task
    Never,
    /// Always restart on failure
    Always,
    /// Restart with exponential backoff
    ExponentialBackoff {
        /// Initial backoff duration
        initial: Duration,
        /// Maximum backoff duration
        max: Duration,
        /// Backoff multiplier factor
        multiplier: f64,
    },
}

/// Handle to communicate with a task
pub struct TaskHandle<T> {
    tx: mpsc::Sender<T>,
    task_id: TaskId,
    name: Option<String>,
    lifecycle: Arc<LifecycleManager>,
}

impl<T> Clone for TaskHandle<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            task_id: self.task_id,
            name: self.name.clone(),
            lifecycle: self.lifecycle.clone(),
        }
    }
}

impl<T> TaskHandle<T> {
    /// Create a new task handle
    pub fn new(
        tx: mpsc::Sender<T>,
        name: Option<String>,
        lifecycle: Arc<LifecycleManager>,
    ) -> Self {
        Self {
            tx,
            task_id: TaskId::new(),
            name,
            lifecycle,
        }
    }

    /// Send a message to the task (fire-and-forget)
    pub async fn cast(&self, msg: T) -> Result<()> {
        self.tx
            .send(msg)
            .await
            .map_err(|_| anyhow!("Task channel closed"))
    }

    /// Send a message to the task (alias for cast)
    pub async fn send(&self, msg: T) -> Result<()> {
        self.cast(msg).await
    }

    /// Stop the task
    pub fn stop(&self, _reason: Option<String>) {
        self.lifecycle.stop_task(self.task_id);
    }

    /// Get task status
    pub async fn get_status(&self) -> TaskStatus {
        self.lifecycle.get_task_status(self.task_id).await
    }

    /// Get task name if set
    pub fn get_name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// RPC-style message that expects a response
#[allow(dead_code)]
pub trait RpcMessage: Send {
    /// The response type for this message
    type Response: Send;
}

/// Extension trait for TaskHandle to support RPC calls
#[allow(dead_code)]
#[allow(async_fn_in_trait)]
pub trait TaskHandleExt<T> {
    /// Send a message and wait for response
    async fn call<M>(&self, msg: M) -> Result<M::Response>
    where
        M: RpcMessage + Into<T>;
}

/// Information about a running task
struct TaskInfo {
    name: Option<String>,
    status: TaskStatus,
    restart_policy: RestartPolicy,
    handle: Option<JoinHandle<Result<()>>>,
    restart_count: u32,
    last_restart: Option<std::time::Instant>,
    created_at: std::time::Instant,
    last_health_check: Option<std::time::Instant>,
}

/// Manages task lifecycles
pub struct LifecycleManager {
    tasks: Arc<RwLock<HashMap<TaskId, TaskInfo>>>,
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleManager {
    /// Create a new lifecycle manager
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Spawn a new task
    pub async fn spawn_task<T, F, Fut>(
        &self,
        name: Option<String>,
        restart_policy: RestartPolicy,
        buffer_size: usize,
        task_fn: F,
    ) -> Result<TaskHandle<T>>
    where
        T: Send + 'static,
        F: FnOnce(mpsc::Receiver<T>, TaskContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel(buffer_size);
        let task_id = TaskId::new();

        let ctx = TaskContext {
            id: task_id,
            lifecycle: Arc::new(self.clone()),
        };

        let handle = tokio::spawn(async move { task_fn(rx, ctx).await });

        let info = TaskInfo {
            name: name.clone(),
            status: TaskStatus::Running,
            restart_policy,
            handle: Some(handle),
            restart_count: 0,
            last_restart: None,
            created_at: std::time::Instant::now(),
            last_health_check: None,
        };

        self.tasks.write().await.insert(task_id, info);

        Ok(TaskHandle {
            tx,
            task_id,
            name,
            lifecycle: Arc::new(self.clone()),
        })
    }

    /// Stop a task
    pub fn stop_task(&self, task_id: TaskId) {
        tokio::spawn({
            let tasks = self.tasks.clone();
            async move {
                if let Some(mut info) = tasks.write().await.remove(&task_id) {
                    info.status = TaskStatus::Stopped;
                    if let Some(handle) = info.handle.take() {
                        handle.abort();
                    }
                }
            }
        });
    }

    /// Get task status
    pub async fn get_task_status(&self, task_id: TaskId) -> TaskStatus {
        self.tasks
            .read()
            .await
            .get(&task_id)
            .map(|info| info.status)
            .unwrap_or(TaskStatus::Stopped)
    }

    /// Monitor tasks and restart if needed
    pub async fn monitor_tasks(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;

            let tasks_to_restart = {
                let mut tasks = self.tasks.write().await;
                let mut to_restart = Vec::new();

                for (id, info) in tasks.iter_mut() {
                    if let Some(handle) = &info.handle {
                        if handle.is_finished() {
                            match info.restart_policy {
                                RestartPolicy::Never => {
                                    info.status = TaskStatus::Failed;
                                    info.handle = None;
                                }
                                RestartPolicy::Always => {
                                    info.status = TaskStatus::Restarting;
                                    to_restart.push(*id);
                                }
                                RestartPolicy::ExponentialBackoff { .. } => {
                                    // TODO: Implement backoff logic
                                    info.status = TaskStatus::Restarting;
                                    to_restart.push(*id);
                                }
                            }
                        }
                    }
                }
                to_restart
            };

            for task_id in tasks_to_restart {
                self.restart_task(task_id).await;
            }
        }
    }

    /// Restart a specific task
    async fn restart_task(&self, task_id: TaskId) {
        let mut tasks = self.tasks.write().await;
        if let Some(info) = tasks.get_mut(&task_id) {
            // Calculate backoff delay if using exponential backoff
            let delay = match &info.restart_policy {
                RestartPolicy::ExponentialBackoff {
                    initial,
                    max,
                    multiplier,
                } => {
                    let delay =
                        initial.as_millis() as f64 * multiplier.powi(info.restart_count as i32);
                    Duration::from_millis(delay.min(max.as_millis() as f64) as u64)
                }
                _ => Duration::from_millis(100), // Small default delay
            };

            // Wait before restarting
            if delay > Duration::from_millis(0) {
                tracing::info!(
                    task_id = ?task_id,
                    task_name = ?info.name,
                    restart_count = info.restart_count,
                    delay_ms = delay.as_millis(),
                    "Restarting task after delay"
                );
                tokio::time::sleep(delay).await;
            }

            info.restart_count += 1;
            info.last_restart = Some(std::time::Instant::now());
            info.status = TaskStatus::Running;

            tracing::warn!(
                task_id = ?task_id,
                task_name = ?info.name,
                restart_count = info.restart_count,
                "Task restarted"
            );
        }
    }

    /// Get health status of all tasks
    pub async fn get_health_status(&self) -> HashMap<TaskId, TaskHealthInfo> {
        let tasks = self.tasks.read().await;
        let mut health_info = HashMap::new();

        for (id, info) in tasks.iter() {
            let uptime = info.created_at.elapsed();
            let time_since_last_restart = info.last_restart.map(|t| t.elapsed());

            let health = TaskHealthInfo {
                task_id: *id,
                name: info.name.clone(),
                status: info.status,
                restart_count: info.restart_count,
                uptime,
                time_since_last_restart,
                is_healthy: matches!(info.status, TaskStatus::Running),
            };

            health_info.insert(*id, health);
        }

        health_info
    }

    /// Get overall system health
    pub async fn get_system_health(&self) -> SystemHealthInfo {
        let health_status = self.get_health_status().await;
        let total_tasks = health_status.len();
        let healthy_tasks = health_status.values().filter(|h| h.is_healthy).count();
        let failed_tasks = health_status
            .values()
            .filter(|h| matches!(h.status, TaskStatus::Failed))
            .count();
        let restarting_tasks = health_status
            .values()
            .filter(|h| matches!(h.status, TaskStatus::Restarting))
            .count();

        SystemHealthInfo {
            total_tasks,
            healthy_tasks,
            failed_tasks,
            restarting_tasks,
            overall_healthy: failed_tasks == 0 && restarting_tasks == 0,
        }
    }

    /// Perform health check on all tasks
    pub async fn perform_health_check(&self) {
        let mut tasks = self.tasks.write().await;
        let now = std::time::Instant::now();

        for (id, info) in tasks.iter_mut() {
            info.last_health_check = Some(now);

            // Check if task handle is still valid
            if let Some(handle) = &info.handle {
                if handle.is_finished() && matches!(info.status, TaskStatus::Running) {
                    tracing::warn!(
                        task_id = ?id,
                        task_name = ?info.name,
                        "Task finished unexpectedly"
                    );
                    info.status = TaskStatus::Failed;
                }
            }
        }
    }

    /// Get detailed task information
    pub async fn get_task_info(&self, task_id: TaskId) -> Option<TaskHealthInfo> {
        let tasks = self.tasks.read().await;
        tasks.get(&task_id).map(|info| {
            let uptime = info.created_at.elapsed();
            let time_since_last_restart = info.last_restart.map(|t| t.elapsed());

            TaskHealthInfo {
                task_id,
                name: info.name.clone(),
                status: info.status,
                restart_count: info.restart_count,
                uptime,
                time_since_last_restart,
                is_healthy: matches!(info.status, TaskStatus::Running),
            }
        })
    }

    /// Get all task IDs and names
    pub async fn list_tasks(&self) -> Vec<(TaskId, Option<String>)> {
        let tasks = self.tasks.read().await;
        tasks
            .iter()
            .map(|(id, info)| (*id, info.name.clone()))
            .collect()
    }
}

/// Health information for a specific task
#[derive(Debug, Clone)]
pub struct TaskHealthInfo {
    /// Unique identifier for the task
    pub task_id: TaskId,
    /// Optional name of the task
    pub name: Option<String>,
    /// Current status of the task
    pub status: TaskStatus,
    /// Number of times the task has been restarted
    pub restart_count: u32,
    /// How long the task has been running
    pub uptime: Duration,
    /// Time elapsed since the last restart (if any)
    pub time_since_last_restart: Option<Duration>,
    /// Whether the task is currently healthy
    pub is_healthy: bool,
}

/// Overall system health information
#[derive(Debug, Clone)]
pub struct SystemHealthInfo {
    /// Total number of tasks in the system
    pub total_tasks: usize,
    /// Number of healthy (running) tasks
    pub healthy_tasks: usize,
    /// Number of failed tasks
    pub failed_tasks: usize,
    /// Number of tasks currently restarting
    pub restarting_tasks: usize,
    /// Whether the overall system is healthy
    pub overall_healthy: bool,
}

impl Clone for LifecycleManager {
    fn clone(&self) -> Self {
        Self {
            tasks: self.tasks.clone(),
        }
    }
}

/// Context provided to tasks
pub struct TaskContext {
    /// Unique task identifier
    pub id: TaskId,
    /// Shared lifecycle manager
    pub lifecycle: Arc<LifecycleManager>,
}

/// Global task registry for named lookups
#[derive(Clone)]
pub struct TaskRegistry {
    registry: Arc<RwLock<HashMap<String, Box<dyn std::any::Any + Send + Sync>>>>,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry {
    /// Create a new task registry
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a task handle
    #[allow(dead_code)]
    pub async fn register<T>(&self, name: String, handle: TaskHandle<T>)
    where
        T: Send + Sync + 'static,
    {
        self.registry.write().await.insert(name, Box::new(handle));
    }

    /// Unregister a task by name
    #[allow(dead_code)]
    pub async fn unregister(&self, name: &str) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.registry.write().await.remove(name)
    }

    /// Look up a task by name
    #[allow(dead_code)]
    pub async fn lookup<T>(&self, name: &str) -> Option<TaskHandle<T>>
    where
        T: Send + Sync + 'static,
    {
        let registry = self.registry.read().await;
        registry
            .get(name)
            .and_then(|any| any.downcast_ref::<TaskHandle<T>>().cloned())
    }

    /// Get a task by name (alias for lookup)
    pub async fn get_task<T>(&self, name: &str) -> Option<TaskHandle<T>>
    where
        T: Send + Sync + 'static,
    {
        self.lookup(name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[derive(Debug)]
    enum TestMessage {
        Ping,
        GetCount(oneshot::Sender<u32>),
    }

    impl RpcMessage for TestMessage {
        type Response = u32;
    }

    #[tokio::test]
    async fn test_basic_task_spawn_and_message() {
        let lifecycle = LifecycleManager::new();

        let handle = lifecycle
            .spawn_task(
                Some("test_task".to_string()),
                RestartPolicy::Never,
                10,
                |mut rx, _ctx| async move {
                    let mut count = 0u32;
                    while let Some(msg) = rx.recv().await {
                        match msg {
                            TestMessage::Ping => count += 1,
                            TestMessage::GetCount(tx) => {
                                let _ = tx.send(count);
                            }
                        }
                    }
                    Ok(())
                },
            )
            .await
            .unwrap();

        // Send some messages
        handle.cast(TestMessage::Ping).await.unwrap();
        handle.cast(TestMessage::Ping).await.unwrap();

        // Get count via RPC
        let (tx, rx) = oneshot::channel();
        handle.cast(TestMessage::GetCount(tx)).await.unwrap();
        let count = rx.await.unwrap();
        assert_eq!(count, 2);

        // Stop the task
        handle.stop(None);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(handle.get_status().await, TaskStatus::Stopped);
    }

    #[tokio::test]
    async fn test_task_registry() {
        let registry = TaskRegistry::new();
        let lifecycle = LifecycleManager::new();

        let handle = lifecycle
            .spawn_task::<TestMessage, _, _>(
                Some("registered_task".to_string()),
                RestartPolicy::Never,
                10,
                |mut rx, _ctx| async move {
                    while rx.recv().await.is_some() {}
                    Ok(())
                },
            )
            .await
            .unwrap();

        // Register the task
        registry
            .register("my_task".to_string(), handle.clone())
            .await;

        // Look it up
        let found: Option<TaskHandle<TestMessage>> = registry.lookup("my_task").await;
        assert!(found.is_some());

        // Send a message through the looked-up handle
        found.unwrap().cast(TestMessage::Ping).await.unwrap();
    }
}

// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Abstraction layer for migrating from ractor actors to tokio tasks
//!
//! This module provides a compatibility layer that allows gradual migration
//! from ractor actors to tokio tasks while maintaining the same API.

use std::{
    collections::HashMap,
    fmt::Debug,
    future::Future,
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{anyhow, Result};
use tokio::{
    sync::{mpsc, RwLock},
    task::JoinHandle,
};

/// Unique identifier for a task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(u64);

impl TaskId {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        TaskId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

/// Task status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Stopped,
    Failed,
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
        initial: Duration,
        max: Duration,
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
    /// Create a new task handle for testing
    #[cfg(any(test, feature = "test"))]
    pub fn new_for_test(
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
pub trait RpcMessage: Send {
    type Response: Send;
}

/// Extension trait for TaskHandle to support RPC calls
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

            for _task_id in tasks_to_restart {
                // TODO: Implement restart logic
            }
        }
    }
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
    pub id: TaskId,
    pub lifecycle: Arc<LifecycleManager>,
}

/// Global task registry for named lookups
pub struct TaskRegistry {
    registry: Arc<RwLock<HashMap<String, Box<dyn std::any::Any + Send + Sync>>>>,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a task handle
    pub async fn register<T>(&self, name: String, handle: TaskHandle<T>)
    where
        T: Send + Sync + 'static,
    {
        self.registry.write().await.insert(name, Box::new(handle));
    }

    /// Look up a task by name
    pub async fn lookup<T>(&self, name: &str) -> Option<TaskHandle<T>>
    where
        T: Send + Sync + 'static,
    {
        let registry = self.registry.read().await;
        registry
            .get(name)
            .and_then(|any| any.downcast_ref::<TaskHandle<T>>().cloned())
    }
}

/// Compatibility wrapper to make ractor ActorRef work with our abstraction
#[cfg(any(test, feature = "test"))]
pub mod compat {
    use super::*;
    use ractor::ActorRef;

    /// Trait to unify ActorRef and TaskHandle APIs
    pub trait MessageSender<T>: Clone + Send + Sync {
        fn cast(&self, msg: T) -> impl Future<Output = Result<()>> + Send;
        fn stop(&self, reason: Option<String>);
    }

    impl<T: Send + 'static> MessageSender<T> for TaskHandle<T> {
        async fn cast(&self, msg: T) -> Result<()> {
            TaskHandle::cast(self, msg).await
        }

        fn stop(&self, reason: Option<String>) {
            TaskHandle::stop(self, reason)
        }
    }

    impl<T: Send + 'static> MessageSender<T> for ActorRef<T> {
        async fn cast(&self, msg: T) -> Result<()> {
            ractor::ActorRef::cast(self, msg).map_err(|e| anyhow!("Actor error: {:?}", e))
        }

        fn stop(&self, reason: Option<String>) {
            ractor::ActorRef::stop(self, reason)
        }
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

// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! update metrics in case it succeeds or fails

use axum::http::Request;
use pin_project::pin_project;
use prometheus::HistogramTimer;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Layer, Service};

pub type MetricLabels = Arc<dyn MetricLabelProvider + 'static + Send + Sync>;

pub trait MetricLabelProvider {
    fn get_labels(&self) -> Vec<&str>;
}

/// Middleware for metrics
#[derive(Clone)]
pub struct PrometheusMetricsMiddleware<S> {
    inner: S,
    histogram: prometheus::HistogramVec,
    failure: prometheus::CounterVec,
}

/// MetricsMiddleware used in tower components
///
/// Register prometheus metrics in case of success or failure
#[derive(Clone)]
pub struct PrometheusMetricsMiddlewareLayer {
    /// Histogram used to register the processing timer
    histogram: prometheus::HistogramVec,
    /// Counter metric in case of failure
    failure: prometheus::CounterVec,
}

impl PrometheusMetricsMiddlewareLayer {
    pub fn new(histogram: prometheus::HistogramVec, failure: prometheus::CounterVec) -> Self {
        Self { histogram, failure }
    }
}

impl<S> Layer<S> for PrometheusMetricsMiddlewareLayer {
    type Service = PrometheusMetricsMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PrometheusMetricsMiddleware {
            inner,
            histogram: self.histogram.clone(),
            failure: self.failure.clone(),
        }
    }
}

impl<S, ReqBody> Service<Request<ReqBody>> for PrometheusMetricsMiddleware<S>
where
    S: Service<Request<ReqBody>> + Clone + 'static,
    ReqBody: 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = PrometheusMetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> PrometheusMetricsFuture<S::Future> {
        let labels = request.extensions().get::<MetricLabels>().cloned();
        PrometheusMetricsFuture {
            timer: None,
            histogram: self.histogram.clone(),
            failure: self.failure.clone(),
            labels,
            fut: self.inner.call(request),
        }
    }
}

#[pin_project]
pub struct PrometheusMetricsFuture<F> {
    /// Instant at which we started the requst.
    timer: Option<HistogramTimer>,

    histogram: prometheus::HistogramVec,
    failure: prometheus::CounterVec,

    labels: Option<MetricLabels>,

    #[pin]
    fut: F,
}

impl<F, R, E> Future for PrometheusMetricsFuture<F>
where
    F: Future<Output = Result<R, E>>,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let Some(labels) = &this.labels else {
            return this.fut.poll(cx);
        };

        if this.timer.is_none() {
            // Start timer so we can track duration of request.
            let duration_metric = this.histogram.with_label_values(&labels.get_labels());
            *this.timer = Some(duration_metric.start_timer());
        }

        match this.fut.poll(cx) {
            Poll::Ready(result) => {
                if result.is_err() {
                    let _ = this
                        .failure
                        .get_metric_with_label_values(&labels.get_labels());
                }
                // Record the duration of this request.
                if let Some(timer) = this.timer.take() {
                    timer.observe_duration();
                }
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, Response},
    };
    use prometheus::core::Collector;
    use tower::{Service, ServiceBuilder, ServiceExt};

    use crate::middleware::prometheus_metrics::{MetricLabels, PrometheusMetricsMiddlewareLayer};

    use super::MetricLabelProvider;

    struct TestLabel;
    impl MetricLabelProvider for TestLabel {
        fn get_labels(&self) -> Vec<&str> {
            vec!["label1,", "label2", "label3"]
        }
    }
    async fn handle(_: Request<Body>) -> anyhow::Result<Response<Body>> {
        Ok(Response::new(Body::default()))
    }

    async fn handle_err(_: Request<Body>) -> anyhow::Result<Response<Body>> {
        Err(anyhow::anyhow!("Error"))
    }

    #[tokio::test]
    async fn test_metrics_middleware() {
        let registry = prometheus::Registry::new();
        let histogram_metric = prometheus::register_histogram_vec_with_registry!(
            "histogram_metric",
            "Test",
            &["deployment", "sender", "allocation"],
            registry,
        )
        .unwrap();

        let failure_metric = prometheus::register_counter_vec_with_registry!(
            "failure_metric",
            "Test",
            &["deployment", "sender", "allocation"],
            registry,
        )
        .unwrap();

        // check if everything is clean
        assert_eq!(
            histogram_metric
                .collect()
                .first()
                .unwrap()
                .get_metric()
                .len(),
            0
        );
        assert_eq!(
            failure_metric.collect().first().unwrap().get_metric().len(),
            0
        );

        let metrics_layer =
            PrometheusMetricsMiddlewareLayer::new(histogram_metric.clone(), failure_metric.clone());
        let mut service = ServiceBuilder::new()
            .layer(metrics_layer)
            .service_fn(handle);
        let handle = service.ready().await.unwrap();

        // default labels, all empty
        let labels: MetricLabels = Arc::new(TestLabel);

        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(labels.clone());
        let _ = handle.call(req).await;

        assert_eq!(
            histogram_metric
                .collect()
                .first()
                .unwrap()
                .get_metric()
                .len(),
            1
        );

        assert_eq!(
            failure_metric.collect().first().unwrap().get_metric().len(),
            0
        );

        let metrics_layer =
            PrometheusMetricsMiddlewareLayer::new(histogram_metric.clone(), failure_metric.clone());
        let mut service = ServiceBuilder::new()
            .layer(metrics_layer)
            .service_fn(handle_err);
        let handle = service.ready().await.unwrap();

        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(labels);
        let _ = handle.call(req).await;

        // it's using the same labels, should have only one metric
        assert_eq!(
            histogram_metric
                .collect()
                .first()
                .unwrap()
                .get_metric()
                .len(),
            1
        );

        // new failture
        assert_eq!(
            failure_metric.collect().first().unwrap().get_metric().len(),
            1
        );
    }
}

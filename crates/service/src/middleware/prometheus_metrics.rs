// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! update metrics in case it succeeds or fails

use axum::http::Request;
use pin_project::pin_project;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};
use tower::{Layer, Service};

use crate::error::StatusCodeExt;

pub type MetricLabels = Arc<dyn MetricLabelProvider + 'static + Send + Sync>;

pub trait MetricLabelProvider {
    fn get_labels(&self) -> Vec<&str>;
}

/// Middleware for metrics
#[derive(Clone)]
pub struct PrometheusMetricsMiddleware<S> {
    inner: S,
    histogram: prometheus::HistogramVec,
}

/// MetricsMiddleware used in tower components
///
/// Register prometheus metrics in case of success or failure
#[derive(Clone)]
pub struct PrometheusMetricsMiddlewareLayer {
    /// Histogram used to register the processing timer
    histogram: prometheus::HistogramVec,
}

impl PrometheusMetricsMiddlewareLayer {
    pub fn new(histogram: prometheus::HistogramVec) -> Self {
        Self { histogram }
    }
}

impl<S> Layer<S> for PrometheusMetricsMiddlewareLayer {
    type Service = PrometheusMetricsMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PrometheusMetricsMiddleware {
            inner,
            histogram: self.histogram.clone(),
        }
    }
}

impl<S, ReqBody> Service<Request<ReqBody>> for PrometheusMetricsMiddleware<S>
where
    S: Service<Request<ReqBody>> + Clone + 'static,
    ReqBody: 'static,
    Result<S::Response, S::Error>: StatusCodeExt,
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
            labels,
            fut: self.inner.call(request),
        }
    }
}

#[pin_project]
pub struct PrometheusMetricsFuture<F> {
    /// Instant at which we started the requst.
    timer: Option<Instant>,

    histogram: prometheus::HistogramVec,
    labels: Option<MetricLabels>,

    #[pin]
    fut: F,
}

impl<F, T> Future for PrometheusMetricsFuture<F>
where
    F: Future<Output = T>,
    T: StatusCodeExt,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let Some(labels) = this.labels else {
            return this.fut.poll(cx);
        };

        if this.timer.is_none() {
            // Start timer so we can track duration of request.
            *this.timer = Some(Instant::now());
        }

        match this.fut.poll(cx) {
            Poll::Ready(result) => {
                let status_code = result.status_code();
                // add status code
                let mut labels = labels.get_labels();
                labels.push(status_code.as_str());
                let duration_metric = this.histogram.with_label_values(&labels);

                // Record the duration of this request.
                let timer = this.timer.take().expect("timer should exist");
                duration_metric.observe(timer.elapsed().as_secs_f64());

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
    use reqwest::StatusCode;
    use tower::{Service, ServiceBuilder, ServiceExt};

    use crate::{
        error::StatusCodeExt,
        middleware::prometheus_metrics::{MetricLabels, PrometheusMetricsMiddlewareLayer},
    };

    use super::MetricLabelProvider;

    struct TestLabel;
    impl MetricLabelProvider for TestLabel {
        fn get_labels(&self) -> Vec<&str> {
            vec!["label1,", "label2", "label3"]
        }
    }

    #[derive(Debug)]
    struct ErrorResponse;

    impl StatusCodeExt for ErrorResponse {
        fn status_code(&self) -> StatusCode {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }

    async fn handle(_: Request<Body>) -> Result<Response<Body>, ErrorResponse> {
        Ok(Response::new(Body::default()))
    }

    async fn handle_err(_: Request<Body>) -> Result<Response<Body>, ErrorResponse> {
        Err(ErrorResponse)
    }

    #[tokio::test]
    async fn test_metrics_middleware() {
        let registry = prometheus::Registry::new();
        let histogram_metric = prometheus::register_histogram_vec_with_registry!(
            "histogram_metric",
            "Test",
            &["deployment", "sender", "allocation", "status_code"],
            registry,
        )
        .unwrap();

        // check if everything is clean
        assert!(histogram_metric
            .collect()
            .first()
            .unwrap()
            .get_metric()
            .is_empty());

        let metrics_layer = PrometheusMetricsMiddlewareLayer::new(histogram_metric.clone());
        let mut service = ServiceBuilder::new()
            .layer(metrics_layer)
            .service_fn(handle);
        let handle = service.ready().await.unwrap();

        // default labels, all empty
        let labels: MetricLabels = Arc::new(TestLabel);

        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(labels.clone());
        let _ = handle.call(req).await;

        let how_many_metrics = |status: u32| {
            histogram_metric
                .collect()
                .first()
                .unwrap()
                .get_metric()
                .iter()
                .filter(|a| a.get_label()[3].get_value() == status.to_string())
                .count()
        };

        assert_eq!(how_many_metrics(200), 1);
        assert_eq!(how_many_metrics(500), 0);

        let metrics_layer = PrometheusMetricsMiddlewareLayer::new(histogram_metric.clone());
        let mut service = ServiceBuilder::new()
            .layer(metrics_layer)
            .service_fn(handle_err);
        let handle = service.ready().await.unwrap();

        let mut req = Request::new(Default::default());
        req.extensions_mut().insert(labels);
        let _ = handle.call(req).await;

        // it's using the same labels, should have only one metric
        assert_eq!(how_many_metrics(200), 1);
        assert_eq!(how_many_metrics(500), 1);
    }
}

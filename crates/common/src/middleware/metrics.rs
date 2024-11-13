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

pub struct MetricsMiddleware<S> {
    inner: S,
    histogram: prometheus::HistogramVec,
    failure: prometheus::CounterVec,
}

pub struct MetricsMiddlewareLayer {
    histogram: prometheus::HistogramVec,
    failure: prometheus::CounterVec,
}

impl MetricsMiddlewareLayer {
    pub fn new(histogram: prometheus::HistogramVec, failure: prometheus::CounterVec) -> Self {
        Self { histogram, failure }
    }
}

impl<S> Layer<S> for MetricsMiddlewareLayer {
    type Service = MetricsMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsMiddleware {
            inner,
            histogram: self.histogram.clone(),
            failure: self.failure.clone(),
        }
    }
}

impl<S, ReqBody> Service<Request<ReqBody>> for MetricsMiddleware<S>
where
    S: Service<Request<ReqBody>> + Clone + 'static,
    ReqBody: 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = MetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> MetricsFuture<S::Future> {
        let labels = request.extensions().get::<MetricLabels>().cloned();
        MetricsFuture {
            timer: None,
            histogram: self.histogram.clone(),
            failure: self.failure.clone(),
            labels,
            fut: self.inner.call(request),
        }
    }
}

#[pin_project]
pub struct MetricsFuture<F> {
    /// Instant at which we started the requst.
    timer: Option<HistogramTimer>,

    histogram: prometheus::HistogramVec,
    failure: prometheus::CounterVec,

    labels: Option<MetricLabels>,

    #[pin]
    fut: F,
}

impl<F, R, E> Future for MetricsFuture<F>
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

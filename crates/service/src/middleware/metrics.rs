use std::sync::Arc;


pub type MetricLabels = Arc<dyn MetricLabelProvider + 'static + Send + Sync>;

pub trait MetricLabelProvider {
    fn get_labels(&self) -> Vec<&str>;
}

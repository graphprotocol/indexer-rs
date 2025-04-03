use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use reqwest::Client;

// See:
// https://github.com/graphprotocol/indexer-rs/blob/main/crates/tap-agent/src/agent/sender_account.rs#L51-L109
// for some other available metrics
const TAP_UNAGGREGATED_FEES_GRT_TOTAL: &str = "tap_unaggregated_fees_grt_total";
const TAP_PENDING_RAV_GRT_TOTAL: &str = "tap_pending_rav_grt_total";
const TAP_RAV_REQUEST_TRIGGER_VALUE: &str = "tap_rav_request_trigger_value";
const TAP_RAVS_CREATED_TOTAL: &str = "tap_ravs_created_total";

// Metrics data structure to track changes
// using the tap-agent metrics values
#[derive(Debug, Default, Clone)]
pub struct MetricsData {
    pub unaggregated_fees: HashMap<String, f64>,
    pub pending_rav: HashMap<String, f64>,
    pub trigger_value: HashMap<String, f64>,
    pub ravs_created: HashMap<String, u32>,
}

impl MetricsData {
    // the allocation_id is not exactly the key
    // allocation="0xE8B99E1fD28791fefd421aCB081a8c31Da6f10DA",sender="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
    pub fn ravs_created_by_allocation(&self, allocation_id: &str) -> u32 {
        let key = format!("allocation=\"{}\"", allocation_id);
        get_value(&self.ravs_created, &key)
    }

    pub fn _ravs_created_by_sender(&self, sender: &str) -> u32 {
        let key = format!("sender=\"{}\"", sender);
        get_value(&self.ravs_created, &key)
    }

    pub fn unaggregated_fees_by_allocation(&self, allocation_id: &str) -> f64 {
        let key = format!("allocation=\"{}\"", allocation_id);
        get_value(&self.unaggregated_fees, &key)
    }

    pub fn trigger_value_by_sender(&self, sender: &str) -> f64 {
        let key = format!("sender=\"{}\"", sender);
        get_value(&self.trigger_value, &key)
    }
}

pub struct MetricsChecker {
    pub client: Arc<Client>,
    pub metrics_url: String,
}

impl MetricsChecker {
    pub fn new(client: Arc<Client>, metrics_url: String) -> Self {
        Self {
            client,
            metrics_url,
        }
    }

    pub async fn get_current_metrics(&self) -> Result<MetricsData> {
        let response = self.client.get(&self.metrics_url).send().await?;
        let mut data = MetricsData::default();

        if response.status().is_success() {
            let metrics_text = response.text().await?;

            // Parse metrics text line by line
            for line in metrics_text.lines() {
                if line.starts_with('#') {
                    continue; // Skip comments
                }

                // Track unaggregated fees
                if line.contains(TAP_UNAGGREGATED_FEES_GRT_TOTAL) {
                    parse_metric_line(line, &mut data.unaggregated_fees);
                }
                // Track pending RAVs
                else if line.contains(TAP_PENDING_RAV_GRT_TOTAL) {
                    parse_metric_line(line, &mut data.pending_rav);
                }
                // Track trigger value
                else if line.contains(TAP_RAV_REQUEST_TRIGGER_VALUE) {
                    parse_metric_line(line, &mut data.trigger_value);
                } else if line.contains(TAP_RAVS_CREATED_TOTAL) {
                    if let Some(brace_idx) = line.find('{') {
                        if let Some(rbrace_idx) = line.find('}') {
                            let labels = &line[brace_idx + 1..rbrace_idx];

                            if let Some(value_str) = line.split_whitespace().last() {
                                if let Ok(value) = value_str.parse::<u32>() {
                                    let key = labels.to_string();
                                    data.ravs_created.insert(key, value);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(data)
    }
}

// Function to parse a metric line like:
// tap_receipts_received_total{allocation="0xE8B99E1fD28791fefd421aCB081a8c31Da6f10DA",sender="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"} 330
// Notice that all metrics are linked to an allocation_id and sender
// so we are using the combination of both as they come in the metric report
// allocation="0xE8B99E1fD28791fefd421aCB081a8c31Da6f10DA",sender="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
// that is going to be the key for the HashMap
fn parse_metric_line(line: &str, map: &mut HashMap<String, f64>) {
    if let Some(brace_idx) = line.find('{') {
        if let Some(rbrace_idx) = line.find('}') {
            let labels = &line[brace_idx + 1..rbrace_idx];

            if let Some(value_str) = line.split_whitespace().last() {
                if let Ok(value) = value_str.parse::<f64>() {
                    let key = labels.to_string();

                    // If the value changed, log it
                    if !map.contains_key(&key) || map[&key] != value {
                        if map.contains_key(&key) {
                            let old_value = map[&key];
                            println!("📈 Metric changed: {} ({} → {})", key, old_value, value);
                        } else {
                            println!("📈 New metric: {} = {}", key, value);
                        }
                        map.insert(key, value);
                    }
                }
            }
        }
    }
}

fn get_value<T>(map: &HashMap<String, T>, pattern: &str) -> T
where
    T: Default + Copy,
{
    for (key, value) in map {
        if key.contains(pattern) {
            return *value;
        }
    }

    T::default()
}

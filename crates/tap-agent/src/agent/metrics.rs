use prometheus::{
    register_counter_vec, register_gauge_vec, register_histogram_vec, register_int_gauge_vec,
    CounterVec, GaugeVec, HistogramVec, IntGaugeVec,
};

lazy_static::lazy_static! {
    pub static ref SENDER_DENIED: IntGaugeVec =
        register_int_gauge_vec!("tap_sender_denied", "Sender is denied", &["sender"]).unwrap();
    pub static ref ESCROW_BALANCE: GaugeVec = register_gauge_vec!(
        "tap_sender_escrow_balance_grt_total",
        "Sender escrow balance",
        &["sender"]
    )
    .unwrap();
    pub static ref UNAGGREGATED_FEES: GaugeVec = register_gauge_vec!(
        "tap_unaggregated_fees_grt_total",
        "Unggregated Fees value",
        &["sender", "allocation"]
    )
    .unwrap();
    pub static ref SENDER_FEE_TRACKER: GaugeVec = register_gauge_vec!(
        "tap_sender_fee_tracker_grt_total",
        "Sender fee tracker metric",
        &["sender"]
    )
    .unwrap();
    pub static ref INVALID_RECEIPT_FEES: GaugeVec = register_gauge_vec!(
        "tap_invalid_receipt_fees_grt_total",
        "Failed receipt fees",
        &["sender", "allocation"]
    )
    .unwrap();
    pub static ref PENDING_RAV: GaugeVec = register_gauge_vec!(
        "tap_pending_rav_grt_total",
        "Pending ravs values",
        &["sender", "allocation"]
    )
    .unwrap();
    pub static ref MAX_FEE_PER_SENDER: GaugeVec = register_gauge_vec!(
        "tap_max_fee_per_sender_grt_total",
        "Max fee per sender in the config",
        &["sender"]
    )
    .unwrap();
    pub static ref RAV_REQUEST_TRIGGER_VALUE: GaugeVec = register_gauge_vec!(
        "tap_rav_request_trigger_value",
        "RAV request trigger value divisor",
        &["sender"]
    )
    .unwrap();
    pub static ref RECEIPTS_CREATED: CounterVec = register_counter_vec!(
        "tap_receipts_received_total",
        "Receipts received since start of the program.",
        &["sender", "allocation"]
    )
    .unwrap();

    // Allocation metrics
    pub static ref CLOSED_SENDER_ALLOCATIONS: CounterVec = register_counter_vec!(
        "tap_closed_sender_allocation_total",
        "Count of sender-allocation managers closed since the start of the program",
        &["sender"]
    )
    .unwrap();
    pub static ref RAVS_CREATED: CounterVec = register_counter_vec!(
        "tap_ravs_created_total",
        "RAVs updated or created per sender allocation since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap();
    pub static ref RAVS_FAILED: CounterVec = register_counter_vec!(
        "tap_ravs_failed_total",
        "RAV requests failed since the start of the program",
        &["sender", "allocation"]
    )
    .unwrap();
    pub static ref RAV_RESPONSE_TIME: HistogramVec = register_histogram_vec!(
        "tap_rav_response_time_seconds",
        "RAV response time per sender",
        &["sender"]
    )
    .unwrap();

}

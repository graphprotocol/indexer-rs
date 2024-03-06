use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};
use tap_core::{
    checks::{Check, CheckError, CheckResult},
    tap_receipt::{Checking, ReceiptWithState},
};

use crate::tap::executor::error::AdapterError;

#[derive(Serialize, Deserialize, Debug)]
pub struct Value {
    query_appraisals: Option<Arc<RwLock<HashMap<u64, u128>>>>,
}

#[async_trait::async_trait]
#[typetag::serde]
impl Check for Value {
    async fn check(&self, receipt: &ReceiptWithState<Checking>) -> CheckResult<()> {
        let value = receipt.signed_receipt().message.value;
        let query_id = receipt.query_id();

        let query_appraisals = self.query_appraisals.as_ref().expect(
            "Query appraisals should be initialized. The opposite should never happen when \
            receipts value checking is enabled.",
        );
        let query_appraisals_read = query_appraisals.read().unwrap();
        let appraised_value =
            query_appraisals_read
                .get(&query_id)
                .ok_or(AdapterError::ValidationError {
                    error: "No appraised value found for query".to_string(),
                })?;
        if value != *appraised_value {
            return Err(CheckError(format!(
                "Value different from appraised_value. value: {}, appraised_value: {}",
                value, *appraised_value
            )));
        }
        Ok(())
    }
}

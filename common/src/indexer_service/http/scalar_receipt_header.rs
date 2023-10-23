use std::ops::Deref;

use headers::{Header, HeaderName, HeaderValue};
use lazy_static::lazy_static;
use tap_core::tap_manager::SignedReceipt;

pub struct ScalarReceipt(Option<SignedReceipt>);

impl ScalarReceipt {
    pub fn into_signed_receipt(self) -> Option<SignedReceipt> {
        self.0
    }
}

impl Deref for ScalarReceipt {
    type Target = Option<SignedReceipt>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

lazy_static! {
    static ref SCALAR_RECEIPT: HeaderName = HeaderName::from_static("scalar-receipt");
}

impl Header for ScalarReceipt {
    fn name() -> &'static HeaderName {
        &SCALAR_RECEIPT
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let value = values.next();
        let raw_receipt = value
            .map(|value| value.to_str())
            .transpose()
            .map_err(|_| headers::Error::invalid())?;
        let parsed_receipt = raw_receipt
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| headers::Error::invalid())?;
        Ok(ScalarReceipt(parsed_receipt))
    }

    fn encode<E>(&self, _values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        unimplemented!()
    }
}

// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::OutputType;
use async_trait::async_trait;
use bigdecimal::{BigDecimal, Num};
// use ethers::types::Address;

use crate::common::{address::Address, indexer_error::IndexerError};
// use crate::common::address::Address;

pub mod allocations;

// pub struct ReceiptManager;

#[async_trait]
trait ReceiptManager {
    async fn add(
        &mut self,
        receipt_data: String,
    ) -> Result<(String, Address, BigDecimal), IndexerError>;
}

struct BigDecimalWrapper(BigDecimal);

impl OutputType for BigDecimalWrapper {
    // fn parse(_value: &serde_json::Value) -> Option<Self> {
    //     // Implement parsing logic for the wrapper type if needed.
    //     unimplemented!()
    // }

    // fn to_value(&self) -> serde_json::Value {
    //     // Implement conversion logic from the wrapper type to a JSON value.
    //     unimplemented!()
    // }

    fn type_name() -> std::borrow::Cow<'static, str> {
        todo!()
    }

    fn create_type_info(_registry: &mut async_graphql::registry::Registry) -> String {
        todo!()
    }

    fn resolve<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        _ctx: &'life1 async_graphql::ContextSelectionSet<'life2>,
        _field: &'life3 async_graphql::Positioned<async_graphql::parser::types::Field>,
    ) -> core::pin::Pin<
        Box<
            dyn core::future::Future<Output = async_graphql::ServerResult<async_graphql::Value>>
                + core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        todo!()
    }
}

fn read_number(data: &str, start: usize, end: usize) -> BigDecimal {
    let number = &data[start..end];
    BigDecimal::from_str_radix(number, 16).unwrap()
}

// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;

use alloy_primitives::Address;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref INDEXER_ADDRESS: Address =
        Address::from_str("0x1234567890123456789012345678901234567890").unwrap();
}

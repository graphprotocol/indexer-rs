// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy_primitives::Address;
use ethers_core::types::U256;
use std::collections::HashMap;
use std::str::FromStr;

pub const INDEXER_ADDRESS: &str = "0x1234567890123456789012345678901234567890";

pub const ESCROW_QUERY_RESPONSE: &str = r#"
    {
        "data": {
            "escrowAccounts": [
                {
                    "balance": "34",
                    "totalAmountThawing": "10",
                    "sender": {
                        "id": "0x90f8bf6a479f320ead074411a4b0e7944ea8c9c1"
                    }
                },
                {
                    "balance": "42",
                    "totalAmountThawing": "0",
                    "sender": {
                        "id": "0x22d491bde2303f2f43325b2108d26f1eaba1e32b"
                    }
                }
            ]
        }
    }
"#;

pub fn expected_escrow_accounts() -> HashMap<Address, U256> {
    HashMap::from([
        (
            Address::from_str("0x90f8bf6a479f320ead074411a4b0e7944ea8c9c1").unwrap(),
            U256::from(24),
        ),
        (
            Address::from_str("0x22d491bde2303f2f43325b2108d26f1eaba1e32b").unwrap(),
            U256::from(42),
        ),
    ])
}

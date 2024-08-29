// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bigdecimal::{BigDecimal, ToPrimitive};
use serde::{de::Error, Deserialize};

#[derive(Debug, PartialEq)]
pub struct NonZeroGRT(u128);

impl NonZeroGRT {
    pub fn get_value(&self) -> u128 {
        self.0
    }
}

impl<'de> Deserialize<'de> for NonZeroGRT {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = BigDecimal::deserialize(deserializer)?;
        if v <= 0.into() {
            return Err(Error::custom("GRT value must be greater than 0"));
        }
        // Convert to wei
        let v = v * BigDecimal::from(10u64.pow(18));
        // Convert to u128
        let wei = v.to_u128().ok_or_else(|| {
            Error::custom("GRT value cannot be represented as a u128 GRT wei value")
        })?;

        Ok(Self(wei))
    }
}

#[cfg(test)]
mod tests {
    use serde_test::{assert_de_tokens, assert_de_tokens_error, Token};

    use super::*;

    #[test]
    fn test_parse_grt_value_to_u128_deserialize() {
        assert_de_tokens(&NonZeroGRT(1_000_000_000_000_000_000), &[Token::Str("1")]);
        assert_de_tokens(&NonZeroGRT(1_100_000_000_000_000_000), &[Token::Str("1.1")]);
        assert_de_tokens(
            &NonZeroGRT(1_100_000_000_000_000_000),
            &[Token::String("1.1")],
        );
        // The following doesn't work because of rounding errors
        // assert_de_tokens(&NonZeroGRT(1_100_000_000_000_000_000), &[Token::F32(1.1)]);
        // assert_de_tokens(&NonZeroGRT(1_100_000_000_000_000_000), &[Token::F64(1.1)]);
        assert_de_tokens(
            &NonZeroGRT(1_000_000_000_000_000_001),
            &[Token::Str("1.000000000000000001")],
        );

        assert_de_tokens(&NonZeroGRT(1), &[Token::Str("0.000000000000000001")]);
        assert_de_tokens_error::<NonZeroGRT>(
            &[Token::Str("0")],
            "GRT value must be greater than 0",
        );
        assert_de_tokens_error::<NonZeroGRT>(
            &[Token::Str("-1")],
            "GRT value must be greater than 0",
        );
        assert_de_tokens(
            &NonZeroGRT(1_000_000_000_000_000_000),
            &[Token::Str("1.0000000000000000001")],
        );
        let v = Box::leak(Box::new(format!("{}0", u128::MAX)));
        assert_de_tokens_error::<NonZeroGRT>(
            &[Token::Str(v.as_str())],
            "GRT value cannot be represented as a u128 GRT wei value",
        );
    }
}

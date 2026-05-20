// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use bigdecimal::{BigDecimal, ToPrimitive};
use serde::{de::Error, Deserialize};

/// GRT value stored as wei (10^-18 GRT). Allows zero.
///
/// Deserializes from human-readable GRT strings like "1.5" or "0.001".
#[derive(Debug, PartialEq, Default, Clone, Copy)]
pub struct GRT(u128);

impl GRT {
    pub const ZERO: GRT = GRT(0);

    /// Convert GRT string to wei for test construction.
    /// Panics on invalid input - only use in tests.
    #[cfg(test)]
    pub fn from_grt(grt: &str) -> Self {
        use bigdecimal::{BigDecimal, ToPrimitive};
        use std::str::FromStr;
        let v = BigDecimal::from_str(grt).expect("invalid GRT value");
        let wei = (v * BigDecimal::from(10u64.pow(18)))
            .to_u128()
            .expect("GRT value too large");
        GRT(wei)
    }

    pub fn wei(&self) -> u128 {
        self.0
    }
}

impl<'de> Deserialize<'de> for GRT {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = BigDecimal::deserialize(deserializer)?;
        if v < 0.into() {
            return Err(Error::custom("GRT value cannot be negative"));
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

#[derive(Debug, PartialEq, Default, Clone)]
pub struct NonZeroGRT(u128);

impl NonZeroGRT {
    pub fn new(value: u128) -> Result<Self, String> {
        if value == 0 {
            Err("GRT value cannot be represented as a u128 GRT wei value".into())
        } else {
            Ok(NonZeroGRT(value))
        }
    }

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
    fn test_grt_deserialize() {
        // Arrange & Act & Assert
        assert_de_tokens(&GRT(1_000_000_000_000_000_000), &[Token::Str("1")]);
        assert_de_tokens(&GRT(1_100_000_000_000_000_000), &[Token::Str("1.1")]);
        assert_de_tokens(&GRT(0), &[Token::Str("0")]);
    }

    #[test]
    fn test_grt_negative_rejected() {
        // Arrange & Act & Assert
        assert_de_tokens_error::<GRT>(&[Token::Str("-1")], "GRT value cannot be negative");
    }

    #[test]
    fn test_grt_wei() {
        // Arrange
        let grt = GRT(1_500_000_000_000_000_000);

        // Act & Assert
        assert_eq!(grt.wei(), 1_500_000_000_000_000_000);
    }

    #[test]
    fn test_grt_zero_constant() {
        // Arrange & Act & Assert
        assert_eq!(GRT::ZERO.wei(), 0);
    }

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

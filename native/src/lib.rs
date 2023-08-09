// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use arc_swap::ArcSwap;
use keccak_hash::keccak;
use lazy_static::lazy_static;
use secp256k1::{recovery::RecoverableSignature, Message, PublicKey, Secp256k1, VerifyOnly};
use std::sync::Arc;

pub mod attestation;
pub mod signature_verification;

type Address = [u8; 20];

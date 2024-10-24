#![allow(dead_code)]

use std::{
    fs::File,
    io::{self, Write},
};

use alloy::{
    primitives::Address,
    signers::{k256::ecdsa::SigningKey, local::LocalSigner},
};
use indexer_tap_agent::tap::test_utils::wallet;

#[derive(Clone, Debug)]
pub struct Indexer {
    pub signer: LocalSigner<SigningKey>,
    pub address: Address,
}

#[derive(Clone, Debug)]
pub struct QuerySender(pub Address);

pub fn manage_keys() -> io::Result<(Indexer, QuerySender)> {
    let (signer1, address1) = wallet(0);
    let (signer2, address2) = wallet(1);

    write_to_file("indexer_wallet", &signer1)?;
    write_to_file("sender_wallet", &signer2)?;

    let indexer = Indexer {
        signer: signer1,
        address: address1,
    };
    let sender = QuerySender(address2);

    Ok((indexer, sender))
}

fn write_to_file(path: &str, signer: &LocalSigner<SigningKey>) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(signer.to_bytes().as_slice())?;
    Ok(())
}

use crate::common::{indexer_error, indexer_error::{IndexerErrorCode, IndexerError}, address::{Address, to_address}};
use async_graphql::{Error, Object, SimpleObject, ID};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
// use ethers::types::Address;
use ethers_core::{types::U256, utils::hex};
use regex::Regex;
use std::{collections::HashMap, str::FromStr, sync::Arc};
use std::convert::TryInto;
use num_bigint::BigUint;
use crate::common::{indexer_error::indexer_error, database::PgPool};
use native::signature_verification::SignatureVerifier;
use diesel::pg::PgConnection;
use secp256k1::{recovery::{RecoverableSignature, RecoveryId}, Message, PublicKey, Secp256k1, VerifyOnly};
use super::ReceiptManager;

type QueryFees = HashMap<String, HashMap<String, BigDecimal>>;

// Takes a valid big-endian hexadecimal string and parses it as a U256
fn read_number(data: &str, start: usize, end: usize) -> BigDecimal {
    let hex_string = format!("0x{}", &data[start..end]);
    BigDecimal::from_str(&hex_string).unwrap()
}

static ALLOCATION_RECEIPT_VALIDATOR: &'static str = "^[0-9A-Fa-f]{264}$";

async fn validate_signature(
    signer: &SignatureVerifier,
    receipt_data: &str,
) -> Result<String, IndexerError> {
    let message = &receipt_data[0..134].as_bytes();
    //TODO: recover signature properly
    let signature = RecoverableSignature::from_compact(&hex::decode(&receipt_data[134..264]).unwrap(), RecoveryId::from_i32(0).unwrap()).unwrap();

    if signer.verify(message, &signature).is_err() {
        let code = IndexerErrorCode::IE031;
        
        return Err(indexer_error(
            IndexerErrorCode::IE031,
        ));
    }

    Ok(format!("0x{}", &receipt_data[134..264]))
}

// #[derive(SimpleObject)]
struct AllocationReceipt {
    id: String,
    allocation: Address,
    fees: BigDecimal,
    signature: String,
}

pub struct AllocationReceiptManager {
    sequelize: PgPool,
    // query_fee_models: QueryFeeModels,
    cache: HashMap<String, Arc<AllocationReceipt>>,
    flush_queue: Vec<String>,
    allocation_receipt_verifier: SignatureVerifier,
}

#[async_trait]
impl ReceiptManager for AllocationReceiptManager {
    async fn add(
        &mut self,
        receipt_data: String,
    ) -> Result<(String, Address, BigDecimal), IndexerError> {
        let allocation_receipt_validator = Regex::new("^[0-9A-Fa-f]{264}$").unwrap();
        // Security: Input validation
        if !allocation_receipt_validator.is_match(&receipt_data) {
            return Err(indexer_error(
                IndexerErrorCode::IE031,
            ))
        }

        // TODO: (Security) Additional validations are required to remove trust from
        // the Gateway which are deferred until we can fully remove trust which requires:
        //   * A receiptID based routing solution so that some invariants can be tested
        //     in memory instead of hitting the database for performance (eg: collateral,
        //     and that fees are increasing).
        //   * A ZKP to ensure all receipts can be collected without running out of gas.
        //
        // Validations include:
        //   * The address corresponds to an *unresolved* transfer.
        //   * The unresolved transfer has sufficient collateral to pay for the query.
        //   * Recovering the signature for the binary data in chars 20..56 = the specified address.
        //   * The increase in fee amount from the last known valid state covers the cost of the query
        //   * This receipt ID is not being "forked" by concurrent usage.

        let receipt = self.parse_allocation_receipt(&receipt_data)?;
        let signature =
            validate_signature(&self.allocation_receipt_verifier, &receipt_data).await?;

        self.queue(AllocationReceipt {
            id: receipt.0.clone(),
            allocation: receipt.1.clone(),
            fees: receipt.2.clone(),
            signature,
        });

        Ok(receipt)
    }
}

impl AllocationReceiptManager {
    pub fn new(
        sequelize: PgPool,
        // query_fee_models: QueryFeeModels,
        // logger: Logger,
        client_signer_address: Address,
    ) -> Self {
        let addr = client_signer_address.as_bytes();
        let fixed_bytes: [u8; 20] = {
            let mut array = [0u8; 20]; // create a mutable byte array of size 20
            let len = addr.len().min(20); // get the length of the byte array, capped at 20
            array[..len].copy_from_slice(&addr[..len]); // copy bytes from byte array to fixed byte array
            array
        };

        Self {
            sequelize,
            // query_fee_models,
            cache: HashMap::new(),
            flush_queue: Vec::new(),
            allocation_receipt_verifier: SignatureVerifier::new(fixed_bytes),
        }
    }

    fn parse_allocation_receipt(&self, receipt_data: &str) -> Result<(String, Address, BigDecimal), IndexerError> {
        // let id = &receipt_data[104..134].as_bytes(); // 15 bytes
        let id = receipt_data[104..134].to_owned(); // 15 bytes
        let allocation = to_address(&("0x".to_owned() + &receipt_data[0..40])); // 20 bytes
        let fees = read_number(&receipt_data, 40, 104);
        Ok((
            id, allocation, fees
        ))
    }

    /// Flushes all receipts that have been registered by this moment in time
    async fn flush_outstanding(&mut self) -> Result<(), IndexerError> {
        let mut count = self.flush_queue.len();

        while (count > 0) {
            count -= 1;

            // Swap and pop

            // delete from cache

            // Put into a atomic transaction for write that dependend on a read

            // Ensure allocation summary and save new receipts to db

            // Retain receipt in queue for next flush if failed
        }

        Ok(())
    }


    fn queue(&mut self, receipt: AllocationReceipt) {
        // Collision resistent since receipts have globally unique ID
        let latest = self.cache.get(&receipt.id);
        if latest.is_none() || latest.unwrap().fees.lt(&receipt.fees) {
            if latest.is_none() {
                self.flush_queue.push(receipt.id.clone());
            }
            self.cache.insert(receipt.id.clone(), Arc::new(receipt));
        }        
    }
}

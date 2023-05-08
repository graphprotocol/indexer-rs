// use std::collections::HashMap;
// use web3::types::{H256, Address, U256};
// use num::BigInt;
// use binary_heap::BinaryHeap;

// // struct AllocationReceiptsBatch { /* ... */ }

// // type DHeap = BinaryHeap<AllocationReceiptsBatch>;
// struct Allocation {
//     pub id: Address
// }

// pub trait ReceiptCollector {
//     fn rememberAllocations(actionID: number, allocationIDs: Vec<Address>) -> impl Future<bool, reqwest::Error>;
//     fn collectReceipts(actionID: number, allocation: Allocation) -> impl Future<bool, reqwest::Error>;
// }

// struct AllocationReceiptCollector {
//     // logger: Logger,
//     // models: QueryFeeModels,
//     transactionManager: TransactionManager,
//     allocationExchange: Contract,
//     collectEndpoint: URL,
//     partialVoucherEndpoint: URL,
//     voucherEndpoint: URL,
//     receiptsToCollect: DHeap<AllocationReceiptsBatch>,
//     voucherRedemptionThreshold: BigInt,
//     voucherRedemptionBatchThreshold: BigInt,
//     voucherRedemptionMaxBatchSize: u64,
// }

// impl Shape for Square {
//     fn area(&self) -> f64 {
//         self.side * self.side
//     }
// }



// // async rememberAllocations(
// //     actionID: number,
// //     allocationIDs: Address[],
// //   ): Promise<boolean> {
// //     const logger = this.logger.child({
// //       action: actionID,
// //       allocations: allocationIDs,
// //     })

// //     try {
// //       logger.info('Remember allocations for collecting receipts later')

// //       // eslint-disable-next-line @typescript-eslint/no-non-null-assertion
// //       await this.models.allocationSummaries.sequelize!.transaction(
// //         async (transaction) => {
// //           for (const allocation of allocationIDs) {
// //             const [summary] = await ensureAllocationSummary(
// //               this.models,
// //               allocation,
// //               transaction,
// //             )
// //             await summary.save()
// //           }
// //         },
// //       )
// //       return true
// //     } catch (err) {
// //       logger.error(`Failed to remember allocations for collecting receipts later`, {
// //         err: indexerError(IndexerErrorCode.IE056, err),
// //       })
// //       return false
// //     }
// //   }

// //   async collectReceipts(actionID: number, allocation: Allocation): Promise<boolean> {
// //     const logger = this.logger.child({
// //       action: actionID,
// //       allocation: allocation.id,
// //       deployment: allocation.subgraphDeployment.id.display,
// //     })

// //     try {
// //       logger.debug(`Queue allocation receipts for collecting`)

// //       const now = new Date()

// //       const receipts =
// //         // eslint-disable-next-line @typescript-eslint/no-non-null-assertion
// //         await this.models.allocationReceipts.sequelize!.transaction(
// //           async (transaction) => {
// //             // Update the allocation summary
// //             await this.models.allocationSummaries.update(
// //               { closedAt: now },
// //               {
// //                 where: { allocation: allocation.id },
// //                 transaction,
// //               },
// //             )

// //             // Return all receipts for the just-closed allocation
// //             return this.models.allocationReceipts.findAll({
// //               where: { allocation: allocation.id },
// //               order: ['id'],
// //               transaction,
// //             })
// //           },
// //         )

// //       if (receipts.length <= 0) {
// //         logger.debug(`No receipts to collect for allocation`)
// //         return false
// //       }

// //       const timeout = now.valueOf() + RECEIPT_COLLECT_DELAY

// //       // Collect the receipts for this allocation in a bit
// //       this.receiptsToCollect.push({
// //         receipts,
// //         timeout,
// //       })
// //       logger.info(`Successfully queued allocation receipts for collecting`, {
// //         receipts: receipts.length,
// //         timeout: new Date(timeout).toLocaleString(),
// //       })
// //       return true
// //     } catch (err) {
// //       const error = indexerError(IndexerErrorCode.IE053, err)
// //       this.logger.error(`Failed to queue allocation receipts for collecting`, {
// //         error,
// //       })
// //       throw error
// //     }
// //   }

// impl ReceiptCollector {
//     pub fn new() -> Self {
//         Self {
//             receipts: HashMap::new(),
//             block_numbers: HashMap::new(),
//         }
//     }

//     pub fn add(&mut self, tx_hash: H256, receipt: u64, block_number: u64) {
//         self.receipts.insert(tx_hash, receipt);
//         self.block_numbers.insert(tx_hash, block_number);
//     }

//     pub fn get(&self, tx_hash: &H256) -> Option<(u64, u64)> {
//         match (self.receipts.get(tx_hash), self.block_numbers.get(tx_hash)) {
//             (Some(receipt), Some(block_number)) => Some((*receipt, *block_number)),
//             _ => None,
//         }
//     }
// }

// // pub struct ReceiptCollector {
// //     pub receipts: HashMap<H256, u64>,
// //     pub block_numbers: HashMap<H256, u64>,
// // }

// // impl ReceiptCollector {
// //     pub fn new() -> Self {
// //         Self {
// //             receipts: HashMap::new(),
// //             block_numbers: HashMap::new(),
// //         }
// //     }

// //     pub fn add(&mut self, tx_hash: H256, receipt: u64, block_number: u64) {
// //         self.receipts.insert(tx_hash, receipt);
// //         self.block_numbers.insert(tx_hash, block_number);
// //     }

// //     pub fn get(&self, tx_hash: &H256) -> Option<(u64, u64)> {
// //         match (self.receipts.get(tx_hash), self.block_numbers.get(tx_hash)) {
// //             (Some(receipt), Some(block_number)) => Some((*receipt, *block_number)),
// //             _ => None,
// //         }
// //     }
// // }



// pub struct QueryFees {
//     pub receipts: HashMap<H256, u64>,
//     pub block_numbers: HashMap<H256, u64>,
//     pub fee_recipients: HashMap<Address, U256>,
//     pub total_fees: U256,
//     pub block_number: u64,
// }

// impl QueryFees {
//     pub fn new(block_number: u64) -> Self {
//         Self {
//             receipts: HashMap::new(),
//             block_numbers: HashMap::new(),
//             fee_recipients: HashMap::new(),
//             total_fees: U256::zero(),
//             block_number,
//         }
//     }

//     pub fn add_receipt(&mut self, tx_hash: H256, receipt: u64, block_number: u64) {
//         self.receipts.insert(tx_hash, receipt);
//         self.block_numbers.insert(tx_hash, block_number);
//     }

//     pub fn add_fee_recipient(&mut self, fee_recipient: Address, fee: U256) {
//         let prev = self.fee_recipients.get(&fee_recipient).unwrap_or(&U256::zero());
//         self.fee_recipients.insert(fee_recipient, prev + fee);
//         self.total_fees += fee;
//     }

//     pub fn get_receipt(&self, tx_hash: &H256) -> Option<(u64, u64)> {
//         match (self.receipts.get(tx_hash), self.block_numbers.get(tx_hash)) {
//             (Some(receipt), Some(block_number)) => Some((*receipt, *block_number)),
//             _ => None,
//         }
//     }

//     pub fn get_fee_recipient(&self, fee_recipient: &Address) -> Option<U256> {
//         self.fee_recipients.get(fee_recipient).cloned()
//     }
// }

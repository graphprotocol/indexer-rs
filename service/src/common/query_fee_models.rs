// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
// use diesel::associations::{HasMany, HasOne, HasTable};
// use diesel::pg::Pg;
use diesel::dsl;
// use diesel::prelude::{
//     ExpressionMethods, JoinOnDsl, NullableExpressionMethods, OptionalExtension, PgConnection,
//     QueryDsl, RunQueryDsl,
// };
use diesel::prelude::*;
use diesel_derives::{Associations, FromSqlRow};

use diesel::{Identifiable, Queryable};
use serde::{Deserialize, Serialize};

use super::address::Address;
// use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, Queryable, Identifiable, FromSqlRow)]
#[diesel(table_name = schema::allocation_receipts)]
// #[diesel(table_name = crate::schema::posts)]
pub struct AllocationReceipt {
    pub id: String,
    pub allocation: String,
    pub fees: String,
    pub signature: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// #[derive(Clone, Debug, Deserialize, Serialize, Queryable, Identifiable, Associations)]
// #[table_name = "vouchers"]
// #[belongs_to(AllocationSummary)]
// pub struct Voucher {
//     pub id: String,
//     pub allocation: Address,
//     pub amount: String,
//     pub signature: String,
//     pub created_at: DateTime<Utc>,
//     pub updated_at: DateTime<Utc>,
//     pub allocation_summary_id: Option<String>,
// }


// #[derive(Clone, Debug, Deserialize, Serialize, Queryable, Identifiable, Associations)]
// #[belongs_to(Transfer)] // Association macro
// #[table_name = "transfer_receipts"]
// pub struct TransferReceipt {
//     pub id: i32,
//     pub signer: String,
//     pub fees: String,
//     pub signature: String,
//     pub created_at: DateTime<Utc>,
//     pub updated_at: DateTime<Utc>,
//     pub transfer_id: Option<String>,
// }


// #[derive(Clone, Debug, Deserialize, Serialize, Queryable, Identifiable, Associations)]
// #[belongs_to(AllocationSummary)]
// #[table_name = "transfers"]
// pub struct Transfer {
//     pub id: String,
//     pub routing_id: String,
//     pub allocation: Address,
//     pub signer: Address,
//     pub allocation_closed_at: Option<DateTime<Utc>>,
//     pub status: String,    // Make transferStatus enum
//     pub created_at: DateTime<Utc>,
//     pub updated_at: DateTime<Utc>,
//     // Link to a list of receipts (Vec<TransferReceipt>)
//     pub allocation_summary_id: Option<String>,
// }


// #[derive(Clone, Debug, Deserialize, Serialize, Queryable, Identifiable)]
// #[table_name = "allocation_summaries"]
// pub struct AllocationSummary {
//     pub id: String,
//     pub allocation: Address,
//     pub closed_at: Option<DateTime<Utc>>,
//     pub created_transfers: i32,
//     pub resolved_transfers: i32,
//     pub failed_transfers: i32,
//     pub open_transfers: i32,
//     pub collected_fees: String,
//     pub withdrawn_fees: String,
//     pub created_at: DateTime<Utc>,
//     pub updated_at: DateTime<Utc>,
//     // TODO: Add association
//     // transfers: Vec<Transfer>
//     // allocationReceipts: Vec<AllocationReceipt>
//     // voucher: Voucher
// }

// pub struct QueryFeeModels {
//     pub allocation_receipts: AllocationReceipt,
//     pub vouchers: Voucher,
//     pub transfer_receipts: TransferReceipt,
//     pub transfers: Transfer,
//     pub allocation_summaries: AllocationSummary,
// }


// fn define_query_fee_models(connection: &PgConnection) -> QueryFeeModels {
//     AllocationReceipt::table
//         .create_if_not_exists()
//         .execute(connection)
//         .expect("Error creating allocation_receipts table");

//     Voucher::table
//         .create_if_not_exists()
//         .execute(connection)
//         .expect("Error creating vouchers table");

//     TransferReceipt::table
//         .create_if_not_exists()
//         .execute(connection)
//         .expect("Error creating transfer_receipts table");

//     Transfer::table
//         .create_if_not_exists()
//         .execute(connection)
//         .expect("Error creating transfers table");

//     AllocationSummary::table
//         .create_if_not_exists()
//         .execute(connection)
//         .expect("Error creating allocation_summaries table");

//     Transfer::table
//         .has_many(TransferReceipt::table)
//         .on_column("signer")
//         .named("receipts")
//         .create(connection)
//         .expect("Error creating transfers to transfer_receipts association");

//     TransferReceipt::table
//         .belongs_to(Transfer::table)
//         .on_column("signer")
//         .named("transfer")
//         .create(connection)
//         .expect("Error creating transfer_receipts to transfers association");

//     AllocationSummary::table
//         .has_many(Transfer::table)
//         .on_column("allocation")
//         .named("transfers")
//         .create(connection)
//         .expect("Error creating allocation_summaries to transfers association");

//     AllocationSummary::table
//         .has_many(AllocationReceipt::table)
//         .on_column("allocation")
//         .named("allocation_receipts")
//         .create(connection)
//         .expect("Error creating allocation_summaries to allocation_receipts association");

//     AllocationSummary::table
//         .has_one(Voucher::table)
//         .on_column("allocation")
//         .named("voucher")
//         .create(connection)
//         .expect("Error creating allocation_summaries to vouchers association");

//     Transfer::table
//         .belongs_to(AllocationSummary::table)
//         .on_column("allocation")
//         .named("allocation_summary")
//         .create(connection)
//         .expect("Error creating transfers to allocation_summaries association");

//     AllocationReceipt::table
//         .belongs_to(AllocationSummary::table)
//         .on_column("allocation")
//         .named("allocation_summary")
//         .create(connection)
//         .expect("Error creating allocation_receipts to allocation_summaries association");

//     Voucher::table
//         .belongs_to(AllocationSummary::table)
//         .on_column("allocation")
//         .named("allocation_summary")
//         .create(connection)
//         .expect("Error creating vouchers to allocation_summaries association");

//     QueryFeeModels {
//         allocation_receipts: AllocationReceipt::table.into(),
//         vouchers: Voucher::table.into(),
//         transfer_receipts: TransferReceipt::table.into(),
//         transfers: Transfer::table.into(),
//         allocation_summaries: AllocationSummary::table.into(),
//     }
// }

// // import { DataTypes, Sequelize, Model, Association } from 'sequelize'
// // import { Address } from '@graphprotocol/common-ts'

// // export interface AllocationReceiptAttributes {
// //   id: string
// //   allocation: Address
// //   fees: string
// //   signature: string
// // }

// // export class AllocationReceipt
// //   extends Model<AllocationReceiptAttributes>
// //   implements AllocationReceiptAttributes
// // {
// //   public id!: string
// //   public allocation!: Address
// //   public fees!: string
// //   public signature!: string

// //   public readonly createdAt!: Date
// //   public readonly updatedAt!: Date
// // }

// // export interface VoucherAttributes {
// //   allocation: Address
// //   amount: string
// //   signature: string
// // }

// // export class Voucher extends Model<VoucherAttributes> implements VoucherAttributes {
// //   public allocation!: Address
// //   public amount!: string
// //   public signature!: string

// //   public readonly createdAt!: Date
// //   public readonly updatedAt!: Date

// //   public readonly allocationSummary?: AllocationSummary

// //   public static associations: {
// //     allocationSummary: Association<Voucher, AllocationSummary>
// //   }
// // }

// // export interface TransferReceiptAttributes {
// //   id: number
// //   signer: Address
// //   fees: string
// //   signature: string
// // }

// // export class TransferReceipt
// //   extends Model<TransferReceiptAttributes>
// //   implements TransferReceiptAttributes
// // {
// //   public id!: number
// //   public signer!: Address
// //   public fees!: string
// //   public signature!: string

// //   public readonly createdAt!: Date
// //   public readonly updatedAt!: Date

// //   public readonly transfer?: Transfer

// //   public static associations: {
// //     transfer: Association<TransferReceipt, Transfer>
// //   }
// // }

// // export enum TransferStatus {
// //   OPEN = 'OPEN',
// //   ALLOCATION_CLOSED = 'ALLOCATION_CLOSED',
// //   RESOLVED = 'RESOLVED',
// //   FAILED = 'FAILED',
// // }

// // export interface TransferAttributes {
// //   routingId: string
// //   allocation: Address
// //   signer: Address
// //   allocationClosedAt: Date | null
// //   status: TransferStatus
// // }

// // export class Transfer extends Model<TransferAttributes> implements TransferAttributes {
// //   public routingId!: string
// //   public allocation!: Address
// //   public signer!: Address
// //   public allocationClosedAt!: Date | null
// //   public status!: TransferStatus

// //   public readonly createdAt!: Date
// //   public readonly updatedAt!: Date

// //   public readonly receipts?: TransferReceipt[]

// //   public static associations: {
// //     receipts: Association<Transfer, TransferReceipt>
// //   }
// // }

// // export interface AllocationSummaryAttributes {
// //   allocation: Address
// //   closedAt: Date | null
// //   createdTransfers: number
// //   resolvedTransfers: number
// //   failedTransfers: number
// //   openTransfers: number
// //   collectedFees: string
// //   withdrawnFees: string
// // }

// // export class AllocationSummary
// //   extends Model<AllocationSummaryAttributes>
// //   implements AllocationSummaryAttributes
// // {
// //   public allocation!: Address
// //   public closedAt!: Date
// //   public createdTransfers!: number
// //   public resolvedTransfers!: number
// //   public failedTransfers!: number
// //   public openTransfers!: number
// //   public collectedFees!: string
// //   public withdrawnFees!: string

// //   public readonly createdAt!: Date
// //   public readonly updatedAt!: Date

// //   public readonly transfers?: Transfer[]
// //   public readonly allocationReceipts?: AllocationReceipt[]
// //   public readonly voucher?: Voucher

// //   public static associations: {
// //     transfers: Association<AllocationSummary, Transfer>
// //     allocationReceipts: Association<AllocationSummary, AllocationReceipt>
// //     voucher: Association<AllocationSummary, Voucher>
// //   }
// // }

// // export interface QueryFeeModels {
// //   allocationReceipts: typeof AllocationReceipt
// //   vouchers: typeof Voucher
// //   transferReceipts: typeof TransferReceipt
// //   transfers: typeof Transfer
// //   allocationSummaries: typeof AllocationSummary
// // }

// // export function defineQueryFeeModels(sequelize: Sequelize): QueryFeeModels {
// //   AllocationReceipt.init(
// //     {
// //       // TODO: To distinguish between (id, allocation) pairs from different
// //       // clients, the primary key should really be (id, allocation,
// //       // clientAddress)
// //       id: {
// //         type: DataTypes.STRING(66),
// //         allowNull: false,
// //         primaryKey: true,
// //       },
// //       allocation: {
// //         type: DataTypes.STRING(42),
// //         allowNull: false,
// //         primaryKey: true,
// //       },

// //       signature: {
// //         type: DataTypes.STRING(132),
// //         allowNull: false,
// //       },
// //       fees: {
// //         type: DataTypes.DECIMAL,
// //         allowNull: false,
// //         validate: {
// //           min: 0.0,
// //         },
// //       },
// //     },
// //     { sequelize, tableName: 'allocation_receipts' },
// //   )

// //   Voucher.init(
// //     {
// //       allocation: {
// //         type: DataTypes.STRING(42),
// //         allowNull: false,
// //         primaryKey: true,
// //       },
// //       amount: {
// //         type: DataTypes.DECIMAL,
// //         allowNull: false,
// //         validate: {
// //           min: 0.0,
// //         },
// //       },
// //       signature: {
// //         type: DataTypes.STRING,
// //         allowNull: false,
// //       },
// //     },
// //     { sequelize, tableName: 'vouchers' },
// //   )

// //   TransferReceipt.init(
// //     {
// //       id: {
// //         type: DataTypes.INTEGER,
// //         allowNull: false,
// //         primaryKey: true,
// //       },
// //       signer: {
// //         type: DataTypes.STRING(42),
// //         allowNull: false,
// //         primaryKey: true,
// //       },
// //       signature: {
// //         type: DataTypes.STRING(132),
// //         allowNull: false,
// //       },
// //       fees: {
// //         type: DataTypes.DECIMAL,
// //         allowNull: false,
// //         validate: {
// //           min: 0.0,
// //         },
// //       },
// //     },
// //     { sequelize, tableName: 'transfer_receipts' },
// //   )

// //   Transfer.init(
// //     {
// //       signer: {
// //         type: DataTypes.STRING(42),
// //         allowNull: false,
// //         primaryKey: true,
// //       },
// //       allocation: {
// //         type: DataTypes.STRING,
// //         allowNull: false,
// //       },
// //       routingId: {
// //         type: DataTypes.STRING(66),
// //         allowNull: false,
// //         primaryKey: true,
// //       },
// //       allocationClosedAt: {
// //         type: DataTypes.DATE,
// //         allowNull: true,
// //       },
// //       status: {
// //         type: DataTypes.ENUM(
// //           TransferStatus.OPEN,
// //           TransferStatus.ALLOCATION_CLOSED,
// //           TransferStatus.RESOLVED,
// //           TransferStatus.FAILED,
// //         ),
// //         allowNull: false,
// //       },
// //     },
// //     { sequelize, tableName: 'transfers' },
// //   )

// //   AllocationSummary.init(
// //     {
// //       allocation: {
// //         type: DataTypes.STRING(42),
// //         allowNull: false,
// //         primaryKey: true,
// //       },
// //       closedAt: {
// //         type: DataTypes.DATE,
// //         allowNull: true,
// //       },
// //       createdTransfers: {
// //         type: DataTypes.INTEGER,
// //         allowNull: false,
// //       },
// //       resolvedTransfers: {
// //         type: DataTypes.INTEGER,
// //         allowNull: false,
// //       },
// //       failedTransfers: {
// //         type: DataTypes.INTEGER,
// //         allowNull: false,
// //       },
// //       openTransfers: {
// //         type: DataTypes.INTEGER,
// //         allowNull: false,
// //       },
// //       collectedFees: {
// //         type: DataTypes.DECIMAL,
// //         allowNull: false,
// //       },
// //       withdrawnFees: {
// //         type: DataTypes.DECIMAL,
// //         allowNull: false,
// //       },
// //     },
// //     { sequelize, tableName: 'allocation_summaries' },
// //   )

// //   Transfer.hasMany(TransferReceipt, {
// //     sourceKey: 'signer',
// //     foreignKey: 'signer',
// //     as: 'receipts',
// //   })

// //   TransferReceipt.belongsTo(Transfer, {
// //     targetKey: 'signer',
// //     foreignKey: 'signer',
// //     as: 'transfer',
// //   })

// //   AllocationSummary.hasMany(Transfer, {
// //     sourceKey: 'allocation',
// //     foreignKey: 'allocation',
// //     as: 'transfers',
// //   })

// //   AllocationSummary.hasMany(AllocationReceipt, {
// //     sourceKey: 'allocation',
// //     foreignKey: 'allocation',
// //     as: 'allocationReceipts',
// //   })

// //   AllocationSummary.hasOne(Voucher, {
// //     sourceKey: 'allocation',
// //     foreignKey: 'allocation',
// //     as: 'voucher',
// //   })

// //   Transfer.belongsTo(AllocationSummary, {
// //     targetKey: 'allocation',
// //     foreignKey: 'allocation',
// //     as: 'allocationSummary',
// //   })

// //   AllocationReceipt.belongsTo(AllocationSummary, {
// //     targetKey: 'allocation',
// //     foreignKey: 'allocation',
// //     as: 'allocationSummary',
// //   })

// //   Voucher.belongsTo(AllocationSummary, {
// //     targetKey: 'allocation',
// //     foreignKey: 'allocation',
// //     as: 'allocationSummary',
// //   })

// //   return {
// //     allocationReceipts: AllocationReceipt,
// //     vouchers: Voucher,
// //     transferReceipts: TransferReceipt,
// //     transfers: Transfer,
// //     allocationSummaries: AllocationSummary,
// //   }
// // }







// // -----------------------------------------------------------------
// // -----------------------------------------------------------------
// // -----------------------------------------------------------------
// // -----------------------------------------------------------------




// // use sqlx::{postgres::PgPoolOptions, Acquire, Error, PgPool, Pool};
// // use chrono::{NaiveTime, Utc};
// // use std::str::FromStr;
// // use bigdecimal::BigDecimal;
// // use serde::{Deserialize, Serialize};
// // use crate::common::address::Address;

// // use super::super::util;

// // #[derive(Debug, Clone, Deserialize, Serialize, sqlx::FromRow)]
// // pub struct AllocationReceipt {
// //     pub id: String,
// //     pub allocation: Address,
// //     pub fees: String,
// //     pub signature: String,
// //     // pub created_at: NaiveTime,
// //     // pub updated_at: NaiveTime,
// // }

// // #[derive(Debug, Clone, Deserialize, Serialize, sqlx::FromRow)]
// // pub struct Voucher {
// //     pub allocation: Address,
// //     pub amount: BigDecimal,
// //     pub signature: String,
// //     // pub created_at: NaiveTime,
// //     // pub updated_at: NaiveTime,
// // }

// // #[derive(Debug, Clone, Deserialize, Serialize, sqlx::FromRow)]
// // pub struct TransferReceipt {
// //     pub id: i32,
// //     pub signer: Address,
// //     pub fees: String,
// //     pub signature: String,
// //     // pub created_at: NaiveTime,
// //     // pub updated_at: NaiveTime,
// //     pub transfer: Option<Transfer>,
// //     // #[belongs_to(Transfer, foreign_key = "signer")]
// //     // pub transfer: Option<Transfer>,
    
// // }

// // #[derive(Debug, Clone, Copy, PartialEq, Eq)]
// // enum TransferStatus {
// //     OPEN,
// //     CLOSED, // allocation closed
// //     RESOLVED,
// //     FAILED,
// // }

// // #[derive(Debug, Clone, Deserialize, Serialize, sqlx::FromRow)]
// // pub struct Transfer {
// //     pub routing_id: String,
// //     pub allocation: Address,
// //     pub signer: Address,
// //     pub allocation_closed_at: Option<NaiveTime>,
// //     pub status: TransferStatus,
// //     // pub created_at: NaiveTime,
// //     // pub updated_at: NaiveTime,
// // }

// // impl Transfer {
// //     pub async fn receipts(&self, pool: &sqlx::PgPool) -> Result<Vec<TransferReceipt>, sqlx::Error> {
// //         sqlx::query_as!(
// //             TransferReceipt,
// //             "SELECT * FROM transfer_receipts WHERE signer = $1 ORDER BY id ASC",
// //             self.signer,
// //         )
// //         .fetch_all(pool)
// //         .await?
// //     }
// // }

// // #[derive(Debug, Clone, Deserialize, Serialize, sqlx::FromRow)]
// // pub struct AllocationSummary {
// //     pub allocation: Address,
// //     pub closed_at: Option<NaiveTime>,
// //     pub created_transfers: i32,
// //     pub resolved_transfers: i32,
// //     pub failed_transfers: i32,
// //     pub open_transfers: i32,
// //     pub collected_fees: String,
// //     pub withdrawn_fees: String,
// //     // pub created_at: NaiveTime,
// //     // pub updated_at: NaiveTime,

// //     // Associations
// //     pub transfers: Vec<Transfer>,
// //     pub allocation_receipts: Vec<AllocationReceipt>,
// //     pub voucher: Option<Voucher>,
// // }


// // impl AllocationSummary {
// //   pub async fn transfers(&self, db_pool: &SqlitePool) -> sqlx::Result<Vec<Transfer>> {
// //       Transfer::belonging_to(self)
// //           .load(db_pool)
// //           .await
// //   }

// //   pub async fn allocation_receipts(&self, db_pool: &SqlitePool) -> sqlx::Result<Vec<AllocationReceipt>> {
// //       AllocationReceipt::belonging_to(self)
// //           .load(db_pool)
// //           .await
// //   }

// //   pub async fn voucher(&self, db_pool: &SqlitePool) -> sqlx::Result<Option<Voucher>> {
// //       Voucher::belonging_to(self)
// //           .first(db_pool)
// //           .await
// //   }
  
// //   pub async fn find_by_allocation(
// //       pool: &PgPool,
// //       allocation: &str,
// //   ) -> Result<Option<Self>, sqlx::Error> {
// //       let allocation_summary = sqlx::query_as::<_, Self>("
// //           SELECT * FROM allocation_summaries
// //           WHERE allocation = $1
// //       ")
// //       .bind(allocation)
// //       .fetch_optional(pool)
// //       .await?;

// //       if let Some(allocation_summary) = allocation_summary.as_ref() {
// //           let transfers = sqlx::query_as::<_, Transfer>("
// //               SELECT * FROM transfers
// //               WHERE allocation = $1
// //           ")
// //           .bind(&allocation_summary.allocation)
// //           .fetch_all(pool)
// //           .await?;

// //           let allocation_receipts = sqlx::query_as::<_, AllocationReceipt>("
// //               SELECT * FROM allocation_receipts
// //               WHERE allocation = $1
// //           ")
// //           .bind(&allocation_summary.allocation)
// //           .fetch_all(pool)
// //           .await?;

// //           let voucher = sqlx::query_as::<_, Option<Voucher>>("
// //               SELECT * FROM vouchers
// //               WHERE allocation = $1
// //           ")
// //           .bind(&allocation_summary.allocation)
// //           .fetch_optional(pool)
// //           .await?;

// //           Ok(Some(Self {
// //               transfers,
// //               allocation_receipts,
// //               voucher,
// //               ..allocation_summary.clone()
// //           }))
// //       } else {
// //           Ok(None)
// //       }
// //   }
// // }



// // #[derive(Debug, Clone, Deserialize, Serialize)]
// // pub struct QueryFeeModels {
// //     pub allocation_receipts: Vec<AllocationReceipt>,
// //     pub vouchers: Vec<Voucher>,
// //     pub transfer_receipts: Vec<TransferReceipt>,
// //     pub transfers: Vec<Transfer>,
// //     pub allocation_summaries: Vec<AllocationSummary>,
// // }


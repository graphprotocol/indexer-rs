use anyhow::Result;
use bigdecimal::BigDecimal;
use sqlx::{PgPool, Row};
use std::str::FromStr;

/// Unified database checker for both V1 and V2 TAP tables
pub struct DatabaseChecker {
    pool: PgPool,
}

/// TAP version enum to specify which tables to query
#[derive(Debug, Clone, Copy)]
pub enum TapVersion {
    V1, // Legacy receipt aggregator tables
    V2, // Horizon tables
}

/// Unified TAP state that works for both V1 and V2
#[derive(Debug, Clone)]
pub struct TapState {
    pub receipt_count: i64,
    pub receipt_value: BigDecimal,
    pub rav_count: i64,
    pub rav_value: BigDecimal,
    pub pending_rav_count: i64,
    pub failed_rav_count: i64,
    pub invalid_receipt_count: i64,
}

/// Combined state for both versions
#[derive(Debug, Clone)]
pub struct CombinedTapState {
    pub v1: TapState,
    pub v2: TapState,
}

/// Detailed state with breakdowns (V2 focused, but could be extended for V1)
#[derive(Debug, Clone)]
pub struct DetailedTapState {
    pub receipts_by_collection: Vec<ReceiptSummary>,
    pub ravs_by_collection: Vec<RavSummary>,
    pub pending_ravs: Vec<PendingRav>,
    pub recent_receipts: Vec<RecentReceipt>,
}

#[derive(Debug, Clone)]
pub struct ReceiptSummary {
    pub identifier: String, // collection_id for V2, allocation_id for V1
    pub payer: String,
    pub service_provider: String,
    pub data_service: String,
    pub count: i64,
    pub total_value: BigDecimal,
    pub oldest_timestamp: BigDecimal,
    pub newest_timestamp: BigDecimal,
}

#[derive(Debug, Clone)]
pub struct RavSummary {
    pub identifier: String, // collection_id for V2, allocation_id for V1
    pub payer: String,
    pub service_provider: String,
    pub data_service: String,
    pub value_aggregate: BigDecimal,
    pub timestamp_ns: BigDecimal,
    pub is_final: bool,
    pub is_last: bool,
}

#[derive(Debug, Clone)]
pub struct PendingRav {
    pub identifier: String, // collection_id for V2, allocation_id for V1
    pub payer: String,
    pub service_provider: String,
    pub data_service: String,
    pub pending_receipt_count: i64,
    pub pending_value: BigDecimal,
}

#[derive(Debug, Clone)]
pub struct RecentReceipt {
    pub id: i64,
    pub identifier: String, // collection_id for V2, allocation_id for V1
    pub payer: String,
    pub value: BigDecimal,
    pub timestamp_ns: BigDecimal,
}

impl DatabaseChecker {
    /// Create new DatabaseChecker with database connection
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url).await?;
        Ok(Self { pool })
    }

    /// Get combined V1 and V2 state for comprehensive testing
    pub async fn get_combined_state(&self, payer: &str) -> Result<CombinedTapState> {
        let v1 = self.get_state(payer, TapVersion::V1).await?;
        let v2 = self.get_state(payer, TapVersion::V2).await?;

        Ok(CombinedTapState { v1, v2 })
    }

    /// Get TAP state for specified version
    pub async fn get_state(&self, payer: &str, version: TapVersion) -> Result<TapState> {
        match version {
            TapVersion::V1 => self.get_v1_state(payer).await,
            TapVersion::V2 => self.get_v2_state(payer).await,
        }
    }

    /// Get V1 state (scalar TAP tables)
    async fn get_v1_state(&self, payer: &str) -> Result<TapState> {
        let normalized_payer = payer.trim_start_matches("0x").to_lowercase();

        // V1 tables: scalar_tap_receipts, scalar_tap_ravs
        let receipt_stats = sqlx::query(
            r#"
            SELECT 
                COUNT(*) as count,
                COALESCE(SUM(value), 0) as total_value
            FROM scalar_tap_receipts 
            WHERE LOWER(signer_address) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_optional(&self.pool)
        .await?;

        let (receipt_count, receipt_value) = if let Some(stats) = receipt_stats {
            (stats.get("count"), stats.get("total_value"))
        } else {
            (0i64, BigDecimal::from_str("0").unwrap())
        };

        let rav_stats = sqlx::query(
            r#"
            SELECT 
                COUNT(*) as count,
                COALESCE(SUM(value_aggregate), 0) as total_value
            FROM scalar_tap_ravs 
            WHERE LOWER(sender_address) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_optional(&self.pool)
        .await?;

        let (rav_count, rav_value) = if let Some(stats) = rav_stats {
            (stats.get("count"), stats.get("total_value"))
        } else {
            (0i64, BigDecimal::from_str("0").unwrap())
        };

        // V1 scalar tables do have failed/invalid tables
        let failed_rav_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) 
            FROM scalar_tap_rav_requests_failed 
            WHERE LOWER(sender_address) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(0);

        let invalid_receipt_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) 
            FROM scalar_tap_receipts_invalid 
            WHERE LOWER(signer_address) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(0);

        let pending_rav_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(DISTINCT r.allocation_id)
            FROM scalar_tap_receipts r
            LEFT JOIN scalar_tap_ravs rav ON (
                r.allocation_id = rav.allocation_id 
                AND LOWER(r.signer_address) = LOWER(rav.sender_address)
            )
            WHERE LOWER(r.signer_address) = $1 AND rav.allocation_id IS NULL
            "#,
        )
        .bind(&normalized_payer)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(0);

        Ok(TapState {
            receipt_count,
            receipt_value,
            rav_count,
            rav_value,
            pending_rav_count,
            failed_rav_count,
            invalid_receipt_count,
        })
    }

    /// Get V2 state (horizon tables)
    async fn get_v2_state(&self, payer: &str) -> Result<TapState> {
        let normalized_payer = payer.trim_start_matches("0x").to_lowercase();

        // V2 horizon tables
        let receipt_stats = sqlx::query(
            r#"
            SELECT 
                COUNT(*) as count,
                COALESCE(SUM(value), 0) as total_value
            FROM tap_horizon_receipts 
            WHERE LOWER(payer) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_one(&self.pool)
        .await?;

        let receipt_count: i64 = receipt_stats.get("count");
        let receipt_value: BigDecimal = receipt_stats.get("total_value");

        let rav_stats = sqlx::query(
            r#"
            SELECT 
                COUNT(*) as count,
                COALESCE(SUM(value_aggregate), 0) as total_value
            FROM tap_horizon_ravs 
            WHERE LOWER(payer) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_one(&self.pool)
        .await?;

        let rav_count: i64 = rav_stats.get("count");
        let rav_value: BigDecimal = rav_stats.get("total_value");

        let failed_rav_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) 
            FROM tap_horizon_rav_requests_failed 
            WHERE LOWER(payer) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_one(&self.pool)
        .await?;

        let invalid_receipt_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) 
            FROM tap_horizon_receipts_invalid 
            WHERE LOWER(payer) = $1
            "#,
        )
        .bind(&normalized_payer)
        .fetch_one(&self.pool)
        .await?;

        let pending_rav_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(DISTINCT r.collection_id)
            FROM tap_horizon_receipts r
            LEFT JOIN tap_horizon_ravs rav ON (
                r.collection_id = rav.collection_id 
                AND LOWER(r.payer) = LOWER(rav.payer)
                AND LOWER(r.service_provider) = LOWER(rav.service_provider)
                AND LOWER(r.data_service) = LOWER(rav.data_service)
            )
            WHERE LOWER(r.payer) = $1 AND rav.collection_id IS NULL
            "#,
        )
        .bind(&normalized_payer)
        .fetch_one(&self.pool)
        .await?;

        Ok(TapState {
            receipt_count,
            receipt_value,
            rav_count,
            rav_value,
            pending_rav_count,
            failed_rav_count,
            invalid_receipt_count,
        })
    }

    /// Get detailed state with breakdowns (V2 focused)
    pub async fn get_detailed_state(
        &self,
        payer: &str,
        version: TapVersion,
    ) -> Result<DetailedTapState> {
        match version {
            TapVersion::V2 => self.get_v2_detailed_state(payer).await,
            TapVersion::V1 => self.get_v1_detailed_state(payer).await,
        }
    }

    async fn get_v2_detailed_state(&self, payer: &str) -> Result<DetailedTapState> {
        let normalized_payer = payer.trim_start_matches("0x").to_lowercase();

        // Get receipts grouped by collection
        let receipt_rows = sqlx::query(
            r#"
            SELECT 
                collection_id,
                payer,
                service_provider,
                data_service,
                COUNT(*) as count,
                SUM(value) as total_value,
                MIN(timestamp_ns) as oldest_timestamp,
                MAX(timestamp_ns) as newest_timestamp
            FROM tap_horizon_receipts 
            WHERE LOWER(payer) = $1
            GROUP BY collection_id, payer, service_provider, data_service
            ORDER BY newest_timestamp DESC
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let receipts_by_collection = receipt_rows
            .into_iter()
            .map(|row| ReceiptSummary {
                identifier: row.get("collection_id"),
                payer: row.get("payer"),
                service_provider: row.get("service_provider"),
                data_service: row.get("data_service"),
                count: row.get("count"),
                total_value: row.get("total_value"),
                oldest_timestamp: row.get("oldest_timestamp"),
                newest_timestamp: row.get("newest_timestamp"),
            })
            .collect();

        // Get RAVs by collection
        let rav_rows = sqlx::query(
            r#"
            SELECT 
                collection_id,
                payer,
                service_provider,
                data_service,
                value_aggregate,
                timestamp_ns,
                final as is_final,
                last as is_last
            FROM tap_horizon_ravs 
            WHERE LOWER(payer) = $1
            ORDER BY timestamp_ns DESC
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let ravs_by_collection = rav_rows
            .into_iter()
            .map(|row| RavSummary {
                identifier: row.get("collection_id"),
                payer: row.get("payer"),
                service_provider: row.get("service_provider"),
                data_service: row.get("data_service"),
                value_aggregate: row.get("value_aggregate"),
                timestamp_ns: row.get("timestamp_ns"),
                is_final: row.get("is_final"),
                is_last: row.get("is_last"),
            })
            .collect();

        // Get pending RAVs
        let pending_rows = sqlx::query(
            r#"
            SELECT 
                r.collection_id,
                r.payer,
                r.service_provider,
                r.data_service,
                COUNT(r.id) as pending_receipt_count,
                SUM(r.value) as pending_value
            FROM tap_horizon_receipts r
            LEFT JOIN tap_horizon_ravs rav ON (
                r.collection_id = rav.collection_id 
                AND LOWER(r.payer) = LOWER(rav.payer)
                AND LOWER(r.service_provider) = LOWER(rav.service_provider)
                AND LOWER(r.data_service) = LOWER(rav.data_service)
            )
            WHERE LOWER(r.payer) = $1 AND rav.collection_id IS NULL
            GROUP BY r.collection_id, r.payer, r.service_provider, r.data_service
            ORDER BY pending_value DESC
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let pending_ravs = pending_rows
            .into_iter()
            .map(|row| PendingRav {
                identifier: row.get("collection_id"),
                payer: row.get("payer"),
                service_provider: row.get("service_provider"),
                data_service: row.get("data_service"),
                pending_receipt_count: row.get("pending_receipt_count"),
                pending_value: row.get("pending_value"),
            })
            .collect();

        // Get recent receipts
        let recent_receipt_rows = sqlx::query(
            r#"
            SELECT id, collection_id, payer, value, timestamp_ns
            FROM tap_horizon_receipts 
            WHERE LOWER(payer) = $1
            ORDER BY id DESC 
            LIMIT 10
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let recent_receipts = recent_receipt_rows
            .into_iter()
            .map(|row| RecentReceipt {
                id: row.get("id"),
                identifier: row.get("collection_id"),
                payer: row.get("payer"),
                value: row.get("value"),
                timestamp_ns: row.get("timestamp_ns"),
            })
            .collect();

        Ok(DetailedTapState {
            receipts_by_collection,
            ravs_by_collection,
            pending_ravs,
            recent_receipts,
        })
    }

    async fn get_v1_detailed_state(&self, payer: &str) -> Result<DetailedTapState> {
        let normalized_payer = payer.trim_start_matches("0x").to_lowercase();

        // Get receipts grouped by allocation for V1
        let receipt_rows = sqlx::query(
            r#"
            SELECT 
                allocation_id,
                signer_address as payer,
                allocation_id as service_provider,
                allocation_id as data_service,
                COUNT(*) as count,
                SUM(value) as total_value,
                MIN(timestamp_ns) as oldest_timestamp,
                MAX(timestamp_ns) as newest_timestamp
            FROM scalar_tap_receipts 
            WHERE LOWER(signer_address) = $1
            GROUP BY allocation_id, signer_address
            ORDER BY newest_timestamp DESC
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let receipts_by_collection = receipt_rows
            .into_iter()
            .map(|row| ReceiptSummary {
                identifier: row.get("allocation_id"),
                payer: row.get("payer"),
                service_provider: row.get("service_provider"),
                data_service: row.get("data_service"),
                count: row.get("count"),
                total_value: row.get("total_value"),
                oldest_timestamp: row.get("oldest_timestamp"),
                newest_timestamp: row.get("newest_timestamp"),
            })
            .collect();

        // Get RAVs by allocation for V1
        let rav_rows = sqlx::query(
            r#"
            SELECT 
                allocation_id,
                sender_address as payer,
                allocation_id as service_provider,
                allocation_id as data_service,
                value_aggregate,
                timestamp_ns,
                final as is_final,
                last as is_last
            FROM scalar_tap_ravs 
            WHERE LOWER(sender_address) = $1
            ORDER BY timestamp_ns DESC
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let ravs_by_collection = rav_rows
            .into_iter()
            .map(|row| RavSummary {
                identifier: row.get("allocation_id"),
                payer: row.get("payer"),
                service_provider: row.get("service_provider"),
                data_service: row.get("data_service"),
                value_aggregate: row.get("value_aggregate"),
                timestamp_ns: row.get("timestamp_ns"),
                is_final: row.get("is_final"),
                is_last: row.get("is_last"),
            })
            .collect();

        // Get pending RAVs for V1
        let pending_rows = sqlx::query(
            r#"
            SELECT 
                r.allocation_id,
                r.signer_address as payer,
                r.allocation_id as service_provider,
                r.allocation_id as data_service,
                COUNT(r.id) as pending_receipt_count,
                SUM(r.value) as pending_value
            FROM scalar_tap_receipts r
            LEFT JOIN scalar_tap_ravs rav ON (
                r.allocation_id = rav.allocation_id 
                AND LOWER(r.signer_address) = LOWER(rav.sender_address)
            )
            WHERE LOWER(r.signer_address) = $1 AND rav.allocation_id IS NULL
            GROUP BY r.allocation_id, r.signer_address
            ORDER BY pending_value DESC
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let pending_ravs = pending_rows
            .into_iter()
            .map(|row| PendingRav {
                identifier: row.get("allocation_id"),
                payer: row.get("payer"),
                service_provider: row.get("service_provider"),
                data_service: row.get("data_service"),
                pending_receipt_count: row.get("pending_receipt_count"),
                pending_value: row.get("pending_value"),
            })
            .collect();

        // Get recent receipts for V1
        let recent_receipt_rows = sqlx::query(
            r#"
            SELECT id, allocation_id, signer_address as payer, value, timestamp_ns
            FROM scalar_tap_receipts 
            WHERE LOWER(signer_address) = $1
            ORDER BY id DESC 
            LIMIT 10
            "#,
        )
        .bind(&normalized_payer)
        .fetch_all(&self.pool)
        .await?;

        let recent_receipts = recent_receipt_rows
            .into_iter()
            .map(|row| RecentReceipt {
                id: row.get("id"),
                identifier: row.get("allocation_id"),
                payer: row.get("payer"),
                value: row.get("value"),
                timestamp_ns: row.get("timestamp_ns"),
            })
            .collect();

        Ok(DetailedTapState {
            receipts_by_collection,
            ravs_by_collection,
            pending_ravs,
            recent_receipts,
        })
    }

    /// Check if RAV was created for a specific collection/allocation
    pub async fn has_rav_for_identifier(
        &self,
        identifier: &str, // collection_id for V2, allocation_id for V1
        payer: &str,
        service_provider: &str,
        data_service: &str,
        version: TapVersion,
    ) -> Result<bool> {
        let normalized_payer = payer.trim_start_matches("0x").to_lowercase();
        let normalized_service_provider = service_provider.trim_start_matches("0x").to_lowercase();
        let normalized_data_service = data_service.trim_start_matches("0x").to_lowercase();

        let count: i64 = match version {
            TapVersion::V2 => {
                sqlx::query_scalar(
                    r#"
                    SELECT COUNT(*) 
                    FROM tap_horizon_ravs 
                    WHERE collection_id = $1 
                    AND LOWER(payer) = $2 
                    AND LOWER(service_provider) = $3 
                    AND LOWER(data_service) = $4
                    "#,
                )
                .bind(identifier)
                .bind(&normalized_payer)
                .bind(&normalized_service_provider)
                .bind(&normalized_data_service)
                .fetch_one(&self.pool)
                .await?
            }
            TapVersion::V1 => sqlx::query_scalar(
                r#"
                    SELECT COUNT(*) 
                    FROM tap_ravs 
                    WHERE allocation_id = $1 
                    AND LOWER(sender_address) = $2
                    "#,
            )
            .bind(identifier)
            .bind(&normalized_payer)
            .fetch_optional(&self.pool)
            .await?
            .unwrap_or(0),
        };

        Ok(count > 0)
    }

    /// Get the total value of receipts for an identifier that don't have a RAV yet
    pub async fn get_pending_receipt_value(
        &self,
        identifier: &str, // collection_id for V2, allocation_id for V1
        payer: &str,
        version: TapVersion,
    ) -> Result<BigDecimal> {
        let normalized_payer = payer.trim_start_matches("0x").to_lowercase();

        let pending_value: Option<BigDecimal> = match version {
            TapVersion::V2 => {
                sqlx::query_scalar(
                    r#"
                    SELECT SUM(r.value)
                    FROM tap_horizon_receipts r
                    LEFT JOIN tap_horizon_ravs rav ON (
                        r.collection_id = rav.collection_id 
                        AND LOWER(r.payer) = LOWER(rav.payer)
                        AND LOWER(r.service_provider) = LOWER(rav.service_provider)
                        AND LOWER(r.data_service) = LOWER(rav.data_service)
                    )
                    WHERE r.collection_id = $1 
                    AND LOWER(r.payer) = $2 
                    AND rav.collection_id IS NULL
                    "#,
                )
                .bind(identifier)
                .bind(&normalized_payer)
                .fetch_one(&self.pool)
                .await?
            }
            TapVersion::V1 => sqlx::query_scalar(
                r#"
                    SELECT SUM(r.value)
                    FROM tap_receipts r
                    LEFT JOIN tap_ravs rav ON (
                        r.allocation_id = rav.allocation_id 
                        AND LOWER(r.sender_address) = LOWER(rav.sender_address)
                    )
                    WHERE r.allocation_id = $1 
                    AND LOWER(r.sender_address) = $2 
                    AND rav.allocation_id IS NULL
                    "#,
            )
            .bind(identifier)
            .bind(&normalized_payer)
            .fetch_optional(&self.pool)
            .await?
            .flatten(),
        };

        Ok(pending_value.unwrap_or_else(|| BigDecimal::from_str("0").unwrap()))
    }

    /// Wait for a RAV to be created with timeout
    pub async fn wait_for_rav_creation(
        &self,
        payer: &str,
        initial_rav_count: i64,
        timeout_seconds: u64,
        check_interval_seconds: u64,
        version: TapVersion,
    ) -> Result<bool> {
        let start_time = std::time::Instant::now();
        let timeout_duration = std::time::Duration::from_secs(timeout_seconds);

        while start_time.elapsed() < timeout_duration {
            let current_state = self.get_state(payer, version).await?;
            if current_state.rav_count > initial_rav_count {
                return Ok(true);
            }

            tokio::time::sleep(std::time::Duration::from_secs(check_interval_seconds)).await;
        }

        Ok(false)
    }

    /// Print a detailed summary of the current TAP state
    pub async fn print_detailed_summary(&self, payer: &str, version: TapVersion) -> Result<()> {
        let state = self.get_state(payer, version).await?;
        let detailed = self.get_detailed_state(payer, version).await?;

        let version_name = match version {
            TapVersion::V1 => "V1 (Legacy)",
            TapVersion::V2 => "V2 (Horizon)",
        };

        println!("\n=== {} TAP Database State ===", version_name);
        println!("Payer: {}", payer);
        println!("📊 Overall Statistics:");
        println!(
            "   Receipts: {} (total value: {} wei)",
            state.receipt_count, state.receipt_value
        );
        println!(
            "   RAVs: {} (total value: {} wei)",
            state.rav_count, state.rav_value
        );
        println!("   Pending RAV Collections: {}", state.pending_rav_count);
        println!("   Failed RAV Requests: {}", state.failed_rav_count);
        println!("   Invalid Receipts: {}", state.invalid_receipt_count);

        if !detailed.receipts_by_collection.is_empty() {
            let identifier_name = match version {
                TapVersion::V1 => "Allocation",
                TapVersion::V2 => "Collection",
            };
            println!("\n📋 Receipts by {}:", identifier_name);
            for summary in &detailed.receipts_by_collection {
                // For V2 collections, show the last 16 chars (the actual allocation part)
                // For V1 allocations, show the full ID
                let display_id = match version {
                    TapVersion::V2 => {
                        if summary.identifier.len() >= 16 {
                            format!(
                                "...{}",
                                &summary.identifier[summary.identifier.len() - 16..]
                            )
                        } else {
                            summary.identifier.clone()
                        }
                    }
                    TapVersion::V1 => summary.identifier.clone(),
                };
                println!(
                    "   {} {}: {} receipts, {} wei",
                    identifier_name, display_id, summary.count, summary.total_value
                );
            }
        }

        if !detailed.ravs_by_collection.is_empty() {
            let identifier_name = match version {
                TapVersion::V1 => "Allocation",
                TapVersion::V2 => "Collection",
            };
            println!("\n🎯 RAVs by {}:", identifier_name);
            for rav in &detailed.ravs_by_collection {
                let display_id = match version {
                    TapVersion::V2 => {
                        if rav.identifier.len() >= 16 {
                            format!("...{}", &rav.identifier[rav.identifier.len() - 16..])
                        } else {
                            rav.identifier.clone()
                        }
                    }
                    TapVersion::V1 => rav.identifier.clone(),
                };
                println!(
                    "   {} {}: {} wei (final: {}, last: {})",
                    identifier_name, display_id, rav.value_aggregate, rav.is_final, rav.is_last
                );
            }
        }

        if !detailed.pending_ravs.is_empty() {
            let identifier_name = match version {
                TapVersion::V1 => "Allocation",
                TapVersion::V2 => "Collection",
            };
            println!(
                "\n⏳ Pending RAVs ({}s with receipts but no RAVs):",
                identifier_name
            );
            for pending in &detailed.pending_ravs {
                println!(
                    "   {} {}: {} receipts pending, {} wei total",
                    identifier_name,
                    &pending.identifier[..8.min(pending.identifier.len())],
                    pending.pending_receipt_count,
                    pending.pending_value
                );
            }
        }

        if !detailed.recent_receipts.is_empty() {
            let identifier_name = match version {
                TapVersion::V1 => "Allocation",
                TapVersion::V2 => "Collection",
            };
            println!("\n🕒 Recent Receipts:");
            for receipt in &detailed.recent_receipts {
                let display_id = match version {
                    TapVersion::V2 => {
                        if receipt.identifier.len() >= 16 {
                            format!(
                                "...{}",
                                &receipt.identifier[receipt.identifier.len() - 16..]
                            )
                        } else {
                            receipt.identifier.clone()
                        }
                    }
                    TapVersion::V1 => receipt.identifier.clone(),
                };
                println!(
                    "   ID {}: {} {}, {} wei",
                    receipt.id, identifier_name, display_id, receipt.value
                );
            }
        }

        Ok(())
    }

    /// Print combined V1 and V2 summary
    pub async fn print_combined_summary(&self, payer: &str) -> Result<()> {
        let combined = self.get_combined_state(payer).await?;

        println!("\n=== Combined TAP Database State ===");
        println!("Payer: {}", payer);
        println!("\n📊 V1 (Legacy) Statistics:");
        println!(
            "   Receipts: {} (total value: {} wei)",
            combined.v1.receipt_count, combined.v1.receipt_value
        );
        println!(
            "   RAVs: {} (total value: {} wei)",
            combined.v1.rav_count, combined.v1.rav_value
        );
        println!(
            "   Pending RAV Collections: {}",
            combined.v1.pending_rav_count
        );

        println!("\n📊 V2 (Horizon) Statistics:");
        println!(
            "   Receipts: {} (total value: {} wei)",
            combined.v2.receipt_count, combined.v2.receipt_value
        );
        println!(
            "   RAVs: {} (total value: {} wei)",
            combined.v2.rav_count, combined.v2.rav_value
        );
        println!(
            "   Pending RAV Collections: {}",
            combined.v2.pending_rav_count
        );
        println!("   Failed RAV Requests: {}", combined.v2.failed_rav_count);
        println!("   Invalid Receipts: {}", combined.v2.invalid_receipt_count);

        let total_receipts = combined.v1.receipt_count + combined.v2.receipt_count;
        let total_ravs = combined.v1.rav_count + combined.v2.rav_count;

        println!("\n📊 Combined Totals:");
        println!(
            "   Total Receipts: {} (V1: {}, V2: {})",
            total_receipts, combined.v1.receipt_count, combined.v2.receipt_count
        );
        println!(
            "   Total RAVs: {} (V1: {}, V2: {})",
            total_ravs, combined.v1.rav_count, combined.v2.rav_count
        );

        Ok(())
    }
}

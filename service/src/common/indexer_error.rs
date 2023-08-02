use std::{
    error::Error,
    fmt::{self, Display},
};

const ERROR_BASE_URL: &str = "https://github.com/graphprotocol/indexer/blob/main/docs/errors.md";

#[derive(Debug, Clone)]
pub enum IndexerErrorCode {
    IE001,
    IE002,
    IE003,
    IE004,
    IE005,
    IE006,
    IE007,
    IE008,
    IE009,
    IE010,
    IE011,
    IE012,
    IE013,
    IE014,
    IE015,
    IE016,
    IE017,
    IE018,
    IE019,
    IE020,
    IE021,
    IE022,
    IE023,
    IE024,
    IE025,
    IE026,
    IE027,
    IE028,
    IE029,
    IE030,
    IE031,
    IE032,
    IE033,
    IE034,
    IE035,
    IE036,
    IE037,
    IE038,
    IE039,
    IE040,
    IE041,
    IE042,
    IE043,
    IE044,
    IE045,
    IE046,
    IE047,
    IE048,
    IE049,
    IE050,
    IE051,
    IE052,
    IE053,
    IE054,
    IE055,
    IE056,
    IE057,
    IE058,
    IE059,
    IE060,
    IE061,
    IE062,
    IE063,
    IE064,
    IE065,
    IE066,
    IE067,
    IE068,
    IE069,
    IE070,
    IE071,
    IE072,
    IE073,
    IE074,
    IE075,
}

impl fmt::Display for IndexerErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            IndexerErrorCode::IE001 => write!(f, "IE001"),
            IndexerErrorCode::IE002 => write!(f, "IE002"),
            IndexerErrorCode::IE003 => write!(f, "IE003"),
            IndexerErrorCode::IE004 => write!(f, "IE004"),
            IndexerErrorCode::IE005 => write!(f, "IE005"),
            IndexerErrorCode::IE006 => write!(f, "IE006"),
            IndexerErrorCode::IE007 => write!(f, "IE007"),
            IndexerErrorCode::IE008 => write!(f, "IE008"),
            IndexerErrorCode::IE009 => write!(f, "IE009"),
            IndexerErrorCode::IE010 => write!(f, "IE010"),
            IndexerErrorCode::IE011 => write!(f, "IE011"),
            IndexerErrorCode::IE012 => write!(f, "IE012"),
            IndexerErrorCode::IE013 => write!(f, "IE013"),
            IndexerErrorCode::IE014 => write!(f, "IE014"),
            IndexerErrorCode::IE015 => write!(f, "IE015"),
            IndexerErrorCode::IE016 => write!(f, "IE016"),
            IndexerErrorCode::IE017 => write!(f, "IE017"),
            IndexerErrorCode::IE018 => write!(f, "IE018"),
            IndexerErrorCode::IE019 => write!(f, "IE019"),
            IndexerErrorCode::IE020 => write!(f, "IE020"),
            IndexerErrorCode::IE021 => write!(f, "IE021"),
            IndexerErrorCode::IE022 => write!(f, "IE022"),
            IndexerErrorCode::IE023 => write!(f, "IE023"),
            IndexerErrorCode::IE024 => write!(f, "IE024"),
            IndexerErrorCode::IE025 => write!(f, "IE025"),
            IndexerErrorCode::IE026 => write!(f, "IE026"),
            IndexerErrorCode::IE027 => write!(f, "IE027"),
            IndexerErrorCode::IE028 => write!(f, "IE028"),
            IndexerErrorCode::IE029 => write!(f, "IE029"),
            IndexerErrorCode::IE030 => write!(f, "IE030"),
            IndexerErrorCode::IE031 => write!(f, "IE031"),
            IndexerErrorCode::IE032 => write!(f, "IE032"),
            IndexerErrorCode::IE033 => write!(f, "IE033"),
            IndexerErrorCode::IE034 => write!(f, "IE034"),
            IndexerErrorCode::IE035 => write!(f, "IE035"),
            IndexerErrorCode::IE036 => write!(f, "IE036"),
            IndexerErrorCode::IE037 => write!(f, "IE037"),
            IndexerErrorCode::IE038 => write!(f, "IE038"),
            IndexerErrorCode::IE039 => write!(f, "IE039"),
            IndexerErrorCode::IE040 => write!(f, "IE040"),
            IndexerErrorCode::IE041 => write!(f, "IE041"),
            IndexerErrorCode::IE042 => write!(f, "IE042"),
            IndexerErrorCode::IE043 => write!(f, "IE043"),
            IndexerErrorCode::IE044 => write!(f, "IE044"),
            IndexerErrorCode::IE045 => write!(f, "IE045"),
            IndexerErrorCode::IE046 => write!(f, "IE046"),
            IndexerErrorCode::IE047 => write!(f, "IE047"),
            IndexerErrorCode::IE048 => write!(f, "IE048"),
            IndexerErrorCode::IE049 => write!(f, "IE049"),
            IndexerErrorCode::IE050 => write!(f, "IE050"),
            IndexerErrorCode::IE051 => write!(f, "IE051"),
            IndexerErrorCode::IE052 => write!(f, "IE052"),
            IndexerErrorCode::IE053 => write!(f, "IE053"),
            IndexerErrorCode::IE054 => write!(f, "IE054"),
            IndexerErrorCode::IE055 => write!(f, "IE055"),
            IndexerErrorCode::IE056 => write!(f, "IE056"),
            IndexerErrorCode::IE057 => write!(f, "IE057"),
            IndexerErrorCode::IE058 => write!(f, "IE058"),
            IndexerErrorCode::IE059 => write!(f, "IE059"),
            IndexerErrorCode::IE060 => write!(f, "IE060"),
            IndexerErrorCode::IE061 => write!(f, "IE061"),
            IndexerErrorCode::IE062 => write!(f, "IE062"),
            IndexerErrorCode::IE063 => write!(f, "IE063"),
            IndexerErrorCode::IE064 => write!(f, "IE064"),
            IndexerErrorCode::IE065 => write!(f, "IE065"),
            IndexerErrorCode::IE066 => write!(f, "IE066"),
            IndexerErrorCode::IE067 => write!(f, "IE067"),
            IndexerErrorCode::IE068 => write!(f, "IE068"),
            IndexerErrorCode::IE069 => write!(f, "IE069"),
            IndexerErrorCode::IE070 => write!(f, "IE070"),
            IndexerErrorCode::IE071 => write!(f, "IE071"),
            IndexerErrorCode::IE072 => write!(f, "IE072"),
            IndexerErrorCode::IE073 => write!(f, "IE073"),
            IndexerErrorCode::IE074 => write!(f, "IE074"),
            IndexerErrorCode::IE075 => write!(f, "IE075"),
        }
    }
}

impl IndexerErrorCode {
    pub fn message(&self) -> &'static str {
        match self {
            Self::IE001 => "Failed to run database migrations",
            Self::IE002 => "Invalid Ethereum URL",
            Self::IE003 => "Failed to index network subgraph",
            Self::IE004 => "Failed to synchronize with network",
            Self::IE005 => "Failed to reconcile indexer and network",
            Self::IE006 => "Failed to cross-check allocation state with contracts",
            Self::IE007 => "Failed to check for network pause",
            Self::IE008 => "Failed to check operator status for indexer",
            Self::IE009 => "Failed to query subgraph deployments worth indexing",
            Self::IE010 => "Failed to query indexer allocations",
            Self::IE011 => "Failed to query claimable indexer allocations",
            Self::IE012 => "Failed to register indexer",
            Self::IE013 => "Failed to allocate: insufficient free stake",
            Self::IE014 => "Failed to allocate: allocation not created on chain",
            Self::IE015 => "Failed to close allocation",
            Self::IE016 => "Failed to claim allocation",
            Self::IE017 => "Failed to ensure default global indexing rule",
            Self::IE018 => "Failed to query indexing status API",
            Self::IE019 => "Failed to query proof of indexing",
            Self::IE020 => "Failed to ensure subgraph deployment is indexing",
            Self::IE021 => "Failed to migrate cost model",
            Self::IE022 => "Failed to identify attestation signer for allocation",
            Self::IE023 => "Failed to handle state channel message",
            Self::IE024 => "Failed to connect to indexing status API",
            Self::IE025 => "Failed to query indexer management API",
            Self::IE026 => "Failed to deploy subgraph deployment",
            Self::IE027 => "Failed to remove subgraph deployment",
            Self::IE028 => "Failed to reassign subgraph deployment",
            Self::IE029 => "Invalid Scalar-Receipt header provided",
            Self::IE030 => "No Scalar-Receipt header provided",
            Self::IE031 => "Invalid Scalar-Receipt value provided",
            Self::IE032 => "Failed to process paid query",
            Self::IE033 => "Failed to process free query",
            Self::IE034 => "Not authorized as an operator for the indexer",
            Self::IE035 => "Unhandled promise rejection",
            Self::IE036 => "Unhandled exception",
            Self::IE037 => "Failed to query disputable allocations",
            Self::IE038 => "Failed to query epochs",
            Self::IE039 => "Failed to store potential POI disputes",
            Self::IE040 => "Failed to fetch POI disputes",
            Self::IE041 => "Failed to query transfers to resolve",
            Self::IE042 => "Failed to add transfer to the database",
            Self::IE043 => "Failed to mark transfer as resolved",
            Self::IE044 => "Failed to collect query fees on chain",
            Self::IE045 => "Failed to queue transfers for resolving",
            Self::IE046 => "Failed to resolve transfer",
            Self::IE047 => "Failed to mark transfer as failed",
            Self::IE048 => "Failed to withdraw query fees for allocation",
            Self::IE049 => "Failed to clean up transfers for allocation",
            Self::IE050 => "Transaction reverted due to gas limit being hit",
            Self::IE051 => "Transaction reverted for unknown reason",
            Self::IE052 => "Transaction aborted: maximum configured gas price reached",
            Self::IE053 => "Failed to queue receipts for collecting",
            Self::IE054 => "Failed to collect receipts in exchange for query fee voucher",
            Self::IE055 => "Failed to redeem query fee voucher",
            Self::IE056 => "Failed to remember allocation for collecting receipts later",
            Self::IE057 => "Transaction reverted due to failing assertion in contract",
            Self::IE058 => "Transaction failed because nonce has already been used",
            Self::IE059 => "Failed to check latest operator ETH balance",
            Self::IE060 => "Failed to allocate: Already allocating to the subgraph deployment",
            Self::IE061 => "Failed to allocate: Invalid allocation amount provided",
            Self::IE062 => "Did not receive tx receipt, not authorized or network paused",
            Self::IE063 => "No active allocation with provided id found",
            Self::IE064 => {
                "Failed to unallocate: Allocation cannot be closed in the same epoch it was created"
            }
            Self::IE065 => "Failed to unallocate: Allocation has already been closed",
            Self::IE066 => "Failed to allocate: allocation ID already exists on chain",
            Self::IE067 => "Failed to query POI for current epoch start block",
            Self::IE068 => "User-provided POI did not match reference POI from graph-node",
            Self::IE069 => "Failed to query Epoch Block Oracle Subgraph",
            Self::IE070 => "Failed to query latest valid epoch and block hash",
            Self::IE071 => "Add Epoch subgraph support for non-protocol chains",
            Self::IE072 => "Failed to execute batch tx (contract: staking)",
            Self::IE073 => "Failed to query subgraph features from indexing statuses endpoint",
            Self::IE074 => "Failed to resolve the release version",
            Self::IE075 => "Failed to parse response body to query string",
        }
    }

    pub fn explanation(&self) -> String {
        format!("{}#{}", ERROR_BASE_URL, self.message())
    }
}

// pub type IndexerErrorCause = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
pub struct IndexerErrorCause(Box<dyn Error + Send + Sync>);

impl IndexerErrorCause {
    pub fn new<E>(error: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        IndexerErrorCause(error.into())
    }
}

impl Display for IndexerErrorCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for IndexerErrorCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

impl From<String> for IndexerErrorCause {
    fn from(error: String) -> Self {
        Self(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            error,
        )))
    }
}

#[derive(Debug)]
pub struct IndexerError {
    code: IndexerErrorCode,
    explanation: String,
    cause: Option<IndexerErrorCause>,
}

impl IndexerError {
    pub fn new(code: IndexerErrorCode, cause: Option<IndexerErrorCause>) -> Self {
        let explanation = code.message();
        Self {
            code,
            explanation: explanation.to_string(),
            cause,
        }
    }

    pub fn code(&self) -> IndexerErrorCode {
        self.code.clone()
    }

    pub fn explanation(&self) -> &str {
        &self.explanation
    }

    pub fn cause(&self) -> Option<&IndexerErrorCause> {
        self.cause.as_ref()
    }
}

// pub fn indexer_error(code: IndexerErrorCode, cause: Option<IndexerErrorCause>) -> IndexerError {
//     IndexerError::new(code, cause)
// }
pub fn indexer_error(code: IndexerErrorCode) -> IndexerError {
    IndexerError::new(code.clone(), Some(code.explanation().into()))
}

impl std::fmt::Display for IndexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Indexer error: {}, explanation: {}",
            self.code, self.explanation
        )?;
        if let Some(cause) = &self.cause {
            write!(f, ", cause: {:?}", cause)?;
        }
        Ok(())
    }
}

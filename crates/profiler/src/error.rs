// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProfilerError {
    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Failed to create flamegraph: {0}")]
    FlamegraphCreationError(String),

    #[error("Failed to generate protobuf: {0}")]
    ProtobufError(String),

    #[error("Failed to generate profile report: {0}")]
    ReportError(String),

    #[error("Failed to serialize profile: {0}")]
    SerializationError(String),

    #[error("System time error: {0}")]
    TimeError(#[from] std::time::SystemTimeError),
}

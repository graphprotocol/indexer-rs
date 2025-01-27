// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

fn main() {
    println!("cargo:rerun-if-changed=proto");
    tonic_build::configure()
        .out_dir("src/proto")
        .include_file("indexer.rs")
        .build_client(false)
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/indexer.proto"], &["proto/"])
        .expect("Failed to compile DIPs indexer RPC proto(s)");

    tonic_build::configure()
        .out_dir("src/proto")
        .include_file("gateway.rs")
        .build_server(false)
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/gateway.proto"], &["proto"])
        .expect("Failed to compile DIPs gateway RPC proto(s)");
}

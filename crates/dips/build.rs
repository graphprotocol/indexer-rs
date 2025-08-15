// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

fn main() {
    #[cfg(feature = "rpc")]
    {
        println!("cargo:rerun-if-changed=proto");

        tonic_prost_build::configure()
            .build_server(true)
            .out_dir("src/proto")
            .include_file("indexer.rs")
            .protoc_arg("--experimental_allow_proto3_optional")
            .compile_protos(&["proto/indexer.proto"], &["proto/"])
            .expect("Failed to compile DIPs indexer RPC proto(s)");

        tonic_prost_build::configure()
            .build_server(true)
            .out_dir("src/proto")
            .include_file("gateway.rs")
            .protoc_arg("--experimental_allow_proto3_optional")
            .compile_protos(&["proto/gateway.proto"], &["proto"])
            .expect("Failed to compile DIPs gateway RPC proto(s)");
    }
}

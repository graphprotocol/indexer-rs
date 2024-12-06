// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

fn main() {
    println!("cargo:rerun-if-changed=proto");
    tonic_build::configure()
        .out_dir("src/proto")
        .include_file("mod.rs")
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/dips.proto"], &["proto"])
        .expect("Failed to compile dips proto(s)");
}

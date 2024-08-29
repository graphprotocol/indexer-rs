// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use build_info_build::DependencyDepth;

fn main() {
    build_info_build::build_script().collect_dependencies(DependencyDepth::Depth(1));
}

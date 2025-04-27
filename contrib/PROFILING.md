# Profiling Tools

This document explains the profiling infrastructure set up for our indexer-service and tap-agent services. The profiling setup enables developers to diagnose performance issues, memory leaks, and analyze runtime behavior in both development and production environments.

## Overview

Our project includes an integrated profiling system for the indexer services. The system supports multiple profiling methods through:

1. A custom `profiler` library (included in the workspace)
2. Docker-based profiling environments
3. Various third-party profiling tools

## Available Profiling Methods

### Built-in Profiler (pprof-based Flamegraphs)

A Rust library that uses [pprof](https://crates.io/crates/pprof) to continuously profile the application and generate flamegraphs at specified intervals.
This solution was particularly suitable because tools like `perf`, while powerful, often pose configuration challenges or require specific capabilities (like CAP_SYS_ADMIN) that complicate their deployment within standard Docker containers.

- **Configuration**: Set in code with the `setup_profiling` function
- **Activation**: Enabled via the `profiling` feature flag
- **Output**: Flamegraphs (SVG) and protobuf profiles in `/opt/profiling/{service-name}/`

### External Profiling Tools

The profiling environment also supports the following tools:

| Tool          | Description                              | Output                                        |
| ------------- | ---------------------------------------- | --------------------------------------------- |
| **strace**    | Traces system calls with detailed timing | `/opt/profiling/{service-name}/strace.log`    |
| **valgrind**  | Memory profiling with Massif             | `/opt/profiling/{service-name}/massif.out`    |
| **callgrind** | CPU profiling (part of valgrind)         | `/opt/profiling/{service-name}/callgrind.out` |

## How to Use

### Prerequisites

Run the setup command first to prepare the testing environment:

```bash
just setup
```

### Profiling Commands

Use the following commands to profile specific services:

```bash
# Profile with flamegraph (default)
just profile-flamegraph

# Profile with valgrind
just profile-valgrind

# Profile with strace
just profile-strace

# Profile with callgrind
just profile-callgrind

# Stop profiling (gracefully terminate to generate output)
just stop-profiling

# Restore normal service without profiling
just profile-restore
```

### Viewing Results

Profiling data is stored in:

- `contrib/profiling/indexer-service/`
- `contrib/profiling/tap-agent/`

#### Visualization Tools

- **Flamegraphs**: Open the SVG files in any web browser
- **Callgrind**: Use `callgrind_annotate` or KCachegrind for visualization:

  ```bash
  callgrind_annotate contrib/profiling/tap-agent/callgrind.out
  ```

- **Massif**: Use `ms_print` to view memory profiling results:

  ```bash
  ms_print contrib/profiling/tap-agent/massif.out
  ```

- **Protobuf Profiles**: View with Go pprof tools:

```go
# Install Go pprof tools if needed
go install github.com/google/pprof@latest

# View interactive web UI (most user-friendly)
pprof -http=:8080 contrib/profiling/indexer-service/profile-*.pb

# Or generate a flamegraph from protobuf data
pprof -flamegraph contrib/profiling/indexer-service/profile-*.pb > custom_flamegraph.svg
```

## Implementation Details

### Profiler Integration

The profiler library is conditionally compiled using the `profiling` feature flag:

```rust
#[cfg(feature = "profiling")]
if let Err(e) = profiler::setup_profiling(
    "/opt/profiling/indexer-service".to_string(),
    150,    // sampling frequency (Hz)
    120,    // interval between reports (seconds)
    Some("Indexer Service".to_string()),
) {
    tracing::error!("Failed to setup profiling: {e}");
} else {
    tracing::info!("Profiling setup complete.");
}
```

### Docker Environment

The profiling infrastructure uses a custom Docker image with all necessary tools pre-installed. The container runs with elevated privileges to support profiling:

```yaml
cap_add:
  - SYS_ADMIN
privileged: true
security_opt:
  - seccomp:unconfined
```

## Notes

- The flamegraph profiling is enabled whenever using any of the profiling commands through the Justfile, as the binaries are compiled with the `profiling` feature flag.
- For production use, prefer the built-in profiler over the external tools to minimize performance impact.
- When using callgrind, consider enabling debug information and frame pointers in your Cargo.toml for better output:

  ```toml
  [profile.release]
  debug = true
  force-frame-pointers = true
  ```

# Load Testing and Profiling Guide for Indexer Service

## Overview

This guide explains how to perform load testing and profiling on the indexer service and tap-agent components. The system includes built-in load testing tools and various profiling methods to help identify performance bottlenecks and optimize the services.

Our project includes an integrated profiling system that supports multiple profiling methods through:

1. A custom `profiler` library (included in the workspace)
2. Docker-based profiling environments
3. Various third-party profiling tools

## Important Note About Load Limits

⚠️ **Important**: The current indexer-service implementation has a built-in protection mechanism against high load from a single sender. When receiving too many receipts from the same server (approximately 1000 receipts), the sender will be marked as denied. This is a security feature to prevent abuse.

## Load Testing

### Prerequisites

1. Set up the local test network:

```bash
just setup
```

2. Fund the escrow account (required for testing):

```bash
cd integration-tests
./fund_escrow.sh
```

### Running Load Tests

The integration tests include a dedicated load testing tool that communicates directly with the indexer-service (bypassing the gateway). This allows for more accurate performance measurements.

To run a load test:

```bash
# Run with 1000 receipts
cargo run -- load --num-receipts 1000

# Run with custom number of receipts
cargo run -- load --num-receipts <number>
```

The load test will:

- Send the specified number of receipts concurrently
- Use all available CPU cores for concurrency
- Report success/failure rates
- Show average processing time per request
- Display total duration

### Understanding Load Test Results

The load test output includes:

- Total number of receipts processed
- Processing duration
- Average time per request
- Number of successful receipts
- Number of failed receipts

Example output:

```
Completed processing 1000 requests in 2.5s
Average time per request: 2.5ms
Successfully sent receipts: 998
Failed receipts: 2
```

## Profiling

### Available Profiling Methods

The system supports multiple profiling tools:

1. **Flamegraph** (Default)

   - Visual representation of CPU usage
   - Shows function call stacks
   - Based on [pprof](https://crates.io/crates/pprof)
   - Output: SVG files and protobuf profiles in `/opt/profiling/{service-name}/`

2. **Valgrind Massif**

   - Memory profiling
   - Tracks heap usage
   - Output: `massif.out` files

3. **Callgrind**

   - Detailed CPU profiling
   - Cache and branch prediction analysis
   - Output: `callgrind.out` files

4. **Strace**
   - System call tracing
   - Detailed timing information
   - Output: `strace.log` files

### Running Profiling

1. Start profiling with your chosen tool:

```bash
# For flamegraph (default)
just profile-flamegraph

# For memory profiling
just profile-valgrind

# For system call tracing
just profile-strace

# For CPU profiling
just profile-callgrind
```

2. Run your load test while profiling:

```bash
cargo run -- load --num-receipts 1000
```

3. Stop profiling to generate results:

```bash
just stop-profiling
```

4. Restore normal service:

```bash
just profile-restore
```

### Viewing Profiling Results

Profiling data is stored in:

- `contrib/profiling/indexer-service/` (for indexer-service)
- `contrib/profiling/tap-agent/` (for tap-agent)

#### Flamegraph Analysis

- Open the SVG files in a web browser
- Look for wide bars indicating high CPU usage
- Identify hot paths in the code

#### Memory Profiling (Massif)

```bash
ms_print contrib/profiling/indexer-service/massif.out
```

#### CPU Profiling (Callgrind)

```bash
callgrind_annotate contrib/profiling/indexer-service/callgrind.out
```

#### Protobuf Profiles

View with Go pprof tools:

```bash
# Install Go pprof tools if needed
go install github.com/google/pprof@latest

# View interactive web UI (most user-friendly)
pprof -http=:8080 contrib/profiling/indexer-service/profile-*.pb

# Or generate a flamegraph from protobuf data
pprof -flamegraph contrib/profiling/indexer-service/profile-*.pb > custom_flamegraph.svg
```

## Best Practices

1. **Load Testing**

   - Start with small numbers (100-500 receipts)
   - Gradually increase load to find breaking points
   - Monitor system resources during tests
   - Be aware of the 1000 receipt limit per sender

2. **Profiling**

   - Use flamegraph for general performance analysis
   - Use Massif for memory leak detection
   - Use Callgrind for detailed CPU optimization
   - Use strace for system call bottlenecks
   - For production use, prefer the built-in profiler over external tools to minimize performance impact

3. **Environment**
   - Ensure clean state before testing
   - Monitor system resources
   - Check logs for errors
   - Verify escrow funding before testing

## Troubleshooting

1. If load tests fail:

   - Check escrow funding
   - Verify service health
   - Check logs for errors
   - Ensure proper network setup

2. If profiling fails:
   - Check container permissions
   - Verify profiling tool installation
   - Check available disk space
   - Ensure proper service shutdown

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
- Built-in profiler was chosen because tools like `perf`, while powerful, often pose configuration challenges or require specific capabilities (like CAP_SYS_ADMIN) that complicate their deployment within standard Docker containers.
- When using callgrind, consider enabling debug information and frame pointers in your Cargo.toml for better output:

  ```toml
  [profile.release]
  debug = true
  force-frame-pointers = true
  ```

## Additional Resources

- [Developer Setup Guide](README.md)
- [Integration Tests](../integration-tests/testing_plan.md)

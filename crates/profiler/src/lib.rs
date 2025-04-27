// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use pprof::protos::Message;

mod error;
pub use error::ProfilerError;

/// Save a flamegraph to the specified path
fn save_flamegraph(
    report: &pprof::Report,
    path: &Path,
    options: &mut pprof::flamegraph::Options,
) -> Result<(), ProfilerError> {
    let file = File::create(path)?;

    report
        .flamegraph_with_options(file, options)
        .map_err(|e| ProfilerError::FlamegraphCreationError(e.to_string()))?;

    tracing::info!("✅ Generated flamegraph: {:?}", path);
    Ok(())
}

/// Save a protobuf profile to the specified path
fn save_protobuf(profile: &pprof::protos::Profile, path: &Path) -> Result<(), ProfilerError> {
    // Try write_to_bytes first
    match profile.write_to_bytes() {
        Ok(bytes) => {
            let mut file = File::create(path)?;
            file.write_all(&bytes)?;
        }
        Err(e) => {
            // Alternative approach: try direct file writing
            tracing::info!(
                "⚠️ Failed to serialize profile: {}, trying direct writer",
                e
            );

            let mut file = File::create(path)?;
            profile
                .write_to_writer(&mut file)
                .map_err(|e| ProfilerError::SerializationError(e.to_string()))?;
        }
    }

    tracing::info!("✅ Generated protobuf profile: {:?}", path);
    Ok(())
}

/// Generate a unique filename with timestamp and counter
fn generate_filename(
    base_path: &str,
    prefix: &str,
    extension: &str,
    counter: u64,
) -> Result<PathBuf, ProfilerError> {
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs()
        .to_string();

    let filename = format!("{}-{}-{}.{}", prefix, timestamp, counter, extension);
    Ok(Path::new(base_path).join(filename))
}

/// Process a single profiling report
fn process_profiling_report(
    guard: &pprof::ProfilerGuard<'_>,
    path: &str,
    counter: u64,
    options: &mut pprof::flamegraph::Options,
) -> Result<(), ProfilerError> {
    let report = guard
        .report()
        .build()
        .map_err(|e| ProfilerError::ReportError(e.to_string()))?;

    // Generate flamegraph
    let flamegraph_path = generate_filename(path, "flamegraph", "svg", counter)?;
    if let Err(e) = save_flamegraph(&report, &flamegraph_path, options) {
        tracing::error!("Failed to save flamegraph: {}", e);
        // Continue execution to try saving protobuf
    }

    // Generate protobuf profile
    match report.pprof() {
        Ok(profile) => {
            let proto_path = generate_filename(path, "profile", "pb", counter)?;
            if let Err(e) = save_protobuf(&profile, &proto_path) {
                tracing::error!("Failed to save protobuf: {}", e);
            }
        }
        Err(e) => {
            tracing::error!("Failed to generate pprof profile: {}", e);
        }
    }

    Ok(())
}

fn setup(path: String, frequency: i32, interval: u64, name: String) -> Result<(), ProfilerError> {
    // Ensure the profiling directory exists
    let profile_dir = Path::new(&path);
    if !profile_dir.exists() {
        fs::create_dir_all(profile_dir)?;
    }

    // Create a background thread for continuous profiling
    let path_clone = path.clone();
    thread::spawn(move || {
        // Wait a bit for the application to start
        thread::sleep(Duration::from_secs(10));
        tracing::info!("🔍 Starting continuous profiling...");

        // Counter for tracking report generation
        let counter = Arc::new(AtomicU64::new(0));

        // Create a separate thread for continuous data collection
        thread::spawn(move || {
            // Start continuous profiling
            let guard = match pprof::ProfilerGuardBuilder::default()
                .frequency(frequency)
                .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                .build()
            {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::error!("Failed to initialize profiler: {}", e);
                    return;
                }
            };

            tracing::info!("📊 Continuous profiling active");
            let mut options = pprof::flamegraph::Options::default();
            options.title = name;

            // Create a timer thread to periodically save reports
            thread::spawn(move || {
                loop {
                    // Sleep for `interval` seconds before saving reports
                    thread::sleep(Duration::from_secs(interval));

                    let current_counter = counter.fetch_add(1, Ordering::Relaxed);

                    tracing::info!("💾 Saving profiling snapshot #{}...", current_counter);

                    if let Err(e) =
                        process_profiling_report(&guard, &path_clone, current_counter, &mut options)
                    {
                        tracing::error!("Error processing profiling report: {}", e);
                    }
                }
            });

            // Keep profiling thread alive
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        });
    });

    Ok(())
}

/// Sets up continuous CPU profiling with flamegraph and protobuf output.
///
/// # Arguments
///
/// * `path` - Directory where profiling data will be stored
/// * `frequency` - Sampling frequency in Hz
/// * `interval` - Time between saving reports in seconds
/// * `name` - Optional service name for labeling profiles
///
/// # Errors
///
/// Returns `ProfilerError` if directory creation fails
///
/// # Examples
///
/// ```
/// profiler::setup_profiling(
///     "/opt/profiling/my-service".to_string(),
///     150,
///     120,
///     Some("My Service".to_string())
/// )?;
/// ```
pub fn setup_profiling(
    path: String,
    frequency: i32,
    interval: u64,
    name: Option<String>,
) -> Result<(), ProfilerError> {
    tracing::info!("🔍 Setting up profiling...");
    setup(
        path,
        frequency,
        interval,
        name.unwrap_or("Service".to_string()),
    )
}

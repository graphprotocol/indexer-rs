// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "profiling")]
mod implementation {
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime};

    use pprof::protos::Message;

    pub fn setup() {
        // Ensure the profiling directory exists
        let profile_dir = Path::new("/opt/profiling");
        if !profile_dir.exists() {
            fs::create_dir_all(profile_dir).expect("Failed to create profiling directory");
        }

        // Create a background thread for continuous profiling
        thread::spawn(|| {
            // Wait a bit for the application to start
            thread::sleep(Duration::from_secs(10));
            tracing::info!("🔍 Starting continuous profiling...");

            // Counter for tracking report generation
            let counter = Arc::new(Mutex::new(0));
            let counter_clone = counter.clone();

            // Create a separate thread for continuous data collection
            thread::spawn(move || {
                // Start continuous profiling
                let guard = pprof::ProfilerGuardBuilder::default()
                    .frequency(150) // Sample at 99 Hz (can be adjusted)
                    .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                    .build()
                    .unwrap();

                tracing::info!("📊 Continuous profiling active");
                let mut options = pprof::flamegraph::Options::default();
                options.title = "Profiling: indexer-service".to_string();
                options.min_width = 0.5;

                // Create a timer thread to periodically save reports
                thread::spawn(move || {
                    loop {
                        // Sleep for 2 minutes before saving a report
                        thread::sleep(Duration::from_secs(120));

                        // Get the current counter value and increment
                        let mut counter = counter_clone.lock().unwrap();
                        let current_counter = *counter;
                        *counter += 1;
                        drop(counter); // Release the lock

                        // Generate timestamp for the filename
                        let timestamp =
                            match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
                                Ok(n) => n.as_secs().to_string(),
                                Err(_) => "unknown".to_string(),
                            };
                        let flamegraph_path = format!(
                            "/opt/profiling/flamegraph-{}-{}.svg",
                            timestamp, current_counter
                        );

                        tracing::info!("💾 Saving profiling snapshot #{}...", current_counter);

                        // Get report from the continuous profiler
                        match guard.report().build() {
                            Ok(report) => {
                                match File::create(&flamegraph_path) {
                                    Ok(flamegraph_file) => match report
                                        .flamegraph_with_options(flamegraph_file, &mut options)
                                    {
                                        Ok(_) => {
                                            tracing::info!(
                                                "✅ Generated flamegraph: {}",
                                                flamegraph_path
                                            );
                                        }
                                        Err(e) => {
                                            tracing::info!(
                                                "❌ Failed to generate flamegraph: {}",
                                                e
                                            );
                                        }
                                    },
                                    Err(e) => {
                                        tracing::info!(
                                            "❌ Failed to create flamegraph file: {}",
                                            e
                                        );
                                    }
                                }

                                // Try to save as protobuf format
                                match report.pprof() {
                                    Ok(profile) => {
                                        // Use the write_to_bytes method available on the Message trait
                                        let proto_path = format!(
                                            "/opt/profiling/profile-{}-{}.pb",
                                            timestamp, current_counter
                                        );

                                        // According to the docs, there are multiple ways to do this
                                        // Try write_to_bytes first
                                        match profile.write_to_bytes() {
                                            Ok(ref bytes) => {
                                                if let Ok(mut file) = File::create(&proto_path) {
                                                    if file.write_all(&bytes).is_ok() {
                                                        tracing::info!(
                                                            "✅ Generated protobuf profile: {}",
                                                            proto_path
                                                        );
                                                    } else {
                                                        tracing::info!(
                                                            "❌ Failed to write bytes to file"
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::info!(
                                                    "❌ Failed to serialize profile: {}",
                                                    e
                                                );

                                                // Alternative approach: try direct file writing
                                                if let Ok(mut file) = File::create(&proto_path) {
                                                    if profile.write_to_writer(&mut file).is_ok() {
                                                        tracing::info!("✅ Generated protobuf profile using direct writer: {}", proto_path);
                                                    } else {
                                                        tracing::info!("❌ Failed to write profile directly to file");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::info!("❌ Failed to generate pprof profile: {}", e)
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::info!("❌ Failed to build profile report: {}", e);
                            }
                        }
                    }
                });

                // Keep profiling thread alive
                loop {
                    thread::sleep(Duration::from_secs(3600));
                }
            });
        });
    }
}

#[cfg(not(feature = "profiling"))]
mod implementation {
    // Empty implementation for when profiling is disabled
    pub fn setup() {
        // Do nothing when profiling is disabled
    }
}

// Public API that will either do profiling or nothing based on the feature flag
pub fn setup_profiling() {
    tracing::info!("🔍 Setting up profiling...");
    implementation::setup();
}


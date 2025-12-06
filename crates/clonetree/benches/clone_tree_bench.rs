//! Benchmarks for clonetree copy strategies.

use std::{fs, hint::black_box, path::Path, time::Duration};

use clonetree::{clone_tree, CloneStrategy, Options};
use criterion::{measurement::WallTime, BenchmarkGroup, BenchmarkId, Criterion};
use tempfile::TempDir;

/// Size in bytes for small benchmark files.
const SMALL_SIZE: usize = 1_000;
/// Size in bytes for large benchmark files.
const LARGE_SIZE: usize = 100_000;

/// Parameters describing one benchmark configuration.
struct BenchConfig {
    /// Label used in benchmark output.
    name: &'static str,
    /// Number of files created at each level.
    file_count: usize,
    /// Depth of the directory tree.
    depth: usize,
    /// Number of subdirectories per level.
    fanout: usize,
    /// Size in bytes for each file.
    file_size: usize,
}

/// Create a balanced directory tree populated with files.
fn create_test_tree(dir: &Path, file_count: usize, depth: usize, fanout: usize, file_size: usize) {
    if depth == 0 {
        return;
    }

    for i in 0..file_count {
        let content = vec![b'X'; file_size];
        fs::write(dir.join(format!("file_{i}.txt")), content).unwrap();
    }

    if depth > 1 {
        for i in 0..fanout {
            let subdir = dir.join(format!("subdir_{i}"));
            fs::create_dir(&subdir).unwrap();
            create_test_tree(&subdir, file_count, depth - 1, fanout, file_size);
        }
    }
}

/// Benchmark clone_tree across multiple configurations and strategies.
fn benchmark_clone_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone_tree");
    group.measurement_time(Duration::from_secs(30));

    let configs = [
        BenchConfig {
            name: "shallow_small",
            file_count: 5,
            depth: 2,
            fanout: 3,
            file_size: SMALL_SIZE,
        },
        BenchConfig {
            name: "deep_small",
            file_count: 1,
            depth: 5,
            fanout: 5,
            file_size: SMALL_SIZE,
        },
        BenchConfig {
            name: "shallow_large",
            file_count: 5,
            depth: 2,
            fanout: 3,
            file_size: LARGE_SIZE,
        },
        BenchConfig {
            name: "deep_large",
            file_count: 1,
            depth: 5,
            fanout: 5,
            file_size: LARGE_SIZE,
        },
    ];

    // Run benchmarks for each configuration
    for config in &configs {
        let total_files = calculate_total_files(config.file_count, config.depth, config.fanout);

        // Create the source tree once for this configuration
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        fs::create_dir(&src).unwrap();
        create_test_tree(
            &src,
            config.file_count,
            config.depth,
            config.fanout,
            config.file_size,
        );

        bench_with_strategy(
            &mut group,
            config,
            total_files,
            &temp_dir,
            &src,
            CloneStrategy::FullTraversal,
            "full_traversal",
        );

        bench_with_strategy(
            &mut group,
            config,
            total_files,
            &temp_dir,
            &src,
            CloneStrategy::Auto,
            "auto",
        );

        #[cfg(target_os = "macos")]
        bench_with_strategy(
            &mut group,
            config,
            total_files,
            &temp_dir,
            &src,
            CloneStrategy::SingleCall,
            "single_call",
        );
    }

    group.finish();
}

/// Run a single benchmark configuration using a specific strategy.
fn bench_with_strategy(
    group: &mut BenchmarkGroup<'_, WallTime>,
    config: &BenchConfig,
    total_files: usize,
    temp_dir: &TempDir,
    src: &Path,
    strategy: CloneStrategy,
    label: &str,
) {
    group.bench_with_input(
        BenchmarkId::new(
            format!("{}-{label}", config.name),
            format!("{total_files}_files"),
        ),
        &(temp_dir, src),
        |b, &(temp_dir, src)| {
            b.iter_with_setup(
                || {
                    let dest = temp_dir.path().join(format!("dest_{label}"));
                    if dest.exists() {
                        fs::remove_dir_all(&dest).unwrap();
                    }
                    dest
                },
                |dest| {
                    let options = Options::new().strategy(strategy);
                    clone_tree(black_box(src), black_box(&dest), black_box(&options)).unwrap();
                },
            );
        },
    );
}

/// Calculate the total number of files produced by `create_test_tree`.
fn calculate_total_files(files_per_level: usize, depth: usize, dirs_per_level: usize) -> usize {
    if depth == 0 {
        return 0;
    }

    let mut total = files_per_level;

    if depth > 1 {
        for _i in 0..dirs_per_level {
            total += calculate_total_files(files_per_level, depth - 1, dirs_per_level);
        }
    }

    total
}

/// Entry point invoked by `cargo bench`.
fn main() {
    let mut criterion = Criterion::default().configure_from_args();
    benchmark_clone_tree(&mut criterion);
}

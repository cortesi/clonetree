use clonetree::{clone_tree, Options};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::fs;
use tempfile::TempDir;

// Constants for benchmark configuration
const SMALL_SIZE: usize = 1_000; // 1KB per file
const LARGE_SIZE: usize = 100_000; // 100KB per file

// Benchmark configuration
struct BenchConfig {
    name: &'static str,
    file_count: usize,
    depth: usize,
    fanout: usize,
    file_size: usize,
}

fn create_test_tree(
    dir: &std::path::Path,
    file_count: usize,
    depth: usize,
    fanout: usize,
    file_size: usize,
) {
    // Create a balanced tree structure
    if depth == 0 {
        return;
    }

    // Create files at this level
    for i in 0..file_count {
        // Create a file with file_size bytes
        let content = vec![b'X'; file_size];
        fs::write(dir.join(format!("file_{i}.txt")), content).unwrap();
    }

    // Create subdirectories and recurse
    if depth > 1 {
        for i in 0..fanout {
            let subdir = dir.join(format!("subdir_{i}"));
            fs::create_dir(&subdir).unwrap();
            create_test_tree(&subdir, file_count, depth - 1, fanout, file_size);
        }
    }
}

fn benchmark_clone_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone_tree");
    group.measurement_time(std::time::Duration::from_secs(30));

    // Define benchmark configurations
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

        group.bench_with_input(
            BenchmarkId::new(config.name, format!("{total_files}_files")),
            config,
            |b, config| {
                b.iter_with_setup(
                    || {
                        let temp_dir = TempDir::new().unwrap();
                        let src = temp_dir.path().join("src");
                        let dest = temp_dir.path().join("dest");
                        fs::create_dir(&src).unwrap();
                        create_test_tree(
                            &src,
                            config.file_count,
                            config.depth,
                            config.fanout,
                            config.file_size,
                        );
                        (temp_dir, src, dest)
                    },
                    |(temp_dir, src, dest)| {
                        let options = Options::new();
                        clone_tree(black_box(&src), black_box(&dest), black_box(&options)).unwrap();
                        temp_dir // Return to keep it alive
                    },
                );
            },
        );
    }

    group.finish();
}

fn calculate_total_files(files_per_level: usize, depth: usize, dirs_per_level: usize) -> usize {
    if depth == 0 {
        return 0;
    }

    let mut total = files_per_level; // Files at current level

    if depth > 1 {
        // Only recurse if depth > 1 (matching create_test_tree logic)
        for _i in 0..dirs_per_level {
            total += calculate_total_files(files_per_level, depth - 1, dirs_per_level);
        }
    }

    total
}

criterion_group!(benches, benchmark_clone_tree);
criterion_main!(benches);

//! A library for efficiently cloning directory trees with copy-on-write support.
//!
//! This crate provides functionality to clone entire directory structures while
//! leveraging filesystem-level copy-on-write (CoW) capabilities when available
//! through reflinks. This can result in significant space savings and improved
//! performance compared to traditional file copying.
//!
//! # Features
//!
//! - **Copy-on-Write Support**: Automatically uses reflinks when available on
//!   supported filesystems (Btrfs, XFS, APFS, etc.)
//! - **Glob Filtering**: Include or exclude files using glob patterns
//! - **Efficient Traversal**: Built on the `ignore` crate for fast directory walking
//! - **Type-Safe Errors**: Comprehensive error handling with descriptive error types
//!
//! # Example
//!
//! ```no_run
//! use clonetree::{clone_tree, Options};
//!
//! # fn main() -> clonetree::Result<()> {
//! // Clone a directory tree
//! let options = Options::new();
//! clone_tree("/source/path", "/destination/path", &options)?;
//!
//! // Clone with glob filters
//! let options = Options::new()
//!     .glob("**/*.rs")      // Include only Rust files
//!     .glob("!target/**");  // Exclude target directory
//! clone_tree("/source", "/dest", &options)?;
//!
//! // Clone with overwrite enabled
//! let options = Options::new()
//!     .overwrite(true);     // Allow overwriting existing files
//! clone_tree("/source", "/existing_dest", &options)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Validation
//!
//! The `clone_tree` function enforces the following constraints:
//! - Source path must exist and be a directory
//! - Destination path must not exist (unless `overwrite` option is enabled)
//!
//! These constraints are validated before any filesystem operations begin.

use ignore::{overrides::OverrideBuilder, WalkBuilder};
use reflink_copy::{reflink, reflink_or_copy};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Failed to create directory at {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to copy file from {src} to {dest}: {source}")]
    Copy {
        src: PathBuf,
        dest: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to remove existing destination at {path}: {source}")]
    RemoveDestination {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Invalid glob pattern '{pattern}': {source}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: ignore::Error,
    },

    #[error("Destination already exists: {path}")]
    DestinationExists { path: PathBuf },

    #[error("Destination is not a directory: {path}")]
    DestinationNotDirectory { path: PathBuf },

    #[error("Source is not a directory: {path}")]
    SourceNotDirectory { path: PathBuf },

    #[error("Source does not exist: {path}")]
    SourceNotFound { path: PathBuf },

    #[error("Operation error: {0}")]
    Other(String),

    #[error("Single-call cloning is only available on macOS")]
    SingleCallUnsupported,

    #[error("Single-call cloning cannot be combined with glob filters: {patterns:?}")]
    IncompatibleOptions { patterns: Vec<String> },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneStrategy {
    /// Choose the fastest supported strategy. On macOS this prefers a
    /// single `clonefile` call when possible; otherwise it falls back to the
    /// full directory traversal used on other platforms.
    Auto,
    /// Force a single system call to clone the root directory (macOS only).
    /// This cannot be combined with glob filters because the kernel copies the
    /// entire tree.
    SingleCall,
    /// Walk the tree in userspace and reflink each file individually.
    FullTraversal,
}

impl Default for CloneStrategy {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Default)]
pub struct Options {
    globs: Vec<String>,
    overwrite: bool,
    strategy: CloneStrategy,
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn glob<S: Into<String>>(mut self, pattern: S) -> Self {
        self.globs.push(pattern.into());
        self
    }

    pub fn overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }

    pub fn strategy(mut self, strategy: CloneStrategy) -> Self {
        self.strategy = strategy;
        self
    }
}

pub fn clone_tree<P: AsRef<Path>, Q: AsRef<Path>>(
    src: P,
    dest: Q,
    options: &Options,
) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    // Validate source exists
    if !src.exists() {
        return Err(Error::SourceNotFound {
            path: src.to_path_buf(),
        });
    }

    // Validate source is a directory
    if !src.is_dir() {
        return Err(Error::SourceNotDirectory {
            path: src.to_path_buf(),
        });
    }

    // Validate destination state early to keep semantics predictable
    if dest.exists() && !dest.is_dir() {
        return Err(Error::DestinationNotDirectory {
            path: dest.to_path_buf(),
        });
    }

    if dest.exists() && !options.overwrite {
        return Err(Error::DestinationExists {
            path: dest.to_path_buf(),
        });
    }

    let use_single_call = should_use_single_call(&options)?;

    if use_single_call {
        return clone_tree_single_call(src, dest, options);
    }

    clone_tree_full_traversal(src, dest, options)
}

fn should_use_single_call(options: &Options) -> Result<bool> {
    if !options.globs.is_empty() {
        if matches!(options.strategy, CloneStrategy::SingleCall) {
            return Err(Error::IncompatibleOptions {
                patterns: options.globs.clone(),
            });
        }
        return Ok(false);
    }

    match options.strategy {
        CloneStrategy::SingleCall => {
            if cfg!(target_os = "macos") {
                Ok(true)
            } else {
                Err(Error::SingleCallUnsupported)
            }
        }
        CloneStrategy::Auto => Ok(cfg!(target_os = "macos")),
        CloneStrategy::FullTraversal => Ok(false),
    }
}

#[cfg(target_os = "macos")]
fn clone_tree_single_call<P: AsRef<Path>, Q: AsRef<Path>>(
    src: P,
    dest: Q,
    options: &Options,
) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    if dest.exists() {
        if options.overwrite {
            remove_destination(dest)?;
        } else {
            return Err(Error::DestinationExists {
                path: dest.to_path_buf(),
            });
        }
    }

    if let Some(parent) = dest.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    reflink(src, dest).map_err(|source| Error::Copy {
        src: src.to_path_buf(),
        dest: dest.to_path_buf(),
        source,
    })
}

#[cfg(not(target_os = "macos"))]
fn clone_tree_single_call<P: AsRef<Path>, Q: AsRef<Path>>(
    _src: P,
    _dest: Q,
    _options: &Options,
) -> Result<()> {
    Err(Error::SingleCallUnsupported)
}

fn clone_tree_full_traversal<P: AsRef<Path>, Q: AsRef<Path>>(
    src: P,
    dest: Q,
    options: &Options,
) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    // Create destination directory if it doesn't exist
    if !dest.exists() {
        std::fs::create_dir_all(dest).map_err(|source| Error::CreateDirectory {
            path: dest.to_path_buf(),
            source,
        })?;
    }

    // Track created directories to avoid redundant create_dir_all calls
    let mut created_dirs = HashSet::new();
    created_dirs.insert(dest.to_path_buf());

    // Build walker with standard filters disabled
    let mut builder = WalkBuilder::new(src);
    builder.standard_filters(false);

    // Add glob patterns using overrides
    if !options.globs.is_empty() {
        let mut overrides = OverrideBuilder::new(src);
        for pattern in &options.globs {
            overrides
                .add(pattern)
                .map_err(|source| Error::InvalidGlob {
                    pattern: pattern.clone(),
                    source,
                })?;
        }
        builder.overrides(
            overrides
                .build()
                .map_err(|e| Error::Other(format!("Failed to build glob overrides: {e}")))?,
        );
    }

    // Walk the source directory
    for entry in builder.build() {
        let entry = entry.map_err(|source| Error::Other(format!("Walk error: {source}")))?;
        let path = entry.path();

        // Skip the root directory itself
        if path == src {
            continue;
        }

        // Calculate relative path and destination path
        let relative_path = path
            .strip_prefix(src)
            .map_err(|e| Error::Other(format!("Failed to strip prefix from path: {e}")))?;
        let dest_path = dest.join(relative_path);

        // Only process files
        if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            // Create parent directories if needed
            if let Some(parent) = dest_path.parent() {
                // Only create directory if we haven't created it before
                if !created_dirs.contains(parent) {
                    std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                    created_dirs.insert(parent.to_path_buf());
                }
            }

            // If overwrite is enabled and the destination exists, remove it first
            if options.overwrite && dest_path.exists() {
                if let Err(err) = std::fs::remove_file(&dest_path) {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        return Err(Error::Io(err));
                    }
                }
            }

            // Copy file using reflink when available
            reflink_or_copy(path, &dest_path).map_err(|source| Error::Copy {
                src: path.to_path_buf(),
                dest: dest_path.clone(),
                source,
            })?;
        }
    }

    Ok(())
}

fn remove_destination(dest: &Path) -> Result<()> {
    if dest.is_file() {
        std::fs::remove_file(dest).map_err(|source| Error::RemoveDestination {
            path: dest.to_path_buf(),
            source,
        })?;
    } else {
        std::fs::remove_dir_all(dest).map_err(|source| Error::RemoveDestination {
            path: dest.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn test_clone_tree_basic() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create source structure
        fs::create_dir_all(&src)?;
        write_file(&src.join("file1.txt"), "content1");
        fs::create_dir(src.join("subdir"))?;
        write_file(&src.join("subdir/file2.txt"), "content2");

        // Clone the tree
        let opts = Options::new();
        clone_tree(&src, &dest, &opts)?;

        // Verify structure
        assert!(dest.join("file1.txt").exists());
        assert!(dest.join("subdir/file2.txt").exists());
        assert_eq!(fs::read_to_string(dest.join("file1.txt"))?, "content1");
        assert_eq!(
            fs::read_to_string(dest.join("subdir/file2.txt"))?,
            "content2"
        );

        Ok(())
    }

    #[test]
    fn test_clone_tree_with_excludes() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create source structure
        fs::create_dir_all(&src)?;
        fs::write(src.join("file.txt"), "keep")?;
        fs::create_dir(src.join("target"))?;
        fs::write(src.join("target/build.out"), "exclude")?;
        fs::create_dir(src.join(".git"))?;
        write_file(&src.join(".git/config"), "exclude");

        // Clone with exclude globs (! prefix excludes)
        let opts = Options::new().glob("!target/**").glob("!.git/**");
        clone_tree(&src, &dest, &opts)?;

        // Verify excludes worked
        assert!(dest.join("file.txt").exists());
        assert!(!dest.join("target").exists());
        assert!(!dest.join(".git").exists());

        Ok(())
    }

    #[test]
    fn test_clone_tree_with_positive_globs() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create source structure
        fs::create_dir_all(&src)?;
        write_file(&src.join("include1.txt"), "include");
        write_file(&src.join("include2.txt"), "include");
        write_file(&src.join("exclude.log"), "exclude");
        fs::create_dir(src.join("data"))?;
        write_file(&src.join("data/file.txt"), "include");
        write_file(&src.join("data/debug.log"), "exclude");

        // Clone with positive globs (only include .txt files)
        let opts = Options::new().glob("**/*.txt");
        clone_tree(&src, &dest, &opts)?;

        // Verify only .txt files were included
        assert!(dest.join("include1.txt").exists());
        assert!(dest.join("include2.txt").exists());
        assert!(dest.join("data/file.txt").exists());
        assert!(!dest.join("exclude.log").exists());
        assert!(!dest.join("data/debug.log").exists());

        Ok(())
    }

    #[test]
    fn test_source_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("nonexistent");
        let dest = temp_dir.path().join("dest");

        let opts = Options::new();
        let result = clone_tree(&src, &dest, &opts);

        assert!(matches!(result, Err(Error::SourceNotFound { .. })));
    }

    #[test]
    fn test_source_not_directory() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("file.txt");
        let dest = temp_dir.path().join("dest");

        // Create source as a file, not a directory
        fs::write(&src, "content").unwrap();

        let opts = Options::new();
        let result = clone_tree(&src, &dest, &opts);

        assert!(matches!(result, Err(Error::SourceNotDirectory { .. })));
    }

    #[test]
    fn test_destination_exists() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create both source and destination directories
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let opts = Options::new();
        let result = clone_tree(&src, &dest, &opts);

        assert!(matches!(result, Err(Error::DestinationExists { .. })));
    }

    #[test]
    fn test_overwrite_existing_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create source structure
        fs::create_dir_all(&src)?;
        write_file(&src.join("file1.txt"), "new_content1");
        write_file(&src.join("file2.txt"), "new_content2");
        fs::create_dir(src.join("subdir"))?;
        write_file(&src.join("subdir/file3.txt"), "new_content3");

        // Create destination with some existing files
        fs::create_dir_all(&dest)?;
        write_file(&dest.join("file1.txt"), "old_content1");
        write_file(&dest.join("existing_file.txt"), "should_remain");
        fs::create_dir(dest.join("subdir"))?;
        write_file(&dest.join("subdir/file3.txt"), "old_content3");
        write_file(&dest.join("subdir/existing_file.txt"), "should_remain");

        // Clone with overwrite enabled
        let opts = Options::new().overwrite(true);

        let use_single_call = super::should_use_single_call(&opts).unwrap_or(false);
        clone_tree(&src, &dest, &opts)?;

        // Verify overwrites happened
        assert!(dest.join("file1.txt").exists(), "file1.txt missing");
        assert_eq!(fs::read_to_string(dest.join("file1.txt"))?, "new_content1");
        assert!(dest.join("file2.txt").exists(), "file2.txt missing");
        assert_eq!(fs::read_to_string(dest.join("file2.txt"))?, "new_content2");
        assert!(dest.join("subdir/file3.txt").exists(), "file3.txt missing");
        assert_eq!(
            fs::read_to_string(dest.join("subdir/file3.txt"))?,
            "new_content3"
        );

        // Verify existing files that weren't in source remain untouched
        if use_single_call {
            assert!(
                !dest.join("existing_file.txt").exists(),
                "single-call clone should replace destination tree"
            );
            assert!(
                !dest.join("subdir/existing_file.txt").exists(),
                "single-call clone should replace destination tree"
            );
        } else {
            assert!(
                dest.join("existing_file.txt").exists(),
                "existing_file.txt missing"
            );
            assert_eq!(
                fs::read_to_string(dest.join("existing_file.txt"))?,
                "should_remain"
            );
            assert!(
                dest.join("subdir/existing_file.txt").exists(),
                "subdir/existing_file.txt missing"
            );
            assert_eq!(
                fs::read_to_string(dest.join("subdir/existing_file.txt"))?,
                "should_remain"
            );
        }

        Ok(())
    }

    #[test]
    fn single_call_strategy_rejected_with_globs() {
        let opts = Options::new()
            .glob("**/*.rs")
            .strategy(CloneStrategy::SingleCall);
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        fs::create_dir_all(&src).unwrap();

        let result = clone_tree(&src, &dest, &opts);
        assert!(matches!(result, Err(Error::IncompatibleOptions { .. })));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn single_call_overwrite_replaces_destination_dir() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        fs::create_dir_all(&src)?;
        write_file(&src.join("file.txt"), "new");

        fs::create_dir_all(&dest)?;
        write_file(&dest.join("file.txt"), "old");
        write_file(&dest.join("old_only.txt"), "stay?");

        let opts = Options::new()
            .strategy(CloneStrategy::SingleCall)
            .overwrite(true);
        clone_tree(&src, &dest, &opts)?;

        assert_eq!(fs::read_to_string(dest.join("file.txt"))?, "new");
        assert!(!dest.join("old_only.txt").exists());
        Ok(())
    }
}

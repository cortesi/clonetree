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
use reflink_copy::reflink_or_copy;
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

    #[error("Invalid glob pattern '{pattern}': {source}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: ignore::Error,
    },

    #[error("Destination already exists: {path}")]
    DestinationExists { path: PathBuf },

    #[error("Source is not a directory: {path}")]
    SourceNotDirectory { path: PathBuf },

    #[error("Source does not exist: {path}")]
    SourceNotFound { path: PathBuf },

    #[error("Operation error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Default)]
pub struct Options {
    globs: Vec<String>,
    overwrite: bool,
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

    // Validate destination
    if dest.exists() && !options.overwrite {
        return Err(Error::DestinationExists {
            path: dest.to_path_buf(),
        });
    }

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
                std::fs::remove_file(&dest_path).map_err(Error::Io)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_clone_tree_basic() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create source structure
        fs::create_dir_all(&src)?;
        fs::write(src.join("file1.txt"), "content1")?;
        fs::create_dir(src.join("subdir"))?;
        fs::write(src.join("subdir/file2.txt"), "content2")?;

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
        fs::write(src.join(".git/config"), "exclude")?;

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
        fs::write(src.join("include1.txt"), "include")?;
        fs::write(src.join("include2.txt"), "include")?;
        fs::write(src.join("exclude.log"), "exclude")?;
        fs::create_dir(src.join("data"))?;
        fs::write(src.join("data/file.txt"), "include")?;
        fs::write(src.join("data/debug.log"), "exclude")?;

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
        fs::write(src.join("file1.txt"), "new_content1")?;
        fs::write(src.join("file2.txt"), "new_content2")?;
        fs::create_dir(src.join("subdir"))?;
        fs::write(src.join("subdir/file3.txt"), "new_content3")?;

        // Create destination with some existing files
        fs::create_dir_all(&dest)?;
        fs::write(dest.join("file1.txt"), "old_content1")?;
        fs::write(dest.join("existing_file.txt"), "should_remain")?;
        fs::create_dir(dest.join("subdir"))?;
        fs::write(dest.join("subdir/file3.txt"), "old_content3")?;
        fs::write(dest.join("subdir/existing_file.txt"), "should_remain")?;

        // Clone with overwrite enabled
        let opts = Options::new().overwrite(true);
        clone_tree(&src, &dest, &opts)?;

        // Verify overwrites happened
        assert_eq!(fs::read_to_string(dest.join("file1.txt"))?, "new_content1");
        assert_eq!(fs::read_to_string(dest.join("file2.txt"))?, "new_content2");
        assert_eq!(
            fs::read_to_string(dest.join("subdir/file3.txt"))?,
            "new_content3"
        );

        // Verify existing files that weren't in source remain untouched
        assert_eq!(
            fs::read_to_string(dest.join("existing_file.txt"))?,
            "should_remain"
        );
        assert_eq!(
            fs::read_to_string(dest.join("subdir/existing_file.txt"))?,
            "should_remain"
        );

        Ok(())
    }
}

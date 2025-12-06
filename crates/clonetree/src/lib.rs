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
//! - **Symlink Preservation**: Symbolic links are recreated with their original targets
//! - **Empty Directory Preservation**: Empty directories in the source tree are preserved
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
//!
//! # Symlink Handling
//!
//! Symbolic links in the source tree are preserved as symbolic links in the
//! destination. The link targets are copied verbatim (not resolved), so relative
//! symlinks maintain their relative paths.
//!
//! - **`FullTraversal` strategy**: Symlinks are recreated using platform-native
//!   symlink creation (`symlink(2)` on Unix, `CreateSymbolicLink` on Windows).
//! - **`SingleCall` strategy** (macOS only): The kernel's `clonefile(2)` preserves
//!   symlinks automatically as part of the atomic directory clone.

use std::{
    collections::HashSet,
    env, fs, io,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    result,
};

use ignore::{overrides::OverrideBuilder, WalkBuilder};
use reflink_copy::{reflink, reflink_or_copy};
use thiserror::Error;

/// Errors that can occur while cloning directory trees.
#[derive(Error, Debug)]
pub enum Error {
    /// Propagated I/O error from an underlying filesystem operation.
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Failed to create directory at {path}: {source}")]
    /// Creation of a destination directory failed.
    CreateDirectory {
        /// Destination directory path that failed to be created.
        path: PathBuf,
        #[source]
        /// Source I/O error returned by the filesystem.
        source: io::Error,
    },

    #[error("Failed to copy file from {src} to {dest}: {source}")]
    /// Copying or reflinking a file from source to destination failed.
    Copy {
        /// Source file being copied.
        src: PathBuf,
        /// Destination path for the copy.
        dest: PathBuf,
        #[source]
        /// Underlying I/O error from the copy operation.
        source: io::Error,
    },

    #[error("Failed to remove existing destination at {path}: {source}")]
    /// Removing a pre-existing destination entry failed.
    RemoveDestination {
        /// Path that could not be removed.
        path: PathBuf,
        #[source]
        /// Underlying I/O error from removal.
        source: io::Error,
    },

    #[error("Invalid glob pattern '{pattern}': {source}")]
    /// Supplied glob pattern could not be parsed by the ignore crate.
    InvalidGlob {
        /// Glob expression supplied by the caller.
        pattern: String,
        #[source]
        /// Parser error from the ignore crate.
        source: ignore::Error,
    },

    #[error("Destination already exists: {path}")]
    /// Destination already exists and overwrite was not requested.
    DestinationExists {
        /// Conflicting destination path.
        path: PathBuf,
    },

    #[error("Destination is not a directory: {path}")]
    /// Destination exists but is not a directory.
    DestinationNotDirectory {
        /// Destination path that is not a directory.
        path: PathBuf,
    },

    #[error("Source is not a directory: {path}")]
    /// Source path exists but is not a directory.
    SourceNotDirectory {
        /// Source path that is not a directory.
        path: PathBuf,
    },

    #[error("Source does not exist: {path}")]
    /// Source path does not exist.
    SourceNotFound {
        /// Missing source path.
        path: PathBuf,
    },

    #[error("Operation error: {0}")]
    /// Generic error message for unexpected conditions.
    Other(String),

    #[error("Single-call cloning is only available on macOS")]
    /// Single-call cloning was requested on an unsupported platform.
    SingleCallUnsupported,

    #[error("Single-call cloning cannot be combined with glob filters: {patterns:?}")]
    /// Single-call cloning cannot be used when glob filters are present.
    IncompatibleOptions {
        /// Glob patterns provided alongside the single-call strategy.
        patterns: Vec<String>,
    },

    #[error("Source and destination resolve to the same path: {path}")]
    /// Source and destination paths are identical after resolution.
    IdenticalPaths {
        /// Resolved absolute path shared by source and destination.
        path: PathBuf,
    },

    #[error("Destination lies inside the source tree: src={src}, dest={dest}")]
    /// Destination is a descendant of the source path.
    DestinationInsideSource {
        /// Canonicalized source path.
        src: PathBuf,
        /// Canonicalized destination path.
        dest: PathBuf,
    },

    #[error("Source lies inside the destination tree: src={src}, dest={dest}")]
    /// Source is a descendant of the destination path.
    SourceInsideDestination {
        /// Canonicalized source path.
        src: PathBuf,
        /// Canonicalized destination path.
        dest: PathBuf,
    },
}

/// Convenience result type for clonetree operations.
pub type Result<T> = result::Result<T, Error>;

/// Strategy used when cloning a directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloneStrategy {
    /// Choose the fastest supported strategy. On macOS this prefers a
    /// single `clonefile` call when possible; otherwise it falls back to the
    /// full directory traversal used on other platforms.
    #[default]
    Auto,
    /// Force a single system call to clone the root directory (macOS only).
    /// This cannot be combined with glob filters because the kernel copies the
    /// entire tree.
    SingleCall,
    /// Walk the tree in userspace and reflink each file individually.
    FullTraversal,
}

/// Builder-style options that control cloning behaviour.
#[derive(Debug, Default)]
pub struct Options {
    /// Glob patterns applied to the source tree (negated patterns exclude).
    globs: Vec<String>,
    /// Whether existing destination entries should be overwritten.
    overwrite: bool,
    /// How files and directories should be cloned.
    strategy: CloneStrategy,
}

impl Options {
    /// Construct a new options set with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a glob pattern to include or exclude when traversing.
    pub fn glob<S: Into<String>>(mut self, pattern: S) -> Self {
        self.globs.push(pattern.into());
        self
    }

    /// Enable or disable overwriting of existing destination entries.
    pub fn overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }

    /// Specify the cloning strategy to use.
    pub fn strategy(mut self, strategy: CloneStrategy) -> Self {
        self.strategy = strategy;
        self
    }
}

/// Clone a directory tree from `src` to `dest` using the provided `options`.
/// Validates inputs up-front and selects the appropriate cloning strategy.
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

    // Resolve absolute paths to guard against unsafe relationships
    let src_canon = canonicalize_existing(src)?;
    let dest_resolved = canonicalize_for_destination(dest)?;

    if src_canon == dest_resolved {
        return Err(Error::IdenticalPaths { path: src_canon });
    }

    if dest_resolved.starts_with(&src_canon) {
        return Err(Error::DestinationInsideSource {
            src: src_canon,
            dest: dest_resolved,
        });
    }

    if src_canon.starts_with(&dest_resolved) {
        return Err(Error::SourceInsideDestination {
            src: src_canon,
            dest: dest_resolved,
        });
    }

    if dest.exists() && !options.overwrite {
        return Err(Error::DestinationExists {
            path: dest.to_path_buf(),
        });
    }

    let use_single_call = should_use_single_call(options)?;

    if use_single_call {
        return clone_tree_single_call(src, dest, options);
    }

    clone_tree_full_traversal(src, dest, options)
}

/// Determine whether to use the single-call strategy based on options and platform.
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
/// Use the platform single-call clone when available (macOS `clonefile`).
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
            fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
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
/// Stub for platforms that do not support single-call cloning.
fn clone_tree_single_call<P: AsRef<Path>, Q: AsRef<Path>>(
    _src: P,
    _dest: Q,
    _options: &Options,
) -> Result<()> {
    Err(Error::SingleCallUnsupported)
}

/// Walk the source tree and reflink or copy each file into `dest`.
fn clone_tree_full_traversal<P: AsRef<Path>, Q: AsRef<Path>>(
    src: P,
    dest: Q,
    options: &Options,
) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    // Create destination directory if it doesn't exist
    if !dest.exists() {
        fs::create_dir_all(dest).map_err(|source| Error::CreateDirectory {
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

        // Create parent directories if needed
        if let Some(parent) = dest_path.parent() {
            if !created_dirs.contains(parent) {
                fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
                    path: parent.to_path_buf(),
                    source,
                })?;
                created_dirs.insert(parent.to_path_buf());
            }
        }

        // Handle symlinks by recreating them
        if entry.path_is_symlink() {
            let target = fs::read_link(path).map_err(|source| Error::Copy {
                src: path.to_path_buf(),
                dest: dest_path.clone(),
                source,
            })?;

            // If overwrite is enabled and the destination exists, remove it first
            if options.overwrite && dest_path.symlink_metadata().is_ok() {
                remove_dest_entry(&dest_path)?;
            }

            create_symlink(&target, &dest_path, path).map_err(|source| Error::Copy {
                src: path.to_path_buf(),
                dest: dest_path.clone(),
                source,
            })?;
        } else if entry.file_type().is_some_and(|ft| ft.is_file()) {
            // If overwrite is enabled and the destination exists, remove it first
            if options.overwrite && dest_path.exists() {
                if let Err(err) = fs::remove_file(&dest_path) {
                    if err.kind() != ErrorKind::NotFound {
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
        } else if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            // Create directories explicitly to preserve empty directories
            if !created_dirs.contains(&dest_path) {
                fs::create_dir_all(&dest_path).map_err(|source| Error::CreateDirectory {
                    path: dest_path.clone(),
                    source,
                })?;
                created_dirs.insert(dest_path);
            }
        }
    }

    Ok(())
}

/// Remove an existing destination file or directory tree.
fn remove_destination(dest: &Path) -> Result<()> {
    if dest.is_file() {
        fs::remove_file(dest).map_err(|source| Error::RemoveDestination {
            path: dest.to_path_buf(),
            source,
        })?;
    } else {
        fs::remove_dir_all(dest).map_err(|source| Error::RemoveDestination {
            path: dest.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// Create a symbolic link at `dest` pointing to `target`.
///
/// The `original_path` is used on Windows to determine if the target is a directory.
#[cfg(unix)]
#[allow(clippy::absolute_paths)] // Platform-specific import not worth conditional use
fn create_symlink(target: &Path, dest: &Path, _original_path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, dest)
}

/// Create a symbolic link at `dest` pointing to `target`.
///
/// The `original_path` is used on Windows to determine if the target is a directory.
#[cfg(windows)]
#[allow(clippy::absolute_paths)] // Platform-specific import not worth conditional use
fn create_symlink(target: &Path, dest: &Path, original_path: &Path) -> io::Result<()> {
    // On Windows, we need to know if the target is a directory
    let target_is_dir = original_path
        .parent()
        .map(|p| p.join(target))
        .and_then(|full| fs::metadata(full).ok())
        .is_some_and(|m| m.is_dir());

    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, dest)
    } else {
        std::os::windows::fs::symlink_file(target, dest)
    }
}

/// Remove a destination entry (file, symlink, or directory) before overwriting.
fn remove_dest_entry(dest: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(dest).map_err(|source| Error::RemoveDestination {
        path: dest.to_path_buf(),
        source,
    })?;

    if meta.is_dir() {
        fs::remove_dir_all(dest)
    } else {
        // Files and symlinks
        fs::remove_file(dest)
    }
    .map_err(|source| Error::RemoveDestination {
        path: dest.to_path_buf(),
        source,
    })
}

/// Canonicalize an existing path, ensuring symlinks are resolved.
fn canonicalize_existing(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(Error::from)
}

/// Convert a path to an absolute, cleaned form even if the final component does not yet exist.
fn canonicalize_for_destination(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return canonicalize_existing(path);
    }

    let absolute = absolutize(path)?;

    let mut existing = absolute.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| Error::Other("Destination path must include a parent".to_owned()))?;
    }

    let resolved_existing = canonicalize_existing(existing)?;
    let remainder = absolute
        .strip_prefix(existing)
        .map_err(|e| Error::Other(format!("Failed to strip prefix: {e}")))?;

    Ok(clean_path(&resolved_existing.join(remainder)))
}

/// Produce an absolute path with `.`/`..` removed, based on the current directory.
fn absolutize(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };

    Ok(clean_path(&joined))
}

/// Normalize a path by removing `.` and `..` components without touching symlinks.
fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            other => cleaned.push(other),
        }
    }

    cleaned
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn write_file(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
    }

    fn mkdir(path: &Path) {
        fs::create_dir_all(path).unwrap();
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
        // Note: !dir/ excludes the directory itself, !dir/** only excludes contents
        let opts = Options::new().glob("!target/").glob("!.git/");
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

    #[test]
    fn identical_paths_are_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("dir");
        mkdir(&src);

        let opts = Options::new();
        let result = clone_tree(&src, &src, &opts);

        assert!(matches!(result, Err(Error::IdenticalPaths { .. })));
    }

    #[test]
    fn destination_inside_source_is_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("src");
        let dest = src.join("nested/dest");
        mkdir(&src);

        let opts = Options::new().overwrite(true);
        let result = clone_tree(&src, &dest, &opts);

        assert!(matches!(result, Err(Error::DestinationInsideSource { .. })));
    }

    #[test]
    fn source_inside_destination_is_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let dest = temp_dir.path().join("dest");
        let src = dest.join("inner/src");
        mkdir(&src);

        let opts = Options::new().overwrite(true);
        let result = clone_tree(&src, &dest, &opts);

        assert!(matches!(result, Err(Error::SourceInsideDestination { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_recreated() -> Result<()> {
        use std::os::unix::fs as unix_fs;

        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create source structure with symlinks
        fs::create_dir_all(&src)?;
        write_file(&src.join("file.txt"), "content");
        unix_fs::symlink("file.txt", src.join("link_to_file.txt"))?;

        // Create a subdirectory and symlink to it
        fs::create_dir(src.join("subdir"))?;
        write_file(&src.join("subdir/nested.txt"), "nested");
        unix_fs::symlink("subdir", src.join("link_to_dir"))?;

        // Clone the tree
        let opts = Options::new().strategy(CloneStrategy::FullTraversal);
        clone_tree(&src, &dest, &opts)?;

        // Verify file was copied
        assert!(dest.join("file.txt").exists());
        assert_eq!(fs::read_to_string(dest.join("file.txt"))?, "content");

        // Verify symlink to file was recreated as a symlink
        let link_meta = fs::symlink_metadata(dest.join("link_to_file.txt"))?;
        assert!(link_meta.file_type().is_symlink(), "should be a symlink");
        assert_eq!(fs::read_link(dest.join("link_to_file.txt"))?, PathBuf::from("file.txt"));
        // Verify the symlink works
        assert_eq!(fs::read_to_string(dest.join("link_to_file.txt"))?, "content");

        // Verify symlink to directory was recreated
        let dir_link_meta = fs::symlink_metadata(dest.join("link_to_dir"))?;
        assert!(dir_link_meta.file_type().is_symlink(), "should be a symlink");
        assert_eq!(fs::read_link(dest.join("link_to_dir"))?, PathBuf::from("subdir"));

        Ok(())
    }

    /// Verify that SingleCall strategy on macOS preserves symlinks via clonefile(2).
    /// This documents that the kernel handles symlinks correctly in single-call mode.
    #[cfg(target_os = "macos")]
    #[test]
    fn single_call_preserves_symlinks() -> Result<()> {
        use std::os::unix::fs as unix_fs;

        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        // Create source structure with symlinks
        fs::create_dir_all(&src)?;
        write_file(&src.join("file.txt"), "content");
        unix_fs::symlink("file.txt", src.join("link_to_file.txt"))?;
        fs::create_dir(src.join("subdir"))?;
        unix_fs::symlink("subdir", src.join("link_to_dir"))?;

        // Clone using SingleCall strategy
        let opts = Options::new().strategy(CloneStrategy::SingleCall);
        clone_tree(&src, &dest, &opts)?;

        // Verify symlinks are preserved (clonefile preserves symlinks)
        let link_meta = fs::symlink_metadata(dest.join("link_to_file.txt"))?;
        assert!(link_meta.file_type().is_symlink(), "should be a symlink");
        assert_eq!(
            fs::read_link(dest.join("link_to_file.txt"))?,
            PathBuf::from("file.txt")
        );

        let dir_link_meta = fs::symlink_metadata(dest.join("link_to_dir"))?;
        assert!(dir_link_meta.file_type().is_symlink(), "should be a symlink");
        assert_eq!(
            fs::read_link(dest.join("link_to_dir"))?,
            PathBuf::from("subdir")
        );

        Ok(())
    }

    /// Test that file permissions are preserved during cloning.
    #[cfg(unix)]
    #[test]
    fn file_permissions_are_preserved() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        fs::create_dir_all(&src)?;

        // Create a file with specific permissions (executable)
        let executable = src.join("script.sh");
        write_file(&executable, "#!/bin/bash\necho hello");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;

        // Create a file with read-only permissions
        let readonly = src.join("readonly.txt");
        write_file(&readonly, "read only content");
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o444))?;

        // Create a file with default permissions for comparison
        let normal = src.join("normal.txt");
        write_file(&normal, "normal content");

        // Clone the tree
        let opts = Options::new().strategy(CloneStrategy::FullTraversal);
        clone_tree(&src, &dest, &opts)?;

        // Check executable permissions
        let dest_exec_perms = fs::metadata(dest.join("script.sh"))?.permissions().mode();
        let src_exec_perms = fs::metadata(&executable)?.permissions().mode();
        assert_eq!(
            dest_exec_perms & 0o777,
            src_exec_perms & 0o777,
            "executable permissions should be preserved"
        );

        // Check read-only permissions
        let dest_ro_perms = fs::metadata(dest.join("readonly.txt"))?.permissions().mode();
        let src_ro_perms = fs::metadata(&readonly)?.permissions().mode();
        assert_eq!(
            dest_ro_perms & 0o777,
            src_ro_perms & 0o777,
            "read-only permissions should be preserved"
        );

        Ok(())
    }

    /// Test that empty directories are preserved during cloning.
    #[test]
    fn empty_directories_are_preserved() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let src = temp_dir.path().join("src");
        let dest = temp_dir.path().join("dest");

        fs::create_dir_all(&src)?;

        // Create some empty directories
        fs::create_dir(src.join("empty1"))?;
        fs::create_dir(src.join("empty2"))?;
        fs::create_dir_all(src.join("nested/empty"))?;

        // Create a non-empty directory for comparison
        fs::create_dir(src.join("nonempty"))?;
        write_file(&src.join("nonempty/file.txt"), "content");

        // Clone the tree
        let opts = Options::new().strategy(CloneStrategy::FullTraversal);
        clone_tree(&src, &dest, &opts)?;

        // Check that empty directories exist
        assert!(
            dest.join("empty1").exists() && dest.join("empty1").is_dir(),
            "empty1 directory should exist"
        );
        assert!(
            dest.join("empty2").exists() && dest.join("empty2").is_dir(),
            "empty2 directory should exist"
        );
        assert!(
            dest.join("nested/empty").exists() && dest.join("nested/empty").is_dir(),
            "nested/empty directory should exist"
        );
        assert!(
            dest.join("nonempty/file.txt").exists(),
            "nonempty directory with file should exist"
        );

        Ok(())
    }
}

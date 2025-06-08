use ignore::{overrides::OverrideBuilder, WalkBuilder};
use reflink_copy::reflink_or_copy;
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

    #[error("Operation error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Default)]
pub struct Options {
    globs: Vec<String>,
    no_reflink: bool,
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn glob<S: Into<String>>(mut self, pattern: S) -> Self {
        self.globs.push(pattern.into());
        self
    }

    pub fn no_reflink(mut self, no_reflink: bool) -> Self {
        self.no_reflink = no_reflink;
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

    // Create destination directory
    std::fs::create_dir_all(dest).map_err(|e| Error::CreateDirectory {
        path: dest.to_path_buf(),
        source: e,
    })?;

    // Build walker with standard filters disabled
    let mut builder = WalkBuilder::new(src);
    builder.standard_filters(false);

    // Add glob patterns using overrides
    if !options.globs.is_empty() {
        let mut overrides = OverrideBuilder::new(src);
        for pattern in &options.globs {
            overrides.add(pattern).map_err(|e| Error::InvalidGlob {
                pattern: pattern.clone(),
                source: e,
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
        let entry = entry.map_err(|e| Error::Other(format!("Walk error: {e}")))?;
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

        // Copy file or create directory
        if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            // Create parent directories if needed
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::CreateDirectory {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }

            // Copy file
            if options.no_reflink {
                std::fs::copy(path, &dest_path).map_err(|e| Error::Copy {
                    src: path.to_path_buf(),
                    dest: dest_path.clone(),
                    source: e,
                })?;
            } else {
                reflink_or_copy(path, &dest_path).map_err(|e| Error::Copy {
                    src: path.to_path_buf(),
                    dest: dest_path.clone(),
                    source: e,
                })?;
            }
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
}

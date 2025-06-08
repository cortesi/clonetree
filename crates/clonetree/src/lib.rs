use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use reflink_copy::reflink_or_copy;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Options {
    excludes: Vec<String>,
    force: bool,
    no_reflink: bool,
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn exclude<S: Into<String>>(mut self, pattern: S) -> Self {
        self.excludes.push(pattern.into());
        self
    }

    pub fn force(mut self, force: bool) -> Self {
        self.force = force;
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

    // If force is enabled and destination exists, remove it
    if options.force && dest.exists() {
        if dest.is_dir() {
            std::fs::remove_dir_all(dest)?;
        } else {
            std::fs::remove_file(dest)?;
        }
    }

    // Create destination directory
    std::fs::create_dir_all(dest)?;

    // Build glob set for exclusions
    let mut glob_set_builder = GlobSetBuilder::new();
    for pattern in &options.excludes {
        glob_set_builder.add(Glob::new(pattern)?);
    }
    let glob_set = glob_set_builder.build()?;

    // Build walker with standard filters disabled
    let mut builder = WalkBuilder::new(src);
    builder.standard_filters(false);

    // Walk the source directory
    for entry in builder.build() {
        let entry = entry?;
        let path = entry.path();

        // Skip the root directory itself
        if path == src {
            continue;
        }

        // Calculate relative path
        let relative_path = path.strip_prefix(src)?;

        // Check if path matches any exclude pattern
        let mut skip = false;

        // Check the path itself and all its parent components
        for ancestor in relative_path.ancestors() {
            if glob_set.is_match(ancestor) {
                skip = true;
                break;
            }
        }

        // Also check with the path as a directory (append / to match directory patterns)
        let as_dir = format!("{}/", relative_path.display());
        if glob_set.is_match(&as_dir) {
            skip = true;
        }

        if skip {
            continue;
        }

        // Destination path
        let dest_path = dest.join(relative_path);

        // Copy file or create directory
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            // Create directory only if we won't skip its contents
            std::fs::create_dir_all(&dest_path)?;
        } else if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            // Create parent directories if needed
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Copy file
            if options.no_reflink {
                std::fs::copy(path, &dest_path)?;
            } else {
                reflink_or_copy(path, &dest_path)?;
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

        // Clone with excludes
        let opts = Options::new().exclude("target/**").exclude(".git/**");
        clone_tree(&src, &dest, &opts)?;

        // Verify excludes worked
        assert!(dest.join("file.txt").exists());
        assert!(!dest.join("target").exists());
        assert!(!dest.join(".git").exists());

        Ok(())
    }
}

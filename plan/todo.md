# Clonetree Improvement Plan

## Stage 1: Core Correctness Issues

These issues affect the fundamental correctness of clone operations.

- [x] **Handle symlinks during traversal**
  - Decided to recreate symlinks with their original targets
  - Added symlink handling in `clone_tree_full_traversal`
  - Added `symlinks_are_recreated` test (FullTraversal strategy)
  - Added `single_call_preserves_symlinks` test (SingleCall strategy, macOS)
  - Documented behavior in module docs and README

- [ ] **Preserve empty directories**
  - Modify traversal to create directories, not just file parents
  - Process directory entries in addition to file entries (lib.rs:431)
  - Add test case for empty directory preservation

- [ ] **Preserve file permissions/metadata**
  - Copy file permissions after `reflink_or_copy` (Unix: mode bits)
  - Consider timestamps (mtime/atime) preservation
  - Add tests verifying permission preservation

## Stage 2: API Consistency

These issues affect predictability and user expectations.

- [ ] **Document or unify overwrite semantics**
  - SingleCall: replaces entire destination tree
  - FullTraversal: merges, only overwrites conflicting files
  - Option A: Document the difference clearly in API docs and README
  - Option B: Add an `OverwriteMode` enum (`Replace` vs `Merge`)
  - Update README to reflect accurate overwrite behavior

- [ ] **Add `WalkError` variant to Error enum**
  - Replace `Error::Other` for walk errors (lib.rs:416) with dedicated variant
  - Include path context in the error
  - Preserve underlying `ignore::Error` as source

## Stage 3: CLI Improvements

- [ ] **Add `--overwrite` flag to ctree**
  - Add `-o, --overwrite` argument to Args struct
  - Wire through to `Options::overwrite()`
  - Update CLI help text

- [ ] **Add `--dry-run` flag to ctree**
  - Show what would be copied without copying
  - Useful for verifying glob patterns before execution

## Stage 4: Robustness

- [ ] **Handle `clean_path` edge cases**
  - Guard against popping past root (lib.rs:521-535)
  - Return error or preserve root component when path normalizes to empty

- [ ] **Address TOCTOU race condition**
  - Consider atomic creation patterns where possible
  - Document the limitation for concurrent scenarios
  - For single-call strategy on macOS, `clonefile` is already atomic

## Stage 5: Testing & Documentation

- [ ] **Add integration tests**
  - Cross-platform behavior verification
  - Real filesystem edge cases (permissions, special files)

- [x] **Add symlink test cases**
  - Symlink to file (done in `symlinks_are_recreated`)
  - Symlink to directory (done in `symlinks_are_recreated`)
  - [ ] Broken symlinks (future)
  - [ ] Symlink loops (future)

- [ ] **Add empty directory test cases**
  - Nested empty directories
  - Mix of empty and non-empty directories

- [x] **Review and update README**
  - [ ] Accurate description of overwrite behavior
  - [x] Document symlink handling policy
  - [ ] Document metadata preservation policy

## Stage 6: Future Enhancements (Optional)

- [ ] **Progress callbacks**
  - Allow callers to receive progress updates during clone
  - Useful for CLI progress bars and cancellation

- [ ] **Parallel file copying**
  - Use rayon or similar for parallel file operations in FullTraversal
  - Could significantly speed up large trees on SSD

- [ ] **Extended attribute (xattr) support**
  - Preserve extended attributes where supported
  - macOS: resource forks, Linux: security labels

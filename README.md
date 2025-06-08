# clonetree

**A copy‑on‑write directory library for Rust**

`clonetree` is a **Rust crate** for fast directory duplication. It uses
filesystem‑level *reflinks* so large trees are cloned in quickly and consume no
extra space until files diverge.

* macOS → `clonefile(2)` on APFS
* Linux → `FICLONE` / `copy_file_range()` on Btrfs, XFS (`reflink=1`), overlayfs, etc.

When reflinks are unavailable the crate falls back to a normal copy, so it
works everywhere—just faster where supported.

---

## Library quick start

Add the dependency:

```bash
cargo add clonetree
```

Clone the current working directory into `./sandbox`, skipping build artefacts:

```rust
use clonetree::{clone_tree, Options};

fn main() -> anyhow::Result<()> {
    let opts = Options::new()
        .glob("!.git/**")
        .glob("!target/**");

    clone_tree("./", "./sandbox", &opts)?;
    Ok(())
}
```

### Highlights

* **Fast COW clone** on APFS, Btrfs, XFS, overlayfs…
* **Flexible glob patterns** to include/exclude files (uses `!` prefix for exclusions).
* **Graceful fallback** to `std::fs::copy` when reflinks are unsupported.
* **Force‑overwrite** option to replace existing destinations.
* **Pure Rust**, no unsafe code, minimal deps.

---

## `ctree` ‑ the companion CLI

The crate ships with a convenience binary so users can benefit without writing code.

### Install

```bash
cargo install ctree
```

### Basic usage

```bash
ctree <SRC> <DEST> [OPTIONS]

OPTIONS:
  -x, --exclude <GLOB>   Skip paths matching the glob (repeatable)
  -f, --force            Remove DEST if it already exists
      --no-reflink       Disable reflink, perform a regular copy
  -q, --quiet            Suppress progress output
  -h, --help             Show this help
```

Example: snapshot a repo while excluding Git metadata and build output:

```bash
ctree . ./sandbox \
  --exclude .git/** \
  --exclude target/**
```

---

## Filesystem support matrix

| OS / FS                      | Reflink supported | Behaviour          |
| ---------------------------- | ----------------- | ------------------ |
| macOS 11+ / APFS             | ✅ via `clonefile` | COW clone          |
| Linux 5.3+ / Btrfs           | ✅ via `FICLONE`   | COW clone          |
| Linux 5.4+ / XFS reflink=1   | ✅ via `FICLONE`   | COW clone          |
| ext4 (default Ubuntu/Fedora) | ❌                 | Byte‑for‑byte copy |


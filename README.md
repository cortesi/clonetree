# clonetree

**A copy‑on‑write directory library for Rust**

`clonetree` is a **Rust crate** for fast directory duplication. It uses
filesystem‑level *reflinks* so large trees are cloned quickly and consume no
extra space until files diverge.

---

## Highlights

* **Fast copy‑on‑write clone** on APFS, Btrfs, XFS, bcachefs, overlayfs, ReFS…
* **Powered by [`reflink-copy`](https://github.com/cargo-bins/reflink-copy)** for portable block‑cloning.
* **Flexible glob patterns** to include/exclude files (uses `!` prefix for exclusions).
* **Graceful fallback** to `std::fs::copy` when reflinks are unsupported.
* **Force‑overwrite** option to replace existing destinations.
* **Pure Rust**, no unsafe code, minimal deps.

---

## Library quick start

Clone only Rust source files under `src/` into `./sandbox`, while excluding
unit tests. This demonstrates the precedence rules from the
[`ignore`](https://docs.rs/ignore) crate: later patterns override earlier ones.


```rust
use clonetree::{clone_tree, Options};

fn main() -> anyhow::Result<()> {
    // Include all source files but drop tests
    let opts = Options::new()
        .glob("src/**")         // positive include
        .glob("!src/tests/**"); // negative exclude; overrides the line above

    clone_tree("./", "./sandbox", &opts)?;
    Ok(())
}
```

---

## Glob syntax

`clonetree` accepts the same glob rules as a single line in a `.gitignore`
file. Prefix a pattern with `!` to **exclude** matching paths instead of
including them.

| Pattern             | Description                                      |
| ------------------- | ------------------------------------------------ |
| `*.rs`              | All Rust source files in the current directory   |
| `**/*.toml`         | Any `.toml` file at any depth                    |
| `!target/**`        | **Exclude** Cargo build artefacts                |
| `images/**/thumb_*` | Every `thumb_*` file under `images/` recursively |

Rules:

* `*` matches any sequence of characters except path separators.
* `**` matches across directory boundaries.
* `?` matches exactly one character.
* A trailing `/` restricts the pattern to directories only.
* Patterns are evaluated **in the order they are given**; later patterns can
  override earlier ones (precedence follows the `ignore` crate).

---

## `ctree` ‑ command‑line tool

The crate ships with a convenience binary so users can benefit without writing code.

### Install

```bash
cargo install ctree
```

### Basic usage

```bash
ctree <SRC> <DEST> [OPTIONS]

OPTIONS:
  -g, --glob <GLOB>      Match or exclude glob (repeatable)
  -f, --force            Remove DEST if it already exists
      --no-reflink       Disable reflink, perform a regular copy
  -q, --quiet            Suppress progress output
  -h, --help             Show this help
```

Example: snapshot a repo while excluding Git metadata and build output:

```bash
ctree . ./sandbox \
  --glob '!target/**' \
  --glob '!.git/**'
```

---

## Filesystem support matrix 

Via [reflink-copy](https://crates.io/crates/reflink-copy)

| OS / FS                        | Reflink supported  | API used                          | Behaviour          |
| ------------------------------ | ------------------ | --------------------------------- | ------------------ |
| macOS 10.13+ / APFS            | ✅                 | `clonefile(2)`                    | COW clone          |
| iOS / APFS                     | ✅                 | `clonefile(2)`                    | COW clone          |
| Linux 6.7+ / Btrfs             | ✅                 | `FICLONE` ioctl                   | COW clone          |
| Linux 5.4+ / XFS (`reflink=1`) | ✅                 | `FICLONE` ioctl                   | COW clone          |
| Linux 6.1+ / bcachefs          | ✅                 | `remap_file_range`                | COW clone          |
| Linux 5.13+ / overlayfs        | ✅                 | `remap_file_range`                | COW clone          |
| Windows Server 2016+ / ReFS    | ✅                 | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | COW clone          |
| ext4 (Ubuntu/Fedora default)   | ❌                 | –                                 | Byte‑for‑byte copy |

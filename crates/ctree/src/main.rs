use anyhow::{Context, Result};
use clap::Parser;
use clonetree::{clone_tree, Options};

#[derive(Parser)]
#[command(
    name = "ctree",
    about = "Copy-on-write directory tree cloning",
    long_about = "Copies a directory tree using filesystem reflinks when available, with glob-based filtering"
)]
struct Args {
    /// Source directory to clone
    src: String,

    /// Destination directory
    dest: String,

    /// Match or exclude glob patterns (repeatable)
    /// Prefix with ! to exclude
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    globs: Vec<String>,

    /// Disable reflink, perform a regular copy
    #[arg(long = "no-reflink")]
    no_reflink: bool,

    /// Suppress progress output
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Build options
    let mut options = Options::new().no_reflink(args.no_reflink);
    for glob in args.globs {
        options = options.glob(glob);
    }

    // Show progress message if not quiet
    if !args.quiet {
        println!("Cloning '{}' to '{}'...", args.src, args.dest);
        if args.no_reflink {
            println!("Using regular file copy (reflink disabled)");
        }
    }

    // Perform the clone
    clone_tree(&args.src, &args.dest, &options)
        .with_context(|| format!("Failed to clone '{}' to '{}'", args.src, args.dest))?;

    if !args.quiet {
        println!("Done!");
    }

    Ok(())
}

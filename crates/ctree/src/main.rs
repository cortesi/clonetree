use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use clonetree::{clone_tree, CloneStrategy, Options};

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

    /// Cloning strategy
    #[arg(long, value_enum, default_value_t = StrategyArg::Auto)]
    strategy: StrategyArg,

    /// Suppress progress output
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum StrategyArg {
    Auto,
    SingleCall,
    FullTraversal,
}

impl From<StrategyArg> for CloneStrategy {
    fn from(arg: StrategyArg) -> Self {
        match arg {
            StrategyArg::Auto => CloneStrategy::Auto,
            StrategyArg::SingleCall => CloneStrategy::SingleCall,
            StrategyArg::FullTraversal => CloneStrategy::FullTraversal,
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Build options
    let mut options = Options::new().strategy(args.strategy.into());
    for glob in args.globs {
        options = options.glob(glob);
    }

    // Show progress message if not quiet
    if !args.quiet {
        println!("Cloning '{}' to '{}'...", args.src, args.dest);
    }

    // Perform the clone
    clone_tree(&args.src, &args.dest, &options)
        .with_context(|| format!("Failed to clone '{}' to '{}'", args.src, args.dest))?;

    if !args.quiet {
        println!("Done!");
    }

    Ok(())
}

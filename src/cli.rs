use clap::Parser;

#[derive(Parser)]
#[command(about = "Determine semver bump requirements for workspace crates.")]
pub struct Cli {
    /// Source git ref to compare from (the base)
    #[arg(long, default_value = "main")]
    pub source: String,

    /// Target git ref to compare to
    #[arg(long, default_value = "HEAD")]
    pub target: String,

    /// Comma-separated crate names to treat as breaking-change seeds directly,
    /// skipping git-based version detection
    #[arg(long, value_delimiter = ',')]
    pub direct: Option<Vec<String>>,

    /// Disable colored output
    #[arg(long)]
    pub no_color: bool,

    /// Print which public API items cause each leak
    #[arg(long, short)]
    pub verbose: bool,

    /// Print an influence tree showing how bumps propagate
    #[arg(long, short)]
    pub tree: bool,

    /// Show cargo rustdoc stderr output (warnings, errors) during analysis
    #[arg(long)]
    pub rustdoc_stderr: bool,

    /// Rust toolchain to use for rustdoc JSON generation (e.g. "nightly-2025-01-15")
    #[arg(long, default_value = "nightly")]
    pub toolchain: String,

    /// Include binary-only crates in the analysis (skipped by default)
    #[arg(long)]
    pub include_binaries: bool,
}

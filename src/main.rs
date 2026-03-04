//! # semwave
//!
//! A static analysis tool that answers the question:
//!
//! > *"If I bump crates A, B and C in this Rust project - what else do I need to bump and how?"*
//!
//! ## How it works
//!
//! 1. Accepts the list of breaking version bumps (the "seeds"). By default this means
//!    diffing `Cargo.toml` files between two git refs to find dependency versions that
//!    changed in breaking or additive ways. Alternatively, use `--direct` to specify
//!    seeds explicitly.
//!
//! 2. Walks the workspace dependency graph starting from the seeds. For each dependent,
//!    it checks whether the crate leaks any seed types in its public API. If it does,
//!    that crate itself needs a bump - and becomes a new seed, triggering the same check
//!    on *its* dependents, and so on until the wave settles. The bump level
//!    (major/minor/patch) depends on the change type and the consumer's version scheme
//!    (`0.y.z` vs `>=1.0.0`).
//!
//! The output is three lists: **MAJOR** bumps, **MINOR** bumps, and **PATCH** bumps,
//! plus optional warnings when the tool had to guess conservatively.
//!
//! Read [README.md](https://github.com/uandere/semwave/blob/main/README.md) for more details.

/// Print helpers
mod display;
/// Bump evaluation
mod evaluate;
/// Leak handling
mod leak;
/// Wave propagation loop
mod propagate;
/// Output printing & bump validation
mod report;
/// Seed detection & management
mod seeds;
/// Semver helpers
mod semver;

use anyhow::{Context, Result};
use cargo_metadata::{CargoOpt, MetadataCommand};
use clap::Parser;
use colored::Colorize as _;
use std::collections::{HashMap, HashSet};

use crate::evaluate::{AnalysisOptions, WaveState, WorkspaceContext};
use crate::seeds::detect_version_changes;
use crate::semver::{Bump, format_name_set};

struct ResolvedSeeds {
    all_seeds: HashSet<String>,
    state: WaveState,
    local_bumps: HashMap<String, Bump>,
    new_crates: HashSet<String>,
}

#[derive(Parser)]
#[command(about = "Determine semver bump requirements for workspace crates.")]
struct Cli {
    /// Source git ref to compare from (the base)
    #[arg(long, default_value = "main")]
    source: String,

    /// Target git ref to compare to
    #[arg(long, default_value = "HEAD")]
    target: String,

    /// Comma-separated crate names to treat as breaking-change seeds directly,
    /// skipping git-based version detection
    #[arg(long, value_delimiter = ',')]
    direct: Option<Vec<String>>,

    /// Disable colored output
    #[arg(long)]
    no_color: bool,

    /// Print which public API items cause each leak
    #[arg(long, short)]
    verbose: bool,

    /// Print an influence tree showing how bumps propagate
    #[arg(long, short)]
    tree: bool,

    /// Show cargo rustdoc stderr output (warnings, errors) during analysis
    #[arg(long)]
    rustdoc_stderr: bool,

    /// Rust toolchain to use for rustdoc JSON generation (e.g. "nightly-2025-01-15")
    #[arg(long, default_value = "nightly")]
    toolchain: String,

    /// Include binary-only crates in the analysis (they are skipped by default)
    #[arg(long)]
    include_binaries: bool,

    /// Number of parallel rustdoc jobs (defaults to number of CPU cores)
    #[arg(long, short)]
    jobs: Option<usize>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.no_color {
        colored::control::set_override(false);
    }

    if let Some(jobs) = cli.jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global()
            .context("Failed to configure rayon thread pool")?;
    }

    let show_tree = cli.tree;
    let opts = AnalysisOptions {
        verbose: cli.verbose,
        rustdoc_stderr: cli.rustdoc_stderr,
        toolchain: cli.toolchain,
        include_binaries: cli.include_binaries,
    };

    let ResolvedSeeds {
        all_seeds,
        state,
        local_bumps,
        new_crates,
    } = resolve_seeds(cli.direct, &cli.source, &cli.target)?;

    let metadata = MetadataCommand::new()
        .features(CargoOpt::AllFeatures)
        .exec()
        .context("Failed to run cargo metadata")?;

    let resolve = metadata
        .resolve
        .as_ref()
        .context("No resolve graph found")?;
    let ctx = WorkspaceContext::from_metadata(&metadata);

    let result = propagate::run(
        &ctx,
        state,
        &opts,
        resolve,
        &all_seeds,
        &new_crates,
        &local_bumps,
    )?;

    let bump_lists = report::print_results(&ctx, &result, &all_seeds, show_tree);
    if report::validate_bumps(&bump_lists, &local_bumps, &result.state.failed, &new_crates) {
        std::process::exit(1);
    }

    Ok(())
}

fn resolve_seeds(direct: Option<Vec<String>>, source: &str, target: &str) -> Result<ResolvedSeeds> {
    if let Some(direct_crates) = direct {
        let seeds: HashSet<String> = direct_crates.into_iter().collect();
        println!(
            "{} assuming BREAKING change for {}\n",
            "Direct mode:".bold(),
            format_name_set(&seeds).cyan()
        );
        let state = WaveState {
            breaking_crates: seeds.clone(),
            additive_crates: HashSet::new(),
            failed: HashSet::new(),
        };
        return Ok(ResolvedSeeds {
            all_seeds: seeds,
            state,
            local_bumps: HashMap::new(),
            new_crates: HashSet::new(),
        });
    }

    println!(
        "Comparing versions between {} and {}...\n",
        source.cyan().bold(),
        target.cyan().bold()
    );
    let changes = detect_version_changes(source, target)?;

    if changes.breaking_seeds.is_empty() && changes.additive_seeds.is_empty() {
        println!(
            "{}",
            "No breaking/additive version changes detected. Nothing to propagate.".green()
        );
        std::process::exit(0);
    }

    if !changes.breaking_seeds.is_empty() {
        println!(
            "\n{} {}\n",
            "Breaking seeds:".bold(),
            format_name_set(&changes.breaking_seeds).red()
        );
    }
    if !changes.additive_seeds.is_empty() {
        println!(
            "{} {}\n",
            "Additive seeds:".bold(),
            format_name_set(&changes.additive_seeds).yellow()
        );
    }

    let all_seeds: HashSet<String> = changes
        .breaking_seeds
        .iter()
        .chain(changes.additive_seeds.iter())
        .cloned()
        .collect();

    let state = WaveState {
        breaking_crates: changes.breaking_seeds,
        additive_crates: changes.additive_seeds,
        failed: HashSet::new(),
    };

    Ok(ResolvedSeeds {
        all_seeds,
        state,
        local_bumps: changes.local_bumps,
        new_crates: changes.new_crates,
    })
}

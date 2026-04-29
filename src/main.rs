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

/// CLI interface.
mod cli;
/// Print helpers.
mod display;
/// Bump evaluation.
mod evaluate;
/// Leak handling.
mod leak;
/// Seed detection & management.
mod seeds;
/// Semver helpers.
mod semver;

mod run;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    if cli.no_color {
        colored::control::set_override(false);
    }

    if run::run(cli).is_err() {
        std::process::exit(1);
    }

    Ok(())
}

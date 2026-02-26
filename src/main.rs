//! # 🌊 semwave
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

#![allow(clippy::format_in_format_args)]

/// Print helpers
pub mod display;
/// Bump evaluation
pub mod evaluate;
/// Cycle detection (DFS-based)
pub mod graph;
/// Leak handling
pub mod leak;
/// Seed detection & management
pub mod seeds;
/// Semver helpers
pub mod semver;

use anyhow::{Context, Result};
use cargo_metadata::{CargoOpt, MetadataCommand, Node, PackageId};
use clap::Parser;
use colored::Colorize;
use std::collections::{HashMap, HashSet};

use crate::display::print_influence_tree;
use crate::evaluate::{WorkspaceContext, evaluate_crate_bump};
use crate::graph::{find_cycle, is_normal_dep};
use crate::seeds::detect_version_changes;
use crate::semver::{Bump, ChangeKind, required_bump};

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.no_color {
        colored::control::set_override(false);
    }

    let (breaking_seeds, additive_seeds, local_bumps) = if let Some(direct_crates) = cli.direct {
        let seeds: HashSet<String> = direct_crates.into_iter().collect();
        println!(
            "{} assuming BREAKING change for {}\n",
            "Direct mode:".bold(),
            format!("{:?}", seeds).cyan()
        );
        (seeds, HashSet::new(), HashMap::new())
    } else {
        println!(
            "Comparing versions between {} and {}...\n",
            cli.source.cyan().bold(),
            cli.target.cyan().bold()
        );
        let (breaking_seeds, additive_seeds, local_bumps) =
            detect_version_changes(&cli.source, &cli.target)?;

        if breaking_seeds.is_empty() && additive_seeds.is_empty() {
            println!(
                "{}",
                "No breaking/additive version changes detected. Nothing to propagate.".green()
            );
            return Ok(());
        }

        if !breaking_seeds.is_empty() {
            println!(
                "\n{} {}\n",
                "Breaking seeds:".bold(),
                format!("{:?}", breaking_seeds).red()
            );
        }
        if !additive_seeds.is_empty() {
            println!(
                "{} {}\n",
                "Additive seeds:".bold(),
                format!("{:?}", additive_seeds).yellow()
            );
        }
        (breaking_seeds, additive_seeds, local_bumps)
    };

    let all_seeds: HashSet<String> = breaking_seeds
        .iter()
        .chain(additive_seeds.iter())
        .cloned()
        .collect();

    let mut breaking_crates = breaking_seeds.clone();
    let mut additive_crates = additive_seeds.clone();
    let mut patch_crates: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    let mut tree_edges: HashMap<String, Vec<(String, Bump)>> = HashMap::new();

    let metadata = MetadataCommand::new()
        .features(CargoOpt::AllFeatures)
        .exec()
        .context("Failed to run cargo metadata")?;

    let resolve = metadata.resolve.context("No resolve graph found")?;

    let workspace_members: HashSet<&PackageId> = metadata.workspace_members.iter().collect();

    let ctx = WorkspaceContext {
        pkg_names: metadata
            .packages
            .iter()
            .map(|p| (p.id.clone(), p.name.to_string()))
            .collect(),
        pkg_manifest_paths: metadata
            .packages
            .iter()
            .filter(|p| workspace_members.contains(&p.id))
            .map(|p| (p.name.to_string(), p.manifest_path.to_string()))
            .collect(),
        pkg_has_lib: metadata
            .packages
            .iter()
            .filter(|p| workspace_members.contains(&p.id))
            .filter(|p| p.targets.iter().any(|t| t.is_lib() || t.is_proc_macro()))
            .map(|p| p.name.to_string())
            .collect(),
        pkg_versions: metadata
            .packages
            .iter()
            .filter(|p| workspace_members.contains(&p.id))
            .map(|p| (p.name.to_string(), p.version.clone()))
            .collect(),
    };

    let mut pending_nodes: Vec<&Node> = resolve
        .nodes
        .iter()
        .filter(|n| workspace_members.contains(&n.id))
        .collect();

    let mut processed: HashSet<String> = HashSet::new();

    while !pending_nodes.is_empty() {
        let mut made_progress = false;

        for i in (0..pending_nodes.len()).rev() {
            let node = pending_nodes[i];
            let node_name = ctx.pkg_names[&node.id].clone();

            let deps_ready = node.deps.iter().filter(|d| is_normal_dep(d)).all(|dep| {
                if dep.pkg == node.id {
                    true
                } else if workspace_members.contains(&dep.pkg) {
                    processed.contains(&ctx.pkg_names[&dep.pkg])
                } else {
                    true
                }
            });

            if deps_ready {
                let (change_kind, _bump, influences) = evaluate_crate_bump(
                    node,
                    &ctx,
                    &breaking_crates,
                    &additive_crates,
                    &mut failed,
                    cli.verbose,
                    cli.rustdoc_stderr,
                )?;

                for inf in &influences {
                    tree_edges
                        .entry(inf.dep_name.clone())
                        .or_default()
                        .push((node_name.clone(), inf.bump));
                }

                match change_kind {
                    ChangeKind::Breaking => {
                        breaking_crates.insert(node_name.clone());
                    }
                    ChangeKind::Additive => {
                        additive_crates.insert(node_name.clone());
                    }
                    ChangeKind::Patch => {
                        patch_crates.insert(node_name.clone());
                    }
                    ChangeKind::None => {}
                }

                processed.insert(node_name);
                pending_nodes.remove(i);
                made_progress = true;
            }
        }

        if !made_progress {
            let stuck: Vec<String> = pending_nodes
                .iter()
                .map(|n| ctx.pkg_names[&n.id].clone())
                .collect();
            let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
            for node in &pending_nodes {
                let name = ctx.pkg_names[&node.id].as_str();
                for dep in node.deps.iter().filter(|d| is_normal_dep(d)) {
                    if dep.pkg == node.id {
                        continue;
                    }
                    let dep_name = ctx.pkg_names[&dep.pkg].as_str();
                    if stuck.iter().any(|s| s == dep_name) {
                        adj.entry(name).or_default().push(dep_name);
                    }
                }
            }
            let cycle = find_cycle(&adj);
            eprintln!(
                "\n{} Cycle detected among unresolved crates:",
                "ERROR:".red().bold()
            );
            if let Some(cycle) = cycle {
                let path = cycle
                    .iter()
                    .map(|s| s.cyan().bold().to_string())
                    .collect::<Vec<_>>()
                    .join(&format!(" {} ", "->".red()));
                eprintln!("  {}", path);
            } else {
                for name in &stuck {
                    eprintln!("  {}", name.cyan());
                }
            }
            anyhow::bail!("Cannot make progress - cycle in workspace dependencies");
        }
    }

    // Remove seeds and already-bumped crates from the result sets
    for seed in &all_seeds {
        breaking_crates.remove(seed);
        additive_crates.remove(seed);
        patch_crates.remove(seed);
    }

    for (name, existing_bump) in &local_bumps {
        match *existing_bump {
            Bump::Major => {
                breaking_crates.remove(name);
                additive_crates.remove(name);
                patch_crates.remove(name);
            }
            Bump::Minor => {
                additive_crates.remove(name);
                patch_crates.remove(name);
            }
            Bump::Patch => {
                patch_crates.remove(name);
            }
            Bump::None => {}
        }
    }

    // Map each set to final bump based on crate version
    let mut major_list: HashSet<String> = HashSet::new();
    let mut minor_list: HashSet<String> = HashSet::new();
    let mut patch_list: HashSet<String> = patch_crates;

    for name in &breaking_crates {
        let bump = ctx
            .pkg_versions
            .get(name)
            .map(|v| required_bump(v, ChangeKind::Breaking))
            .unwrap_or(Bump::Minor);
        match bump {
            Bump::Major => {
                major_list.insert(name.clone());
            }
            _ => {
                minor_list.insert(name.clone());
            }
        }
    }

    for name in &additive_crates {
        let bump = ctx
            .pkg_versions
            .get(name)
            .map(|v| required_bump(v, ChangeKind::Additive))
            .unwrap_or(Bump::Patch);
        match bump {
            Bump::Minor => {
                minor_list.insert(name.clone());
            }
            _ => {
                patch_list.insert(name.clone());
            }
        }
    }

    if cli.tree {
        println!("\n{}", "=== Influence Tree ===".bold().green());
        print_influence_tree(&all_seeds, &tree_edges);
        println!();
    }

    println!("{}", "=== Analysis Complete ===".bold().green());
    println!(
        "{} {:?}",
        "MAJOR-bump list (Requires MAJOR bump / ↑.0.0):"
            .red()
            .bold(),
        major_list
    );
    println!(
        "{} {:?}",
        "MINOR-bump list (Requires MINOR bump / x.↑.0):"
            .yellow()
            .bold(),
        minor_list
    );
    println!(
        "{} {:?}",
        "PATCH-bump list (Requires PATCH bump / x.y.↑):"
            .cyan()
            .bold(),
        patch_list
    );

    if !failed.is_empty() {
        eprintln!(
            "\n{} The following crates failed rustdoc JSON generation \
             and were conservatively assumed breaking. Verify manually:\n  {:?}",
            "WARNING:".yellow().bold(),
            failed
        );
    }

    let all_required: HashMap<&String, Bump> = major_list
        .iter()
        .map(|n| (n, Bump::Major))
        .chain(minor_list.iter().map(|n| (n, Bump::Minor)))
        .chain(patch_list.iter().map(|n| (n, Bump::Patch)))
        .collect();

    let mut has_errors = false;

    let under_bumped: Vec<(&String, Bump, &Bump)> = all_required
        .iter()
        .filter(|(name, _)| !failed.contains(**name))
        .filter_map(|(name, required)| {
            local_bumps
                .get(*name)
                .filter(|local| local < &required)
                .map(|local| (*name, *required, local))
        })
        .collect();
    if !under_bumped.is_empty() {
        has_errors = true;
        eprintln!(
            "\n{} These crates have insufficient version bumps:",
            "ERROR:".red().bold()
        );
        for (name, required, local) in &under_bumped {
            eprintln!(
                "  {} has {:?} bump but requires {:?}",
                name.cyan(),
                local,
                required
            );
        }
    }

    if !local_bumps.is_empty() {
        let missing: Vec<(&String, &Bump)> = all_required
            .iter()
            .filter(|(name, _)| !local_bumps.contains_key(**name) && !failed.contains(**name))
            .map(|(name, bump)| (*name, bump))
            .collect();
        if !missing.is_empty() {
            has_errors = true;
            eprintln!(
                "\n{} These crates need a version bump but have none:",
                "ERROR:".red().bold()
            );
            for (name, required) in &missing {
                eprintln!("  {} requires {:?}", name.cyan(), required);
            }
        }
    }

    if has_errors {
        std::process::exit(1);
    }

    Ok(())
}

use anyhow::{Context, Result, anyhow};
use cargo_metadata::{CargoOpt, MetadataCommand, Node, PackageId};
use colored::Colorize;
use std::collections::{HashMap, HashSet};

use crate::cli::Cli;
use crate::display::print_influence_tree;
use crate::evaluate::{
    AnalysisOptions, WaveState, WorkspaceContext, evaluate_crate_bump, is_normal_dep,
};
use crate::seeds::detect_version_changes;
use crate::semver::{Bump, ChangeKind, format_name_set, required_bump};

pub fn run(cli: Cli) -> Result<()> {
    let opts = AnalysisOptions {
        verbose: cli.verbose,
        rustdoc_stderr: cli.rustdoc_stderr,
        toolchain: cli.toolchain,
        include_binaries: cli.include_binaries,
        tree: cli.tree,
    };

    let is_direct = cli.direct.is_some();
    let (all_seeds, mut state, local_bumps, new_crates) = if let Some(direct_crates) = cli.direct {
        let seeds: HashSet<String> = direct_crates.into_iter().collect();
        println!(
            "{} assuming BREAKING change for {}\n",
            "Direct mode:".bold(),
            format_name_set(&seeds).cyan()
        );
        let wave = WaveState {
            breaking_crates: seeds.clone(),
            additive_crates: HashSet::new(),
            failed: HashSet::new(),
        };
        (seeds, wave, HashMap::new(), HashSet::new())
    } else {
        println!(
            "Comparing versions between {} and {}...\n",
            cli.source.cyan().bold(),
            cli.target.cyan().bold()
        );
        let changes = detect_version_changes(&cli.source, &cli.target)?;

        if changes.breaking_seeds.is_empty() && changes.additive_seeds.is_empty() {
            println!(
                "{}",
                "No breaking/additive version changes detected. Nothing to propagate.".green()
            );
            return Ok(());
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

        let wave = WaveState {
            breaking_crates: changes.breaking_seeds,
            additive_crates: changes.additive_seeds,
            failed: HashSet::new(),
        };
        (all_seeds, wave, changes.local_bumps, changes.new_crates)
    };

    let mut patch_crates: HashSet<String> = HashSet::new();
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

    if is_direct {
        let all_known: HashSet<&str> = metadata.packages.iter().map(|p| p.name.as_str()).collect();
        let unknown: Vec<&str> = all_seeds
            .iter()
            .filter(|s| !all_known.contains(s.as_str()))
            .map(|s| s.as_str())
            .collect();
        if !unknown.is_empty() {
            anyhow::bail!(
                "Unknown crate(s) passed to --direct: {}",
                unknown.join(", ")
            );
        }
    }

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
            let node_name = &ctx.pkg_names[&node.id];

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
                let (change_kind, _bump, influences) =
                    evaluate_crate_bump(node, &ctx, &mut state, &opts)?;

                for inf in &influences {
                    tree_edges
                        .entry(inf.dep_name.clone())
                        .or_default()
                        .push((node_name.clone(), inf.bump));
                }

                match change_kind {
                    ChangeKind::Breaking => {
                        state.breaking_crates.insert(node_name.clone());
                    }
                    ChangeKind::Additive => {
                        state.additive_crates.insert(node_name.clone());
                    }
                    ChangeKind::Patch => {
                        patch_crates.insert(node_name.clone());
                    }
                    ChangeKind::None => {}
                }

                processed.insert(node_name.clone());
                pending_nodes.remove(i);
                made_progress = true;
            }
        }

        if !made_progress {
            let stuck: Vec<&str> = pending_nodes
                .iter()
                .map(|n| ctx.pkg_names[&n.id].as_str())
                .collect();
            anyhow::bail!(
                "Cannot make progress on crates: {:?}. \
                 This should not happen with a valid Cargo workspace.",
                stuck
            );
        }
    }

    for seed in &all_seeds {
        state.breaking_crates.remove(seed);
        state.additive_crates.remove(seed);
        patch_crates.remove(seed);
    }

    for name in &new_crates {
        state.breaking_crates.remove(name);
        state.additive_crates.remove(name);
        patch_crates.remove(name);
    }

    for (name, existing_bump) in &local_bumps {
        if *existing_bump >= Bump::Major {
            state.breaking_crates.remove(name);
            state.additive_crates.remove(name);
            patch_crates.remove(name);
        }
        if *existing_bump >= Bump::Minor {
            state.additive_crates.remove(name);
            patch_crates.remove(name);
        }
        if *existing_bump >= Bump::Patch {
            patch_crates.remove(name);
        }
    }

    let mut major_list: HashSet<String> = HashSet::new();
    let mut minor_list: HashSet<String> = HashSet::new();
    let mut patch_list: HashSet<String> = patch_crates;

    for name in &state.breaking_crates {
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

    for name in &state.additive_crates {
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
        "{} {}",
        "MAJOR-bump list (Requires MAJOR bump / ↑.0.0):"
            .red()
            .bold(),
        format_name_set(&major_list)
    );
    println!(
        "{} {}",
        "MINOR-bump list (Requires MINOR bump / x.↑.0):"
            .yellow()
            .bold(),
        format_name_set(&minor_list)
    );
    println!(
        "{} {}",
        "PATCH-bump list (Requires PATCH bump / x.y.↑):"
            .cyan()
            .bold(),
        format_name_set(&patch_list)
    );

    if !state.failed.is_empty() {
        eprintln!(
            "\n{} The following crates failed rustdoc JSON generation \
             and were conservatively assumed breaking. Verify manually:\n  {}",
            "WARNING:".yellow().bold(),
            format_name_set(&state.failed)
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
        .filter(|(name, _)| !state.failed.contains(**name))
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
                "  {} has {} bump but requires {}",
                name.cyan(),
                local,
                required
            );
        }
    }

    if !local_bumps.is_empty() {
        let missing: Vec<(&String, &Bump)> = all_required
            .iter()
            .filter(|(name, _)| {
                !local_bumps.contains_key(**name)
                    && !state.failed.contains(**name)
                    && !new_crates.contains(**name)
            })
            .map(|(name, bump)| (*name, bump))
            .collect();
        if !missing.is_empty() {
            has_errors = true;
            eprintln!(
                "\n{} These crates need a version bump but have none:",
                "ERROR:".red().bold()
            );
            for (name, required) in &missing {
                eprintln!("  {} requires {}", name.cyan(), required);
            }
        }
    }

    if !has_errors {
        return Err(anyhow!("`semwave` had encountered errors during analysis"));
    }

    Ok(())
}

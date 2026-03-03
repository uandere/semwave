use std::collections::{HashMap, HashSet};

use crate::{
    leak::find_leaked_deps,
    semver::{Bump, ChangeKind, required_bump},
};
use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, Node, NodeDep, PackageId};
use colored::Colorize as _;
use semver::Version;

/// Returns true if this dependency edge includes a Normal (non-dev, non-build)
/// dependency kind. Only normal deps affect the public API and semver surface.
pub fn is_normal_dep(dep: &NodeDep) -> bool {
    dep.dep_kinds
        .iter()
        .any(|dk| dk.kind == DependencyKind::Normal)
}

/// Shared read-only context about the workspace, built once from `cargo metadata`.
pub struct WorkspaceContext {
    pub pkg_names: HashMap<PackageId, String>,
    pub pkg_manifest_paths: HashMap<String, String>,
    pub pkg_has_lib: HashSet<String>,
    pub pkg_versions: HashMap<String, Version>,
}

/// Mutable state accumulated during the propagation wave.
pub struct WaveState {
    pub breaking_crates: HashSet<String>,
    pub additive_crates: HashSet<String>,
    pub failed: HashSet<String>,
}

/// Per-crate analysis options.
pub struct AnalysisOptions {
    pub verbose: bool,
    pub rustdoc_stderr: bool,
    pub toolchain: String,
    pub include_binaries: bool,
}

/// Per-dependency influence: which dep caused the bump and how.
#[derive(Debug, Clone)]
pub struct DepInfluence {
    pub dep_name: String,
    pub bump: Bump,
}

pub fn evaluate_crate_bump(
    node: &Node,
    ctx: &WorkspaceContext,
    state: &mut WaveState,
    opts: &AnalysisOptions,
) -> Result<(ChangeKind, Bump, Vec<DepInfluence>)> {
    let node_name = &ctx.pkg_names[&node.id];
    let node_version = ctx.pkg_versions.get(node_name);

    let affected_deps: Vec<(&str, ChangeKind)> = node
        .deps
        .iter()
        .filter(|d| d.pkg != node.id && is_normal_dep(d))
        .filter_map(|d| {
            let name = ctx.pkg_names[&d.pkg].as_str();
            if state.breaking_crates.contains(name) {
                Some((name, ChangeKind::Breaking))
            } else if state.additive_crates.contains(name) {
                Some((name, ChangeKind::Additive))
            } else {
                None
            }
        })
        .collect();

    if affected_deps.is_empty() {
        return Ok((ChangeKind::None, Bump::None, vec![]));
    }

    let dep_names: Vec<&str> = affected_deps.iter().map(|(n, _)| *n).collect();

    if !ctx.pkg_has_lib.contains(node_name) {
        if !opts.include_binaries {
            return Ok((ChangeKind::None, Bump::None, vec![]));
        }
        println!(
            "  {} {} is binary-only, no public API to leak",
            "->".dimmed(),
            node_name.cyan()
        );
        let bump = node_version
            .map(|v| required_bump(v, ChangeKind::Patch))
            .unwrap_or(Bump::Patch);
        let influences = affected_deps
            .into_iter()
            .map(|(dep_name, _)| DepInfluence {
                dep_name: dep_name.to_owned(),
                bump: Bump::Patch,
            })
            .collect();
        return Ok((ChangeKind::Patch, bump, influences));
    }

    println!(
        "Analyzing {} for public API exposure of {:?}",
        node_name.cyan().bold(),
        dep_names
    );

    let manifest = ctx
        .pkg_manifest_paths
        .get(node_name)
        .with_context(|| format!("No manifest path for {}", node_name))?;

    let json_path = match rustdoc_json::Builder::default()
        .toolchain(&opts.toolchain)
        .manifest_path(manifest)
        .all_features(true)
        .cap_lints(Some("allow"))
        .silent(!opts.rustdoc_stderr)
        .build()
    {
        Ok(path) => path,
        Err(e) => {
            let worst_change = affected_deps
                .iter()
                .map(|(_, ck)| *ck)
                .max()
                .unwrap_or(ChangeKind::Breaking);
            let conservative_bump = node_version
                .map(|v| required_bump(v, worst_change))
                .unwrap_or(Bump::Minor);
            eprintln!(
                "  {} rustdoc JSON generation failed for {}: {}\n  \
                 Conservatively assuming {} bump.",
                "WARNING:".yellow().bold(),
                node_name.cyan(),
                e,
                conservative_bump
            );
            state.failed.insert(node_name.to_owned());
            let influences = affected_deps
                .into_iter()
                .map(|(dep_name, _)| DepInfluence {
                    dep_name: dep_name.to_owned(),
                    bump: conservative_bump,
                })
                .collect();
            return Ok((worst_change, conservative_bump, influences));
        }
    };

    let json_str = std::fs::read_to_string(&json_path)
        .with_context(|| format!("Failed to read rustdoc JSON for {}", node_name))?;
    let krate: rustdoc_types::Crate = serde_json::from_str(&json_str)
        .with_context(|| format!("Failed to parse rustdoc JSON for {}", node_name))?;

    let dep_norm_set: HashSet<String> = affected_deps
        .iter()
        .map(|(n, _)| n.replace('-', "_"))
        .collect();

    let dep_crate_id_to_name: HashMap<u32, String> = krate
        .external_crates
        .iter()
        .filter(|(_, ec)| dep_norm_set.contains(&ec.name.replace('-', "_")))
        .map(|(id, ec)| (*id, ec.name.clone()))
        .collect();

    let leaked = find_leaked_deps(&krate, &dep_crate_id_to_name);

    let mut worst_change = ChangeKind::Patch;
    let mut influences = Vec::new();

    for (dep_name, dep_change) in affected_deps {
        let dep_norm = dep_name.replace('-', "_");
        let is_leaked = leaked.keys().any(|k| k.replace('-', "_") == dep_norm);

        if is_leaked {
            let edge_bump = node_version
                .map(|v| required_bump(v, dep_change))
                .unwrap_or(Bump::Minor);
            println!(
                "  {} {} leaks {} ({}):",
                "->".red().bold(),
                node_name.red().bold(),
                dep_name.yellow(),
                edge_bump
            );
            if opts.verbose {
                for (leaked_name, details) in &leaked {
                    if leaked_name.replace('-', "_") == dep_norm {
                        for detail in details {
                            let types_str = detail
                                .leaked_types
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ");
                            println!(
                                "     {} {} — uses {}",
                                detail.item_kind.dimmed(),
                                detail.item_name.dimmed(),
                                types_str.dimmed()
                            );
                        }
                    }
                }
            }
            influences.push(DepInfluence {
                dep_name: dep_name.to_owned(),
                bump: edge_bump,
            });
            worst_change = worst_change.max(dep_change);
        } else {
            influences.push(DepInfluence {
                dep_name: dep_name.to_owned(),
                bump: Bump::Patch,
            });
        }
    }

    let final_bump = node_version
        .map(|v| required_bump(v, worst_change))
        .unwrap_or(Bump::Patch);

    Ok((worst_change, final_bump, influences))
}

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

/// Shared read-only context passed to `evaluate_crate_bump` to avoid too many arguments.
pub struct WorkspaceContext {
    pub pkg_names: HashMap<PackageId, String>,
    pub pkg_manifest_paths: HashMap<String, String>,
    pub pkg_has_lib: HashSet<String>,
    pub pkg_versions: HashMap<String, Version>,
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
    breaking_crates: &HashSet<String>,
    additive_crates: &HashSet<String>,
    failed: &mut HashSet<String>,
    verbose: bool,
    rustdoc_stderr: bool,
) -> Result<(ChangeKind, Bump, Vec<DepInfluence>)> {
    let node_name = ctx.pkg_names[&node.id].clone();
    let node_version = ctx.pkg_versions.get(&node_name);

    let affected_deps: Vec<(String, ChangeKind)> = node
        .deps
        .iter()
        .filter(|d| d.pkg != node.id && is_normal_dep(d))
        .map(|d| ctx.pkg_names[&d.pkg].clone())
        .filter_map(|name| {
            if breaking_crates.contains(&name) {
                Some((name, ChangeKind::Breaking))
            } else if additive_crates.contains(&name) {
                Some((name, ChangeKind::Additive))
            } else {
                None
            }
        })
        .collect();

    if affected_deps.is_empty() {
        return Ok((ChangeKind::None, Bump::None, vec![]));
    }

    let dep_names: Vec<&str> = affected_deps.iter().map(|(n, _)| n.as_str()).collect();

    if !ctx.pkg_has_lib.contains(&node_name) {
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
                dep_name,
                bump: Bump::Patch,
            })
            .collect();
        return Ok((ChangeKind::Patch, bump, influences));
    }

    println!(
        "Analyzing {} for public API exposure of {}",
        node_name.cyan().bold(),
        format!("{:?}", dep_names).dimmed()
    );

    let manifest = ctx
        .pkg_manifest_paths
        .get(&node_name)
        .with_context(|| format!("No manifest path for {}", node_name))?;

    let json_path = match rustdoc_json::Builder::default()
        .toolchain("nightly")
        .manifest_path(manifest)
        .all_features(true)
        .cap_lints(Some("allow"))
        .silent(!rustdoc_stderr)
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
                 Conservatively assuming {:?} bump.",
                "WARNING:".yellow().bold(),
                node_name.cyan(),
                e,
                conservative_bump
            );
            failed.insert(node_name);
            let influences = affected_deps
                .into_iter()
                .map(|(dep_name, _)| DepInfluence {
                    dep_name,
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
                "  {} {} leaks {} ({:?}):",
                "->".red().bold(),
                node_name.red().bold(),
                dep_name.yellow(),
                edge_bump
            );
            if verbose {
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
                dep_name,
                bump: edge_bump,
            });
            worst_change = worst_change.max(dep_change);
        } else {
            influences.push(DepInfluence {
                dep_name,
                bump: Bump::Patch,
            });
        }
    }

    let final_bump = node_version
        .map(|v| required_bump(v, worst_change))
        .unwrap_or(Bump::Patch);

    Ok((worst_change, final_bump, influences))
}

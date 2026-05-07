use std::collections::{HashMap, HashSet};

use crate::{
    cli::Cli,
    leak::find_leaked_deps,
    report::{Event, EventSink, LeakDetail},
    semver::{Bump, ChangeKind, required_bump},
    types::{CrateName, ManifestPath},
};
use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, Node, NodeDep, PackageId};
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
    pub pkg_names: HashMap<PackageId, CrateName>,
    pub pkg_manifest_paths: HashMap<CrateName, ManifestPath>,
    pub pkg_has_lib: HashSet<CrateName>,
    pub pkg_versions: HashMap<CrateName, Version>,
}

/// Mutable state accumulated during the propagation wave.
pub struct WaveState {
    pub breaking_crates: HashSet<CrateName>,
    pub additive_crates: HashSet<CrateName>,
    pub failed: HashSet<CrateName>,
}

/// Per-dependency influence: which dep caused the bump and how.
#[derive(Debug, Clone)]
pub struct DepInfluence {
    pub dep_name: CrateName,
    pub bump: Bump,
}

pub fn evaluate_affected_deps<'a>(
    node: &Node,
    ctx: &'a WorkspaceContext,
    state: &mut WaveState,
) -> Vec<(&'a CrateName, ChangeKind)> {
    node.deps
        .iter()
        .filter(|dep| dep.pkg != node.id && is_normal_dep(dep))
        .filter_map(|dep| {
            let name = &ctx.pkg_names[&dep.pkg];
            if state.breaking_crates.contains(name) {
                Some((name, ChangeKind::Breaking))
            } else if state.additive_crates.contains(name) {
                Some((name, ChangeKind::Additive))
            } else {
                None
            }
        })
        .collect()
}

pub fn evaluate_crate_bump(
    node: &Node,
    ctx: &WorkspaceContext,
    state: &mut WaveState,
    cli: &Cli,
    sink: &dyn EventSink,
) -> Result<(ChangeKind, Bump, Vec<DepInfluence>)> {
    let node_name = &ctx.pkg_names[&node.id];
    let node_version = ctx.pkg_versions.get(node_name);

    if !cli.tree && state.breaking_crates.contains(node_name) {
        return Ok((ChangeKind::None, Bump::None, vec![]));
    }

    let affected_deps = evaluate_affected_deps(node, ctx, state);

    if affected_deps.is_empty() {
        return Ok((ChangeKind::None, Bump::None, vec![]));
    }

    let dep_names: Vec<&str> = affected_deps
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();

    if !ctx.pkg_has_lib.contains(node_name) {
        if !cli.include_binaries {
            return Ok((ChangeKind::None, Bump::None, vec![]));
        }
        sink.emit(&Event::BinaryCrateSkipped {
            name: node_name.as_str(),
        });
        let bump = node_version
            .map(|version| required_bump(version, ChangeKind::Patch))
            .unwrap_or(Bump::Patch);
        let influences = affected_deps
            .into_iter()
            .map(|(dep_name, _)| DepInfluence {
                dep_name: dep_name.clone(),
                bump: Bump::Patch,
            })
            .collect();
        return Ok((ChangeKind::Patch, bump, influences));
    }

    sink.emit(&Event::AnalyzingCrate {
        name: node_name.as_str(),
        deps: &dep_names,
    });

    let manifest = ctx
        .pkg_manifest_paths
        .get(node_name)
        .with_context(|| format!("No manifest path for {}", node_name))?;

    let json_path = match rustdoc_json::Builder::default()
        .toolchain(cli.toolchain.as_str())
        .manifest_path(manifest.as_str())
        .all_features(true)
        .cap_lints(Some("allow"))
        .silent(!cli.rustdoc_stderr)
        .build()
    {
        Ok(path) => path,
        Err(e) => {
            let worst_change = affected_deps
                .iter()
                .map(|(_, change)| *change)
                .max()
                .unwrap_or(ChangeKind::Breaking);
            let conservative_bump = node_version
                .map(|version| required_bump(version, worst_change))
                .unwrap_or(Bump::Minor);
            sink.emit(&Event::RustdocFailed {
                crate_name: node_name.as_str(),
                error: &e.to_string(),
                conservative_bump,
            });
            state.failed.insert(node_name.clone());
            let influences = affected_deps
                .into_iter()
                .map(|(dep_name, _)| DepInfluence {
                    dep_name: dep_name.clone(),
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
        .map(|(name, _)| name.as_str().replace('-', "_"))
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
        let dep_norm = dep_name.as_str().replace('-', "_");
        let is_leaked = leaked.keys().any(|k| k.replace('-', "_") == dep_norm);

        if is_leaked {
            let edge_bump = node_version
                .map(|version| required_bump(version, dep_change))
                .unwrap_or(Bump::Minor);
            let details: Vec<LeakDetail> = if cli.verbose {
                leaked
                    .iter()
                    .filter(|(leaked_name, _)| leaked_name.replace('-', "_") == dep_norm)
                    .flat_map(|(_, items)| items.iter())
                    .map(|detail| LeakDetail {
                        item_kind: detail.item_kind.to_string(),
                        item_name: detail.item_name.clone(),
                        leaked_types: detail.leaked_types.iter().cloned().collect(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            sink.emit(&Event::LeakDetected {
                crate_name: node_name.as_str(),
                dep: dep_name.as_str(),
                bump: edge_bump,
                details: &details,
            });
            influences.push(DepInfluence {
                dep_name: dep_name.clone(),
                bump: edge_bump,
            });
            worst_change = worst_change.max(dep_change);
        } else {
            influences.push(DepInfluence {
                dep_name: dep_name.clone(),
                bump: Bump::Patch,
            });
        }
    }

    let final_bump = node_version
        .map(|version| required_bump(version, worst_change))
        .unwrap_or(Bump::Patch);

    Ok((worst_change, final_bump, influences))
}

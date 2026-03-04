use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo_metadata::Resolve;
use rayon::iter::{IntoParallelRefIterator as _, ParallelIterator as _};

use crate::evaluate::{
    AnalysisOptions, DepInfluence, PreCheckResult, WaveState, WorkspaceContext, analyze_rustdoc,
    conservative_fallback, is_normal_dep, pre_check_crate,
};
use crate::semver::Bump;

pub struct PropagationResult {
    pub state: WaveState,
    pub patch_crates: HashSet<String>,
    pub tree_edges: HashMap<String, Vec<(String, Bump)>>,
}

/// Run the wave-based propagation loop: for each wave of crates whose deps are
/// already resolved, pre-check, build rustdoc JSON in parallel, then analyze
/// leaks sequentially. Returns the accumulated state with seeds/new/local
/// bumps already filtered out.
pub fn run(
    ctx: &WorkspaceContext,
    mut state: WaveState,
    opts: &AnalysisOptions,
    resolve: &Resolve,
    all_seeds: &HashSet<String>,
    new_crates: &HashSet<String>,
    local_bumps: &HashMap<String, Bump>,
) -> Result<PropagationResult> {
    let mut patch_crates: HashSet<String> = HashSet::new();
    let mut tree_edges: HashMap<String, Vec<(String, Bump)>> = HashMap::new();

    let mut pending_nodes: Vec<_> = resolve
        .nodes
        .iter()
        .filter(|n| ctx.workspace_members.contains(&n.id))
        .collect();

    let mut processed: HashSet<String> = HashSet::new();

    while !pending_nodes.is_empty() {
        let ready: Vec<_> = pending_nodes
            .iter()
            .filter(|node| {
                node.deps.iter().filter(|d| is_normal_dep(d)).all(|dep| {
                    if dep.pkg == node.id {
                        true
                    } else if ctx.workspace_members.contains(&dep.pkg) {
                        processed.contains(&ctx.pkg_names[&dep.pkg])
                    } else {
                        true
                    }
                })
            })
            .collect();

        let pre_checks: Vec<_> = ready
            .iter()
            .map(|node| (node, pre_check_crate(node, ctx, &state, opts)))
            .map(|(node, res)| res.map(|val| (node, val)))
            .collect::<Result<Vec<_>, _>>()?;

        let rustdoc_results: HashMap<String, Result<PathBuf>> = pre_checks
            .par_iter()
            .filter_map(|(node, result)| match result {
                PreCheckResult::NeedsRustdoc { manifest, .. } => {
                    let name = ctx.pkg_names[&node.id].clone();
                    let json = rustdoc_json::Builder::default()
                        .target_dir(Path::new("target/semwave/").join(&name))
                        .manifest_path(manifest)
                        .toolchain(&opts.toolchain)
                        .all_features(true)
                        .cap_lints(Some("allow"))
                        .silent(!opts.rustdoc_stderr)
                        .build()
                        .context("cannot build rustdoc json");
                    Some((name, json))
                }
                _ => None,
            })
            .collect();

        for (node, pre_check) in &pre_checks {
            let node_name = &ctx.pkg_names[&node.id];

            match pre_check {
                PreCheckResult::EarlyReturn(change_kind, _bump, influences) => {
                    record_influences(&mut tree_edges, node_name, influences);
                    state.record_change(node_name, *change_kind, &mut patch_crates);
                }
                PreCheckResult::NeedsRustdoc {
                    manifest: _,
                    affected_deps,
                } => {
                    let node_version = ctx.pkg_versions.get(node_name);

                    let json_result = rustdoc_results
                        .get(node_name)
                        .context("missing rustdoc result")?;

                    let (worst_change, influences) = match json_result {
                        Ok(json_path) => {
                            let analysis = analyze_rustdoc(
                                node_name,
                                json_path,
                                affected_deps,
                                node_version,
                                opts,
                            )?;
                            (analysis.worst_change, analysis.influences)
                        }
                        Err(e) => {
                            state.failed.insert(node_name.clone());
                            conservative_fallback(node_name, affected_deps, node_version, e)
                        }
                    };

                    record_influences(&mut tree_edges, node_name, &influences);
                    state.record_change(node_name, worst_change, &mut patch_crates);
                }
            }
        }

        for (node, _) in &pre_checks {
            let node_name = &ctx.pkg_names[&node.id];
            processed.insert(node_name.clone());
        }
        pending_nodes.retain(|n| !processed.contains(&ctx.pkg_names[&n.id]));
    }

    let semwave_target = Path::new("target/semwave");
    if semwave_target.exists() {
        let _ = std::fs::remove_dir_all(semwave_target);
    }

    for seed in all_seeds {
        state.breaking_crates.remove(seed);
        state.additive_crates.remove(seed);
        patch_crates.remove(seed);
    }

    for name in new_crates {
        state.breaking_crates.remove(name);
        state.additive_crates.remove(name);
        patch_crates.remove(name);
    }

    for (name, existing_bump) in local_bumps {
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

    Ok(PropagationResult {
        state,
        patch_crates,
        tree_edges,
    })
}

fn record_influences(
    tree_edges: &mut HashMap<String, Vec<(String, Bump)>>,
    node_name: &str,
    influences: &[DepInfluence],
) {
    for inf in influences {
        tree_edges
            .entry(inf.dep_name.clone())
            .or_default()
            .push((node_name.to_owned(), inf.bump));
    }
}

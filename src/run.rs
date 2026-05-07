use anyhow::{Context, Result, anyhow};
use cargo_metadata::{CargoOpt, MetadataCommand, Node, PackageId};
use std::collections::{HashMap, HashSet};

use crate::cli::Cli;
use crate::display::print_influence_tree;
use crate::evaluate::{WaveState, WorkspaceContext, evaluate_crate_bump, is_normal_dep};
use crate::report::{Event, EventSink};
use crate::seeds::detect_version_changes;
use crate::semver::{Bump, ChangeKind, required_bump};
use crate::types::{CrateName, ManifestPath, MissingBumpItem, TreeEdge, UnderBumpedItem};

pub fn run(cli: &Cli, sink: &dyn EventSink) -> Result<()> {
    let is_direct = cli.direct.is_some();
    let (all_seeds, mut state, local_bumps, new_crates) = if let Some(direct_crates) = &cli.direct {
        let seeds: HashSet<CrateName> = direct_crates
            .iter()
            .map(|name| CrateName::from(name.as_str()))
            .collect();
        sink.emit(&Event::DirectModeBanner { seeds: &seeds });
        let wave = WaveState {
            breaking_crates: seeds.clone(),
            additive_crates: HashSet::new(),
            failed: HashSet::new(),
        };
        (seeds, wave, HashMap::new(), HashSet::new())
    } else {
        sink.emit(&Event::ComparingRefs {
            source: &cli.source,
            target: &cli.target,
        });
        let changes = detect_version_changes(&cli.source, &cli.target, sink)?;

        if changes.breaking_seeds.is_empty() && changes.additive_seeds.is_empty() {
            sink.emit(&Event::NoChangesDetected);
            return Ok(());
        }

        if !changes.breaking_seeds.is_empty() {
            sink.emit(&Event::BreakingSeeds {
                names: &changes.breaking_seeds,
            });
        }
        if !changes.additive_seeds.is_empty() {
            sink.emit(&Event::AdditiveSeeds {
                names: &changes.additive_seeds,
            });
        }

        let all_seeds: HashSet<CrateName> = changes
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

    let mut patch_crates: HashSet<CrateName> = HashSet::new();
    let mut tree_edges: HashMap<CrateName, Vec<TreeEdge>> = HashMap::new();

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
            .map(|pkg| (pkg.id.clone(), CrateName::from(pkg.name.to_string())))
            .collect(),
        pkg_manifest_paths: metadata
            .packages
            .iter()
            .filter(|pkg| workspace_members.contains(&pkg.id))
            .map(|pkg| {
                (
                    CrateName::from(pkg.name.to_string()),
                    ManifestPath::from(pkg.manifest_path.to_string()),
                )
            })
            .collect(),
        pkg_has_lib: metadata
            .packages
            .iter()
            .filter(|pkg| workspace_members.contains(&pkg.id))
            .filter(|pkg| {
                pkg.targets
                    .iter()
                    .any(|target| target.is_lib() || target.is_proc_macro())
            })
            .map(|pkg| CrateName::from(pkg.name.to_string()))
            .collect(),
        pkg_versions: metadata
            .packages
            .iter()
            .filter(|pkg| workspace_members.contains(&pkg.id))
            .map(|pkg| (CrateName::from(pkg.name.to_string()), pkg.version.clone()))
            .collect(),
    };

    if is_direct {
        let all_known: HashSet<&str> = metadata
            .packages
            .iter()
            .map(|pkg| pkg.name.as_str())
            .collect();
        let unknown: Vec<&str> = all_seeds
            .iter()
            .filter(|name| !all_known.contains(name.as_str()))
            .map(|name| name.as_str())
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
        .filter(|node| workspace_members.contains(&node.id))
        .collect();

    let mut processed: HashSet<CrateName> = HashSet::new();

    while !pending_nodes.is_empty() {
        let mut made_progress = false;

        for i in (0..pending_nodes.len()).rev() {
            let node = pending_nodes[i];
            let node_name = &ctx.pkg_names[&node.id];

            let deps_ready = node
                .deps
                .iter()
                .filter(|dep| is_normal_dep(dep))
                .all(|dep| {
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
                    evaluate_crate_bump(node, &ctx, &mut state, cli, sink)?;

                for inf in &influences {
                    tree_edges
                        .entry(inf.dep_name.clone())
                        .or_default()
                        .push(TreeEdge {
                            child: node_name.clone(),
                            bump: inf.bump,
                        });
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
                .map(|node| ctx.pkg_names[&node.id].as_str())
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

    let mut major_list: HashSet<CrateName> = HashSet::new();
    let mut minor_list: HashSet<CrateName> = HashSet::new();
    let mut patch_list: HashSet<CrateName> = patch_crates;

    for name in &state.breaking_crates {
        let bump = ctx
            .pkg_versions
            .get(name)
            .map(|version| required_bump(version, ChangeKind::Breaking))
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
            .map(|version| required_bump(version, ChangeKind::Additive))
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
        sink.emit(&Event::InfluenceTreeHeader);
        print_influence_tree(&all_seeds, &tree_edges, sink);
    }

    sink.emit(&Event::AnalysisCompleteHeader);
    sink.emit(&Event::BumpList {
        level: Bump::Major,
        names: &major_list,
    });
    sink.emit(&Event::BumpList {
        level: Bump::Minor,
        names: &minor_list,
    });
    sink.emit(&Event::BumpList {
        level: Bump::Patch,
        names: &patch_list,
    });

    if !state.failed.is_empty() {
        sink.emit(&Event::FailedRustdocSummary {
            names: &state.failed,
        });
    }

    let all_required: HashMap<&CrateName, Bump> = major_list
        .iter()
        .map(|name| (name, Bump::Major))
        .chain(minor_list.iter().map(|name| (name, Bump::Minor)))
        .chain(patch_list.iter().map(|name| (name, Bump::Patch)))
        .collect();

    let mut has_errors = false;

    let under_bumped: Vec<UnderBumpedItem> = all_required
        .iter()
        .filter(|(name, _)| !state.failed.contains(**name))
        .filter_map(|(&name, required)| {
            local_bumps
                .get(name)
                .filter(|local| local < &required)
                .map(|local| UnderBumpedItem {
                    name,
                    required: *required,
                    local: *local,
                })
        })
        .collect();
    if !under_bumped.is_empty() {
        has_errors = true;
        sink.emit(&Event::UnderBumped {
            items: &under_bumped,
        });
    }

    if !local_bumps.is_empty() {
        let missing: Vec<MissingBumpItem> = all_required
            .iter()
            .filter(|(name, _)| {
                !local_bumps.contains_key(**name)
                    && !state.failed.contains(**name)
                    && !new_crates.contains(**name)
            })
            .map(|(name, bump)| MissingBumpItem {
                name,
                required: *bump,
            })
            .collect();
        if !missing.is_empty() {
            has_errors = true;
            sink.emit(&Event::MissingBumps { items: &missing });
        }
    }

    if has_errors {
        return Err(anyhow!("semwave encountered errors during analysis"));
    }

    Ok(())
}

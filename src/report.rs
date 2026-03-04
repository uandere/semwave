use std::collections::{HashMap, HashSet};

use colored::Colorize as _;

use crate::display::print_influence_tree;
use crate::evaluate::WorkspaceContext;
use crate::propagate::PropagationResult;
use crate::semver::{Bump, ChangeKind, format_name_set, required_bump};

pub struct BumpLists {
    pub major: HashSet<String>,
    pub minor: HashSet<String>,
    pub patch: HashSet<String>,
}

/// Compute the final MAJOR/MINOR/PATCH lists from propagation results,
/// print the influence tree (if requested) and the summary table.
pub fn print_results(
    ctx: &WorkspaceContext,
    result: &PropagationResult,
    all_seeds: &HashSet<String>,
    show_tree: bool,
) -> BumpLists {
    let mut major: HashSet<String> = HashSet::new();
    let mut minor: HashSet<String> = HashSet::new();
    let mut patch: HashSet<String> = result.patch_crates.clone();

    for name in &result.state.breaking_crates {
        let bump = ctx
            .pkg_versions
            .get(name)
            .map(|v| required_bump(v, ChangeKind::Breaking))
            .unwrap_or(Bump::Minor);
        match bump {
            Bump::Major => {
                major.insert(name.clone());
            }
            _ => {
                minor.insert(name.clone());
            }
        }
    }

    for name in &result.state.additive_crates {
        let bump = ctx
            .pkg_versions
            .get(name)
            .map(|v| required_bump(v, ChangeKind::Additive))
            .unwrap_or(Bump::Patch);
        match bump {
            Bump::Minor => {
                minor.insert(name.clone());
            }
            _ => {
                patch.insert(name.clone());
            }
        }
    }

    if show_tree {
        println!("\n{}", "=== Influence Tree ===".bold().green());
        print_influence_tree(all_seeds, &result.tree_edges);
        println!();
    }

    println!("{}", "=== Analysis Complete ===".bold().green());
    println!(
        "{} {}",
        "MAJOR-bump list (Requires MAJOR bump / ↑.0.0):"
            .red()
            .bold(),
        format_name_set(&major)
    );
    println!(
        "{} {}",
        "MINOR-bump list (Requires MINOR bump / x.↑.0):"
            .yellow()
            .bold(),
        format_name_set(&minor)
    );
    println!(
        "{} {}",
        "PATCH-bump list (Requires PATCH bump / x.y.↑):"
            .cyan()
            .bold(),
        format_name_set(&patch)
    );

    if !result.state.failed.is_empty() {
        eprintln!(
            "\n{} The following crates failed rustdoc JSON generation \
             and were conservatively assumed breaking. Verify manually:\n  {}",
            "WARNING:".yellow().bold(),
            format_name_set(&result.state.failed)
        );
    }

    BumpLists {
        major,
        minor,
        patch,
    }
}

/// Check whether local version bumps are sufficient. Returns `true` when
/// there are validation errors (under-bumped or missing bumps).
pub fn validate_bumps(
    bump_lists: &BumpLists,
    local_bumps: &HashMap<String, Bump>,
    failed: &HashSet<String>,
    new_crates: &HashSet<String>,
) -> bool {
    let all_required: HashMap<&String, Bump> = bump_lists
        .major
        .iter()
        .map(|n| (n, Bump::Major))
        .chain(bump_lists.minor.iter().map(|n| (n, Bump::Minor)))
        .chain(bump_lists.patch.iter().map(|n| (n, Bump::Patch)))
        .collect();

    let mut has_errors = false;

    let under_bumped: Vec<_> = all_required
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
                "  {} has {} bump but requires {}",
                name.cyan(),
                local,
                required
            );
        }
    }

    if !local_bumps.is_empty() {
        let missing: Vec<_> = all_required
            .iter()
            .filter(|(name, _)| {
                !local_bumps.contains_key(**name)
                    && !failed.contains(**name)
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

    has_errors
}

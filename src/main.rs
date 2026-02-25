//! # 🌊 semwave
//!
//! A static analysis tool that answers the question:
//!
//! > *"If I bump crates A, B and C in this Rust project — what else do I need to bump and how?"*
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
//!    that crate itself needs a bump — and becomes a new seed, triggering the same check
//!    on *its* dependents, and so on until the wave settles. The bump level
//!    (major/minor/patch) depends on the change type and the consumer's version scheme
//!    (`0.y.z` vs `>=1.0.0`).
//!
//! The output is three lists: **MAJOR** bumps, **MINOR** bumps, and **PATCH** bumps,
//! plus optional warnings when the tool had to guess conservatively.

#![allow(clippy::format_in_format_args)]

use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, MetadataCommand, Node, NodeDep, PackageId};
use clap::Parser;
use colored::Colorize;
use regex::Regex;
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

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

    /// Print the public API lines that cause leaks
    #[arg(long, short)]
    verbose: bool,

    /// Print an influence tree showing how bumps propagate
    #[arg(long, short)]
    tree: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bump {
    None,
    Patch,
    Minor,
    Major,
}

/// Semantic classification of a version change, independent of the version scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ChangeKind {
    None,
    Patch,
    Additive,
    Breaking,
}

/// Per-dependency influence: which dep caused the bump and how.
#[derive(Debug, Clone)]
struct DepInfluence {
    dep_name: String,
    bump: Bump,
}

/// Return type for `detect_version_changes`: (breaking_seeds, additive_seeds, local_bumps).
type VersionChanges = (HashSet<String>, HashSet<String>, HashMap<String, Bump>);

/// Shared read-only context passed to `evaluate_crate_bump` to avoid too many arguments.
struct WorkspaceContext {
    pkg_names: HashMap<PackageId, String>,
    pkg_manifest_paths: HashMap<String, String>,
    pkg_has_lib: HashSet<String>,
    pkg_versions: HashMap<String, Version>,
}

/// Classify the semantic change between two versions.
/// For `0.y.z`: minor change = Breaking, patch change = Patch.
/// For `>=1.0.0`: major change = Breaking, minor change = Additive, patch change = Patch.
fn classify_version_change(old: &Version, new: &Version) -> ChangeKind {
    if old.major == 0 && new.major == 0 {
        if old.minor != new.minor {
            ChangeKind::Breaking
        } else if old.patch != new.patch {
            ChangeKind::Patch
        } else {
            ChangeKind::None
        }
    } else if old.major != new.major {
        ChangeKind::Breaking
    } else if old.minor != new.minor {
        ChangeKind::Additive
    } else if old.patch != new.patch {
        ChangeKind::Patch
    } else {
        ChangeKind::None
    }
}

/// Map a semantic change kind to the concrete version bump needed,
/// based on the consumer crate's current version scheme.
fn required_bump(version: &Version, change: ChangeKind) -> Bump {
    match change {
        ChangeKind::Breaking => {
            if version.major == 0 {
                Bump::Minor
            } else {
                Bump::Major
            }
        }
        ChangeKind::Additive => {
            if version.major == 0 {
                Bump::Patch
            } else {
                Bump::Minor
            }
        }
        ChangeKind::Patch => Bump::Patch,
        ChangeKind::None => Bump::None,
    }
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
            anyhow::bail!("Cannot make progress — cycle in workspace dependencies");
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
            "\n{} The following crates failed to build with `cargo public-api` \
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

    let under_bumped: Vec<(&String, Bump, &Bump)> = all_required
        .iter()
        .filter_map(|(name, required)| {
            local_bumps
                .get(*name)
                .filter(|local| local < &required)
                .map(|local| (*name, *required, local))
        })
        .collect();
    if !under_bumped.is_empty() {
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

    Ok(())
}

// ---------------------------------------------------------------------------
// Influence tree printing
// ---------------------------------------------------------------------------

fn print_influence_tree(
    seeds: &HashSet<String>,
    tree_edges: &HashMap<String, Vec<(String, Bump)>>,
) {
    let mut sorted_seeds: Vec<&String> = seeds.iter().collect();
    sorted_seeds.sort();

    for (i, seed) in sorted_seeds.iter().enumerate() {
        let is_last_root = i == sorted_seeds.len() - 1;
        let connector = if is_last_root {
            "└── "
        } else {
            "├── "
        };
        println!(
            "{}{}",
            connector.dimmed(),
            format!("{} (seed)", seed).yellow().bold()
        );
        let prefix = if is_last_root { "    " } else { "│   " };
        print_tree_children(seed, tree_edges, prefix, &mut HashSet::new());
    }
}

fn print_tree_children(
    parent: &str,
    tree_edges: &HashMap<String, Vec<(String, Bump)>>,
    prefix: &str,
    visited: &mut HashSet<String>,
) {
    let Some(children) = tree_edges.get(parent) else {
        return;
    };

    let mut sorted: Vec<&(String, Bump)> = children.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    for (i, (child, bump)) in sorted.iter().enumerate() {
        let is_last = i == sorted.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        let (colored_connector, bump_label) = match bump {
            Bump::Major => (
                connector.red().bold().to_string(),
                "MAJOR".red().bold().to_string(),
            ),
            Bump::Minor => (
                connector.red().bold().to_string(),
                "MINOR".red().bold().to_string(),
            ),
            Bump::Patch => (connector.green().to_string(), "PATCH".green().to_string()),
            Bump::None => (connector.dimmed().to_string(), "none".dimmed().to_string()),
        };

        if visited.contains(child) {
            println!(
                "{}{}{} {}",
                prefix.dimmed(),
                colored_connector,
                child.cyan(),
                format!("({}, already shown above)", bump_label).dimmed()
            );
            continue;
        }
        visited.insert(child.clone());

        println!(
            "{}{}{}  {}",
            prefix.dimmed(),
            colored_connector,
            child.cyan().bold(),
            format!("({})", bump_label)
        );

        let next_prefix = format!("{}{}", prefix, child_prefix);
        print_tree_children(child, tree_edges, &next_prefix, visited);
    }
}

// ---------------------------------------------------------------------------
// Step 1: Detect dependency version changes between two git refs
// ---------------------------------------------------------------------------

fn detect_version_changes(source: &str, target: &str) -> Result<VersionChanges> {
    let changed_files = get_changed_cargo_tomls(source, target)?;

    let mut breaking_seeds: HashSet<String> = HashSet::new();
    let mut additive_seeds: HashSet<String> = HashSet::new();
    let mut local_bumps: HashMap<String, Bump> = HashMap::new();

    println!("{}", "Detected version changes:".bold());

    for file in &changed_files {
        let old_doc = read_toml_at_ref(source, file);
        let new_doc = read_toml_at_ref(target, file);

        let (old_doc, new_doc) = match (old_doc, new_doc) {
            (Ok(o), Ok(n)) => (o, n),
            _ => continue,
        };

        let old_deps = extract_dep_versions(&old_doc);
        let new_deps = extract_dep_versions(&new_doc);

        for (name, new_ver_str) in &new_deps {
            let Some(old_ver_str) = old_deps.get(name) else {
                continue;
            };
            if old_ver_str == new_ver_str {
                continue;
            }
            let (Ok(ov), Ok(nv)) = (
                normalize_version(old_ver_str),
                normalize_version(new_ver_str),
            ) else {
                continue;
            };
            let change = classify_version_change(&ov, &nv);
            match change {
                ChangeKind::Breaking => {
                    if breaking_seeds.insert(name.clone()) {
                        println!(
                            "  {} {}: {} -> {} {}",
                            "[dep]".dimmed(),
                            name.cyan(),
                            old_ver_str.dimmed(),
                            new_ver_str.white().bold(),
                            "(BREAKING)".red().bold()
                        );
                    }
                }
                ChangeKind::Additive => {
                    if !breaking_seeds.contains(name) && additive_seeds.insert(name.clone()) {
                        println!(
                            "  {} {}: {} -> {} {}",
                            "[dep]".dimmed(),
                            name.cyan(),
                            old_ver_str.dimmed(),
                            new_ver_str.white().bold(),
                            "(ADDITIVE)".yellow().bold()
                        );
                    }
                }
                _ => {}
            }
        }

        let old_pkg = extract_package_version(&old_doc, source, file);
        let new_pkg = extract_package_version(&new_doc, target, file);

        if let (Ok((name, ov)), Ok((_, nv))) = (old_pkg, new_pkg) {
            if local_bumps.contains_key(&name) {
                continue;
            }
            let change = classify_version_change(&ov, &nv);
            let bump = required_bump(&ov, change);
            match change {
                ChangeKind::Breaking => {
                    println!(
                        "  {} {}: {} -> {} {}",
                        "[local]".dimmed(),
                        name.cyan(),
                        ov.to_string().dimmed(),
                        nv.to_string().white().bold(),
                        "(BREAKING)".red().bold()
                    );
                    breaking_seeds.insert(name.clone());
                    local_bumps.insert(name, bump);
                }
                ChangeKind::Additive => {
                    println!(
                        "  {} {}: {} -> {} {}",
                        "[local]".dimmed(),
                        name.cyan(),
                        ov.to_string().dimmed(),
                        nv.to_string().white().bold(),
                        "(ADDITIVE)".yellow().bold()
                    );
                    additive_seeds.insert(name.clone());
                    local_bumps.insert(name, bump);
                }
                ChangeKind::Patch => {
                    println!(
                        "  {} {}: {} -> {} {}",
                        "[local]".dimmed(),
                        name.cyan(),
                        ov.to_string().dimmed(),
                        nv.to_string().white().bold(),
                        "(PATCH)".green()
                    );
                    local_bumps.insert(name, Bump::Patch);
                }
                ChangeKind::None => {}
            }
        }
    }

    Ok((breaking_seeds, additive_seeds, local_bumps))
}

fn get_changed_cargo_tomls(source: &str, target: &str) -> Result<Vec<String>> {
    let diff_range = format!("{}..{}", source, target);
    let output = Command::new("git")
        .args(["diff", "--name-only", &diff_range])
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| l.ends_with("Cargo.toml"))
        .map(|l| l.to_string())
        .collect())
}

/// Extract all dependency name -> version mappings from a parsed Cargo.toml.
/// Covers \[workspace.dependencies\], \[dependencies\], \[dev-dependencies\],
/// and \[build-dependencies\]. Entries using `workspace = true` (no explicit
/// version) are skipped -- their versions come from the workspace root.
fn extract_dep_versions(doc: &toml::Value) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    if let Some(ws_deps) = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, value) in ws_deps {
            if let Some(ver) = dep_version_string(value) {
                versions.insert(name.clone(), ver);
            }
        }
    }

    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = doc.get(section).and_then(|d| d.as_table()) {
            for (name, value) in deps {
                if let Some(ver) = dep_version_string(value) {
                    versions.entry(name.clone()).or_insert(ver);
                }
            }
        }
    }

    versions
}

/// Extract the version string from a dependency value.
/// Returns None for workspace references or entries without an explicit version.
fn dep_version_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Table(t) => t
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Normalize a Cargo version requirement string to a proper semver Version.
/// Handles short forms like "0.30" -> "0.30.0" and common prefixes.
fn normalize_version(ver_str: &str) -> Result<Version> {
    let ver = ver_str
        .trim()
        .trim_start_matches('^')
        .trim_start_matches('~')
        .trim_start_matches('=');

    if ver.contains(',') || ver.contains('>') || ver.contains('<') || ver.contains('*') {
        anyhow::bail!("Complex version requirement not supported: {}", ver_str);
    }

    let parts: Vec<&str> = ver.split('.').collect();
    let normalized = match parts.len() {
        1 => format!("{}.0.0", ver),
        2 => format!("{}.0", ver),
        _ => ver.to_string(),
    };

    Version::parse(&normalized).with_context(|| format!("Invalid version: {}", ver_str))
}

// ---------------------------------------------------------------------------
// TOML / git helpers
// ---------------------------------------------------------------------------

fn read_toml_at_ref(git_ref: &str, file_path: &str) -> Result<toml::Value> {
    let output = Command::new("git")
        .args(["show", &format!("{}:{}", git_ref, file_path)])
        .output()
        .with_context(|| format!("Failed to get {} at {}", file_path, git_ref))?;

    if !output.status.success() {
        anyhow::bail!("File {} does not exist at ref {}", file_path, git_ref);
    }

    let content = String::from_utf8_lossy(&output.stdout);
    content
        .parse()
        .with_context(|| format!("Failed to parse {} at {}", file_path, git_ref))
}

/// Extract package name and version from an already-parsed Cargo.toml.
/// Resolves `version.workspace = true` by walking up to the workspace root.
fn extract_package_version(
    doc: &toml::Value,
    git_ref: &str,
    file_path: &str,
) -> Result<(String, Version)> {
    let pkg = doc.get("package").context("No [package] table")?;

    let name = pkg
        .get("name")
        .and_then(|n| n.as_str())
        .context("No package.name")?
        .to_string();

    let version_value = pkg.get("version").context("No package.version")?;

    let version = if let Some(v) = version_value.as_str() {
        Version::parse(v).with_context(|| format!("Invalid semver: {}", v))?
    } else if version_value.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
        find_workspace_version(git_ref, file_path).with_context(|| {
            format!(
                "{} uses version.workspace = true but could not resolve workspace version",
                file_path
            )
        })?
    } else {
        anyhow::bail!("Unexpected version format in {}", file_path);
    };

    Ok((name, version))
}

/// Walk parent directories to find the nearest \[workspace\] manifest and
/// return its \[workspace.package\].version.
fn find_workspace_version(git_ref: &str, crate_toml_path: &str) -> Result<Version> {
    let mut dir = Path::new(crate_toml_path)
        .parent()
        .context("Cargo.toml has no parent dir")?;

    loop {
        dir = dir.parent().unwrap_or(Path::new(""));

        let candidate = if dir == Path::new("") {
            "Cargo.toml".to_string()
        } else {
            format!("{}/Cargo.toml", dir.display())
        };

        if let Ok(doc) = read_toml_at_ref(git_ref, &candidate)
            && let Some(ws) = doc.get("workspace")
        {
            if let Some(version_str) = ws
                .get("package")
                .and_then(|p| p.get("version"))
                .and_then(|v| v.as_str())
            {
                return Version::parse(version_str)
                    .with_context(|| format!("Invalid workspace version: {}", version_str));
            }
            anyhow::bail!(
                "Found workspace at {} but no [workspace.package].version",
                candidate
            );
        }

        if dir == Path::new("") {
            anyhow::bail!(
                "Walked to repo root without finding a workspace manifest for {}",
                crate_toml_path
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Cycle detection (DFS-based)
// ---------------------------------------------------------------------------

fn find_cycle<'a>(adj: &HashMap<&'a str, Vec<&'a str>>) -> Option<Vec<&'a str>> {
    let nodes: HashSet<&str> = adj
        .keys()
        .copied()
        .chain(adj.values().flatten().copied())
        .collect();

    let mut state: HashMap<&str, u8> = nodes.iter().map(|&n| (n, 0u8)).collect();
    let mut stack: Vec<&str> = Vec::new();

    for &start in &nodes {
        if state[start] != 0 {
            continue;
        }
        if let Some(cycle) = dfs_cycle(start, adj, &mut state, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

fn dfs_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    state: &mut HashMap<&'a str, u8>,
    stack: &mut Vec<&'a str>,
) -> Option<Vec<&'a str>> {
    state.insert(node, 1);
    stack.push(node);

    if let Some(neighbors) = adj.get(node) {
        for &next in neighbors {
            match state.get(next).copied().unwrap_or(0) {
                0 => {
                    if let Some(cycle) = dfs_cycle(next, adj, state, stack) {
                        return Some(cycle);
                    }
                }
                1 => {
                    let pos = stack.iter().position(|&s| s == next).unwrap();
                    let mut cycle: Vec<&str> = stack[pos..].to_vec();
                    cycle.push(next);
                    return Some(cycle);
                }
                _ => {}
            }
        }
    }

    stack.pop();
    state.insert(node, 2);
    None
}

/// Returns true if this dependency edge includes a Normal (non-dev, non-build)
/// dependency kind. Only normal deps affect the public API and semver surface.
fn is_normal_dep(dep: &NodeDep) -> bool {
    dep.dep_kinds
        .iter()
        .any(|dk| dk.kind == DependencyKind::Normal)
}

// ---------------------------------------------------------------------------
// Evaluate public API exposure
// ---------------------------------------------------------------------------

fn evaluate_crate_bump(
    node: &Node,
    ctx: &WorkspaceContext,
    breaking_crates: &HashSet<String>,
    additive_crates: &HashSet<String>,
    failed: &mut HashSet<String>,
    verbose: bool,
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

    let output = Command::new("cargo")
        .args([
            "+nightly",
            "public-api",
            "--manifest-path",
            manifest,
            "--simplified",
        ])
        .output()
        .with_context(|| format!("Failed to run cargo public-api on {}", node_name))?;

    if !output.status.success() {
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        let last_meaningful_line = stderr_text
            .lines()
            .rev()
            .find(|l| !l.is_empty())
            .unwrap_or("(no stderr)");
        let worst_change = affected_deps
            .iter()
            .map(|(_, ck)| *ck)
            .max()
            .unwrap_or(ChangeKind::Breaking);
        let conservative_bump = node_version
            .map(|v| required_bump(v, worst_change))
            .unwrap_or(Bump::Minor);
        eprintln!(
            "  {} cargo public-api failed for {}: {}\n  \
             Conservatively assuming {:?} bump.",
            "WARNING:".yellow().bold(),
            node_name.cyan(),
            last_meaningful_line,
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

    let api_text = String::from_utf8_lossy(&output.stdout);

    let mut worst_change = ChangeKind::Patch;
    let mut influences = Vec::new();

    for (dep_name, dep_change) in affected_deps {
        let mod_name = dep_name.replace('-', "_");

        let pattern = format!(r"(?:^|[^a-zA-Z0-9_:]){}::", regex::escape(&mod_name));
        let re = Regex::new(&pattern)?;

        let matching_lines: Vec<&str> = api_text.lines().filter(|line| re.is_match(line)).collect();

        if !matching_lines.is_empty() {
            let edge_change = dep_change;
            let edge_bump = node_version
                .map(|v| required_bump(v, edge_change))
                .unwrap_or(Bump::Minor);
            println!(
                "  {} {} leaks {} ({:?}):",
                "->".red().bold(),
                node_name.red().bold(),
                dep_name.yellow(),
                edge_bump
            );
            if verbose {
                for line in &matching_lines {
                    println!("     {}", line.dimmed());
                }
            }
            influences.push(DepInfluence {
                dep_name,
                bump: edge_bump,
            });
            worst_change = worst_change.max(edge_change);
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

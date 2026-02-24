#![allow(clippy::format_in_format_args)]

use anyhow::{Context, Result};
use cargo_metadata::{MetadataCommand, Node, PackageId};
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

    /// Comma-separated crate names to treat as MINOR-bumped seeds directly,
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Bump {
    Minor,
    Patch,
    None,
}

/// Per-dependency influence: which dep caused the bump and how.
#[derive(Debug, Clone)]
struct DepInfluence {
    dep_name: String,
    bump: Bump,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.no_color {
        colored::control::set_override(false);
    }

    let (seeds, local_bumps) = if let Some(direct_crates) = cli.direct {
        let seeds: HashSet<String> = direct_crates.into_iter().collect();
        println!(
            "{} assuming MINOR bump for {}\n",
            "Direct mode:".bold(),
            format!("{:?}", seeds).cyan()
        );
        (seeds, HashMap::new())
    } else {
        println!(
            "Comparing versions between {} and {}...\n",
            cli.source.cyan().bold(),
            cli.target.cyan().bold()
        );
        let (seeds, local_bumps) = detect_version_changes(&cli.source, &cli.target)?;

        if seeds.is_empty() {
            println!(
                "{}",
                "No minor/major version bumps detected. Nothing to propagate.".green()
            );
            return Ok(());
        }

        println!(
            "\n{} {}\n",
            "Seed dependencies (MINOR/MAJOR-bumped):".bold(),
            format!("{:?}", seeds).cyan()
        );
        (seeds, local_bumps)
    };

    let mut current_y = seeds.clone();
    let mut current_x: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    // influencer -> Vec<(influenced_crate, edge_bump)>
    let mut tree_edges: HashMap<String, Vec<(String, Bump)>> = HashMap::new();

    let metadata = MetadataCommand::new()
        .exec()
        .context("Failed to run cargo metadata")?;

    let resolve = metadata.resolve.context("No resolve graph found")?;

    let workspace_members: HashSet<&PackageId> = metadata.workspace_members.iter().collect();

    let pkg_names: HashMap<&PackageId, String> = metadata
        .packages
        .iter()
        .map(|p| (&p.id, p.name.to_string().clone()))
        .collect();

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
            let node_name = pkg_names[&node.id].clone();

            let deps_ready = node.deps.iter().all(|dep| {
                if dep.pkg == node.id {
                    true
                } else if workspace_members.contains(&dep.pkg) {
                    processed.contains(&pkg_names[&dep.pkg])
                } else {
                    true
                }
            });

            if deps_ready {
                let (bump, influences) =
                    evaluate_crate_bump(node, &pkg_names, &current_y, &mut failed, cli.verbose)?;

                for inf in &influences {
                    tree_edges
                        .entry(inf.dep_name.clone())
                        .or_default()
                        .push((node_name.clone(), inf.bump.clone()));
                }

                match bump {
                    Bump::Minor => {
                        current_y.insert(node_name.clone());
                    }
                    Bump::Patch => {
                        current_x.insert(node_name.clone());
                    }
                    Bump::None => {}
                }

                processed.insert(node_name);
                pending_nodes.remove(i);
                made_progress = true;
            }
        }

        if !made_progress {
            let stuck: Vec<String> = pending_nodes
                .iter()
                .map(|n| pkg_names[&n.id].clone())
                .collect();
            let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
            for node in &pending_nodes {
                let name = pkg_names[&node.id].as_str();
                for dep in &node.deps {
                    if dep.pkg == node.id {
                        continue;
                    }
                    let dep_name = pkg_names[&dep.pkg].as_str();
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

    for seed in &seeds {
        current_y.remove(seed);
        current_x.remove(seed);
    }

    for (name, existing_bump) in &local_bumps {
        match existing_bump {
            Bump::Minor => {
                current_y.remove(name);
                current_x.remove(name);
            }
            Bump::Patch => {
                current_x.remove(name);
            }
            Bump::None => {}
        }
    }

    if cli.tree {
        println!("\n{}", "=== Influence Tree ===".bold().green());
        print_influence_tree(&seeds, &tree_edges);
        println!();
    }

    println!("{}", "=== Analysis Complete ===".bold().green());
    println!(
        "{} {:?}",
        "List Y (Requires MINOR bump / 0.↑.0):".yellow().bold(),
        current_y
    );
    println!(
        "{} {:?}",
        "List X (Requires PATCH bump / 0.y.↑):".cyan().bold(),
        current_x
    );

    if !failed.is_empty() {
        eprintln!(
            "\n{} The following crates failed to build with `cargo public-api` \
             and were conservatively placed in list Y (MINOR). Verify manually:\n  {:?}",
            "WARNING:".yellow().bold(),
            failed
        );
    }

    let under_bumped: Vec<&String> = current_y
        .iter()
        .filter(|name| local_bumps.get(*name) == Some(&Bump::Patch))
        .collect();
    if !under_bumped.is_empty() {
        eprintln!(
            "\n{} These crates received PATCH bumps but require MINOR bumps:\n  {:?}",
            "ERROR:".red().bold(),
            under_bumped
        );
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

fn detect_version_changes(
    source: &str,
    target: &str,
) -> Result<(HashSet<String>, HashMap<String, Bump>)> {
    let changed_files = get_changed_cargo_tomls(source, target)?;

    let mut seeds: HashSet<String> = HashSet::new();
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
            if (nv.major != ov.major || nv.minor != ov.minor) && seeds.insert(name.clone()) {
                println!(
                    "  {} {}: {} -> {} {}",
                    "[dep]".dimmed(),
                    name.cyan(),
                    old_ver_str.dimmed(),
                    new_ver_str.white().bold(),
                    "(MINOR+)".yellow().bold()
                );
            }
        }

        let old_pkg = extract_package_version(&old_doc, source, file);
        let new_pkg = extract_package_version(&new_doc, target, file);

        if let (Ok((name, ov)), Ok((_, nv))) = (old_pkg, new_pkg) {
            if local_bumps.contains_key(&name) {
                continue;
            }
            if nv.major != ov.major || nv.minor != ov.minor {
                println!(
                    "  {} {}: {} -> {} {}",
                    "[local]".dimmed(),
                    name.cyan(),
                    ov.to_string().dimmed(),
                    nv.to_string().white().bold(),
                    "(MINOR+)".yellow().bold()
                );
                seeds.insert(name.clone());
                local_bumps.insert(name, Bump::Minor);
            } else if nv.patch != ov.patch {
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
        }
    }

    Ok((seeds, local_bumps))
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
/// Covers [workspace.dependencies], [dependencies], [dev-dependencies],
/// and [build-dependencies]. Entries using `workspace = true` (no explicit
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

/// Walk parent directories to find the nearest [workspace] manifest and
/// return its [workspace.package].version.
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

// ---------------------------------------------------------------------------
// Evaluate public API exposure
// ---------------------------------------------------------------------------

fn evaluate_crate_bump(
    node: &Node,
    pkg_names: &HashMap<&PackageId, String>,
    current_y: &HashSet<String>,
    failed: &mut HashSet<String>,
    verbose: bool,
) -> Result<(Bump, Vec<DepInfluence>)> {
    let node_name = pkg_names[&node.id].clone();

    let bumped_deps: Vec<String> = node
        .deps
        .iter()
        .filter(|d| d.pkg != node.id)
        .map(|d| pkg_names[&d.pkg].clone())
        .filter(|name| current_y.contains(name))
        .collect();

    if bumped_deps.is_empty() {
        return Ok((Bump::None, vec![]));
    }

    println!(
        "Analyzing {} for public API exposure of {}",
        node_name.cyan().bold(),
        format!("{:?}", bumped_deps).dimmed()
    );

    let output = Command::new("cargo")
        .args(["+nightly", "public-api", "-p", &node_name, "--simplified"])
        .output()
        .with_context(|| format!("Failed to run cargo public-api on {}", node_name))?;

    if !output.status.success() {
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        let last_meaningful_line = stderr_text
            .lines()
            .rev()
            .find(|l| !l.is_empty())
            .unwrap_or("(no stderr)");
        eprintln!(
            "  {} cargo public-api failed for {}: {}\n  \
             Conservatively assuming MINOR bump.",
            "WARNING:".yellow().bold(),
            node_name.cyan(),
            last_meaningful_line
        );
        failed.insert(node_name);
        let influences = bumped_deps
            .into_iter()
            .map(|dep_name| DepInfluence {
                dep_name,
                bump: Bump::Minor,
            })
            .collect();
        return Ok((Bump::Minor, influences));
    }

    let api_text = String::from_utf8_lossy(&output.stdout);

    let mut final_bump = Bump::Patch;
    let mut influences = Vec::new();

    for dep_name in bumped_deps {
        let mod_name = dep_name.replace('-', "_");

        let pattern = format!(r"\b{}::", regex::escape(&mod_name));
        let re = Regex::new(&pattern)?;

        let matching_lines: Vec<&str> = api_text.lines().filter(|line| re.is_match(line)).collect();

        if !matching_lines.is_empty() {
            println!(
                "  {} {} leaks {}:",
                "->".red().bold(),
                node_name.red().bold(),
                dep_name.yellow()
            );
            if verbose {
                for line in &matching_lines {
                    println!("     {}", line.dimmed());
                }
            }
            influences.push(DepInfluence {
                dep_name,
                bump: Bump::Minor,
            });
            final_bump = Bump::Minor;
        } else {
            influences.push(DepInfluence {
                dep_name,
                bump: Bump::Patch,
            });
        }
    }

    Ok((final_bump, influences))
}

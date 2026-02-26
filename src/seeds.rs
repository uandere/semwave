use std::{
    collections::{HashMap, HashSet},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result};
use colored::Colorize as _;
use semver::Version;

use crate::semver::{Bump, ChangeKind, classify_version_change, required_bump};

/// Return type for `detect_version_changes`: (breaking_seeds, additive_seeds, local_bumps).
pub type VersionChanges = (HashSet<String>, HashSet<String>, HashMap<String, Bump>);

pub fn detect_version_changes(source: &str, target: &str) -> Result<VersionChanges> {
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

pub fn get_changed_cargo_tomls(source: &str, target: &str) -> Result<Vec<String>> {
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
pub fn extract_dep_versions(doc: &toml::Value) -> HashMap<String, String> {
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
pub fn dep_version_string(value: &toml::Value) -> Option<String> {
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
pub fn normalize_version(ver_str: &str) -> Result<Version> {
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

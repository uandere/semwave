use std::{
    collections::{HashMap, HashSet},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result};
use semver::Version;

use crate::report::{BumpSource, Event, EventSink};
use crate::semver::{Bump, ChangeKind, classify_version_change, required_bump};
use crate::types::CrateName;

/// Result of scanning git diffs for version changes.
pub struct VersionChanges {
    pub breaking_seeds: HashSet<CrateName>,
    pub additive_seeds: HashSet<CrateName>,
    pub local_bumps: HashMap<CrateName, Bump>,
    /// Crates whose Cargo.toml didn't exist on the base ref (newly added).
    pub new_crates: HashSet<CrateName>,
}

pub fn detect_version_changes(
    source: &str,
    target: &str,
    sink: &dyn EventSink,
) -> Result<VersionChanges> {
    let base = merge_base(source, target)?;
    let changed_files = get_changed_cargo_tomls(&base, target)?;

    let mut breaking_seeds: HashSet<CrateName> = HashSet::new();
    let mut additive_seeds: HashSet<CrateName> = HashSet::new();
    let mut local_bumps: HashMap<CrateName, Bump> = HashMap::new();
    let mut new_crates: HashSet<CrateName> = HashSet::new();

    sink.emit(&Event::DetectedChangesHeader);

    for file in &changed_files {
        let (old_toml, new_toml) = match get_toml_file_change(&base, target, file, sink) {
            Some(TomlFileChange::Added { name }) => {
                if let Some(name) = name {
                    new_crates.insert(CrateName::from(name));
                }
                continue;
            }
            Some(TomlFileChange::Changed { old_toml, new_toml }) => (old_toml, new_toml),
            _ => continue,
        };

        let old_deps = extract_dep_versions(&old_toml);
        let new_deps = extract_dep_versions(&new_toml);

        for (name, new_ver_str) in &new_deps {
            let Some(old_ver_str) = old_deps.get(name) else {
                continue;
            };
            if old_ver_str == new_ver_str {
                continue;
            }
            let (Ok(old_ver), Ok(new_ver)) = (
                normalize_version(old_ver_str),
                normalize_version(new_ver_str),
            ) else {
                continue;
            };
            let change = classify_version_change(&old_ver, &new_ver);
            match change {
                ChangeKind::Breaking => {
                    if breaking_seeds.insert(CrateName::from(name.as_str())) {
                        sink.emit(&Event::DepVersionChanged {
                            name,
                            old: old_ver_str,
                            new: new_ver_str,
                            kind: ChangeKind::Breaking,
                        });
                    }
                }
                ChangeKind::Additive
                    if !breaking_seeds.contains(name.as_str())
                        && additive_seeds.insert(CrateName::from(name.as_str())) =>
                {
                    sink.emit(&Event::DepVersionChanged {
                        name,
                        old: old_ver_str,
                        new: new_ver_str,
                        kind: ChangeKind::Additive,
                    });
                }
                _ => {}
            }
        }

        let old_pkg = extract_package_version(&old_toml, &base, file);
        let new_pkg = extract_package_version(&new_toml, target, file);

        if let (Ok((name, old_ver)), Ok((_, new_ver))) = (old_pkg, new_pkg)
            && !local_bumps.contains_key(name.as_str())
        {
            record_local_bump(
                name,
                &old_ver,
                &new_ver,
                BumpSource::Package,
                &mut breaking_seeds,
                &mut additive_seeds,
                &mut local_bumps,
                sink,
            );
        }

        // Workspace root version change: propagate to members using version.workspace = true
        if let (Some(old_ws_ver), Some(new_ws_ver)) = (
            extract_workspace_package_version(&old_toml),
            extract_workspace_package_version(&new_toml),
        ) && old_ws_ver != new_ws_ver
        {
            let workspace_dir = Path::new(file).parent().unwrap_or(Path::new(""));
            for member_rel in extract_workspace_members(&new_toml) {
                if member_rel.contains('*') {
                    sink.emit(&Event::GlobMemberSkipped {
                        member: &member_rel,
                    });
                    continue;
                }
                let member_toml = if workspace_dir == Path::new("") {
                    format!("{}/Cargo.toml", member_rel)
                } else {
                    format!("{}/{}/Cargo.toml", workspace_dir.display(), member_rel)
                };
                if changed_files.contains(&member_toml) {
                    continue;
                }
                let Ok(member_doc) = read_toml_at_ref(target, &member_toml) else {
                    continue;
                };
                let Some(pkg) = member_doc.get("package") else {
                    continue;
                };
                let inherits = pkg
                    .get("version")
                    .and_then(|ver| ver.get("workspace"))
                    .and_then(|ws| ws.as_bool())
                    == Some(true);
                if !inherits {
                    continue;
                }
                let Some(name) = pkg.get("name").and_then(|val| val.as_str()) else {
                    continue;
                };
                let name = name.to_string();
                if !local_bumps.contains_key(name.as_str()) {
                    record_local_bump(
                        name,
                        &old_ws_ver,
                        &new_ws_ver,
                        BumpSource::Workspace,
                        &mut breaking_seeds,
                        &mut additive_seeds,
                        &mut local_bumps,
                        sink,
                    );
                }
            }
        }
    }

    Ok(VersionChanges {
        breaking_seeds,
        additive_seeds,
        local_bumps,
        new_crates,
    })
}

enum TomlFileChange {
    Added {
        name: Option<String>,
    },
    Removed,
    Changed {
        old_toml: toml::Value,
        new_toml: toml::Value,
    },
}

fn get_toml_file_change(
    base: &str,
    target: &str,
    filename: &str,
    sink: &dyn EventSink,
) -> Option<TomlFileChange> {
    let old_toml = read_toml_at_ref(base, filename);
    let new_toml = read_toml_at_ref(target, filename);

    let (old_toml, new_toml) = match (old_toml, new_toml) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(_), Ok(new)) => {
            let name = if let Ok((name, _)) = extract_package_version(&new, target, filename) {
                sink.emit(&Event::NewCrate { name: &name });
                Some(name)
            } else {
                None
            };
            return Some(TomlFileChange::Added { name });
        }
        (Ok(old), Err(_)) => {
            if let Ok((name, _)) = extract_package_version(&old, base, filename) {
                sink.emit(&Event::RemovedCrate { name: &name });
            }
            return Some(TomlFileChange::Removed);
        }
        _ => return None,
    };

    Some(TomlFileChange::Changed { old_toml, new_toml })
}

fn merge_base(source: &str, target: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["merge-base", source, target])
        .output()
        .context("Failed to run git merge-base")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git merge-base failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn get_changed_cargo_tomls(base: &str, target: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", base, target])
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.ends_with("Cargo.toml"))
        .map(|line| line.to_string())
        .collect())
}

fn extract_dep_versions(doc: &toml::Value) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    if let Some(ws_deps) = doc
        .get("workspace")
        .and_then(|ws| ws.get("dependencies"))
        .and_then(|deps| deps.as_table())
    {
        for (name, value) in ws_deps {
            if let Some(version) = dep_version_string(value) {
                versions.insert(name.clone(), version);
            }
        }
    }

    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = doc.get(section).and_then(|val| val.as_table()) {
            for (name, value) in deps {
                if let Some(version) = dep_version_string(value) {
                    versions.entry(name.clone()).or_insert(version);
                }
            }
        }
    }

    versions
}

fn dep_version_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(version) => Some(version.clone()),
        toml::Value::Table(table) => table
            .get("version")
            .and_then(|val| val.as_str())
            .map(|ver| ver.to_string()),
        _ => None,
    }
}

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

fn extract_package_version(
    doc: &toml::Value,
    git_ref: &str,
    file_path: &str,
) -> Result<(String, Version)> {
    let pkg = doc.get("package").context("No [package] table")?;

    let name = pkg
        .get("name")
        .and_then(|val| val.as_str())
        .context("No package.name")?
        .to_string();

    let version_value = pkg.get("version").context("No package.version")?;

    let version = if let Some(ver_str) = version_value.as_str() {
        Version::parse(ver_str).with_context(|| format!("Invalid semver: {}", ver_str))?
    } else if version_value.get("workspace").and_then(|ws| ws.as_bool()) == Some(true) {
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
                .and_then(|pkg| pkg.get("version"))
                .and_then(|val| val.as_str())
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

#[allow(clippy::too_many_arguments)]
fn record_local_bump(
    name: String,
    old_ver: &Version,
    new_ver: &Version,
    source: BumpSource,
    breaking_seeds: &mut HashSet<CrateName>,
    additive_seeds: &mut HashSet<CrateName>,
    local_bumps: &mut HashMap<CrateName, Bump>,
    sink: &dyn EventSink,
) {
    let change = classify_version_change(old_ver, new_ver);
    if matches!(change, ChangeKind::None) {
        return;
    }
    let bump = required_bump(old_ver, change);
    sink.emit(&Event::LocalPackageBump {
        name: &name,
        old: old_ver,
        new: new_ver,
        kind: change,
        source,
    });
    let crate_name = CrateName::from(name);
    match change {
        ChangeKind::Breaking => {
            breaking_seeds.insert(crate_name.clone());
            local_bumps.insert(crate_name, bump);
        }
        ChangeKind::Additive => {
            additive_seeds.insert(crate_name.clone());
            local_bumps.insert(crate_name, bump);
        }
        ChangeKind::Patch => {
            local_bumps.insert(crate_name, Bump::Patch);
        }
        ChangeKind::None => {}
    }
}

fn extract_workspace_package_version(doc: &toml::Value) -> Option<Version> {
    doc.get("workspace")?
        .get("package")?
        .get("version")?
        .as_str()
        .and_then(|ver_str| Version::parse(ver_str).ok())
}

fn extract_workspace_members(doc: &toml::Value) -> Vec<String> {
    doc.get("workspace")
        .and_then(|ws| ws.get("members"))
        .and_then(|members| members.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|val| val.as_str().map(|path| path.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_single_part() {
        let v = normalize_version("1").unwrap();
        assert_eq!(v, Version::parse("1.0.0").unwrap());
    }

    #[test]
    fn normalize_two_parts() {
        let v = normalize_version("0.30").unwrap();
        assert_eq!(v, Version::parse("0.30.0").unwrap());
    }

    #[test]
    fn normalize_three_parts() {
        let v = normalize_version("1.2.3").unwrap();
        assert_eq!(v, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn normalize_strips_caret() {
        let v = normalize_version("^1.2").unwrap();
        assert_eq!(v, Version::parse("1.2.0").unwrap());
    }

    #[test]
    fn normalize_strips_tilde() {
        let v = normalize_version("~1.2.3").unwrap();
        assert_eq!(v, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn normalize_strips_equals() {
        let v = normalize_version("=1.2.3").unwrap();
        assert_eq!(v, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn normalize_complex_version_fails() {
        assert!(normalize_version(">=1.0, <2.0").is_err());
        assert!(normalize_version(">1.0").is_err());
        assert!(normalize_version("<2.0").is_err());
        assert!(normalize_version("*").is_err());
    }

    #[test]
    fn normalize_strips_whitespace() {
        let v = normalize_version("  1.2.3  ").unwrap();
        assert_eq!(v, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn dep_version_string_from_plain_string() {
        let val = toml::Value::String("0.5".to_string());
        assert_eq!(dep_version_string(&val), Some("0.5".to_string()));
    }

    #[test]
    fn dep_version_string_from_table_with_version() {
        let mut table = toml::map::Map::new();
        table.insert(
            "version".to_string(),
            toml::Value::String("1.2".to_string()),
        );
        let val = toml::Value::Table(table);
        assert_eq!(dep_version_string(&val), Some("1.2".to_string()));
    }

    #[test]
    fn dep_version_string_from_table_without_version() {
        let mut table = toml::map::Map::new();
        table.insert(
            "path".to_string(),
            toml::Value::String("../foo".to_string()),
        );
        let val = toml::Value::Table(table);
        assert_eq!(dep_version_string(&val), None);
    }

    #[test]
    fn dep_version_string_workspace_ref_returns_none() {
        let mut table = toml::map::Map::new();
        table.insert("workspace".to_string(), toml::Value::Boolean(true));
        let val = toml::Value::Table(table);
        assert_eq!(dep_version_string(&val), None);
    }

    #[test]
    fn extract_dep_versions_workspace_deps() {
        let doc: toml::Value = r#"
            [workspace.dependencies]
            tokio = "1.0"
            serde = { version = "1.0.100" }
        "#
        .parse()
        .unwrap();
        let deps = extract_dep_versions(&doc);
        assert_eq!(deps.get("tokio"), Some(&"1.0".to_string()));
        assert_eq!(deps.get("serde"), Some(&"1.0.100".to_string()));
    }

    #[test]
    fn extract_dep_versions_regular_deps() {
        let doc: toml::Value = r#"
            [dependencies]
            anyhow = "1.0"
            
            [dev-dependencies]
            insta = "1.30"
            
            [build-dependencies]
            cc = "1.0"
        "#
        .parse()
        .unwrap();
        let deps = extract_dep_versions(&doc);
        assert_eq!(deps.get("anyhow"), Some(&"1.0".to_string()));
        assert_eq!(deps.get("insta"), Some(&"1.30".to_string()));
        assert_eq!(deps.get("cc"), Some(&"1.0".to_string()));
    }

    #[test]
    fn extract_dep_versions_workspace_takes_priority() {
        let doc: toml::Value = r#"
            [workspace.dependencies]
            serde = "1.0.200"
            
            [dependencies]
            serde = { workspace = true }
        "#
        .parse()
        .unwrap();
        let deps = extract_dep_versions(&doc);
        assert_eq!(deps.get("serde"), Some(&"1.0.200".to_string()));
    }

    #[test]
    fn extract_dep_versions_empty_manifest() {
        let doc: toml::Value = r#"
            [package]
            name = "foo"
            version = "0.1.0"
        "#
        .parse()
        .unwrap();
        let deps = extract_dep_versions(&doc);
        assert!(deps.is_empty());
    }

    #[test]
    fn extract_package_version_simple() {
        let doc: toml::Value = r#"
            [package]
            name = "my-crate"
            version = "1.2.3"
        "#
        .parse()
        .unwrap();
        let (name, version) = extract_package_version(&doc, "HEAD", "Cargo.toml").unwrap();
        assert_eq!(name, "my-crate");
        assert_eq!(version, Version::parse("1.2.3").unwrap());
    }

    #[test]
    fn extract_package_version_missing_package_table() {
        let doc: toml::Value = r#"
            [workspace]
            members = ["crates/*"]
        "#
        .parse()
        .unwrap();
        assert!(extract_package_version(&doc, "HEAD", "Cargo.toml").is_err());
    }

    #[test]
    fn extract_package_version_missing_name() {
        let doc: toml::Value = r#"
            [package]
            version = "1.0.0"
        "#
        .parse()
        .unwrap();
        assert!(extract_package_version(&doc, "HEAD", "Cargo.toml").is_err());
    }

    #[test]
    fn extract_workspace_package_version_present() {
        let doc: toml::Value = r#"
            [workspace.package]
            version = "0.10.0"
        "#
        .parse()
        .unwrap();
        let ver = extract_workspace_package_version(&doc).unwrap();
        assert_eq!(ver, Version::parse("0.10.0").unwrap());
    }

    #[test]
    fn extract_workspace_package_version_absent() {
        let doc: toml::Value = r#"
            [workspace]
            members = ["foo"]
        "#
        .parse()
        .unwrap();
        assert!(extract_workspace_package_version(&doc).is_none());
    }

    #[test]
    fn extract_workspace_package_version_non_workspace_manifest() {
        let doc: toml::Value = r#"
            [package]
            name = "my-crate"
            version = "1.0.0"
        "#
        .parse()
        .unwrap();
        assert!(extract_workspace_package_version(&doc).is_none());
    }

    #[test]
    fn extract_workspace_members_returns_paths() {
        let doc: toml::Value = r#"
            [workspace]
            members = ["tokio", "axum"]
        "#
        .parse()
        .unwrap();
        let members = extract_workspace_members(&doc);
        assert_eq!(members, vec!["tokio", "axum"]);
    }

    #[test]
    fn extract_workspace_members_empty_when_missing() {
        let doc: toml::Value = r#"
            [package]
            name = "my-crate"
            version = "1.0.0"
        "#
        .parse()
        .unwrap();
        let members = extract_workspace_members(&doc);
        assert!(members.is_empty());
    }

    #[test]
    fn extract_workspace_members_empty_when_no_members_key() {
        let doc: toml::Value = r#"
            [workspace]
            resolver = "2"
        "#
        .parse()
        .unwrap();
        let members = extract_workspace_members(&doc);
        assert!(members.is_empty());
    }
}

use std::fmt;

use semver::Version;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    None,
    Patch,
    Minor,
    Major,
}

impl fmt::Display for Bump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Bump::None => f.write_str("None"),
            Bump::Patch => f.write_str("Patch"),
            Bump::Minor => f.write_str("Minor"),
            Bump::Major => f.write_str("Major"),
        }
    }
}

/// Semantic classification of a version change, independent of the version scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    None,
    Patch,
    Additive,
    Breaking,
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChangeKind::None => f.write_str("None"),
            ChangeKind::Patch => f.write_str("Patch"),
            ChangeKind::Additive => f.write_str("Additive"),
            ChangeKind::Breaking => f.write_str("Breaking"),
        }
    }
}

/// Classify the semantic change between two versions.
/// For `0.y.z`: minor change = Breaking, patch change = Patch.
/// For `>=1.0.0`: major change = Breaking, minor change = Additive, patch change = Patch.
pub fn classify_version_change(old: &Version, new: &Version) -> ChangeKind {
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
pub fn required_bump(version: &Version, change: ChangeKind) -> Bump {
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

/// Format a set of strings as `{"a", "b", "c"}` for user-facing output.
pub fn format_name_set<'a>(names: impl IntoIterator<Item = &'a String>) -> String {
    let mut sorted: Vec<&str> = names.into_iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    format!(
        "{{{}}}",
        sorted
            .iter()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

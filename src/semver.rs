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

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn classify_same_version_is_none() {
        assert_eq!(
            classify_version_change(&v("1.2.3"), &v("1.2.3")),
            ChangeKind::None
        );
        assert_eq!(
            classify_version_change(&v("0.1.0"), &v("0.1.0")),
            ChangeKind::None
        );
    }

    #[test]
    fn classify_zero_minor_bump_is_breaking() {
        assert_eq!(
            classify_version_change(&v("0.1.0"), &v("0.2.0")),
            ChangeKind::Breaking
        );
        assert_eq!(
            classify_version_change(&v("0.5.3"), &v("0.6.0")),
            ChangeKind::Breaking
        );
    }

    #[test]
    fn classify_zero_patch_bump_is_patch() {
        assert_eq!(
            classify_version_change(&v("0.1.0"), &v("0.1.1")),
            ChangeKind::Patch
        );
    }

    #[test]
    fn classify_stable_major_bump_is_breaking() {
        assert_eq!(
            classify_version_change(&v("1.0.0"), &v("2.0.0")),
            ChangeKind::Breaking
        );
        assert_eq!(
            classify_version_change(&v("3.1.4"), &v("4.0.0")),
            ChangeKind::Breaking
        );
    }

    #[test]
    fn classify_stable_minor_bump_is_additive() {
        assert_eq!(
            classify_version_change(&v("1.0.0"), &v("1.1.0")),
            ChangeKind::Additive
        );
        assert_eq!(
            classify_version_change(&v("2.3.0"), &v("2.4.0")),
            ChangeKind::Additive
        );
    }

    #[test]
    fn classify_stable_patch_bump_is_patch() {
        assert_eq!(
            classify_version_change(&v("1.0.0"), &v("1.0.1")),
            ChangeKind::Patch
        );
    }

    #[test]
    fn required_bump_breaking_on_zero_is_minor() {
        assert_eq!(
            required_bump(&v("0.3.0"), ChangeKind::Breaking),
            Bump::Minor
        );
    }

    #[test]
    fn required_bump_breaking_on_stable_is_major() {
        assert_eq!(
            required_bump(&v("1.0.0"), ChangeKind::Breaking),
            Bump::Major
        );
        assert_eq!(
            required_bump(&v("2.5.1"), ChangeKind::Breaking),
            Bump::Major
        );
    }

    #[test]
    fn required_bump_additive_on_zero_is_patch() {
        assert_eq!(
            required_bump(&v("0.1.0"), ChangeKind::Additive),
            Bump::Patch
        );
    }

    #[test]
    fn required_bump_additive_on_stable_is_minor() {
        assert_eq!(
            required_bump(&v("1.0.0"), ChangeKind::Additive),
            Bump::Minor
        );
    }

    #[test]
    fn required_bump_patch_is_always_patch() {
        assert_eq!(required_bump(&v("0.1.0"), ChangeKind::Patch), Bump::Patch);
        assert_eq!(required_bump(&v("1.0.0"), ChangeKind::Patch), Bump::Patch);
    }

    #[test]
    fn required_bump_none_is_always_none() {
        assert_eq!(required_bump(&v("0.1.0"), ChangeKind::None), Bump::None);
        assert_eq!(required_bump(&v("1.0.0"), ChangeKind::None), Bump::None);
    }

    #[test]
    fn format_name_set_empty() {
        let empty: Vec<String> = vec![];
        assert_eq!(format_name_set(&empty), "{}");
    }

    #[test]
    fn format_name_set_single() {
        let names = vec!["foo".to_string()];
        assert_eq!(format_name_set(&names), "{\"foo\"}");
    }

    #[test]
    fn format_name_set_multiple_sorted() {
        let names = vec![
            "charlie".to_string(),
            "alpha".to_string(),
            "bravo".to_string(),
        ];
        assert_eq!(
            format_name_set(&names),
            "{\"alpha\", \"bravo\", \"charlie\"}"
        );
    }

    #[test]
    fn bump_display() {
        assert_eq!(Bump::None.to_string(), "None");
        assert_eq!(Bump::Patch.to_string(), "Patch");
        assert_eq!(Bump::Minor.to_string(), "Minor");
        assert_eq!(Bump::Major.to_string(), "Major");
    }

    #[test]
    fn change_kind_display() {
        assert_eq!(ChangeKind::None.to_string(), "None");
        assert_eq!(ChangeKind::Patch.to_string(), "Patch");
        assert_eq!(ChangeKind::Additive.to_string(), "Additive");
        assert_eq!(ChangeKind::Breaking.to_string(), "Breaking");
    }

    #[test]
    fn bump_ordering() {
        assert!(Bump::None < Bump::Patch);
        assert!(Bump::Patch < Bump::Minor);
        assert!(Bump::Minor < Bump::Major);
    }

    #[test]
    fn change_kind_ordering() {
        assert!(ChangeKind::None < ChangeKind::Patch);
        assert!(ChangeKind::Patch < ChangeKind::Additive);
        assert!(ChangeKind::Additive < ChangeKind::Breaking);
    }
}

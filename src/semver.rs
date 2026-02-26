use semver::Version;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    None,
    Patch,
    Minor,
    Major,
}

/// Semantic classification of a version change, independent of the version scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    None,
    Patch,
    Additive,
    Breaking,
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

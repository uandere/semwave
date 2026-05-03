use std::collections::HashSet;

use semver::Version;

use crate::semver::{Bump, ChangeKind};

/// Where a local package version bump came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BumpSource {
    Package,
    Workspace,
}

/// One leaked-type detail line in verbose mode.
#[derive(Debug, Clone)]
pub struct LeakDetail {
    pub item_kind: String,
    pub item_name: String,
    pub leaked_types: Vec<String>,
}

/// Role of a node in the influence tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeNodeKind {
    Seed,
    Child { bump: Bump, already_shown: bool },
}

/// One node in the influence tree, in depth-first order.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub depth: usize,
    pub is_last_sibling: bool,
    pub name: String,
    pub kind: TreeNodeKind,
}

/// Structured user-facing event.
#[derive(Debug)]
pub enum Event<'a> {
    DirectModeBanner {
        seeds: &'a HashSet<String>,
    },
    ComparingRefs {
        source: &'a str,
        target: &'a str,
    },
    NoChangesDetected,
    DetectedChangesHeader,
    DepVersionChanged {
        name: &'a str,
        old: &'a str,
        new: &'a str,
        kind: ChangeKind,
    },
    LocalPackageBump {
        name: &'a str,
        old: &'a Version,
        new: &'a Version,
        kind: ChangeKind,
        source: BumpSource,
    },
    NewCrate {
        name: &'a str,
    },
    RemovedCrate {
        name: &'a str,
    },
    GlobMemberSkipped {
        member: &'a str,
    },
    BreakingSeeds {
        names: &'a HashSet<String>,
    },
    AdditiveSeeds {
        names: &'a HashSet<String>,
    },
    AnalyzingCrate {
        name: &'a str,
        deps: &'a [&'a str],
    },
    BinaryCrateSkipped {
        name: &'a str,
    },
    LeakDetected {
        crate_name: &'a str,
        dep: &'a str,
        bump: Bump,
        details: &'a [LeakDetail],
    },
    RustdocFailed {
        crate_name: &'a str,
        error: &'a str,
        conservative_bump: Bump,
    },
    InfluenceTreeHeader,
    InfluenceTree {
        nodes: &'a [TreeNode],
    },
    AnalysisCompleteHeader,
    BumpList {
        level: Bump,
        names: &'a HashSet<String>,
    },
    FailedRustdocSummary {
        names: &'a HashSet<String>,
    },
    UnderBumped {
        items: &'a [(&'a str, Bump, Bump)],
    },
    MissingBumps {
        items: &'a [(&'a str, Bump)],
    },
}

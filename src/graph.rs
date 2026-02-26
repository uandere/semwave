use std::collections::{HashMap, HashSet};

use cargo_metadata::{DependencyKind, NodeDep};

pub fn find_cycle<'a>(adj: &HashMap<&'a str, Vec<&'a str>>) -> Option<Vec<&'a str>> {
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
pub fn is_normal_dep(dep: &NodeDep) -> bool {
    dep.dep_kinds
        .iter()
        .any(|dk| dk.kind == DependencyKind::Normal)
}

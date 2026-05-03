use std::collections::{HashMap, HashSet};

use crate::report::{Event, EventSink, TreeNode, TreeNodeKind};
use crate::semver::Bump;

/// Build the influence tree as a flat node list and emit it.
pub fn print_influence_tree(
    seeds: &HashSet<String>,
    tree_edges: &HashMap<String, Vec<(String, Bump)>>,
    sink: &dyn EventSink,
) {
    let mut sorted_seeds: Vec<&String> = seeds.iter().collect();
    sorted_seeds.sort();

    let mut nodes: Vec<TreeNode> = Vec::new();

    let last_idx = sorted_seeds.len().saturating_sub(1);
    for (i, seed) in sorted_seeds.iter().enumerate() {
        let is_last_root = i == last_idx;
        nodes.push(TreeNode {
            depth: 0,
            is_last_sibling: is_last_root,
            name: (*seed).clone(),
            kind: TreeNodeKind::Seed,
        });
        let mut visited: HashSet<String> = HashSet::new();
        push_children(seed, tree_edges, 1, &mut nodes, &mut visited);
    }

    sink.emit(&Event::InfluenceTree { nodes: &nodes });
}

fn push_children(
    parent: &str,
    tree_edges: &HashMap<String, Vec<(String, Bump)>>,
    depth: usize,
    nodes: &mut Vec<TreeNode>,
    visited: &mut HashSet<String>,
) {
    let Some(children) = tree_edges.get(parent) else {
        return;
    };

    let mut sorted: Vec<&(String, Bump)> = children.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let last_idx = sorted.len().saturating_sub(1);
    for (i, (child, bump)) in sorted.iter().enumerate() {
        let is_last = i == last_idx;
        let already_shown = visited.contains(child);
        nodes.push(TreeNode {
            depth,
            is_last_sibling: is_last,
            name: child.clone(),
            kind: TreeNodeKind::Child {
                bump: *bump,
                already_shown,
            },
        });
        if already_shown {
            continue;
        }
        visited.insert(child.clone());
        push_children(child, tree_edges, depth + 1, nodes, visited);
    }
}

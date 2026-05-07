use std::collections::{HashMap, HashSet};

use crate::report::{Event, EventSink, TreeNode, TreeNodeKind};
use crate::types::{CrateName, TreeEdge};

pub fn print_influence_tree(
    seeds: &HashSet<CrateName>,
    tree_edges: &HashMap<CrateName, Vec<TreeEdge>>,
    sink: &dyn EventSink,
) {
    let mut sorted_seeds: Vec<&CrateName> = seeds.iter().collect();
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
        let mut visited: HashSet<CrateName> = HashSet::new();
        push_children(seed.as_str(), tree_edges, 1, &mut nodes, &mut visited);
    }

    sink.emit(&Event::InfluenceTree { nodes: &nodes });
}

fn push_children(
    parent: &str,
    tree_edges: &HashMap<CrateName, Vec<TreeEdge>>,
    depth: usize,
    nodes: &mut Vec<TreeNode>,
    visited: &mut HashSet<CrateName>,
) {
    let Some(children) = tree_edges.get(parent) else {
        return;
    };

    let mut sorted: Vec<&TreeEdge> = children.iter().collect();
    sorted.sort_by(|a, b| a.child.cmp(&b.child));

    let last_idx = sorted.len().saturating_sub(1);
    for (i, edge) in sorted.iter().enumerate() {
        let is_last = i == last_idx;
        let already_shown = visited.contains(&edge.child);
        nodes.push(TreeNode {
            depth,
            is_last_sibling: is_last,
            name: edge.child.clone(),
            kind: TreeNodeKind::Child {
                bump: edge.bump,
                already_shown,
            },
        });
        if already_shown {
            continue;
        }
        visited.insert(edge.child.clone());
        push_children(edge.child.as_str(), tree_edges, depth + 1, nodes, visited);
    }
}

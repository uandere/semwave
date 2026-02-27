use std::collections::{HashMap, HashSet};

use colored::Colorize as _;

use crate::semver::Bump;

pub fn print_influence_tree(
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
            Bump::Major => (
                connector.red().bold().to_string(),
                "MAJOR".red().bold().to_string(),
            ),
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
            "{}{}{}  ({})",
            prefix.dimmed(),
            colored_connector,
            child.cyan().bold(),
            bump_label
        );

        let next_prefix = format!("{}{}", prefix, child_prefix);
        print_tree_children(child, tree_edges, &next_prefix, visited);
    }
}

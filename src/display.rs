use std::collections::{HashMap, HashSet};

use colored::Colorize as _;
use rustdoc_types::{GenericBound, ItemEnum, Type};

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

pub fn print_tree_children(
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
            "{}{}{}  {}",
            prefix.dimmed(),
            colored_connector,
            child.cyan().bold(),
            format!("({})", bump_label)
        );

        let next_prefix = format!("{}{}", prefix, child_prefix);
        print_tree_children(child, tree_edges, &next_prefix, visited);
    }
}

pub fn item_kind_label(item: &rustdoc_types::Item) -> &'static str {
    match &item.inner {
        ItemEnum::Use(_) => "re-export",
        ItemEnum::Function(_) => "fn",
        ItemEnum::Struct(_) => "struct",
        ItemEnum::StructField(_) => "field",
        ItemEnum::Enum(_) => "enum",
        ItemEnum::Variant(_) => "variant",
        ItemEnum::Union(_) => "union",
        ItemEnum::TypeAlias(_) => "type",
        ItemEnum::Trait(_) => "trait",
        ItemEnum::TraitAlias(_) => "trait alias",
        ItemEnum::Impl(_) => "impl",
        ItemEnum::Constant { .. } => "const",
        ItemEnum::Static(_) => "static",
        ItemEnum::AssocConst { .. } => "assoc const",
        ItemEnum::AssocType { .. } => "assoc type",
        ItemEnum::Macro(_) | ItemEnum::ProcMacro(_) => "macro",
        _ => "item",
    }
}

pub fn type_display_name(ty: &Type) -> String {
    match ty {
        Type::ResolvedPath(p) => p.path.clone(),
        Type::BorrowedRef { type_, .. } => format!("&{}", type_display_name(type_)),
        Type::RawPointer { type_, .. } => format!("*{}", type_display_name(type_)),
        Type::Slice(inner) => format!("[{}]", type_display_name(inner)),
        Type::Array { type_, .. } => format!("[{}; _]", type_display_name(type_)),
        Type::Tuple(types) => {
            let inner: Vec<_> = types.iter().map(type_display_name).collect();
            format!("({})", inner.join(", "))
        }
        Type::Generic(name) => name.clone(),
        Type::Primitive(name) => name.clone(),
        Type::QualifiedPath {
            name, self_type, ..
        } => {
            format!("<{}>::{}", type_display_name(self_type), name)
        }
        Type::DynTrait(dt) => dt
            .traits
            .first()
            .map(|p| format!("dyn {}", p.trait_.path))
            .unwrap_or_else(|| "dyn ...".to_string()),
        Type::ImplTrait(bounds) => {
            let names: Vec<_> = bounds
                .iter()
                .filter_map(|b| {
                    if let GenericBound::TraitBound { trait_, .. } = b {
                        Some(trait_.path.clone())
                    } else {
                        None
                    }
                })
                .collect();
            format!("impl {}", names.join(" + "))
        }
        _ => "_".to_string(),
    }
}

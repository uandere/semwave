use std::collections::{BTreeSet, HashMap, HashSet};

use rustdoc_types::{
    AssocItemConstraintKind, GenericArg, GenericArgs, GenericBound, ItemEnum, StructKind, Term,
    Type, VariantKind, WherePredicate,
};

/// One public API item that leaks an external dependency.
pub struct LeakDetail {
    pub item_name: String,
    pub item_kind: &'static str,
    pub leaked_types: BTreeSet<String>,
}

pub fn find_leaked_deps(
    krate: &rustdoc_types::Crate,
    dep_crate_ids: &HashMap<u32, String>,
) -> HashMap<String, Vec<LeakDetail>> {
    let mut child_parents: HashMap<&rustdoc_types::Id, String> = HashMap::new();
    for (id, item) in &krate.index {
        let parent_path = krate
            .paths
            .get(id)
            .map(|s| s.path.join("::"))
            .or_else(|| item.name.clone())
            .or_else(|| {
                if let ItemEnum::Impl(imp) = &item.inner {
                    let self_ty = type_display_name(&imp.for_);
                    Some(match &imp.trait_ {
                        Some(t) => format!("<{} as {}>", self_ty, t.path),
                        None => self_ty,
                    })
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let child_ids: Vec<&rustdoc_types::Id> = match &item.inner {
            ItemEnum::Struct(s) => match &s.kind {
                StructKind::Plain { fields, .. } => fields.iter().collect(),
                StructKind::Tuple(fields) => fields.iter().filter_map(|f| f.as_ref()).collect(),
                StructKind::Unit => vec![],
            },
            ItemEnum::Variant(v) => match &v.kind {
                VariantKind::Plain => vec![],
                VariantKind::Tuple(fields) => fields.iter().filter_map(|f| f.as_ref()).collect(),
                VariantKind::Struct { fields, .. } => fields.iter().collect(),
            },
            ItemEnum::Impl(imp) => imp.items.iter().collect(),
            ItemEnum::Trait(t) => t.items.iter().collect(),
            _ => vec![],
        };

        for cid in child_ids {
            child_parents.insert(cid, parent_path.clone());
        }
    }

    let mut result: HashMap<String, Vec<LeakDetail>> = HashMap::new();

    for (id, item) in &krate.index {
        let refs = collect_crate_ids_from_item(item, krate);
        if refs.is_empty() {
            continue;
        }

        let item_name = krate
            .paths
            .get(id)
            .map(|s| s.path.join("::"))
            .or_else(|| {
                let child_name = item.name.as_deref().unwrap_or("?");
                child_parents
                    .get(id)
                    .map(|parent| format!("{}::{}", parent, child_name))
            })
            .or_else(|| match &item.inner {
                ItemEnum::Use(use_) => use_
                    .id
                    .as_ref()
                    .and_then(|uid| krate.paths.get(uid))
                    .map(|s| s.path.join("::")),
                ItemEnum::Impl(imp) => {
                    let self_ty = type_display_name(&imp.for_);
                    Some(match &imp.trait_ {
                        Some(t) => format!("{} for {}", t.path, self_ty),
                        None => self_ty,
                    })
                }
                _ => item.name.clone(),
            })
            .unwrap_or_else(|| "<unnamed>".to_string());
        let item_kind = item_kind_label(item);

        let mut per_dep: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (crate_id, type_path) in refs {
            if let Some(dep_name) = dep_crate_ids.get(&crate_id) {
                per_dep
                    .entry(dep_name.clone())
                    .or_default()
                    .insert(type_path);
            }
        }

        for (dep_name, leaked_types) in per_dep {
            result.entry(dep_name).or_default().push(LeakDetail {
                item_name: item_name.clone(),
                item_kind,
                leaked_types,
            });
        }
    }

    for details in result.values_mut() {
        details.sort_by(|a, b| a.item_name.cmp(&b.item_name));
    }

    result
}

/// Collect all external crate IDs (with type paths) referenced by a single item.
pub fn collect_crate_ids_from_item(
    item: &rustdoc_types::Item,
    krate: &rustdoc_types::Crate,
) -> HashSet<(u32, String)> {
    let mut ids = HashSet::new();

    match &item.inner {
        ItemEnum::Use(use_) => {
            if let Some(ref target_id) = use_.id
                && let Some(summary) = krate.paths.get(target_id)
            {
                ids.insert((summary.crate_id, summary.path.join("::")));
            }
        }
        ItemEnum::Function(f) => {
            ids.extend(collect_crate_ids_from_fn_sig(&f.sig, &krate.paths));
            ids.extend(collect_crate_ids_from_generics(&f.generics, &krate.paths));
        }
        ItemEnum::Struct(s) => {
            ids.extend(collect_crate_ids_from_generics(&s.generics, &krate.paths));
        }
        ItemEnum::StructField(ty) => {
            ids.extend(collect_crate_ids_from_type(ty, &krate.paths));
        }
        ItemEnum::Enum(e) => {
            ids.extend(collect_crate_ids_from_generics(&e.generics, &krate.paths));
        }
        ItemEnum::Variant(_) => {}
        ItemEnum::Union(u) => {
            ids.extend(collect_crate_ids_from_generics(&u.generics, &krate.paths));
        }
        ItemEnum::TypeAlias(ta) => {
            ids.extend(collect_crate_ids_from_type(&ta.type_, &krate.paths));
            ids.extend(collect_crate_ids_from_generics(&ta.generics, &krate.paths));
        }
        ItemEnum::Trait(t) => {
            ids.extend(collect_crate_ids_from_generics(&t.generics, &krate.paths));
            for bound in &t.bounds {
                ids.extend(collect_crate_ids_from_bound(bound, &krate.paths));
            }
        }
        ItemEnum::TraitAlias(ta) => {
            ids.extend(collect_crate_ids_from_generics(&ta.generics, &krate.paths));
            for bound in &ta.params {
                ids.extend(collect_crate_ids_from_bound(bound, &krate.paths));
            }
        }
        ItemEnum::Impl(imp) => {
            ids.extend(collect_crate_ids_from_generics(&imp.generics, &krate.paths));
            ids.extend(collect_crate_ids_from_type(&imp.for_, &krate.paths));
            if let Some(ref trait_path) = imp.trait_ {
                ids.extend(collect_crate_ids_from_path(trait_path, &krate.paths));
            }
        }
        ItemEnum::Constant { type_, .. } => {
            ids.extend(collect_crate_ids_from_type(type_, &krate.paths));
        }
        ItemEnum::Static(s) => {
            ids.extend(collect_crate_ids_from_type(&s.type_, &krate.paths));
        }
        ItemEnum::AssocConst { type_, .. } => {
            ids.extend(collect_crate_ids_from_type(type_, &krate.paths));
        }
        ItemEnum::AssocType {
            generics,
            bounds,
            type_,
        } => {
            ids.extend(collect_crate_ids_from_generics(generics, &krate.paths));
            for bound in bounds {
                ids.extend(collect_crate_ids_from_bound(bound, &krate.paths));
            }
            if let Some(ty) = type_ {
                ids.extend(collect_crate_ids_from_type(ty, &krate.paths));
            }
        }
        ItemEnum::Module(_)
        | ItemEnum::ExternCrate { .. }
        | ItemEnum::ExternType
        | ItemEnum::Macro(_)
        | ItemEnum::ProcMacro(_)
        | ItemEnum::Primitive(_) => {}
    }

    ids.retain(|(id, _)| *id != 0);
    ids
}

pub fn collect_crate_ids_from_fn_sig(
    sig: &rustdoc_types::FunctionSignature,
    paths: &HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>,
) -> HashSet<(u32, String)> {
    let mut ids = HashSet::new();
    for (_, ty) in &sig.inputs {
        ids.extend(collect_crate_ids_from_type(ty, paths));
    }
    if let Some(ref out) = sig.output {
        ids.extend(collect_crate_ids_from_type(out, paths));
    }
    ids
}

pub fn collect_crate_ids_from_type(
    ty: &Type,
    paths: &HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>,
) -> HashSet<(u32, String)> {
    let mut ids = HashSet::new();
    match ty {
        Type::ResolvedPath(path) => {
            ids.extend(collect_crate_ids_from_path(path, paths));
        }
        Type::DynTrait(dyn_trait) => {
            for poly in &dyn_trait.traits {
                ids.extend(collect_crate_ids_from_path(&poly.trait_, paths));
            }
        }
        Type::FunctionPointer(fp) => {
            ids.extend(collect_crate_ids_from_fn_sig(&fp.sig, paths));
        }
        Type::Tuple(types) => {
            for t in types {
                ids.extend(collect_crate_ids_from_type(t, paths));
            }
        }
        Type::Slice(inner) => {
            ids.extend(collect_crate_ids_from_type(inner, paths));
        }
        Type::Array { type_, .. } => {
            ids.extend(collect_crate_ids_from_type(type_, paths));
        }
        Type::Pat { type_, .. } => {
            ids.extend(collect_crate_ids_from_type(type_, paths));
        }
        Type::ImplTrait(bounds) => {
            for bound in bounds {
                ids.extend(collect_crate_ids_from_bound(bound, paths));
            }
        }
        Type::RawPointer { type_, .. } => {
            ids.extend(collect_crate_ids_from_type(type_, paths));
        }
        Type::BorrowedRef { type_, .. } => {
            ids.extend(collect_crate_ids_from_type(type_, paths));
        }
        Type::QualifiedPath {
            self_type,
            trait_,
            args,
            ..
        } => {
            ids.extend(collect_crate_ids_from_type(self_type, paths));
            if let Some(trait_path) = trait_ {
                ids.extend(collect_crate_ids_from_path(trait_path, paths));
            }
            if let Some(ga) = args {
                ids.extend(collect_crate_ids_from_generic_args(ga, paths));
            }
        }
        Type::Generic(_) | Type::Primitive(_) | Type::Infer => {}
    }
    ids
}

pub fn collect_crate_ids_from_path(
    path: &rustdoc_types::Path,
    paths: &HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>,
) -> HashSet<(u32, String)> {
    let mut ids = HashSet::new();
    if let Some(summary) = paths.get(&path.id) {
        ids.insert((summary.crate_id, summary.path.join("::")));
    }
    if let Some(ref args) = path.args {
        ids.extend(collect_crate_ids_from_generic_args(args, paths));
    }
    ids
}

pub fn collect_crate_ids_from_generic_args(
    args: &GenericArgs,
    paths: &HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>,
) -> HashSet<(u32, String)> {
    let mut ids = HashSet::new();
    match args {
        GenericArgs::AngleBracketed { args, constraints } => {
            for arg in args {
                match arg {
                    GenericArg::Type(ty) => {
                        ids.extend(collect_crate_ids_from_type(ty, paths));
                    }
                    GenericArg::Lifetime(_) | GenericArg::Const(_) | GenericArg::Infer => {}
                }
            }
            for constraint in constraints {
                match &constraint.binding {
                    AssocItemConstraintKind::Equality(term) => {
                        if let Term::Type(ty) = term {
                            ids.extend(collect_crate_ids_from_type(ty, paths));
                        }
                    }
                    AssocItemConstraintKind::Constraint(bounds) => {
                        for bound in bounds {
                            ids.extend(collect_crate_ids_from_bound(bound, paths));
                        }
                    }
                }
            }
        }
        GenericArgs::Parenthesized { inputs, output } => {
            for ty in inputs {
                ids.extend(collect_crate_ids_from_type(ty, paths));
            }
            if let Some(ty) = output {
                ids.extend(collect_crate_ids_from_type(ty, paths));
            }
        }
        GenericArgs::ReturnTypeNotation => {}
    }
    ids
}

pub fn collect_crate_ids_from_bound(
    bound: &GenericBound,
    paths: &HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>,
) -> HashSet<(u32, String)> {
    let mut ids = HashSet::new();
    if let GenericBound::TraitBound { trait_, .. } = bound {
        ids.extend(collect_crate_ids_from_path(trait_, paths));
    }
    ids
}

pub fn collect_crate_ids_from_generics(
    generics: &rustdoc_types::Generics,
    paths: &HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>,
) -> HashSet<(u32, String)> {
    let mut ids = HashSet::new();
    for param in &generics.params {
        if let rustdoc_types::GenericParamDefKind::Type {
            bounds, default, ..
        } = &param.kind
        {
            for bound in bounds {
                ids.extend(collect_crate_ids_from_bound(bound, paths));
            }
            if let Some(ty) = default {
                ids.extend(collect_crate_ids_from_type(ty, paths));
            }
        }
    }
    for pred in &generics.where_predicates {
        match pred {
            WherePredicate::BoundPredicate { type_, bounds, .. } => {
                ids.extend(collect_crate_ids_from_type(type_, paths));
                for bound in bounds {
                    ids.extend(collect_crate_ids_from_bound(bound, paths));
                }
            }
            WherePredicate::EqPredicate { lhs, rhs } => {
                ids.extend(collect_crate_ids_from_type(lhs, paths));
                if let Term::Type(ty) = rhs {
                    ids.extend(collect_crate_ids_from_type(ty, paths));
                }
            }
            WherePredicate::LifetimePredicate { .. } => {}
        }
    }
    ids
}

/// Walk the entire rustdoc JSON index and find which target deps are leaked
/// in the public API. Returns a map from dep name to the list of item names
/// that expose it.
fn item_kind_label(item: &rustdoc_types::Item) -> &'static str {
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

use std::collections::{BTreeSet, HashMap, HashSet};

use rustdoc_types::{
    AssocItemConstraintKind, GenericArg, GenericArgs, GenericBound, ItemEnum, StructKind, Term,
    Type, VariantKind, WherePredicate,
};

use crate::display::{item_kind_label, type_display_name};

type PathsMap = HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>;
type CrateIdSet = HashSet<(u32, String)>;

/// One public API item that leaks an external dependency.
pub struct LeakDetail {
    pub item_name: String,
    pub item_kind: &'static str,
    pub leaked_types: BTreeSet<String>,
}

/// Recursively collect external crate IDs (with their fully qualified type
/// paths) that are referenced by a rustdoc JSON node.
trait CollectCrateIds {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet);
}

impl CollectCrateIds for Type {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet) {
        match self {
            Type::ResolvedPath(path) => path.collect_crate_ids(paths, out),
            Type::DynTrait(dyn_trait) => {
                for poly in &dyn_trait.traits {
                    poly.trait_.collect_crate_ids(paths, out);
                }
            }
            Type::FunctionPointer(fp) => fp.sig.collect_crate_ids(paths, out),
            Type::Tuple(types) => {
                for t in types {
                    t.collect_crate_ids(paths, out);
                }
            }
            Type::Slice(inner)
            | Type::Array { type_: inner, .. }
            | Type::Pat { type_: inner, .. }
            | Type::RawPointer { type_: inner, .. }
            | Type::BorrowedRef { type_: inner, .. } => {
                inner.collect_crate_ids(paths, out);
            }
            Type::ImplTrait(bounds) => {
                for bound in bounds {
                    bound.collect_crate_ids(paths, out);
                }
            }
            Type::QualifiedPath {
                self_type,
                trait_,
                args,
                ..
            } => {
                self_type.collect_crate_ids(paths, out);
                if let Some(trait_path) = trait_ {
                    trait_path.collect_crate_ids(paths, out);
                }
                if let Some(ga) = args {
                    ga.collect_crate_ids(paths, out);
                }
            }
            Type::Generic(_) | Type::Primitive(_) | Type::Infer => {}
        }
    }
}

impl CollectCrateIds for rustdoc_types::Path {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet) {
        if let Some(summary) = paths.get(&self.id) {
            out.insert((summary.crate_id, summary.path.join("::")));
        }
        if let Some(ref args) = self.args {
            args.collect_crate_ids(paths, out);
        }
    }
}

impl CollectCrateIds for GenericArgs {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet) {
        match self {
            GenericArgs::AngleBracketed { args, constraints } => {
                for arg in args {
                    if let GenericArg::Type(ty) = arg {
                        ty.collect_crate_ids(paths, out);
                    }
                }
                for constraint in constraints {
                    match &constraint.binding {
                        AssocItemConstraintKind::Equality(term) => {
                            if let Term::Type(ty) = term {
                                ty.collect_crate_ids(paths, out);
                            }
                        }
                        AssocItemConstraintKind::Constraint(bounds) => {
                            for bound in bounds {
                                bound.collect_crate_ids(paths, out);
                            }
                        }
                    }
                }
            }
            GenericArgs::Parenthesized { inputs, output } => {
                for ty in inputs {
                    ty.collect_crate_ids(paths, out);
                }
                if let Some(ty) = output {
                    ty.collect_crate_ids(paths, out);
                }
            }
            GenericArgs::ReturnTypeNotation => {}
        }
    }
}

impl CollectCrateIds for GenericBound {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet) {
        if let GenericBound::TraitBound { trait_, .. } = self {
            trait_.collect_crate_ids(paths, out);
        }
    }
}

impl CollectCrateIds for rustdoc_types::Generics {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet) {
        for param in &self.params {
            if let rustdoc_types::GenericParamDefKind::Type {
                bounds, default, ..
            } = &param.kind
            {
                for bound in bounds {
                    bound.collect_crate_ids(paths, out);
                }
                if let Some(ty) = default {
                    ty.collect_crate_ids(paths, out);
                }
            }
        }
        for pred in &self.where_predicates {
            match pred {
                WherePredicate::BoundPredicate { type_, bounds, .. } => {
                    type_.collect_crate_ids(paths, out);
                    for bound in bounds {
                        bound.collect_crate_ids(paths, out);
                    }
                }
                WherePredicate::EqPredicate { lhs, rhs } => {
                    lhs.collect_crate_ids(paths, out);
                    if let Term::Type(ty) = rhs {
                        ty.collect_crate_ids(paths, out);
                    }
                }
                WherePredicate::LifetimePredicate { .. } => {}
            }
        }
    }
}

impl CollectCrateIds for rustdoc_types::FunctionSignature {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet) {
        for (_, ty) in &self.inputs {
            ty.collect_crate_ids(paths, out);
        }
        if let Some(ref ret) = self.output {
            ret.collect_crate_ids(paths, out);
        }
    }
}

impl CollectCrateIds for rustdoc_types::Item {
    fn collect_crate_ids(&self, paths: &PathsMap, out: &mut CrateIdSet) {
        match &self.inner {
            ItemEnum::Use(use_) => {
                if let Some(ref target_id) = use_.id
                    && let Some(summary) = paths.get(target_id)
                {
                    out.insert((summary.crate_id, summary.path.join("::")));
                }
            }
            ItemEnum::Function(f) => {
                f.sig.collect_crate_ids(paths, out);
                f.generics.collect_crate_ids(paths, out);
            }
            ItemEnum::Struct(s) => {
                s.generics.collect_crate_ids(paths, out);
            }
            ItemEnum::StructField(ty) => {
                ty.collect_crate_ids(paths, out);
            }
            ItemEnum::Enum(e) => {
                e.generics.collect_crate_ids(paths, out);
            }
            ItemEnum::Variant(_) => {}
            ItemEnum::Union(u) => {
                u.generics.collect_crate_ids(paths, out);
            }
            ItemEnum::TypeAlias(ta) => {
                ta.type_.collect_crate_ids(paths, out);
                ta.generics.collect_crate_ids(paths, out);
            }
            ItemEnum::Trait(t) => {
                t.generics.collect_crate_ids(paths, out);
                for bound in &t.bounds {
                    bound.collect_crate_ids(paths, out);
                }
            }
            ItemEnum::TraitAlias(ta) => {
                ta.generics.collect_crate_ids(paths, out);
                for bound in &ta.params {
                    bound.collect_crate_ids(paths, out);
                }
            }
            ItemEnum::Impl(imp) => {
                imp.generics.collect_crate_ids(paths, out);
                imp.for_.collect_crate_ids(paths, out);
                if let Some(ref trait_path) = imp.trait_ {
                    trait_path.collect_crate_ids(paths, out);
                }
            }
            ItemEnum::Constant { type_, .. } => {
                type_.collect_crate_ids(paths, out);
            }
            ItemEnum::Static(s) => {
                s.type_.collect_crate_ids(paths, out);
            }
            ItemEnum::AssocConst { type_, .. } => {
                type_.collect_crate_ids(paths, out);
            }
            ItemEnum::AssocType {
                generics,
                bounds,
                type_,
            } => {
                generics.collect_crate_ids(paths, out);
                for bound in bounds {
                    bound.collect_crate_ids(paths, out);
                }
                if let Some(ty) = type_ {
                    ty.collect_crate_ids(paths, out);
                }
            }
            ItemEnum::Module(_)
            | ItemEnum::ExternCrate { .. }
            | ItemEnum::ExternType
            | ItemEnum::Macro(_)
            | ItemEnum::ProcMacro(_)
            | ItemEnum::Primitive(_) => {}
        }

        out.retain(|(id, _)| *id != 0);
    }
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
        let mut refs = CrateIdSet::new();
        item.collect_crate_ids(&krate.paths, &mut refs);
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

use std::collections::{BTreeSet, HashMap, HashSet};

use rustdoc_types::{
    AssocItemConstraintKind, GenericArg, GenericArgs, GenericBound, ItemEnum, StructKind, Term,
    Type, VariantKind, WherePredicate,
};

type PathsMap = HashMap<rustdoc_types::Id, rustdoc_types::ItemSummary>;
type CrateIdSet = HashSet<(u32, String)>;

/// One public API item that leaks an external dependency.
pub struct LeakDetail {
    pub item_name: String,
    pub item_kind: &'static str,
    pub leaked_types: BTreeSet<String>,
}

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

fn type_display_name(ty: &Type) -> String {
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

        let mut per_dep: HashMap<&str, BTreeSet<String>> = HashMap::new();
        for (crate_id, type_path) in &refs {
            if let Some(dep_name) = dep_crate_ids.get(crate_id) {
                per_dep
                    .entry(dep_name)
                    .or_default()
                    .insert(type_path.clone());
            }
        }

        for (dep_name, leaked_types) in per_dep {
            result
                .entry(dep_name.to_owned())
                .or_default()
                .push(LeakDetail {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rustdoc_types::*;
    use std::path::PathBuf;

    static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

    fn next_id() -> Id {
        Id(NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    fn id(n: u32) -> Id {
        Id(n)
    }

    fn empty_generics() -> Generics {
        Generics {
            params: vec![],
            where_predicates: vec![],
        }
    }

    fn fn_header() -> FunctionHeader {
        FunctionHeader {
            is_const: false,
            is_unsafe: false,
            is_async: false,
            abi: Abi::Rust,
        }
    }

    fn make_item(item_id: Id, name: Option<&str>, inner: ItemEnum) -> Item {
        Item {
            id: item_id,
            crate_id: 0,
            name: name.map(|s| s.to_string()),
            span: None,
            visibility: Visibility::Public,
            docs: None,
            links: HashMap::new(),
            attrs: vec![],
            deprecation: None,
            inner,
        }
    }

    fn make_path(path_str: &str, target_id: Id) -> rustdoc_types::Path {
        rustdoc_types::Path {
            path: path_str.to_string(),
            id: target_id,
            args: None,
        }
    }

    fn resolved_path_type(path_str: &str, target_id: Id) -> Type {
        Type::ResolvedPath(make_path(path_str, target_id))
    }

    fn make_crate(
        index: HashMap<Id, Item>,
        paths: HashMap<Id, ItemSummary>,
        external_crates: HashMap<u32, ExternalCrate>,
    ) -> Crate {
        Crate {
            root: id(0),
            crate_version: Some("0.1.0".to_string()),
            includes_private: false,
            index,
            paths,
            external_crates,
            target: Target {
                triple: "x86_64-unknown-linux-gnu".to_string(),
                target_features: vec![],
            },
            format_version: FORMAT_VERSION,
        }
    }

    // --- type_display_name tests ---

    #[test]
    fn display_primitive() {
        assert_eq!(
            type_display_name(&Type::Primitive("u32".to_string())),
            "u32"
        );
    }

    #[test]
    fn display_generic() {
        assert_eq!(type_display_name(&Type::Generic("T".to_string())), "T");
    }

    #[test]
    fn display_resolved_path() {
        let ty = resolved_path_type("std::vec::Vec", next_id());
        assert_eq!(type_display_name(&ty), "std::vec::Vec");
    }

    #[test]
    fn display_borrowed_ref() {
        let inner = Type::Primitive("str".to_string());
        let ty = Type::BorrowedRef {
            lifetime: Some("'a".to_string()),
            is_mutable: false,
            type_: Box::new(inner),
        };
        assert_eq!(type_display_name(&ty), "&str");
    }

    #[test]
    fn display_raw_pointer() {
        let inner = Type::Primitive("u8".to_string());
        let ty = Type::RawPointer {
            is_mutable: true,
            type_: Box::new(inner),
        };
        assert_eq!(type_display_name(&ty), "*u8");
    }

    #[test]
    fn display_slice() {
        let inner = Type::Primitive("u8".to_string());
        let ty = Type::Slice(Box::new(inner));
        assert_eq!(type_display_name(&ty), "[u8]");
    }

    #[test]
    fn display_tuple() {
        let ty = Type::Tuple(vec![
            Type::Primitive("u32".to_string()),
            Type::Primitive("bool".to_string()),
        ]);
        assert_eq!(type_display_name(&ty), "(u32, bool)");
    }

    #[test]
    fn display_empty_tuple() {
        let ty = Type::Tuple(vec![]);
        assert_eq!(type_display_name(&ty), "()");
    }

    // --- CollectCrateIds tests ---

    #[test]
    fn collect_ids_from_resolved_path() {
        let ext_id = next_id();
        let mut paths_map: PathsMap = HashMap::new();
        paths_map.insert(
            ext_id,
            ItemSummary {
                crate_id: 5,
                path: vec!["dep_crate".to_string(), "SomeType".to_string()],
                kind: ItemKind::Struct,
            },
        );

        let ty = resolved_path_type("dep_crate::SomeType", ext_id);
        let mut out = CrateIdSet::new();
        ty.collect_crate_ids(&paths_map, &mut out);

        assert_eq!(out.len(), 1);
        assert!(out.contains(&(5, "dep_crate::SomeType".to_string())));
    }

    #[test]
    fn collect_ids_from_primitive_is_empty() {
        let paths_map: PathsMap = HashMap::new();
        let ty = Type::Primitive("u32".to_string());
        let mut out = CrateIdSet::new();
        ty.collect_crate_ids(&paths_map, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_ids_from_generic_is_empty() {
        let paths_map: PathsMap = HashMap::new();
        let ty = Type::Generic("T".to_string());
        let mut out = CrateIdSet::new();
        ty.collect_crate_ids(&paths_map, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_ids_from_borrowed_ref_delegates() {
        let ext_id = next_id();
        let mut paths_map: PathsMap = HashMap::new();
        paths_map.insert(
            ext_id,
            ItemSummary {
                crate_id: 3,
                path: vec!["dep".to_string(), "Foo".to_string()],
                kind: ItemKind::Struct,
            },
        );

        let ty = Type::BorrowedRef {
            lifetime: None,
            is_mutable: false,
            type_: Box::new(resolved_path_type("dep::Foo", ext_id)),
        };
        let mut out = CrateIdSet::new();
        ty.collect_crate_ids(&paths_map, &mut out);
        assert!(out.contains(&(3, "dep::Foo".to_string())));
    }

    #[test]
    fn collect_ids_from_tuple_collects_all() {
        let ext1 = next_id();
        let ext2 = next_id();
        let mut paths_map: PathsMap = HashMap::new();
        paths_map.insert(
            ext1,
            ItemSummary {
                crate_id: 2,
                path: vec!["a".to_string(), "A".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            ext2,
            ItemSummary {
                crate_id: 3,
                path: vec!["b".to_string(), "B".to_string()],
                kind: ItemKind::Struct,
            },
        );

        let ty = Type::Tuple(vec![
            resolved_path_type("a::A", ext1),
            resolved_path_type("b::B", ext2),
        ]);
        let mut out = CrateIdSet::new();
        ty.collect_crate_ids(&paths_map, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&(2, "a::A".to_string())));
        assert!(out.contains(&(3, "b::B".to_string())));
    }

    #[test]
    fn collect_ids_from_fn_sig() {
        let arg_id = next_id();
        let ret_id = next_id();
        let mut paths_map: PathsMap = HashMap::new();
        paths_map.insert(
            arg_id,
            ItemSummary {
                crate_id: 7,
                path: vec!["dep".to_string(), "Input".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            ret_id,
            ItemSummary {
                crate_id: 7,
                path: vec!["dep".to_string(), "Output".to_string()],
                kind: ItemKind::Struct,
            },
        );

        let sig = FunctionSignature {
            inputs: vec![("x".to_string(), resolved_path_type("dep::Input", arg_id))],
            output: Some(resolved_path_type("dep::Output", ret_id)),
            is_c_variadic: false,
        };
        let mut out = CrateIdSet::new();
        sig.collect_crate_ids(&paths_map, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&(7, "dep::Input".to_string())));
        assert!(out.contains(&(7, "dep::Output".to_string())));
    }

    #[test]
    fn collect_ids_from_item_strips_local_crate() {
        let local_id = next_id();
        let fn_id = next_id();
        let mut paths_map: PathsMap = HashMap::new();
        paths_map.insert(
            local_id,
            ItemSummary {
                crate_id: 0,
                path: vec!["my_crate".to_string(), "MyType".to_string()],
                kind: ItemKind::Struct,
            },
        );

        let item = make_item(
            fn_id,
            Some("my_fn"),
            ItemEnum::Function(Function {
                sig: FunctionSignature {
                    inputs: vec![],
                    output: Some(resolved_path_type("my_crate::MyType", local_id)),
                    is_c_variadic: false,
                },
                generics: empty_generics(),
                header: fn_header(),
                has_body: true,
            }),
        );

        let mut out = CrateIdSet::new();
        item.collect_crate_ids(&paths_map, &mut out);
        assert!(
            out.is_empty(),
            "crate_id 0 (local crate) should be filtered out"
        );
    }

    // --- find_leaked_deps tests ---

    #[test]
    fn no_leaks_when_no_deps_tracked() {
        let fn_id = next_id();
        let fn_item = make_item(
            fn_id,
            Some("my_fn"),
            ItemEnum::Function(Function {
                sig: FunctionSignature {
                    inputs: vec![],
                    output: Some(Type::Primitive("u32".to_string())),
                    is_c_variadic: false,
                },
                generics: empty_generics(),
                header: fn_header(),
                has_body: true,
            }),
        );

        let mut index = HashMap::new();
        index.insert(fn_id, fn_item);

        let krate = make_crate(index, HashMap::new(), HashMap::new());
        let dep_ids: HashMap<u32, String> = HashMap::new();

        let leaked = find_leaked_deps(&krate, &dep_ids);
        assert!(leaked.is_empty());
    }

    #[test]
    fn function_returning_external_type_leaks() {
        let ext_type_id = next_id();
        let fn_id = next_id();

        let mut paths_map: HashMap<Id, ItemSummary> = HashMap::new();
        paths_map.insert(
            ext_type_id,
            ItemSummary {
                crate_id: 5,
                path: vec!["ext_dep".to_string(), "Widget".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            fn_id,
            ItemSummary {
                crate_id: 0,
                path: vec!["my_crate".to_string(), "get_widget".to_string()],
                kind: ItemKind::Function,
            },
        );

        let fn_item = make_item(
            fn_id,
            Some("get_widget"),
            ItemEnum::Function(Function {
                sig: FunctionSignature {
                    inputs: vec![],
                    output: Some(resolved_path_type("ext_dep::Widget", ext_type_id)),
                    is_c_variadic: false,
                },
                generics: empty_generics(),
                header: fn_header(),
                has_body: true,
            }),
        );

        let mut index = HashMap::new();
        index.insert(fn_id, fn_item);

        let mut ext_crates = HashMap::new();
        ext_crates.insert(
            5,
            ExternalCrate {
                name: "ext_dep".to_string(),
                html_root_url: None,
                path: PathBuf::new(),
            },
        );

        let krate = make_crate(index, paths_map, ext_crates);
        let mut dep_ids = HashMap::new();
        dep_ids.insert(5u32, "ext_dep".to_string());

        let leaked = find_leaked_deps(&krate, &dep_ids);
        assert!(leaked.contains_key("ext_dep"));
        let details = &leaked["ext_dep"];
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].item_kind, "fn");
        assert!(details[0].leaked_types.contains("ext_dep::Widget"));
    }

    #[test]
    fn function_with_only_local_types_no_leak() {
        let local_type_id = next_id();
        let fn_id = next_id();

        let mut paths_map: HashMap<Id, ItemSummary> = HashMap::new();
        paths_map.insert(
            local_type_id,
            ItemSummary {
                crate_id: 0,
                path: vec!["my_crate".to_string(), "MyStruct".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            fn_id,
            ItemSummary {
                crate_id: 0,
                path: vec!["my_crate".to_string(), "get_mine".to_string()],
                kind: ItemKind::Function,
            },
        );

        let fn_item = make_item(
            fn_id,
            Some("get_mine"),
            ItemEnum::Function(Function {
                sig: FunctionSignature {
                    inputs: vec![],
                    output: Some(resolved_path_type("my_crate::MyStruct", local_type_id)),
                    is_c_variadic: false,
                },
                generics: empty_generics(),
                header: fn_header(),
                has_body: true,
            }),
        );

        let mut index = HashMap::new();
        index.insert(fn_id, fn_item);

        let mut ext_crates = HashMap::new();
        ext_crates.insert(
            5,
            ExternalCrate {
                name: "ext_dep".to_string(),
                html_root_url: None,
                path: PathBuf::new(),
            },
        );

        let krate = make_crate(index, paths_map, ext_crates);
        let mut dep_ids = HashMap::new();
        dep_ids.insert(5u32, "ext_dep".to_string());

        let leaked = find_leaked_deps(&krate, &dep_ids);
        assert!(
            leaked.is_empty(),
            "functions using only local types should not leak"
        );
    }

    #[test]
    fn struct_field_leaks_external_type() {
        let ext_type_id = next_id();
        let struct_id = next_id();
        let field_id = next_id();

        let mut paths_map: HashMap<Id, ItemSummary> = HashMap::new();
        paths_map.insert(
            ext_type_id,
            ItemSummary {
                crate_id: 3,
                path: vec!["dep".to_string(), "Config".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            struct_id,
            ItemSummary {
                crate_id: 0,
                path: vec!["my_crate".to_string(), "MyWrapper".to_string()],
                kind: ItemKind::Struct,
            },
        );

        let field_item = make_item(
            field_id,
            Some("inner"),
            ItemEnum::StructField(resolved_path_type("dep::Config", ext_type_id)),
        );

        let struct_item = make_item(
            struct_id,
            Some("MyWrapper"),
            ItemEnum::Struct(Struct {
                generics: empty_generics(),
                kind: StructKind::Plain {
                    fields: vec![field_id],
                    has_stripped_fields: false,
                },
                impls: vec![],
            }),
        );

        let mut index = HashMap::new();
        index.insert(field_id, field_item);
        index.insert(struct_id, struct_item);

        let mut ext_crates = HashMap::new();
        ext_crates.insert(
            3,
            ExternalCrate {
                name: "dep".to_string(),
                html_root_url: None,
                path: PathBuf::new(),
            },
        );

        let krate = make_crate(index, paths_map, ext_crates);
        let mut dep_ids = HashMap::new();
        dep_ids.insert(3u32, "dep".to_string());

        let leaked = find_leaked_deps(&krate, &dep_ids);
        assert!(leaked.contains_key("dep"));
        let details = &leaked["dep"];
        assert!(details.iter().any(|d| d.item_kind == "field"));
        assert!(
            details
                .iter()
                .any(|d| d.leaked_types.contains("dep::Config"))
        );
    }

    #[test]
    fn reexport_leaks_external_type() {
        let ext_type_id = next_id();
        let use_id = next_id();

        let mut paths_map: HashMap<Id, ItemSummary> = HashMap::new();
        paths_map.insert(
            ext_type_id,
            ItemSummary {
                crate_id: 2,
                path: vec!["foreign".to_string(), "Gadget".to_string()],
                kind: ItemKind::Struct,
            },
        );

        let use_item = make_item(
            use_id,
            Some("Gadget"),
            ItemEnum::Use(Use {
                source: "foreign::Gadget".to_string(),
                name: "Gadget".to_string(),
                id: Some(ext_type_id),
                is_glob: false,
            }),
        );

        let mut index = HashMap::new();
        index.insert(use_id, use_item);

        let mut ext_crates = HashMap::new();
        ext_crates.insert(
            2,
            ExternalCrate {
                name: "foreign".to_string(),
                html_root_url: None,
                path: PathBuf::new(),
            },
        );

        let krate = make_crate(index, paths_map, ext_crates);
        let mut dep_ids = HashMap::new();
        dep_ids.insert(2u32, "foreign".to_string());

        let leaked = find_leaked_deps(&krate, &dep_ids);
        assert!(leaked.contains_key("foreign"));
        let details = &leaked["foreign"];
        assert!(details.iter().any(|d| d.item_kind == "re-export"));
    }

    #[test]
    fn multiple_deps_tracked_separately() {
        let ext_a_id = next_id();
        let ext_b_id = next_id();
        let fn_id = next_id();

        let mut paths_map: HashMap<Id, ItemSummary> = HashMap::new();
        paths_map.insert(
            ext_a_id,
            ItemSummary {
                crate_id: 2,
                path: vec!["dep_a".to_string(), "TypeA".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            ext_b_id,
            ItemSummary {
                crate_id: 3,
                path: vec!["dep_b".to_string(), "TypeB".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            fn_id,
            ItemSummary {
                crate_id: 0,
                path: vec!["my_crate".to_string(), "convert".to_string()],
                kind: ItemKind::Function,
            },
        );

        let fn_item = make_item(
            fn_id,
            Some("convert"),
            ItemEnum::Function(Function {
                sig: FunctionSignature {
                    inputs: vec![(
                        "a".to_string(),
                        resolved_path_type("dep_a::TypeA", ext_a_id),
                    )],
                    output: Some(resolved_path_type("dep_b::TypeB", ext_b_id)),
                    is_c_variadic: false,
                },
                generics: empty_generics(),
                header: fn_header(),
                has_body: true,
            }),
        );

        let mut index = HashMap::new();
        index.insert(fn_id, fn_item);

        let mut ext_crates = HashMap::new();
        ext_crates.insert(
            2,
            ExternalCrate {
                name: "dep_a".to_string(),
                html_root_url: None,
                path: PathBuf::new(),
            },
        );
        ext_crates.insert(
            3,
            ExternalCrate {
                name: "dep_b".to_string(),
                html_root_url: None,
                path: PathBuf::new(),
            },
        );

        let krate = make_crate(index, paths_map, ext_crates);
        let mut dep_ids = HashMap::new();
        dep_ids.insert(2u32, "dep_a".to_string());
        dep_ids.insert(3u32, "dep_b".to_string());

        let leaked = find_leaked_deps(&krate, &dep_ids);
        assert!(
            leaked.contains_key("dep_a"),
            "dep_a should be detected as leaked"
        );
        assert!(
            leaked.contains_key("dep_b"),
            "dep_b should be detected as leaked"
        );

        assert!(leaked["dep_a"][0].leaked_types.contains("dep_a::TypeA"));
        assert!(leaked["dep_b"][0].leaked_types.contains("dep_b::TypeB"));
    }

    #[test]
    fn untracked_dep_not_leaked() {
        let ext_type_id = next_id();
        let fn_id = next_id();

        let mut paths_map: HashMap<Id, ItemSummary> = HashMap::new();
        paths_map.insert(
            ext_type_id,
            ItemSummary {
                crate_id: 9,
                path: vec!["untracked".to_string(), "Thing".to_string()],
                kind: ItemKind::Struct,
            },
        );
        paths_map.insert(
            fn_id,
            ItemSummary {
                crate_id: 0,
                path: vec!["my_crate".to_string(), "do_thing".to_string()],
                kind: ItemKind::Function,
            },
        );

        let fn_item = make_item(
            fn_id,
            Some("do_thing"),
            ItemEnum::Function(Function {
                sig: FunctionSignature {
                    inputs: vec![],
                    output: Some(resolved_path_type("untracked::Thing", ext_type_id)),
                    is_c_variadic: false,
                },
                generics: empty_generics(),
                header: fn_header(),
                has_body: true,
            }),
        );

        let mut index = HashMap::new();
        index.insert(fn_id, fn_item);

        let krate = make_crate(index, paths_map, HashMap::new());
        let dep_ids: HashMap<u32, String> = HashMap::new();

        let leaked = find_leaked_deps(&krate, &dep_ids);
        assert!(
            leaked.is_empty(),
            "deps not in dep_crate_ids should not appear as leaked"
        );
    }
}

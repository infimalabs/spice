use super::*;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct NamespaceScopes {
    // Statement and block visitors clone scopes without changing most bindings.
    // Share those snapshots; copy-on-write keeps sibling scopes independent.
    visible: Arc<BTreeMap<String, NamespaceResolution>>,
    module: Arc<BTreeMap<String, NamespaceResolution>>,
    parents: Arc<Vec<Arc<BTreeMap<String, NamespaceResolution>>>>,
    root: Arc<BTreeMap<String, NamespaceResolution>>,
    global: Arc<BTreeMap<String, NamespaceResolution>>,
}

#[derive(Clone)]
enum NamespaceTarget {
    Known(NamespaceResolution),
    Path {
        segments: Vec<String>,
        leading_global: bool,
    },
}

impl NamespaceScopes {
    pub(super) fn new(global: BTreeMap<String, NamespaceResolution>) -> Self {
        let global = Arc::new(global);
        Self {
            visible: global.clone(),
            module: global.clone(),
            parents: Arc::new(Vec::new()),
            root: global.clone(),
            global,
        }
    }

    pub(super) fn resolve(&self, name: &str, leading_global: bool) -> NamespaceResolution {
        let bindings = if leading_global {
            &self.global
        } else {
            &self.visible
        };
        bindings
            .get(name)
            .copied()
            .unwrap_or(NamespaceResolution::Unresolved)
    }

    pub(super) fn visible(&self) -> &BTreeMap<String, NamespaceResolution> {
        &self.visible
    }

    pub(super) fn enter_child_module(&mut self) {
        Arc::make_mut(&mut self.parents).push(self.module.clone());
        self.visible = self.global.clone();
        self.module = self.global.clone();
    }

    pub(super) fn resolve_items(
        &mut self,
        items: &[&Item],
        crate_root: bool,
        module_scope: bool,
        edition: RustEdition,
        crate_std: NamespaceResolution,
        crate_core: NamespaceResolution,
    ) {
        let mut targets = BTreeMap::<String, Vec<NamespaceTarget>>::new();
        let mut generated = false;
        let mut explicit_externs = BTreeSet::new();
        for item in items {
            generated |= generated_namespace_is_unresolved(item)
                || item_attributes(item).iter().any(attribute_may_expand_items);
            collect_namespace_item(
                item,
                self,
                crate_std,
                crate_core,
                &mut targets,
                &mut generated,
            );
            if let Item::ExternCrate(item) = item {
                explicit_externs.insert(
                    item.rename
                        .as_ref()
                        .map_or_else(|| ident_name(&item.ident), |(_, name)| ident_name(name)),
                );
            }
        }

        let mut resolved = self.visible.clone();
        for name in targets.keys() {
            Arc::make_mut(&mut resolved).insert(name.clone(), NamespaceResolution::Unresolved);
        }
        for _ in 0..=targets.len() {
            let mut next = resolved.clone();
            for (name, candidates) in &targets {
                let mut candidates = candidates
                    .iter()
                    .map(|target| self.resolve_target(target, &resolved, crate_root, module_scope));
                let Some(mut resolution) = candidates.next() else {
                    continue;
                };
                for candidate in candidates {
                    if candidate != resolution {
                        resolution = NamespaceResolution::Unresolved;
                    }
                }
                Arc::make_mut(&mut next).insert(name.clone(), resolution);
            }
            if next == resolved {
                break;
            }
            resolved = next;
        }
        if generated {
            for resolution in Arc::make_mut(&mut resolved).values_mut() {
                if *resolution == NamespaceResolution::Compiler {
                    *resolution = NamespaceResolution::Unresolved;
                }
            }
        }

        self.visible = resolved.clone();
        if module_scope {
            self.module = resolved.clone();
        }
        if crate_root {
            self.root = resolved.clone();
            if matches!(edition, RustEdition::Edition2015) {
                self.global = resolved;
            } else {
                for name in explicit_externs {
                    if let Some(resolution) = resolved.get(&name) {
                        Arc::make_mut(&mut self.global).insert(name, *resolution);
                    }
                }
            }
        }
    }

    pub(super) fn resolve_path(
        &self,
        path: &[String],
        leading_global: bool,
        current: &BTreeMap<String, NamespaceResolution>,
        crate_root: bool,
        module_scope: bool,
    ) -> NamespaceResolution {
        if leading_global {
            return single_name_resolution(path, &self.global);
        }
        match path {
            [name] => current
                .get(name)
                .copied()
                .unwrap_or(NamespaceResolution::Unresolved),
            [root, name] if root == "crate" => {
                let bindings = if crate_root { current } else { &self.root };
                bindings
                    .get(name)
                    .copied()
                    .unwrap_or(NamespaceResolution::Unresolved)
            }
            [root, name] if root == "self" => {
                let bindings = if module_scope { current } else { &self.module };
                bindings
                    .get(name)
                    .copied()
                    .unwrap_or(NamespaceResolution::Unresolved)
            }
            _ => resolve_parent_namespace(path, &self.parents),
        }
    }

    fn resolve_target(
        &self,
        target: &NamespaceTarget,
        current: &BTreeMap<String, NamespaceResolution>,
        crate_root: bool,
        module_scope: bool,
    ) -> NamespaceResolution {
        match target {
            NamespaceTarget::Known(resolution) => *resolution,
            NamespaceTarget::Path {
                segments,
                leading_global,
            } => self.resolve_path(segments, *leading_global, current, crate_root, module_scope),
        }
    }
}

fn collect_namespace_item(
    item: &Item,
    scope: &NamespaceScopes,
    crate_std: NamespaceResolution,
    crate_core: NamespaceResolution,
    targets: &mut BTreeMap<String, Vec<NamespaceTarget>>,
    generated: &mut bool,
) {
    let conditional = conditional_binding(item_attributes(item));
    match item {
        Item::ExternCrate(item) => {
            let bound = item
                .rename
                .as_ref()
                .map_or_else(|| ident_name(&item.ident), |(_, name)| ident_name(name));
            let source = ident_name(&item.ident);
            let resolution = if conditional {
                NamespaceResolution::Unresolved
            } else if let Some(namespace) = qualified_namespace(&source) {
                match namespace {
                    QualifiedNamespace::Std => crate_std,
                    QualifiedNamespace::Core => crate_core,
                }
            } else if source == "self" {
                NamespaceResolution::Shadowed
            } else {
                scope.resolve(&source, true)
            };
            targets
                .entry(bound)
                .or_default()
                .push(NamespaceTarget::Known(resolution));
        }
        Item::Mod(item) => {
            let resolution = if conditional {
                NamespaceResolution::Unresolved
            } else {
                NamespaceResolution::Shadowed
            };
            targets
                .entry(ident_name(&item.ident))
                .or_default()
                .push(NamespaceTarget::Known(resolution));
        }
        Item::Use(item) => collect_namespace_use_tree(
            &item.tree,
            &mut Vec::new(),
            item.leading_colon.is_some(),
            conditional,
            targets,
            generated,
        ),
        _ => {}
    }
}

fn collect_namespace_use_tree(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    leading_global: bool,
    conditional: bool,
    targets: &mut BTreeMap<String, Vec<NamespaceTarget>>,
    generated: &mut bool,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(ident_name(&path.ident));
            collect_namespace_use_tree(
                &path.tree,
                prefix,
                leading_global,
                conditional,
                targets,
                generated,
            );
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let name = ident_name(&name.ident);
            let (source, bound) = if name == "self" {
                (prefix.clone(), prefix.last().cloned())
            } else {
                let mut source = prefix.clone();
                source.push(name.clone());
                (source, Some(name))
            };
            if let Some(bound) = bound {
                push_namespace_target(targets, bound, source, leading_global, conditional);
            }
        }
        syn::UseTree::Rename(rename) => {
            let mut source = prefix.clone();
            let name = ident_name(&rename.ident);
            if name != "self" {
                source.push(name);
            }
            push_namespace_target(
                targets,
                ident_name(&rename.rename),
                source,
                leading_global,
                conditional,
            );
        }
        syn::UseTree::Glob(_) => *generated = true,
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                collect_namespace_use_tree(
                    tree,
                    prefix,
                    leading_global,
                    conditional,
                    targets,
                    generated,
                );
            }
        }
    }
}

fn push_namespace_target(
    targets: &mut BTreeMap<String, Vec<NamespaceTarget>>,
    bound: String,
    source: Vec<String>,
    leading_global: bool,
    conditional: bool,
) {
    let target = if conditional {
        NamespaceTarget::Known(NamespaceResolution::Unresolved)
    } else {
        NamespaceTarget::Path {
            segments: source,
            leading_global,
        }
    };
    targets.entry(bound).or_default().push(target);
}

fn single_name_resolution(
    path: &[String],
    bindings: &BTreeMap<String, NamespaceResolution>,
) -> NamespaceResolution {
    let [name] = path else {
        return NamespaceResolution::Unresolved;
    };
    bindings
        .get(name)
        .copied()
        .unwrap_or(NamespaceResolution::Unresolved)
}

fn resolve_parent_namespace(
    path: &[String],
    parents: &[Arc<BTreeMap<String, NamespaceResolution>>],
) -> NamespaceResolution {
    let supers = path
        .iter()
        .take_while(|segment| *segment == "super")
        .count();
    if supers == 0 || path.len() != supers + 1 || supers > parents.len() {
        return NamespaceResolution::Unresolved;
    }
    parents[parents.len() - supers]
        .get(&path[supers])
        .copied()
        .unwrap_or(NamespaceResolution::Unresolved)
}

use super::*;

#[derive(Clone, Copy)]
pub(super) struct UseBindingContext<'a> {
    pub(super) leading_global: bool,
    pub(super) exported_macros: &'a BTreeSet<String>,
    pub(super) local_macros: &'a BTreeSet<String>,
    pub(super) conditional: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::workspace_tests) enum FileMacroResolution {
    Compiler(CompilerMacroKind),
    Shadowed,
    Unresolved,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct FileMacroBindings {
    inherited: [FileMacroResolution; 14],
    imports: [Option<FileMacroResolution>; 14],
    textual: [Option<FileMacroResolution>; 14],
}

impl FileMacroBindings {
    pub(super) fn compiler(standard_prelude: bool) -> Self {
        let mut inherited = CompilerMacroKind::ALL.map(FileMacroResolution::Compiler);
        if !standard_prelude {
            inherited[CompilerMacroKind::Println.index()] = FileMacroResolution::Unresolved;
            inherited[CompilerMacroKind::Format.index()] = FileMacroResolution::Unresolved;
        }
        Self {
            inherited,
            imports: [None; 14],
            textual: [None; 14],
        }
    }

    pub(super) fn resolve(self, name: CompilerMacroKind) -> FileMacroResolution {
        self.textual[name.index()]
            .or(self.imports[name.index()])
            .unwrap_or(self.inherited[name.index()])
    }

    pub(super) fn bind_import(&mut self, name: CompilerMacroKind, resolution: FileMacroResolution) {
        let binding = &mut self.imports[name.index()];
        *binding = Some(match *binding {
            None => resolution,
            Some(existing) if existing == resolution => existing,
            Some(_) => FileMacroResolution::Unresolved,
        });
    }

    pub(super) fn bind_textual(
        &mut self,
        name: CompilerMacroKind,
        resolution: FileMacroResolution,
    ) {
        self.textual[name.index()] = Some(resolution);
    }

    pub(super) fn mark_imports_unresolved(&mut self) {
        for kind in CompilerMacroKind::ALL {
            self.bind_import(kind, FileMacroResolution::Unresolved);
        }
    }

    pub(super) fn child_module(self) -> Self {
        let mut inherited = self.inherited;
        for kind in CompilerMacroKind::ALL {
            if let Some(textual) = self.textual[kind.index()] {
                inherited[kind.index()] = textual;
            }
        }
        Self {
            inherited,
            imports: [None; 14],
            textual: [None; 14],
        }
    }

    pub(super) fn child_block(self) -> Self {
        let mut inherited = self.inherited;
        for kind in CompilerMacroKind::ALL {
            inherited[kind.index()] = self.resolve(kind);
        }
        Self {
            inherited,
            imports: [None; 14],
            textual: [None; 14],
        }
    }
}

pub(super) fn generated_namespace_is_unresolved(item: &Item) -> bool {
    matches!(
        item,
        Item::Macro(item)
            if !item.mac.path.is_ident("macro_rules")
                && FileMacroKind::classify_path(&item.mac.path).is_none()
    )
}

pub(in crate::workspace_tests) fn attribute_may_expand_items(attribute: &syn::Attribute) -> bool {
    let path = attribute.path();
    if path.segments.len() != 1 {
        return true;
    }
    let name = ident_name(&path.segments[0].ident);
    if name == "cfg_attr" {
        let parser = Punctuated::<Meta, syn::Token![,]>::parse_terminated;
        let Ok(arguments) = attribute.parse_args_with(parser) else {
            return true;
        };
        return arguments
            .iter()
            .skip(1)
            .any(|meta| !safe_attribute_path(meta.path(), true));
    }
    !safe_attribute_path(path, false)
}

fn safe_attribute_path(path: &syn::Path, nested_cfg: bool) -> bool {
    if path.segments.len() != 1 {
        return false;
    }
    let name = ident_name(&path.segments[0].ident);
    matches!(
        name.as_str(),
        "allow"
            | "bench"
            | "cfg"
            | "cold"
            | "deprecated"
            | "deny"
            | "doc"
            | "expect"
            | "export_name"
            | "forbid"
            | "global_allocator"
            | "ignore"
            | "inline"
            | "link"
            | "link_name"
            | "link_ordinal"
            | "link_section"
            | "macro_export"
            | "macro_use"
            | "must_use"
            | "naked"
            | "no_mangle"
            | "non_exhaustive"
            | "panic_handler"
            | "path"
            | "proc_macro"
            | "proc_macro_attribute"
            | "proc_macro_derive"
            | "repr"
            | "should_panic"
            | "target_feature"
            | "test"
            | "track_caller"
            | "used"
            | "warn"
    ) && !(nested_cfg && name == "macro_use")
}

pub(super) fn macro_rules_name(item: &Item) -> Option<String> {
    let Item::Macro(item) = item else {
        return None;
    };
    item.mac
        .path
        .is_ident("macro_rules")
        .then(|| item.ident.as_ref().map(ident_name))
        .flatten()
}

pub(super) fn local_macro_rules(items: &[Item]) -> BTreeSet<String> {
    items
        .iter()
        .filter(|item| !conditional_binding(item_attributes(item)))
        .filter_map(macro_rules_name)
        .collect()
}

pub(in crate::workspace_tests) fn exported_macro_rules(items: &[Item]) -> BTreeSet<String> {
    items
        .iter()
        .filter(|item| {
            !conditional_binding(item_attributes(item))
                && item_attributes(item)
                    .iter()
                    .any(|attribute| attribute.path().is_ident("macro_export"))
        })
        .filter_map(macro_rules_name)
        .collect()
}

pub(super) fn conditional_binding(attributes: &[syn::Attribute]) -> bool {
    attributes
        .iter()
        .any(|attribute| attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr"))
}

use syn::punctuated::Punctuated;

const BUILTIN_DERIVES: [&str; 9] = [
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Eq",
    "Hash",
    "Ord",
    "PartialEq",
    "PartialOrd",
];

pub(super) fn builtin_derive(attribute: &syn::Attribute) -> bool {
    if !attribute.path().is_ident("derive") {
        return false;
    }
    let parser = Punctuated::<syn::Path, syn::Token![,]>::parse_terminated;
    let Ok(paths) = attribute.parse_args_with(parser) else {
        return false;
    };
    !paths.is_empty()
        && paths.iter().all(|path| {
            path.leading_colon.is_none()
                && path.segments.len() == 1
                && BUILTIN_DERIVES.contains(&path.segments[0].ident.to_string().as_str())
        })
}

pub(super) fn builtin_derives_unshadowed(syntax: &syn::File) -> bool {
    !syntax.items.iter().any(|item| match item {
        syn::Item::Use(item) => use_tree_may_shadow_builtin_derive(&item.tree),
        syn::Item::ExternCrate(item) => item
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("macro_use")),
        _ => false,
    })
}

fn use_tree_may_shadow_builtin_derive(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Path(path) => use_tree_may_shadow_builtin_derive(&path.tree),
        syn::UseTree::Name(name) => BUILTIN_DERIVES.contains(&name.ident.to_string().as_str()),
        syn::UseTree::Rename(rename) => {
            BUILTIN_DERIVES.contains(&rename.rename.to_string().as_str())
        }
        syn::UseTree::Group(group) => group.items.iter().any(use_tree_may_shadow_builtin_derive),
        syn::UseTree::Glob(_) => true,
    }
}

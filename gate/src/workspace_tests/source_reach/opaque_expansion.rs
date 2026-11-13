use super::*;

mod dynamic_macro;

use dynamic_macro::dynamic_macro_invocation_at;

impl FileMacroPath {
    fn spelling(self) -> String {
        match self {
            Self::Unqualified(kind) => kind.name().to_owned(),
            Self::Qualified {
                namespace,
                kind,
                leading_global,
            } => format!(
                "{}{}::{}",
                if leading_global { "::" } else { "" },
                namespace.name(),
                kind.name(),
            ),
        }
    }
}

pub(super) fn opaque_expansion_file_macro_error(
    tokens: TokenStream,
    source: &Path,
) -> Option<WorkspaceTestError> {
    let trees = tokens.into_iter().collect::<Vec<_>>();
    for (index, tree) in trees.iter().enumerate() {
        if let Some((path, _, _)) = file_macro_invocation_at(&trees, index) {
            return Some(WorkspaceTestError::new(format!(
                "Rust source {} cannot prove expansion-site file-macro namespace for {}",
                source.display(),
                path.spelling(),
            )));
        }
        if let Some(name) = dynamic_macro_invocation_at(&trees, index) {
            return Some(WorkspaceTestError::new(format!(
                "Rust source {} cannot prove expansion-site file-macro namespace for macro path containing ${name}",
                source.display(),
            )));
        }
        if let TokenTree::Group(group) = tree
            && let Some(error) = opaque_expansion_file_macro_error(group.stream(), source)
        {
            return Some(error);
        }
    }
    None
}

pub(super) fn macro_rules_body_at(trees: &[TokenTree], index: usize) -> Option<(usize, usize)> {
    let Some(TokenTree::Ident(keyword)) = trees.get(index) else {
        return None;
    };
    if ident_name(keyword) != "macro_rules"
        || !matches!(trees.get(index + 1), Some(TokenTree::Punct(punct)) if punct.as_char() == '!')
        || !matches!(trees.get(index + 2), Some(TokenTree::Ident(_)))
        || !matches!(trees.get(index + 3), Some(TokenTree::Group(_)))
    {
        return None;
    }
    Some((index + 3, index + 4))
}

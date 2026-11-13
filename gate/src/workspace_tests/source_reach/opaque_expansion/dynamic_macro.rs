use super::*;

pub(super) fn dynamic_macro_invocation_at(trees: &[TokenTree], index: usize) -> Option<String> {
    let mut cursor = index;
    if path_separator_at(trees, cursor) {
        cursor += 2;
    }
    let mut variable = None;
    loop {
        match trees.get(cursor)? {
            TokenTree::Ident(_) => cursor += 1,
            TokenTree::Punct(punct) if punct.as_char() == '$' => {
                let Some(TokenTree::Ident(name)) = trees.get(cursor + 1) else {
                    return None;
                };
                variable.get_or_insert_with(|| ident_name(name));
                cursor += 2;
            }
            _ => return None,
        }
        if macro_arguments_at(trees, cursor, cursor + 1) {
            return variable;
        }
        if !path_separator_at(trees, cursor) {
            return None;
        }
        cursor += 2;
    }
}

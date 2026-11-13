//! Rust test-only item selection through the packaged structural grammar.

use tree_sitter::Node;

use super::{StructuralScanError, parse_tree};

/// Exact Rust items whose attributes require the test configuration.
///
/// Tree-sitter owns item boundaries, so a selected inline test module cannot
/// absorb an adjacent production item through textual brace matching.
#[doc(hidden)]
pub fn rust_test_item_sources(source: &str) -> Result<Vec<String>, StructuralScanError> {
    let tree = parse_tree("rust", source)?;
    let mut ranges = Vec::new();
    collect_test_item_ranges(tree.root_node(), source, &mut ranges);
    ranges.sort_unstable();
    ranges.dedup();
    Ok(ranges
        .into_iter()
        .map(|(start, end)| source[start..end].to_owned())
        .collect())
}

fn collect_test_item_ranges(node: Node<'_>, source: &str, ranges: &mut Vec<(usize, usize)>) {
    let mut pending_test_attribute = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "attribute_item" {
            let attribute = &source[child.byte_range()];
            pending_test_attribute |= attribute_requires_test(attribute);
            continue;
        }
        if matches!(child.kind(), "line_comment" | "block_comment") {
            continue;
        }
        if pending_test_attribute {
            ranges.push((child.start_byte(), child.end_byte()));
            pending_test_attribute = false;
            continue;
        }
        collect_test_item_ranges(child, source, ranges);
    }
}

fn attribute_requires_test(attribute: &str) -> bool {
    let compact = attribute
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let Some(body) = compact
        .strip_prefix("#[")
        .and_then(|body| body.strip_suffix(']'))
    else {
        return false;
    };
    let name = body.split_once('(').map_or(body, |(name, _)| name);
    if name == "test" || name.ends_with("::test") {
        return true;
    }
    body.strip_prefix("cfg(")
        .and_then(|expression| expression.strip_suffix(')'))
        .is_some_and(cfg_expression_requires_test)
}

fn cfg_expression_requires_test(expression: &str) -> bool {
    if expression == "test" {
        return true;
    }
    if let Some(arguments) = cfg_call_arguments(expression, "all") {
        return split_cfg_arguments(arguments)
            .into_iter()
            .any(cfg_expression_requires_test);
    }
    if let Some(arguments) = cfg_call_arguments(expression, "any") {
        let arguments = split_cfg_arguments(arguments);
        return !arguments.is_empty() && arguments.into_iter().all(cfg_expression_requires_test);
    }
    false
}

fn cfg_call_arguments<'a>(expression: &'a str, operator: &str) -> Option<&'a str> {
    expression
        .strip_prefix(operator)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

fn split_cfg_arguments(arguments: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut depth = 0_u32;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in arguments.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                found.push(&arguments[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if start < arguments.len() {
        found.push(&arguments[start..]);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_respects_cfg_semantics_and_item_boundaries() {
        let source = concat!(
            "fn production_before() {}\n",
            "#[cfg(test)]\nmod tests { fn nested() {} }\n",
            "#[cfg(all(unix, test))]\nfn unix_test_helper() {}\n",
            "#[test]\nfn direct_test() {}\n",
            "#[tokio::test(flavor = \"current_thread\")]\nasync fn async_test() {}\n",
            "#[cfg(any(target_os = \"linux\", test))]\nfn shared_item() {}\n",
            "#[cfg(not(test))]\nfn production_after() {}\n",
        );

        let selected = rust_test_item_sources(source).expect("Rust test items scan");
        assert_eq!(selected.len(), 4);
        assert!(selected.iter().any(|item| item.contains("mod tests")));
        assert!(
            selected
                .iter()
                .any(|item| item.contains("unix_test_helper"))
        );
        assert!(selected.iter().any(|item| item.contains("direct_test")));
        assert!(selected.iter().any(|item| item.contains("async_test")));
        for production in ["production_before", "shared_item", "production_after"] {
            assert!(selected.iter().all(|item| !item.contains(production)));
        }
    }
}

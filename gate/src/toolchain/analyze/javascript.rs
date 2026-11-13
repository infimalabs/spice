use super::*;

#[derive(Debug)]
struct JavascriptDeclaration {
    path: String,
    line: u32,
    declaration_kind: String,
    symbol: String,
    in_test: bool,
}

#[derive(Debug)]
struct JavascriptInventory {
    examined: usize,
    declarations: Vec<JavascriptDeclaration>,
    references: BTreeMap<String, (usize, usize)>,
}

/// Resolve top-level JavaScript declarations against every reference in the
/// selected modules.
///
/// Only a module's own top-level declarations are candidates, because a symbol
/// nested inside a function or block is that scope's to spend and its disuse is
/// not this gate's finding. The production count includes a declaration's own
/// identifier. Consequently `(1, 0)` means a production declaration is truly
/// unused, while `(1, n)` means its only uses beyond the declaration occur in
/// configured test roots.
pub(in crate::toolchain) fn javascript_unused(
    declaration: ToolchainDeclaration,
    root: &Path,
    reached_paths: &[String],
) -> Analysis {
    let inventory = match javascript_inventory(root, reached_paths) {
        Ok(inventory) => inventory,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    if inventory.examined == 0 {
        return Analysis::not_applicable(declaration);
    }

    let mut findings = Vec::new();
    for declared in inventory.declarations {
        if javascript_exempt(&declared.path, &declared.symbol) {
            continue;
        }
        let (production, tests) = inventory
            .references
            .get(&declared.symbol)
            .copied()
            .unwrap_or_default();
        let (production, tests) = if declared.in_test {
            (production, tests + 1)
        } else {
            (production + 1, tests)
        };
        let status = if production + tests == 1 {
            Some((
                JavascriptUnusedStatus::CandidateUnused,
                "no_references_outside_declaration",
            ))
        } else if !declared.in_test && production == 1 && tests > 0 {
            Some((JavascriptUnusedStatus::TestOnly, "references_only_in_tests"))
        } else {
            None
        };
        let Some((status, reason)) = status else {
            continue;
        };
        let Ok(reference_count) = u32::try_from(production) else {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!(
                    "javascript-unused reference count overflowed for {}",
                    declared.symbol
                ),
            );
        };
        let Ok(test_reference_count) = u32::try_from(tests) else {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!(
                    "javascript-unused test reference count overflowed for {}",
                    declared.symbol
                ),
            );
        };
        findings.push(ArmFinding {
            path: Some(declared.path),
            line: Some(declared.line),
            column: None,
            detail: FindingDetail::JavascriptUnused {
                symbol: declared.symbol,
                declaration_kind: declared.declaration_kind,
                status,
                reason: reason.to_owned(),
                reference_count,
                test_reference_count,
            },
        });
    }
    Analysis::scanned(declaration.id, inventory.examined, findings)
}

/// Count every reference vocabulary where it applies across the selected modules.
///
/// Production code reaches a symbol by name, so identifiers and object
/// shorthand are what a reference looks like there. Test harnesses additionally
/// reach production symbols through a sandbox namespace as `context.symbol`, so
/// a property identifier is a genuine test-side reference and is counted only
/// under a configured test path, where that is what it means.
fn javascript_inventory(
    root: &Path,
    reached_paths: &[String],
) -> Result<JavascriptInventory, String> {
    let sources = javascript_sources(root, reached_paths)?;
    let mut declarations = Vec::new();
    let mut references: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (path, source, in_test) in &sources {
        let matches = scan_javascript_symbols(source)
            .map_err(|error| format!("javascript-unused could not parse {path}: {error}"))?;
        let declaration_loci: BTreeSet<(usize, usize)> = matches
            .iter()
            .filter(|matched| matched.pattern() == StructuralPattern::JavaScriptDeclaration)
            .flat_map(|matched| matched.captures())
            .map(|capture| (capture.start_byte(), capture.end_byte()))
            .collect();
        for matched in matches {
            let [capture] = matched.captures() else {
                return Err(format!(
                    "javascript-unused received a malformed {} match in {path}",
                    matched.pattern().as_str()
                ));
            };
            match matched.pattern() {
                StructuralPattern::JavaScriptDeclaration => {
                    let line = source_line(source, matched.start_byte())
                        .ok_or_else(|| format!("javascript-unused line overflowed in {path}"))?;
                    declarations.push(JavascriptDeclaration {
                        path: path.clone(),
                        line,
                        declaration_kind: capture.name().to_owned(),
                        symbol: capture.text().to_owned(),
                        in_test: *in_test,
                    });
                }
                StructuralPattern::JavaScriptIdentifier => {
                    if declaration_loci.contains(&(capture.start_byte(), capture.end_byte())) {
                        continue;
                    }
                    let counts = references.entry(capture.text().to_owned()).or_default();
                    if *in_test {
                        counts.1 += 1;
                    } else {
                        counts.0 += 1;
                    }
                }
                StructuralPattern::JavaScriptPropertyIdentifier => {
                    if *in_test {
                        references.entry(capture.text().to_owned()).or_default().1 += 1;
                    }
                }
                StructuralPattern::FunctionCall
                | StructuralPattern::CSharpDeclaration
                | StructuralPattern::CSharpIdentifier => {
                    return Err(format!(
                        "javascript-unused received an unrelated structural match in {path}"
                    ));
                }
            }
        }
    }
    Ok(JavascriptInventory {
        examined: sources.len(),
        declarations,
        references,
    })
}

fn javascript_sources(
    root: &Path,
    reached_paths: &[String],
) -> Result<Vec<(String, String, bool)>, String> {
    let paths = selected_changes(root, reached_paths, |path| {
        extension(path).is_some_and(|found| JAVASCRIPT_EXTENSIONS.contains(&found.as_str()))
    });

    paths
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(root.join(&path)).map_err(|error| {
                format!("javascript-unused could not read {path} as UTF-8: {error}")
            })?;
            let in_test = PACKAGED_TEST_PATHS.iter().any(|test_root| {
                path == *test_root
                    || path
                        .strip_prefix(test_root)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            });
            Ok((path, source, in_test))
        })
        .collect()
}

fn javascript_exempt(path: &str, symbol: &str) -> bool {
    PACKAGED_JAVASCRIPT_EXEMPTIONS
        .iter()
        .any(|(exempt_path, exempt_symbol, _)| path == *exempt_path && symbol == *exempt_symbol)
}

pub(super) fn source_line(source: &str, start_byte: usize) -> Option<u32> {
    let prefix = source.as_bytes().get(..start_byte)?;
    u32::try_from(prefix.iter().filter(|byte| **byte == b'\n').count() + 1).ok()
}

//! One tree-sitter structural scan path over every supported source grammar.

use std::error::Error;
use std::fmt;

use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};
use tree_sitter_language::LanguageFn;

mod rust_test_items;

pub use rust_test_items::rust_test_item_sources;

/// One language-independent structural pattern identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum StructuralPattern {
    /// A direct call whose callee is one identifier.
    FunctionCall,
    /// One named JavaScript declaration at a module's own top level.
    JavaScriptDeclaration,
    /// One JavaScript identifier occurrence.
    JavaScriptIdentifier,
    /// One JavaScript property-identifier occurrence, as in `object.symbol`.
    JavaScriptPropertyIdentifier,
    /// One named C# type or member declaration.
    CSharpDeclaration,
    /// One C# identifier occurrence.
    CSharpIdentifier,
}

impl StructuralPattern {
    /// Stable pattern name used in diagnostics and evidence.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FunctionCall => "function-call",
            Self::JavaScriptDeclaration => "javascript-declaration",
            Self::JavaScriptIdentifier => "javascript-identifier",
            Self::JavaScriptPropertyIdentifier => "javascript-property-identifier",
            Self::CSharpDeclaration => "csharp-declaration",
            Self::CSharpIdentifier => "csharp-identifier",
        }
    }
}

/// One named capture retained from a structural match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuralCapture {
    name: String,
    text: String,
    start_byte: usize,
    end_byte: usize,
}

impl StructuralCapture {
    /// Query capture name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact source text selected by the capture.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Inclusive byte offset where the capture begins.
    #[must_use]
    pub const fn start_byte(&self) -> usize {
        self.start_byte
    }

    /// Exclusive byte offset where the capture ends.
    #[must_use]
    pub const fn end_byte(&self) -> usize {
        self.end_byte
    }
}

/// One language-independent structural pattern match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuralMatch {
    pattern: StructuralPattern,
    start_byte: usize,
    end_byte: usize,
    captures: Vec<StructuralCapture>,
}

/// Parsed module edges whose spelling can alter a JavaScript component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JavascriptModuleDependencies {
    /// Literal `import`, re-export, dynamic-import, and `require` specifiers.
    pub(crate) specifiers: Vec<String>,
}

/// Parsed namespace and include edges whose spelling can alter a C# component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CsharpModuleDependencies {
    /// Namespaces declared by this compilation unit.
    pub(crate) namespaces: Vec<String>,
    /// Namespaces imported by this compilation unit.
    pub(crate) imports: Vec<String>,
    /// Literal `#load` paths included by a C# script.
    pub(crate) includes: Vec<String>,
}

impl StructuralMatch {
    /// Pattern that matched.
    #[must_use]
    pub const fn pattern(&self) -> StructuralPattern {
        self.pattern
    }

    /// Inclusive byte offset where the complete match begins.
    #[must_use]
    pub const fn start_byte(&self) -> usize {
        self.start_byte
    }

    /// Exclusive byte offset where the complete match ends.
    #[must_use]
    pub const fn end_byte(&self) -> usize {
        self.end_byte
    }

    /// Named captures retained in source order.
    #[must_use]
    pub fn captures(&self) -> &[StructuralCapture] {
        &self.captures
    }
}

/// Typed reason a structural scan could not produce matches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StructuralScanError {
    /// The caller named no grammar in the scanner's closed registry.
    UnsupportedLanguage {
        observed: String,
        supported: Vec<&'static str>,
    },
    /// A packaged grammar cannot be installed in the current parser.
    Grammar {
        language: &'static str,
        reason: String,
    },
    /// Tree-sitter could not produce one syntax tree.
    ParseStopped { language: &'static str },
    /// The source contains syntax errors under the selected grammar.
    MalformedSource { language: &'static str },
    /// A compiled pattern declaration and grammar binding disagree.
    PatternBinding {
        language: &'static str,
        pattern: StructuralPattern,
        reason: String,
    },
}

impl fmt::Display for StructuralScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedLanguage {
                observed,
                supported,
            } => write!(
                formatter,
                "unsupported structural-scan language {observed:?}; choose {}",
                supported.join(", ")
            ),
            Self::Grammar { language, reason } => {
                write!(
                    formatter,
                    "{language} structural grammar is incompatible: {reason}"
                )
            }
            Self::ParseStopped { language } => {
                write!(
                    formatter,
                    "{language} structural parse stopped without a tree"
                )
            }
            Self::MalformedSource { language } => {
                write!(
                    formatter,
                    "source is malformed under the {language} grammar"
                )
            }
            Self::PatternBinding {
                language,
                pattern,
                reason,
            } => write!(
                formatter,
                "{language} binding for structural pattern {} is invalid: {reason}",
                pattern.as_str()
            ),
        }
    }
}

impl Error for StructuralScanError {}

#[derive(Clone, Copy)]
struct PatternDeclaration {
    pattern: StructuralPattern,
    match_capture: &'static str,
}

#[derive(Clone, Copy)]
struct PatternBinding {
    pattern: StructuralPattern,
    query: &'static str,
}

#[derive(Clone, Copy)]
struct GrammarBinding {
    name: &'static str,
    language: LanguageFn,
    patterns: &'static [PatternBinding],
}

const PATTERNS: &[PatternDeclaration] = &[PatternDeclaration {
    pattern: StructuralPattern::FunctionCall,
    match_capture: "match",
}];

const JAVASCRIPT_SYMBOL_PATTERNS: &[PatternDeclaration] = &[
    PatternDeclaration {
        pattern: StructuralPattern::JavaScriptDeclaration,
        match_capture: "match",
    },
    PatternDeclaration {
        pattern: StructuralPattern::JavaScriptIdentifier,
        match_capture: "match",
    },
    PatternDeclaration {
        pattern: StructuralPattern::JavaScriptPropertyIdentifier,
        match_capture: "match",
    },
];

const CSHARP_SYMBOL_PATTERNS: &[PatternDeclaration] = &[
    PatternDeclaration {
        pattern: StructuralPattern::CSharpDeclaration,
        match_capture: "match",
    },
    PatternDeclaration {
        pattern: StructuralPattern::CSharpIdentifier,
        match_capture: "match",
    },
];

const CSHARP_SYMBOL_BINDINGS: &[PatternBinding] = &[
    PatternBinding {
        pattern: StructuralPattern::CSharpDeclaration,
        query: concat!(
            "[",
            " (class_declaration name: (identifier) @class)",
            " (interface_declaration name: (identifier) @interface)",
            " (struct_declaration name: (identifier) @struct)",
            " (record_declaration name: (identifier) @record)",
            " (enum_declaration name: (identifier) @enum)",
            " (method_declaration name: (identifier) @method)",
            " (property_declaration name: (identifier) @property)",
            " (field_declaration (variable_declaration ",
            "(variable_declarator name: (identifier) @field)))",
            " (event_field_declaration (variable_declaration ",
            "(variable_declarator name: (identifier) @event)))",
            "] @match",
        ),
    },
    PatternBinding {
        pattern: StructuralPattern::CSharpIdentifier,
        query: "(identifier) @symbol @match",
    },
];

/// The named declaration forms a module can carry, as one query alternation.
///
/// Expanded twice below because a declaration is top-level whether it stands
/// directly in the program or is wrapped by one export statement, and a
/// tree-sitter pattern names one parent at a time.
macro_rules! javascript_declaration_forms {
    () => {
        concat!(
            "[",
            " (class_declaration name: (identifier) @class)",
            " (function_declaration name: (identifier) @function)",
            " (generator_function_declaration name: (identifier) @function)",
            " (lexical_declaration \"const\" ",
            "(variable_declarator name: (identifier) @constant))",
            " (lexical_declaration \"let\" ",
            "(variable_declarator name: (identifier) @let))",
            " (variable_declaration ",
            "(variable_declarator name: (identifier) @variable))",
            "] @match",
        )
    };
}

const JAVASCRIPT_SYMBOL_BINDINGS: &[PatternBinding] = &[
    PatternBinding {
        pattern: StructuralPattern::JavaScriptDeclaration,
        query: concat!(
            "(program ",
            javascript_declaration_forms!(),
            ")",
            " (program (export_statement ",
            javascript_declaration_forms!(),
            "))",
        ),
    },
    PatternBinding {
        pattern: StructuralPattern::JavaScriptIdentifier,
        query: "[(identifier) (shorthand_property_identifier)] @symbol @match",
    },
    PatternBinding {
        pattern: StructuralPattern::JavaScriptPropertyIdentifier,
        query: "(property_identifier) @symbol @match",
    },
];

const GRAMMARS: &[GrammarBinding] = &[
    GrammarBinding {
        name: "python",
        language: tree_sitter_python::LANGUAGE,
        patterns: &[PatternBinding {
            pattern: StructuralPattern::FunctionCall,
            query: "(call function: (identifier) @callee) @match",
        }],
    },
    GrammarBinding {
        name: "rust",
        language: tree_sitter_rust::LANGUAGE,
        patterns: &[PatternBinding {
            pattern: StructuralPattern::FunctionCall,
            query: "(call_expression function: (identifier) @callee) @match",
        }],
    },
    GrammarBinding {
        name: "javascript",
        language: tree_sitter_javascript::LANGUAGE,
        patterns: &[PatternBinding {
            pattern: StructuralPattern::FunctionCall,
            query: "(call_expression function: (identifier) @callee) @match",
        }],
    },
    GrammarBinding {
        name: "csharp",
        language: tree_sitter_c_sharp::LANGUAGE,
        patterns: &[PatternBinding {
            pattern: StructuralPattern::FunctionCall,
            query: "(invocation_expression function: (identifier) @callee) @match",
        }],
    },
    GrammarBinding {
        name: "lua",
        language: tree_sitter_lua::LANGUAGE,
        patterns: &[PatternBinding {
            pattern: StructuralPattern::FunctionCall,
            query: "(function_call name: (identifier) @callee) @match",
        }],
    },
];

/// Parse source with one supported grammar and run every declared structural pattern.
///
/// Language selection, parsing, query execution, and result normalization all
/// occur in this entrypoint. Grammar-specific knowledge is confined to the
/// private `GRAMMARS` table.
///
/// # Errors
///
/// Returns [`StructuralScanError::UnsupportedLanguage`] when `language` does
/// not name one of `python`, `rust`, `javascript`, `csharp`, or `lua`. Other
/// variants report packaged grammar, source, or pattern-contract failures.
pub fn scan_structural(
    language: &str,
    source: &str,
) -> Result<Vec<StructuralMatch>, StructuralScanError> {
    let grammar = grammar(language)?;
    scan(grammar, source, PATTERNS, grammar.patterns)
}

/// Parse JavaScript once and retain its top-level declarations, its
/// identifiers, and its property identifiers.
///
/// Only declarations a module states at its own top level are retained, because
/// a symbol nested inside a function or block is local to it and is not the
/// module's to leave unused. Identifiers and property identifiers are retained
/// separately so a caller can count each vocabulary where it applies.
///
/// This stays crate-internal because it is one domain-specific query set over
/// the shared structural scanner, not a second public scanner contract.
pub(crate) fn scan_javascript_symbols(
    source: &str,
) -> Result<Vec<StructuralMatch>, StructuralScanError> {
    let grammar = grammar("javascript")?;
    scan(
        grammar,
        source,
        JAVASCRIPT_SYMBOL_PATTERNS,
        JAVASCRIPT_SYMBOL_BINDINGS,
    )
}

/// Parse C# once and retain its named type and member declarations alongside
/// every identifier occurrence.
///
/// A C# declaration is named through a `name` field on the declaration itself,
/// except for fields and events, which name themselves through the variable
/// declarator they wrap. Identifiers are retained separately so a caller can
/// count references, where a declaration's own name is one of the occurrences.
///
/// This stays crate-internal for the same reason as
/// [`scan_javascript_symbols`]: it is one domain-specific query set over the
/// shared structural scanner, not a second public scanner contract.
pub(crate) fn scan_csharp_symbols(
    source: &str,
) -> Result<Vec<StructuralMatch>, StructuralScanError> {
    let grammar = grammar("csharp")?;
    scan(
        grammar,
        source,
        CSHARP_SYMBOL_PATTERNS,
        CSHARP_SYMBOL_BINDINGS,
    )
}

/// Parse JavaScript module-loading syntax into literal dependency specifiers.
///
/// Dynamic module expressions are rejected rather than guessed. The reach
/// compiler turns that rejection into an explicit exhaustive language-graph
/// plan before any focused analyzer can run.
pub(crate) fn scan_javascript_module_dependencies(
    source: &str,
) -> Result<JavascriptModuleDependencies, String> {
    let tree = parse_tree("javascript", source).map_err(|error| error.to_string())?;
    let mut specifiers = Vec::new();
    visit_nodes(tree.root_node(), &mut |node| match node.kind() {
        "import_statement" | "export_statement" => {
            if let Some(source_node) = node.child_by_field_name("source") {
                specifiers.push(javascript_string(source_node, source)?);
            }
            Ok(())
        }
        "call_expression" => {
            let Some(function) = node.child_by_field_name("function") else {
                return Ok(());
            };
            let function_text = function.utf8_text(source.as_bytes()).unwrap_or_default();
            if function.kind() != "import" && function_text != "require" {
                return Ok(());
            }
            let arguments = node
                .child_by_field_name("arguments")
                .ok_or_else(|| "javascript module loader has no parsed argument list".to_owned())?;
            let mut cursor = arguments.walk();
            let named = arguments.named_children(&mut cursor).collect::<Vec<_>>();
            if named.len() != 1 || named[0].kind() != "string" {
                return Err(
                    "javascript module loader uses a dynamic or unsupported specifier".to_owned(),
                );
            }
            specifiers.push(javascript_string(named[0], source)?);
            Ok(())
        }
        _ => Ok(()),
    })?;
    specifiers.sort();
    specifiers.dedup();
    Ok(JavascriptModuleDependencies { specifiers })
}

/// Parse C# namespaces and script includes into dependency vocabulary.
///
/// `#r` and non-literal `#load` directives are refused because their resolved
/// compilation inventory depends on authorities this parser does not own.
pub(crate) fn scan_csharp_module_dependencies(
    source: &str,
) -> Result<CsharpModuleDependencies, String> {
    let tree = parse_tree("csharp", source).map_err(|error| error.to_string())?;
    let mut namespaces = Vec::new();
    let mut imports = Vec::new();
    visit_nodes(tree.root_node(), &mut |node| {
        let destination = match node.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => &mut namespaces,
            "using_directive" => &mut imports,
            _ => return Ok(()),
        };
        let Some(name) = node.child_by_field_name("name") else {
            if node.kind() == "using_directive" {
                let text = node.utf8_text(source.as_bytes()).unwrap_or_default();
                let imported = text
                    .trim()
                    .strip_prefix("using ")
                    .and_then(|value| value.strip_suffix(';'))
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && !value.contains('='));
                if let Some(imported) = imported {
                    destination.push(imported.trim_start_matches("static ").to_owned());
                    return Ok(());
                }
            }
            return Err(format!("{} has no parsed name", node.kind()));
        };
        let name = name
            .utf8_text(source.as_bytes())
            .map_err(|error| format!("{} name is not valid UTF-8: {error}", node.kind()))?;
        destination.push(name.to_owned());
        Ok(())
    })?;

    let mut includes = Vec::new();
    for line in source.lines().map(str::trim) {
        if line.starts_with("#r") {
            return Err("csharp dependency graph does not resolve #r directives".to_owned());
        }
        let Some(value) = line.strip_prefix("#load") else {
            continue;
        };
        let value = value.trim();
        if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
            return Err("csharp #load uses a dynamic or malformed path".to_owned());
        }
        let path = &value[1..value.len() - 1];
        if path.is_empty() || path.contains(['\\', '\0', '\r', '\n']) {
            return Err("csharp #load path uses an unsupported escape".to_owned());
        }
        includes.push(path.to_owned());
    }
    namespaces.sort();
    namespaces.dedup();
    imports.sort();
    imports.dedup();
    includes.sort();
    includes.dedup();
    Ok(CsharpModuleDependencies {
        namespaces,
        imports,
        includes,
    })
}

fn parse_tree(language: &str, source: &str) -> Result<tree_sitter::Tree, StructuralScanError> {
    let grammar = grammar(language)?;
    let language: Language = grammar.language.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| StructuralScanError::Grammar {
            language: grammar.name,
            reason: error.to_string(),
        })?;
    let tree = parser
        .parse(source, None)
        .ok_or(StructuralScanError::ParseStopped {
            language: grammar.name,
        })?;
    if tree.root_node().has_error() {
        return Err(StructuralScanError::MalformedSource {
            language: grammar.name,
        });
    }
    Ok(tree)
}

fn visit_nodes(
    node: Node<'_>,
    visit: &mut impl FnMut(Node<'_>) -> Result<(), String>,
) -> Result<(), String> {
    visit(node)?;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_nodes(child, visit)?;
    }
    Ok(())
}

fn javascript_string(node: Node<'_>, source: &str) -> Result<String, String> {
    let text = node
        .utf8_text(source.as_bytes())
        .map_err(|error| format!("javascript module specifier is not valid UTF-8: {error}"))?;
    let bytes = text.as_bytes();
    if bytes.len() < 2
        || !matches!(
            (bytes[0], bytes[bytes.len() - 1]),
            (b'\'', b'\'') | (b'"', b'"')
        )
    {
        return Err("javascript module specifier is not a quoted string".to_owned());
    }
    let value = &text[1..text.len() - 1];
    if value.is_empty() || value.contains(['\\', '\0', '\r', '\n']) {
        return Err("javascript module specifier uses an unsupported escape".to_owned());
    }
    Ok(value.to_owned())
}

fn grammar(language: &str) -> Result<&'static GrammarBinding, StructuralScanError> {
    GRAMMARS
        .iter()
        .find(|binding| binding.name == language)
        .ok_or_else(|| StructuralScanError::UnsupportedLanguage {
            observed: language.to_owned(),
            supported: GRAMMARS.iter().map(|binding| binding.name).collect(),
        })
}

fn scan(
    grammar: &GrammarBinding,
    source: &str,
    declarations: &[PatternDeclaration],
    bindings: &[PatternBinding],
) -> Result<Vec<StructuralMatch>, StructuralScanError> {
    let language: Language = grammar.language.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| StructuralScanError::Grammar {
            language: grammar.name,
            reason: error.to_string(),
        })?;
    let tree = parser
        .parse(source, None)
        .ok_or(StructuralScanError::ParseStopped {
            language: grammar.name,
        })?;
    if tree.root_node().has_error() {
        return Err(StructuralScanError::MalformedSource {
            language: grammar.name,
        });
    }

    let mut findings = Vec::new();
    for declaration in declarations {
        scan_pattern(
            grammar,
            bindings,
            &language,
            tree.root_node(),
            source,
            *declaration,
            &mut findings,
        )?;
    }
    findings.sort_by_key(|finding| (finding.start_byte, finding.end_byte, finding.pattern));
    Ok(findings)
}

fn scan_pattern(
    grammar: &GrammarBinding,
    bindings: &[PatternBinding],
    language: &Language,
    root: tree_sitter::Node<'_>,
    source: &str,
    declaration: PatternDeclaration,
    findings: &mut Vec<StructuralMatch>,
) -> Result<(), StructuralScanError> {
    let binding = bindings
        .iter()
        .find(|binding| binding.pattern == declaration.pattern)
        .ok_or_else(|| binding_error(grammar, declaration, "query is missing"))?;
    let query = Query::new(language, binding.query).map_err(|error| {
        binding_error(
            grammar,
            declaration,
            format!("query did not compile: {error}"),
        )
    })?;
    let match_capture = query
        .capture_index_for_name(declaration.match_capture)
        .ok_or_else(|| binding_error(grammar, declaration, "match capture is missing"))?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source.as_bytes());
    while let Some(found) = matches.next() {
        let complete = found
            .captures
            .iter()
            .find(|capture| capture.index == match_capture)
            .ok_or_else(|| {
                binding_error(grammar, declaration, "query omitted its match capture")
            })?;
        let complete_range = complete.node.byte_range();
        let mut captures = found
            .captures
            .iter()
            .filter(|capture| capture.index != match_capture)
            .map(|capture| {
                let range = capture.node.byte_range();
                let text = source.get(range.clone()).ok_or_else(|| {
                    binding_error(
                        grammar,
                        declaration,
                        "capture is not on UTF-8 source boundaries",
                    )
                })?;
                Ok(StructuralCapture {
                    name: query.capture_names()[capture.index as usize].to_owned(),
                    text: text.to_owned(),
                    start_byte: range.start,
                    end_byte: range.end,
                })
            })
            .collect::<Result<Vec<_>, StructuralScanError>>()?;
        captures.sort_by(|left, right| {
            (left.start_byte, left.end_byte, &left.name).cmp(&(
                right.start_byte,
                right.end_byte,
                &right.name,
            ))
        });
        findings.push(StructuralMatch {
            pattern: declaration.pattern,
            start_byte: complete_range.start,
            end_byte: complete_range.end,
            captures,
        });
    }
    Ok(())
}

fn binding_error(
    grammar: &GrammarBinding,
    declaration: PatternDeclaration,
    reason: impl Into<String>,
) -> StructuralScanError {
    StructuralScanError::PatternBinding {
        language: grammar.name,
        pattern: declaration.pattern,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_function_call_pattern_has_the_same_result_shape_in_every_grammar() {
        let fixtures = [
            ("python", "def run():\n    audit(\"fixture\")\n"),
            ("rust", "fn run() { audit(\"fixture\"); }\n"),
            ("javascript", "function run() { audit(\"fixture\"); }\n"),
            (
                "csharp",
                "class Fixture { void Run() { audit(\"fixture\"); } }\n",
            ),
            ("lua", "function run()\n  audit(\"fixture\")\nend\n"),
        ];

        for (language, source) in fixtures {
            let matches = scan_structural(language, source).expect("fixture scans");
            let [matched] = matches.as_slice() else {
                panic!("{language} produced unexpected matches: {matches:?}");
            };
            assert_eq!(matched.pattern(), StructuralPattern::FunctionCall);
            assert_eq!(
                source
                    .get(matched.start_byte()..matched.end_byte())
                    .expect("match uses source boundaries"),
                "audit(\"fixture\")",
            );
            let [callee] = matched.captures() else {
                panic!("{language} produced unexpected captures: {matched:?}");
            };
            assert_eq!(callee.name(), "callee");
            assert_eq!(callee.text(), "audit");
            assert_eq!(
                source
                    .get(callee.start_byte()..callee.end_byte())
                    .expect("capture uses source boundaries"),
                "audit",
            );
        }
    }

    #[test]
    fn javascript_symbol_scan_retains_every_supported_declaration_kind() {
        let source = concat!(
            "class Fixture {}\n",
            "function run() {}\n",
            "function* stream() {}\n",
            "const fixed = 1;\n",
            "let mutable = 2;\n",
            "var legacy = 3;\n",
            "run();\n",
        );
        let matches = scan_javascript_symbols(source).expect("JavaScript symbols scan");
        let declarations: Vec<(&str, &str)> = matches
            .iter()
            .filter(|matched| matched.pattern() == StructuralPattern::JavaScriptDeclaration)
            .map(|matched| {
                let [capture] = matched.captures() else {
                    panic!("declaration has unexpected captures: {matched:?}");
                };
                (capture.name(), capture.text())
            })
            .collect();
        assert_eq!(
            declarations,
            [
                ("class", "Fixture"),
                ("function", "run"),
                ("function", "stream"),
                ("constant", "fixed"),
                ("let", "mutable"),
                ("variable", "legacy"),
            ]
        );
        assert_eq!(
            matches
                .iter()
                .filter(|matched| {
                    matched.pattern() == StructuralPattern::JavaScriptIdentifier
                        && matched
                            .captures()
                            .iter()
                            .any(|capture| capture.text() == "run")
                })
                .count(),
            2,
            "the declaration and call are distinct identifier occurrences"
        );
    }

    /// The module's own declarations, and nobody else's locals.
    ///
    /// A nested symbol still occurs as a reference, so the counts a caller
    /// builds are unaffected; only its candidacy as an unused module symbol is
    /// withdrawn. Object shorthand is one identifier occurrence and a property
    /// access is a property identifier, so both remain observable to a caller
    /// that counts them where they mean a reference.
    #[test]
    fn javascript_symbol_scan_retains_only_top_level_declarations_and_every_reference() {
        let source = concat!(
            "export function outer() {\n",
            "  const nestedLocal = 1;\n",
            "  function nestedHelper() {}\n",
            "  return { nestedLocal, nestedHelper };\n",
            "}\n",
            "const moduleConstant = 2;\n",
            "context.moduleConstant;\n",
        );
        let matches = scan_javascript_symbols(source).expect("JavaScript symbols scan");
        let symbols = |pattern: StructuralPattern| -> Vec<&str> {
            matches
                .iter()
                .filter(|matched| matched.pattern() == pattern)
                .flat_map(|matched| matched.captures())
                .map(StructuralCapture::text)
                .collect()
        };

        assert_eq!(
            symbols(StructuralPattern::JavaScriptDeclaration),
            ["outer", "moduleConstant"],
            "a nested declaration is local to its scope, not the module's to expose"
        );
        assert_eq!(
            symbols(StructuralPattern::JavaScriptIdentifier)
                .into_iter()
                .filter(|text| *text == "nestedLocal")
                .count(),
            2,
            "the nested declaration and its object shorthand are both occurrences"
        );
        assert_eq!(
            symbols(StructuralPattern::JavaScriptPropertyIdentifier),
            ["moduleConstant"],
            "a property access is the only property-identifier occurrence here"
        );
    }

    /// Every declaration form a C# type can carry names itself exactly once.
    ///
    /// Fields and events are the asymmetric pair: they carry no `name` field of
    /// their own and are named by the variable declarator they wrap, so a query
    /// that only reads declaration-level names would silently drop them.
    /// Records are the reverse asymmetry: C# writes one three ways and the
    /// grammar gives all three the same node, so all three are written out
    /// here. Reading the node name alone would leave two of the forms claimed
    /// but never parsed.
    #[test]
    fn csharp_symbol_scan_retains_every_supported_declaration_kind() {
        let source = concat!(
            "class Fixture {\n",
            "  private int field;\n",
            "  public int Property { get; set; }\n",
            "  private void Method() {}\n",
            "  private event Action Fired;\n",
            "}\n",
            "interface IFixture { void Contract(); }\n",
            "struct Point { public int X; }\n",
            "enum Shade { Deep }\n",
            "record Plain { }\n",
            "record class Classy { }\n",
            "record struct Valued { }\n",
        );
        let matches = scan_csharp_symbols(source).expect("C# symbols scan");
        let declarations: Vec<(&str, &str)> = matches
            .iter()
            .filter(|matched| matched.pattern() == StructuralPattern::CSharpDeclaration)
            .map(|matched| {
                let [capture] = matched.captures() else {
                    panic!("declaration has unexpected captures: {matched:?}");
                };
                (capture.name(), capture.text())
            })
            .collect();
        assert_eq!(
            declarations,
            [
                ("class", "Fixture"),
                ("field", "field"),
                ("property", "Property"),
                ("method", "Method"),
                ("event", "Fired"),
                ("interface", "IFixture"),
                ("method", "Contract"),
                ("struct", "Point"),
                ("field", "X"),
                ("enum", "Shade"),
                ("record", "Plain"),
                ("record", "Classy"),
                ("record", "Valued"),
            ],
            "every named declaration is retained once, in source order, with its kind",
        );
    }

    /// A declaration's own name is an identifier occurrence, so an unused
    /// symbol counts one and a referenced one counts more.
    #[test]
    fn csharp_symbol_scan_counts_a_declaration_name_as_its_own_occurrence() {
        let source = concat!(
            "class Fixture {\n",
            "  private int unusedField;\n",
            "  private int usedField;\n",
            "  public int Read() { return usedField; }\n",
            "}\n",
        );
        let matches = scan_csharp_symbols(source).expect("C# symbols scan");
        let occurrences = |symbol: &str| -> usize {
            matches
                .iter()
                .filter(|matched| {
                    matched.pattern() == StructuralPattern::CSharpIdentifier
                        && matched
                            .captures()
                            .iter()
                            .any(|capture| capture.text() == symbol)
                })
                .count()
        };
        assert_eq!(occurrences("unusedField"), 1, "only its own declaration");
        assert_eq!(occurrences("usedField"), 2, "its declaration and one read");
    }

    #[test]
    fn every_grammar_binds_every_pattern_once() {
        for grammar in GRAMMARS {
            assert_eq!(grammar.patterns.len(), PATTERNS.len(), "{}", grammar.name);
            for declaration in PATTERNS {
                assert_eq!(
                    grammar
                        .patterns
                        .iter()
                        .filter(|binding| binding.pattern == declaration.pattern)
                        .count(),
                    1,
                    "{} {}",
                    grammar.name,
                    declaration.pattern.as_str(),
                );
            }
        }
    }

    #[test]
    fn unsupported_language_is_a_typed_refusal_not_an_empty_scan() {
        let error =
            scan_structural("go", "audit(\"fixture\")").expect_err("unsupported grammar refuses");
        assert_eq!(
            error,
            StructuralScanError::UnsupportedLanguage {
                observed: "go".to_owned(),
                supported: vec!["python", "rust", "javascript", "csharp", "lua"],
            }
        );
        assert_eq!(
            error.to_string(),
            concat!(
                "unsupported structural-scan language \"go\"; choose ",
                "python, rust, javascript, csharp, lua",
            )
        );
    }
}

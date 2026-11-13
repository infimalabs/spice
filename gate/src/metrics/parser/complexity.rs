use std::collections::BTreeSet;
use std::path::Path;

use tree_sitter::{Language, Node, Parser};
use tree_sitter_language::LanguageFn;

use super::{ParseError, canonical_path};

/// One named routine and the two quantities the complexity arm measures on it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ComplexityRoutine {
    path: String,
    function: String,
    ccn: u64,
    length: u64,
}

impl ComplexityRoutine {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The routine's name qualified by every named scope enclosing it.
    ///
    /// Qualified rather than bare because the bare name is not unique in any of
    /// these languages: two Rust impl blocks each declaring `new`, or two
    /// classes each declaring `run`, are ordinary. A metric keyed on
    /// path-and-function would collide on the first such file and refuse it.
    #[must_use]
    pub fn function(&self) -> &str {
        &self.function
    }

    /// Cyclomatic complexity: one, plus every decision written inside.
    #[must_use]
    pub const fn ccn(&self) -> u64 {
        self.ccn
    }

    /// Lines the routine spans, first through last inclusive.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }
}

/// One grammar's answers to the three questions this measurement asks: what is a
/// routine, what names a routine's scope, and what is a decision.
///
/// Every entry is a node kind taken from the vendored grammar itself rather than
/// a token spelling, so a construct written in an unusual but legal way is
/// counted the same as the ordinary form.
struct ComplexityGrammar {
    suffixes: &'static [&'static str],
    language: LanguageFn,
    /// Node kinds that introduce one routine bearing a declared name.
    routines: &'static [&'static str],
    /// Node kinds that name a scope the routines inside them are qualified by.
    scopes: &'static [&'static str],
    /// Node kinds that each add exactly one decision.
    decisions: &'static [&'static str],
    /// Node kinds carrying an infix operator, tested against `short_circuit`.
    infix: &'static [&'static str],
    /// Operator spellings that add one decision because they short-circuit.
    short_circuit: &'static [&'static str],
    /// Whether target `cfg` attributes select which declarations exist.
    rust_cfg: bool,
}

/// Every alternative of a multi-way branch is counted, including the default
/// one, because a written alternative is a path a reader has to follow whether
/// or not the grammar gives it a distinct node kind. This is also the only rule
/// that reads the same in all five grammars: Rust spells its alternatives
/// `match_arm`, JavaScript `switch_case` beside `switch_default`, and C# groups
/// them into `switch_section`, and no uniform statement about defaults survives
/// all three.
const GRAMMARS: &[ComplexityGrammar] = &[
    ComplexityGrammar {
        suffixes: &[".rs"],
        language: tree_sitter_rust::LANGUAGE,
        routines: &["function_item"],
        scopes: &["impl_item", "trait_item", "mod_item"],
        decisions: &[
            "if_expression",
            "while_expression",
            "for_expression",
            "match_arm",
            "try_expression",
        ],
        infix: &["binary_expression"],
        short_circuit: &["&&", "||"],
        rust_cfg: true,
    },
    ComplexityGrammar {
        suffixes: &[".py"],
        language: tree_sitter_python::LANGUAGE,
        routines: &["function_definition"],
        scopes: &["class_definition"],
        decisions: &[
            "if_statement",
            "elif_clause",
            "while_statement",
            "for_statement",
            "except_clause",
            "case_clause",
            "boolean_operator",
            "conditional_expression",
            "if_clause",
        ],
        infix: &[],
        short_circuit: &[],
        rust_cfg: false,
    },
    ComplexityGrammar {
        suffixes: &[".js", ".mjs"],
        language: tree_sitter_javascript::LANGUAGE,
        routines: &["function_declaration", "method_definition"],
        scopes: &["class_declaration"],
        decisions: &[
            "if_statement",
            "while_statement",
            "do_statement",
            "for_statement",
            "for_in_statement",
            "switch_case",
            "switch_default",
            "catch_clause",
            "ternary_expression",
        ],
        infix: &["binary_expression"],
        short_circuit: &["&&", "||", "??"],
        rust_cfg: false,
    },
    ComplexityGrammar {
        suffixes: &[".cs"],
        language: tree_sitter_c_sharp::LANGUAGE,
        routines: &["method_declaration", "local_function_statement"],
        scopes: &[
            "namespace_declaration",
            "class_declaration",
            "struct_declaration",
            "interface_declaration",
            "record_declaration",
        ],
        decisions: &[
            "if_statement",
            "while_statement",
            "do_statement",
            "for_statement",
            "foreach_statement",
            "switch_section",
            "catch_clause",
            "conditional_expression",
        ],
        infix: &["binary_expression"],
        short_circuit: &["&&", "||", "??"],
        rust_cfg: false,
    },
    ComplexityGrammar {
        suffixes: &[".lua"],
        language: tree_sitter_lua::LANGUAGE,
        routines: &["function_declaration"],
        scopes: &[],
        decisions: &[
            "if_statement",
            "elseif_statement",
            "while_statement",
            "for_statement",
            "repeat_statement",
        ],
        infix: &["binary_expression"],
        short_circuit: &["and", "or"],
        rust_cfg: false,
    },
];

/// Measure every named routine one selected source file declares.
///
/// # Errors
///
/// Refuses a source class with no vendored grammar as
/// [`ParseError::Unsupported`], so a language the policy selects and this build
/// cannot parse is named rather than measured as having no routines at all.
/// Refuses source the grammar cannot parse, and refuses two active routines
/// that qualify to one identity, since the arm keys its measurements on that
/// pair. Rust declarations under a target predicate proven false for this
/// compiled analyzer do not exist in its execution path and are not measured;
/// predicates the analyzer cannot settle remain visible and can still collide.
pub fn measure_complexity(path: &str, source: &str) -> Result<Vec<ComplexityRoutine>, ParseError> {
    let path = canonical_path(path)?;
    let grammar = grammar_for(&path)?;
    let mut parser = Parser::new();
    parser
        .set_language(&Language::new(grammar.language))
        .map_err(|error| {
            ParseError::malformed(
                format!("complexity grammar for {path:?}"),
                error.to_string(),
            )
        })?;
    let tree = parser.parse(source, None).ok_or_else(|| {
        ParseError::malformed(
            format!("complexity source {path:?}"),
            "grammar produced no tree",
        )
    })?;
    if tree.root_node().has_error() {
        return Err(ParseError::malformed(
            format!("complexity source {path:?}"),
            "grammar produced an error node",
        ));
    }
    let mut routines = Vec::new();
    let mut scope = Vec::new();
    visit(
        tree.root_node(),
        source,
        grammar,
        &path,
        &mut scope,
        None,
        &mut routines,
    );
    let mut seen = BTreeSet::new();
    for routine in &routines {
        if !seen.insert(routine.function.clone()) {
            return Err(ParseError::collision(
                format!("{}:{}", routine.path, routine.function),
                "two routines in this file qualify to one identity",
            ));
        }
    }
    routines.sort();
    Ok(routines)
}

/// The vendored grammar for one selected path's source class.
fn grammar_for(path: &str) -> Result<&'static ComplexityGrammar, ParseError> {
    let suffix = Path::new(path)
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default();
    GRAMMARS
        .iter()
        .find(|grammar| grammar.suffixes.contains(&suffix.as_str()))
        .ok_or_else(|| {
            ParseError::unsupported(
                "policy.languages.complexity",
                format!("source class {suffix:?} has no vendored tree-sitter grammar"),
                "vendor a grammar for this source class or remove it from complexity scope",
            )
        })
}

/// Walk one subtree, attributing each decision to the routine that encloses it.
///
/// `current` is the innermost routine seen on the way down, so a decision inside
/// a nested routine counts for the nested one alone. A decision written outside
/// every routine — a module-level branch — is counted by nothing, because the
/// arm measures routines and there is none to hold it.
fn visit(
    node: Node<'_>,
    source: &str,
    grammar: &ComplexityGrammar,
    path: &str,
    scope: &mut Vec<String>,
    current: Option<usize>,
    routines: &mut Vec<ComplexityRoutine>,
) {
    let mut current = current;
    let mut named = false;
    if grammar.routines.contains(&node.kind()) {
        if let Some(name) = declared_name(node, source) {
            scope.push(name);
            named = true;
            routines.push(ComplexityRoutine {
                path: path.to_owned(),
                function: scope.join("::"),
                ccn: 1,
                length: span_lines(node),
            });
            current = Some(routines.len() - 1);
        }
    } else if grammar.scopes.contains(&node.kind()) {
        if let Some(name) = declared_scope_name(node, source) {
            scope.push(name);
            named = true;
        }
    } else if is_decision(node, grammar)
        && let Some(index) = current
    {
        routines[index].ccn += 1;
    }
    let mut predicate = Predicate::True;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if grammar.rust_cfg && child.kind() == "attribute_item" {
            if let Some(observed) = cfg_attribute(child, source) {
                predicate = predicate.and(observed);
            }
            continue;
        }
        if predicate != Predicate::False {
            visit(child, source, grammar, path, scope, current, routines);
        }
        predicate = Predicate::True;
    }
    if named {
        scope.pop();
    }
}

/// Three-valued result for one Rust configuration predicate.
///
/// Unknown is deliberately retained rather than guessed: only a predicate the
/// compiled target can prove false is allowed to remove a declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Predicate {
    True,
    False,
    Unknown,
}

impl Predicate {
    const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }

    const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }

    const fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }

    const fn known(value: bool) -> Self {
        if value { Self::True } else { Self::False }
    }
}

/// Target truth carried by one `#[cfg(...)]` attribute.
fn cfg_attribute(node: Node<'_>, source: &str) -> Option<Predicate> {
    let text = node.utf8_text(source.as_bytes()).ok()?.trim();
    let attribute = text.strip_prefix("#[")?.strip_suffix(']')?.trim();
    let body = attribute
        .strip_prefix("cfg")?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    let mut parser = CfgParser::new(body);
    let result = parser.expression()?;
    parser.finish().then_some(result)
}

/// Minimal parser for Rust's documented `cfg` predicate grammar.
struct CfgParser<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> CfgParser<'a> {
    const fn new(text: &'a str) -> Self {
        Self { text, offset: 0 }
    }

    fn expression(&mut self) -> Option<Predicate> {
        let name = self.identifier()?;
        if self.take('=') {
            return self.string().map(|value| target_value(name, value));
        }
        if !self.take('(') {
            return Some(target_flag(name));
        }
        let values = self.arguments()?;
        match name {
            "all" => Some(values.into_iter().fold(Predicate::True, Predicate::and)),
            "any" => Some(values.into_iter().fold(Predicate::False, Predicate::or)),
            "not" if values.len() == 1 => Some(values[0].not()),
            _ => Some(Predicate::Unknown),
        }
    }

    fn arguments(&mut self) -> Option<Vec<Predicate>> {
        let mut values = Vec::new();
        if self.take(')') {
            return Some(values);
        }
        loop {
            values.push(self.expression()?);
            if self.take(')') {
                return Some(values);
            }
            if !self.take(',') {
                return None;
            }
        }
    }

    fn identifier(&mut self) -> Option<&'a str> {
        self.space();
        let start = self.offset;
        while self
            .text
            .as_bytes()
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.offset += 1;
        }
        (self.offset > start).then_some(&self.text[start..self.offset])
    }

    fn string(&mut self) -> Option<&'a str> {
        self.space();
        let rest = self.text.get(self.offset..)?;
        let body = rest.strip_prefix('"')?;
        let end = body.find('"')?;
        self.offset += end + 2;
        Some(&body[..end])
    }

    fn take(&mut self, expected: char) -> bool {
        self.space();
        if self
            .text
            .get(self.offset..)
            .is_some_and(|rest| rest.starts_with(expected))
        {
            self.offset += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn finish(&mut self) -> bool {
        self.space();
        self.offset == self.text.len()
    }

    fn space(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }
}

/// Bare target predicates this compiled analyzer can settle.
fn target_flag(name: &str) -> Predicate {
    match name {
        "unix" => Predicate::known(cfg!(unix)),
        "windows" => Predicate::known(cfg!(windows)),
        "debug_assertions" => Predicate::known(cfg!(debug_assertions)),
        // Test and feature selection belong to the analyzed source, not to the
        // build that happens to host this parser, so neither is guessed.
        "test" => Predicate::Unknown,
        _ => Predicate::Unknown,
    }
}

/// Key/value target predicates this compiled analyzer can settle.
fn target_value(name: &str, value: &str) -> Predicate {
    match name {
        "target_arch" => Predicate::known(value == std::env::consts::ARCH),
        "target_os" => Predicate::known(value == std::env::consts::OS),
        "target_family" => Predicate::known(value == std::env::consts::FAMILY),
        "target_endian" => Predicate::known(
            value
                == if cfg!(target_endian = "little") {
                    "little"
                } else {
                    "big"
                },
        ),
        "target_pointer_width" => Predicate::known(value == usize::BITS.to_string()),
        "target_vendor" => target_vendor(value),
        "target_env" => target_environment(value),
        _ => Predicate::Unknown,
    }
}

fn target_vendor(value: &str) -> Predicate {
    match value {
        "apple" => Predicate::known(cfg!(target_vendor = "apple")),
        "pc" => Predicate::known(cfg!(target_vendor = "pc")),
        "unknown" => Predicate::known(cfg!(target_vendor = "unknown")),
        _ => Predicate::Unknown,
    }
}

fn target_environment(value: &str) -> Predicate {
    match value {
        "" => Predicate::known(cfg!(target_env = "")),
        "gnu" => Predicate::known(cfg!(target_env = "gnu")),
        "musl" => Predicate::known(cfg!(target_env = "musl")),
        "msvc" => Predicate::known(cfg!(target_env = "msvc")),
        _ => Predicate::Unknown,
    }
}

/// Whether one node adds a decision to the routine holding it.
fn is_decision(node: Node<'_>, grammar: &ComplexityGrammar) -> bool {
    if grammar.decisions.contains(&node.kind()) {
        return true;
    }
    if !grammar.infix.contains(&node.kind()) {
        return false;
    }
    // An operator token's node kind is its own spelling, so the operator is read
    // from the tree rather than from the source text a field name would locate.
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| grammar.short_circuit.contains(&child.kind()))
}

/// The name a declaration states, from whichever field this grammar puts it in.
///
/// Rust names an impl block by its `type` rather than a `name`, and an
/// unnameable declaration — an anonymous function, an impl of a type this walk
/// cannot spell — yields nothing and is not measured, because there would be no
/// identity to key its measurement on.
fn declared_name(node: Node<'_>, source: &str) -> Option<String> {
    ["name", "type"]
        .into_iter()
        .find_map(|field| declared_field(node, source, field))
}

/// The qualifier contributed by one named scope.
///
/// Rust trait impls need both sides of the impl in their qualifier. Using only
/// the implementor would collapse ordinary `impl First for Worker` and
/// `impl Second for Worker` methods onto the same path/function key. The
/// conventional `<Type as Trait>` spelling keeps those identities distinct;
/// every other supported scope is named by its ordinary `name` or `type` field.
fn declared_scope_name(node: Node<'_>, source: &str) -> Option<String> {
    let name = declared_name(node, source)?;
    if node.kind() != "impl_item" {
        return Some(name);
    }
    let Some(trait_name) = declared_field(node, source, "trait") else {
        return Some(name);
    };
    Some(format!("<{name} as {trait_name}>"))
}

/// Non-empty, single-line source text carried by one declared field.
fn declared_field(node: Node<'_>, source: &str, field: &str) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|child| child.utf8_text(source.as_bytes()).ok())
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.contains(['\r', '\n']))
        .map(str::to_owned)
}

/// Lines one node spans, first through last inclusive.
fn span_lines(node: Node<'_>) -> u64 {
    let start = node.start_position().row;
    let end = node.end_position().row;
    (end.saturating_sub(start) as u64) + 1
}

//! What the C# unused-member arm may accuse, and what it withdraws unnamed.
//!
//! The walk is short and the exceptions are not. Which members are even
//! candidates, which silence about access really means private, and which
//! absences must be withdrawn rather than reported are each a rule about C#
//! itself, and together they outgrew the file every arm shares. The vocabulary
//! the arms have in common stays in [`super`]; only this reasoning lives here.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::{Analysis, extension, plural, source_line};
use crate::structural::{StructuralPattern, scan_csharp_symbols};
use crate::toolchain::{
    ArmFinding, ArmOutcome, CsharpUnusedStatus, FindingDetail, ToolchainDeclaration,
};

/// One C# declaration retained with the span the scanner proved it occupies.
#[derive(Debug)]
struct CsharpDeclaration {
    kind: String,
    symbol: String,
    line: u32,
    start_byte: usize,
    end_byte: usize,
}

/// The declaration kinds a type can enclose, and which can therefore go unused.
const CSHARP_MEMBER_KINDS: &[&str] = &["method", "property", "field", "event"];
/// The declaration kinds that open a type context around their members.
///
/// `record` is one kind and three spellings: C# writes `record`, `record class`,
/// and `record struct`, and the grammar gives all three one node. A kind missing
/// here would not be a type that encloses nothing; it would be a type nothing is
/// found inside, so its members answer to no declaration and every rule read off
/// the enclosing type stops applying to them without saying so.
const CSHARP_TYPE_KINDS: &[&str] = &["class", "interface", "struct", "record"];
/// Access modifiers that publish a member beyond its declaring type.
const CSHARP_PUBLISHING_MODIFIERS: &[&str] = &["public", "protected", "internal"];

/// Report C# members that no name in their own file reaches.
///
/// Only a type's own members are candidates, and only where the declaring file
/// is the whole audience: a member reachable from outside its type is reached by
/// a name this arm cannot see, so publishing modifiers withdraw candidacy rather
/// than producing a finding this gate cannot stand behind. A member's own
/// declaration is one identifier occurrence, so exactly one occurrence means no
/// reference remains.
///
/// Absence is reported as a candidate, never as a deletion, and only where the
/// arm will stand behind the accusation. A C# member can be reached by a
/// framework through an attribute, and a partial type states outright that the
/// rest of its declaration is in another file, so both withdraw the candidacy
/// instead of qualifying it. The reason keeps the count, because an arm carries
/// findings only under a failing outcome and a retained member must not fail.
pub(in crate::toolchain) fn csharp_unused(
    declaration: ToolchainDeclaration,
    root: &Path,
    reached_paths: &[String],
) -> Analysis {
    let sources = match csharp_sources(root, reached_paths) {
        Ok(sources) => sources,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    if sources.is_empty() {
        return Analysis::not_applicable(declaration);
    }

    let mut findings = Vec::new();
    let mut retained: BTreeMap<&str, usize> = BTreeMap::new();
    for (path, source) in &sources {
        let (declarations, references) = match csharp_inventory(source) {
            Ok(inventory) => inventory,
            Err(reason) => {
                return Analysis::terminal(
                    ArmOutcome::ParseFailure,
                    format!("csharp-unused-candidates could not parse {path}: {reason}"),
                );
            }
        };
        for declared in &declarations {
            if !CSHARP_MEMBER_KINDS.contains(&declared.kind.as_str()) {
                continue;
            }
            let enclosing = csharp_enclosing_type(&declarations, declared);
            if !csharp_is_private(source, declared, enclosing) {
                continue;
            }
            if references
                .get(&declared.symbol)
                .copied()
                .unwrap_or_default()
                != 1
            {
                continue;
            }
            if let Some(reason) = csharp_retention(source, declared, enclosing) {
                *retained.entry(reason).or_default() += 1;
                continue;
            }
            findings.push(ArmFinding {
                path: Some(path.clone()),
                line: Some(declared.line),
                column: None,
                detail: FindingDetail::CsharpUnused {
                    symbol: declared.symbol.clone(),
                    declaration_kind: declared.kind.clone(),
                    status: CsharpUnusedStatus::CandidateUnused,
                    reason: "no_references_outside_declaration".to_owned(),
                    reference_count: 1,
                },
            });
        }
    }
    let mut analysis = Analysis::scanned(declaration.id, sources.len(), findings);
    analysis.reason.push_str(&csharp_retention_note(&retained));
    analysis
}

/// Separate the declarations from the reference tally in one parse.
fn csharp_inventory(
    source: &str,
) -> Result<(Vec<CsharpDeclaration>, BTreeMap<String, usize>), String> {
    let matches = scan_csharp_symbols(source).map_err(|error| error.to_string())?;
    let mut declarations = Vec::new();
    let mut references: BTreeMap<String, usize> = BTreeMap::new();
    for matched in matches {
        let [capture] = matched.captures() else {
            return Err(format!(
                "a malformed {} match was received",
                matched.pattern().as_str()
            ));
        };
        match matched.pattern() {
            StructuralPattern::CSharpDeclaration => {
                let line = source_line(source, matched.start_byte())
                    .ok_or_else(|| "a declaration line overflowed".to_owned())?;
                declarations.push(CsharpDeclaration {
                    kind: capture.name().to_owned(),
                    symbol: capture.text().to_owned(),
                    line,
                    start_byte: matched.start_byte(),
                    end_byte: matched.end_byte(),
                });
            }
            StructuralPattern::CSharpIdentifier => {
                *references.entry(capture.text().to_owned()).or_default() += 1;
            }
            StructuralPattern::FunctionCall
            | StructuralPattern::JavaScriptDeclaration
            | StructuralPattern::JavaScriptIdentifier
            | StructuralPattern::JavaScriptPropertyIdentifier => {
                return Err("an unrelated structural match was received".to_owned());
            }
        }
    }
    Ok((declarations, references))
}

/// The innermost type declaration whose span encloses this member.
///
/// Containment is read from the spans the scanner already proved, so no second
/// traversal can disagree with the parse that produced them.
fn csharp_enclosing_type<'a>(
    declarations: &'a [CsharpDeclaration],
    member: &CsharpDeclaration,
) -> Option<&'a CsharpDeclaration> {
    declarations
        .iter()
        .filter(|candidate| CSHARP_TYPE_KINDS.contains(&candidate.kind.as_str()))
        .filter(|candidate| {
            candidate.start_byte <= member.start_byte && member.end_byte <= candidate.end_byte
        })
        .min_by_key(|candidate| candidate.end_byte - candidate.start_byte)
}

/// The modifier and attribute prefix a C# declaration states before its type.
///
/// Attributes and modifiers precede the declared type and name, so the text
/// before the first type, parameter, body, or initializer token is exactly the
/// prefix. The span is the one the scanner proved, so this reads inside a single
/// declaration and never spills into its neighbours.
fn csharp_prefix<'a>(source: &'a str, declared: &CsharpDeclaration) -> &'a str {
    let text = source
        .get(declared.start_byte..declared.end_byte)
        .unwrap_or_default();
    let end = text.find(['{', '(', '=', ';', ':']).unwrap_or(text.len());
    text.get(..end).unwrap_or_default()
}

/// Whether a member is the declaring type's alone to spend.
///
/// C# defaults type members to private, so a member is private when it says so
/// and also when it says nothing. Two forms say nothing and are still
/// published. An interface member is published by the interface itself. An
/// explicit interface implementation is published the same way, and C# forbids
/// it from carrying any access modifier to say so, which is why silence cannot
/// be read as privacy here: it names the interface inside its own declared
/// name instead. That qualified name is the whole signal, and it is the last
/// word of the prefix because the name is what the type, body, or initializer
/// follows. A qualified return type does not reach that position.
fn csharp_is_private(
    source: &str,
    member: &CsharpDeclaration,
    enclosing: Option<&CsharpDeclaration>,
) -> bool {
    let prefix = csharp_prefix(source, member);
    let words: Vec<&str> = prefix.split_whitespace().collect();
    if words.last().is_some_and(|name| name.contains('.')) {
        return false;
    }
    if words.contains(&"private") {
        return true;
    }
    if enclosing.is_some_and(|found| found.kind == "interface") {
        return false;
    }
    !words
        .iter()
        .any(|word| CSHARP_PUBLISHING_MODIFIERS.contains(word))
}

/// The named rule that holds an unreached member live, if one does.
///
/// Retention withdraws the candidacy rather than qualifying it, because an arm
/// carries findings only under [`ArmOutcome::Findings`] and that outcome fails
/// the battery. A row saying a member is retained would therefore fail the gate
/// on the very evidence that the member must be kept.
fn csharp_retention(
    source: &str,
    member: &CsharpDeclaration,
    enclosing: Option<&CsharpDeclaration>,
) -> Option<&'static str> {
    if csharp_prefix(source, member).trim_start().starts_with('[') {
        return Some("attribute_retained");
    }
    let enclosing_is_partial = enclosing.is_some_and(|found| {
        csharp_prefix(source, found)
            .split_whitespace()
            .any(|word| word == "partial")
    });
    enclosing_is_partial.then_some("partial_declaration")
}

/// What the arm examined and chose not to accuse.
///
/// A retained member leaves no finding, so without this sentence a tree where
/// every unreached member is held live reads exactly like a tree where nothing
/// went unreached, and the rule that did the holding leaves no trace.
fn csharp_retention_note(retained: &BTreeMap<&str, usize>) -> String {
    if retained.is_empty() {
        return String::new();
    }
    let named: Vec<String> = retained
        .iter()
        .map(|(reason, count)| format!("{reason} {count}"))
        .collect();
    format!(
        "; {} retained ({})",
        plural(retained.values().sum(), "unreached member"),
        named.join(", "),
    )
}

fn csharp_sources(root: &Path, reached_paths: &[String]) -> Result<Vec<(String, String)>, String> {
    let paths = super::selected_changes(root, reached_paths, |path| {
        extension(path).is_some_and(|found| found == "cs")
    });

    paths
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(root.join(&path)).map_err(|error| {
                format!("csharp-unused-candidates could not read {path} as UTF-8: {error}")
            })?;
            Ok((path, source))
        })
        .collect()
}

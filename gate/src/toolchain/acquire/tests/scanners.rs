use super::{Fixture, admitted, column};
use crate::GateId;
use crate::toolchain::{
    ArmFinding, ArmOutcome, CsharpUnusedStatus, FindingDetail, JavascriptUnusedStatus,
    LOCAL_PATH_MARKER, PACKAGED_JAVASCRIPT_EXEMPTIONS, PACKAGED_TASTE_WORDS,
};

const SOURCE: &str = "src/main.rs";
const PROSE: &str = "docs/guide.md";

/// The first discouraged word and its replacement, read from the vocabulary.
///
/// Read rather than spelled so the fixture cannot drift from the compiled
/// vocabulary, and so this file never has to contain a discouraged word.
fn discouraged_word() -> (String, String) {
    let (pattern, replacement) = PACKAGED_TASTE_WORDS
        .first()
        .expect("the compiled vocabulary is not empty");
    (
        pattern.trim_end_matches('*').to_owned(),
        (*replacement).to_owned(),
    )
}

fn csharp_unused(path: &str, symbol: &str, line: u32) -> ArmFinding {
    ArmFinding {
        path: Some(path.to_owned()),
        line: Some(line),
        column: None,
        detail: FindingDetail::CsharpUnused {
            symbol: symbol.to_owned(),
            declaration_kind: "method".to_owned(),
            status: CsharpUnusedStatus::CandidateUnused,
            reason: "no_references_outside_declaration".to_owned(),
            reference_count: 1,
        },
    }
}

fn javascript_unused(
    path: &str,
    line: u32,
    symbol: &str,
    declaration_kind: &str,
    test_reference_count: u32,
) -> ArmFinding {
    let (status, reason) = if test_reference_count == 0 {
        (
            JavascriptUnusedStatus::CandidateUnused,
            "no_references_outside_declaration",
        )
    } else {
        (JavascriptUnusedStatus::TestOnly, "references_only_in_tests")
    };
    ArmFinding {
        path: Some(path.to_owned()),
        line: Some(line),
        column: None,
        detail: FindingDetail::JavascriptUnused {
            symbol: symbol.to_owned(),
            declaration_kind: declaration_kind.to_owned(),
            status,
            reason: reason.to_owned(),
            reference_count: 1,
            test_reference_count,
        },
    }
}

#[test]
fn the_compiled_scanners_pass_over_a_clean_tree() {
    let mut fixture = Fixture::new("gate-toolchain-clean");
    fixture.write(SOURCE, "fn main() {}\n");
    fixture.write(PROSE, "Every sentence here is ordinary.\n");
    fixture.write("web/app.js", "void 0;\n");
    let source = fixture.source(&[SOURCE, PROSE]);

    for id in [GateId::LocalPaths, GateId::Taste, GateId::JavascriptUnused] {
        let measurement = admitted(&source, id, &[]);
        assert_eq!(
            measurement.outcome,
            ArmOutcome::Passed,
            "{} refused a clean tree: {}",
            id.as_str(),
            measurement.reason
        );
        assert!(measurement.findings.is_empty());
    }
}

/// The fixture repeats one discouraged word and one path marker on one line.
///
/// Each pair carries the same detail on the same line of the same file, so the
/// column is the only field that tells the two apart. Admission refuses findings
/// that agree in every field, which makes these the cases that prove a second
/// occurrence survives as its own place to fix rather than collapsing into the
/// first.
#[test]
fn the_compiled_scanners_report_each_occurrence_at_its_own_column() {
    let (word, replacement) = discouraged_word();
    let prose = format!("This paragraph is {word} and {word} again.\n");
    let marked =
        format!("// {LOCAL_PATH_MARKER}somebody/devel and {LOCAL_PATH_MARKER}somebody/scratch\n");
    let mut fixture = Fixture::new("gate-toolchain-dirty");
    fixture.write(SOURCE, &format!("{marked}fn main() {{}}\n"));
    fixture.write(PROSE, &prose);
    let source = fixture.source(&[SOURCE, PROSE]);

    let local_path = |at| ArmFinding {
        path: Some(SOURCE.to_owned()),
        line: Some(1),
        column: Some(at),
        detail: FindingDetail::LocalPath,
    };
    let measurement = admitted(&source, GateId::LocalPaths, &[]);
    assert_eq!(measurement.outcome, ArmOutcome::Findings);
    assert_eq!(
        measurement.findings,
        [
            local_path(column(&marked, LOCAL_PATH_MARKER)),
            local_path(column(
                &marked,
                &format!("{LOCAL_PATH_MARKER}somebody/scratch")
            )),
        ]
    );

    let discouraged = |at| ArmFinding {
        path: Some(PROSE.to_owned()),
        line: Some(1),
        column: Some(at),
        detail: FindingDetail::Taste {
            word: word.clone(),
            replacement: replacement.clone(),
        },
    };
    let measurement = admitted(&source, GateId::Taste, &[]);
    assert_eq!(measurement.outcome, ArmOutcome::Findings);
    assert_eq!(
        measurement.findings,
        [
            discouraged(column(&prose, &word)),
            discouraged(column(&prose, &format!("{word} again"))),
        ]
    );
}

#[test]
fn javascript_unused_reports_candidate_and_test_only_but_suppresses_the_exact_exemption() {
    let &(exempt_path, exempt_symbol, _) = PACKAGED_JAVASCRIPT_EXEMPTIONS
        .first()
        .expect("the packaged exemption fixture exists");
    let mut fixture = Fixture::new("gate-javascript-unused");
    fixture.write(
        "web/exports.mjs",
        &format!(
            concat!(
                "export function unusedExport() {{}}\n",
                "export const testOnlyExport = 1;\n",
                "export function {}() {{}}\n",
            ),
            exempt_symbol,
        ),
    );
    fixture.write(
        "tests/exports.spec.cjs",
        concat!(
            "import { testOnlyExport } from \"../web/exports.mjs\";\n",
            "console.log(testOnlyExport);\n",
        ),
    );
    fixture.write(
        exempt_path,
        &format!("export function {exempt_symbol}() {{ return true; }}\n"),
    );

    let measurement = admitted(&fixture.source(&[]), GateId::JavascriptUnused, &[]);
    assert_eq!(measurement.outcome, ArmOutcome::Findings);
    assert_eq!(
        measurement.findings,
        [
            javascript_unused("web/exports.mjs", 1, "unusedExport", "function", 0),
            javascript_unused("web/exports.mjs", 2, "testOnlyExport", "constant", 2),
            javascript_unused("web/exports.mjs", 3, exempt_symbol, "function", 0),
        ]
    );
}

/// What a module owns, and what counts as reaching it.
///
/// The nested locals below are declared and never read, so an inventory that
/// admitted nested declarations would report both as unused. `shorthandUse` is
/// reached only through object shorthand and `sandboxSymbol` only through a
/// test harness namespace, so a scanner missing either vocabulary would report
/// that symbol as unused too. The production property access on the last line
/// names `bundle` without reaching it, because production code reaches a symbol
/// by name; counting it there would rescue a symbol nobody uses. The exact list
/// below is what survives all four.
#[test]
fn javascript_unused_scopes_candidates_to_top_level_and_counts_every_reference_form() {
    let mut fixture = Fixture::new("gate-javascript-scope");
    fixture.write(
        "web/regression.mjs",
        concat!(
            "export function outer() {\n",
            "  const nestedLocal = 1;\n",
            "  function nestedHelper() {}\n",
            "  return 0;\n",
            "}\n",
            "const shorthandUse = 2;\n",
            "export const bundle = { shorthandUse };\n",
            "export function sandboxSymbol() {}\n",
            "outer();\n",
            "globalThis.bundle;\n",
        ),
    );
    fixture.write("tests/regression.spec.cjs", "context.sandboxSymbol();\n");

    let measurement = admitted(&fixture.source(&[]), GateId::JavascriptUnused, &[]);
    assert_eq!(measurement.outcome, ArmOutcome::Findings);
    assert_eq!(
        measurement.findings,
        [
            javascript_unused("web/regression.mjs", 7, "bundle", "constant", 0),
            javascript_unused("web/regression.mjs", 8, "sandboxSymbol", "function", 1),
        ]
    );
}

/// Four members go unreached here and only the three private ones are named.
///
/// The fourth is an interface member, which C# publishes through the interface
/// itself and which therefore carries no access modifier to say so. A member
/// that says nothing is otherwise private, so without the interface exception
/// the arm would claim a member the whole world may call.
#[test]
fn csharp_unused_reports_private_candidates_and_separates_what_policy_retains() {
    let mut fixture = Fixture::new("gate-csharp-unused");
    fixture.write(
        "dotnet/Surface.cs",
        concat!(
            "public partial class Retained\n{\n",
            "    private void RetainedByPartial() { }\n}\n\n",
            "public class Surface\n{\n",
            "    [Obsolete]\n    private void AttributeRetained() { }\n\n",
            "    private void Loose() { }\n\n",
            "    private void Reached() { }\n\n",
            "    public void Entry()\n    {\n        Reached();\n    }\n}\n\n",
            "public interface IContract\n{\n    void Unreached();\n}\n",
        ),
    );

    let measurement = admitted(&fixture.source(&[]), GateId::CsharpUnusedCandidates, &[]);
    assert_eq!(measurement.outcome, ArmOutcome::Findings);
    assert_eq!(
        measurement.findings,
        [csharp_unused("dotnet/Surface.cs", "Loose", 11)]
    );
    assert!(
        measurement.reason.ends_with(
            "; 2 unreached members retained (attribute_retained 1, partial_declaration 1)"
        ),
        "the retained members left no trace: {}",
        measurement.reason
    );
}

/// An explicitly implemented interface member is not the class's to delete.
///
/// C# forbids an access modifier on an explicit implementation, so the silence
/// that otherwise means private means published here. The interface is declared
/// in a second file on purpose: references are counted per file, so the member's
/// own name is the only occurrence in the file that declares it, and an arm
/// reading silence as privacy accuses live contract code whose deletion would
/// not compile. `Loose` shares the file so the tree still proves the arm runs
/// and can accuse.
#[test]
fn csharp_unused_withdraws_an_explicit_interface_implementation() {
    let mut fixture = Fixture::new("gate-csharp-explicit");
    fixture.write(
        "dotnet/IPersist.cs",
        "public interface IPersist\n{\n    void Save();\n}\n",
    );
    fixture.write(
        "dotnet/Persister.cs",
        concat!(
            "public class Persister : IPersist\n{\n",
            "    void IPersist.Save() { }\n\n",
            "    private void Loose() { }\n}\n",
        ),
    );

    let measurement = admitted(&fixture.source(&[]), GateId::CsharpUnusedCandidates, &[]);
    assert_eq!(
        measurement.findings,
        [csharp_unused("dotnet/Persister.cs", "Loose", 5)]
    );
}

/// A partial record holds its members the way a partial class does.
///
/// C# spells a record three ways and the grammar gives all three one node, so a
/// fixture naming only `record` would prove the node rather than the language.
/// Each form declares one private member no name reaches, and the count in the
/// reason is what proves every form was held: a form the arm cannot see encloses
/// nothing, so its member answers to no declaration and is accused instead.
#[test]
fn csharp_unused_retains_a_member_of_every_partial_record_form() {
    let mut fixture = Fixture::new("gate-csharp-record");
    fixture.write(
        "dotnet/Records.cs",
        concat!(
            "public partial record Plain\n{\n",
            "    private void HeldByPlain() { }\n}\n\n",
            "public partial record class Classy\n{\n",
            "    private void HeldByClass() { }\n}\n\n",
            "public partial record struct Valued\n{\n",
            "    private void HeldByValue() { }\n}\n",
        ),
    );

    let measurement = admitted(&fixture.source(&[]), GateId::CsharpUnusedCandidates, &[]);
    assert_eq!(measurement.outcome, ArmOutcome::Passed);
    assert!(
        measurement
            .reason
            .ends_with("; 3 unreached members retained (partial_declaration 3)"),
        "a partial record's members left no retained trace: {}",
        measurement.reason
    );
}

/// A nested member answers to the innermost type that declares it.
///
/// The outer class is partial and would retain an unreached member, while the
/// inner record is complete in this file and therefore does not. Choosing the
/// outermost span would silently retain `Loose` instead of reporting it.
#[test]
fn csharp_unused_binds_a_nested_member_to_its_innermost_enclosing_type() {
    let mut fixture = Fixture::new("gate-csharp-nested");
    fixture.write(
        "dotnet/Nested.cs",
        concat!(
            "public partial class Outer\n{\n",
            "    private record Inner\n    {\n",
            "        private void Loose() { }\n    }\n}\n",
        ),
    );

    let measurement = admitted(&fixture.source(&[]), GateId::CsharpUnusedCandidates, &[]);
    assert_eq!(measurement.outcome, ArmOutcome::Findings);
    assert_eq!(
        measurement.findings,
        [csharp_unused("dotnet/Nested.cs", "Loose", 5)]
    );
}

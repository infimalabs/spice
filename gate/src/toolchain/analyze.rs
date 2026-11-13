//! Compiled analyses for the toolchain arms this build answers itself.
//!
//! Every declared analysis enters through this module: the formatter check,
//! native and app compilers, workstation-path and prose scanners,
//! JavaScript and C# declaration/reference analyses, Python typecheck, and the
//! two environment scanners. Each reports only what it observed, and
//! [`super::measure_toolchain`] remains the sole authority that admits it.
//!
//! Each declaration records its exclusions as English sentences and its
//! includes as globs. Those strings are the retained evidence and the
//! predicates here are the compiled form of the same rules, so every rule is
//! written once in this module and the two spellings are proven equal in tests.
//!
//! The environment law is large enough to live on its own in
//! [`super::environment`]. This module still owns which paths the two
//! environment arms select and which outcome they earn; that module owns only
//! what one line of one language family says about the environment.
//!
//! The C# unused-member analysis lives in [`csharp`] for the same reason. Its
//! whole difficulty is which absences it may accuse and which it must withdraw
//! unnamed, and that argument reads better beside itself than interleaved with
//! arms that share none of it.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use sill::Environment;

use super::captured::Captured;
use super::{
    ArmFinding, ArmOutcome, ArmTiming, ENV_NAME_LITERAL_MARKER, EnvironmentLedgerKind,
    FindingDetail, JavascriptUnusedStatus, LOCAL_PATH_MARKER, PACKAGED_ENV_PROFILE,
    PACKAGED_JAVASCRIPT_EXEMPTIONS, PACKAGED_TASTE_WORDS, PACKAGED_TEST_PATHS, SourceLocus,
    TASTE_EXTENSIONS, ToolchainDeclaration, column_of, environment,
};
use crate::GateId;
use crate::jurisdiction::{Ignore, Jurisdiction};
use crate::measure::Bounds;
use crate::structural::{StructuralPattern, scan_javascript_symbols};

mod app;
mod build_root;
mod formatter;

pub(super) use app::app_web_typecheck;
pub(super) use formatter::{formatter_edition, formatter_sources, formatters};
use formatter::{relative_to, reported_roots};

mod compile;
mod csharp;
mod python;
mod rust;

pub(super) use compile::{
    CompileCapture, CompileFailure, CompileInvocation, CompileSuccess, run_compile_analysis,
    run_compile_analysis_with_wait,
};
pub(super) use csharp::csharp_unused;
pub(super) use python::python_typecheck;
pub(super) use rust::{rust_analysis_workspace, rust_typecheck};

/// What one compiled analysis observed about one arm.
///
/// The exit code is carried because [`super::measure_toolchain`] admits one
/// only alongside [`ArmOutcome::NonzeroAnalysis`] and demands one there, so the
/// code and the outcome it explains are produced together or not at all.
pub(super) struct Analysis {
    pub(super) outcome: ArmOutcome,
    pub(super) reason: String,
    pub(super) findings: Vec<ArmFinding>,
    pub(super) exit_code: Option<i32>,
    pub(super) timing: Option<ArmTiming>,
}

/// Resolved compiler programs and the admitted environment they must inherit.
pub(super) struct CompilerResources<'a> {
    programs: &'a BTreeMap<&'a str, &'a str>,
    environment: &'a [(OsString, OsString)],
}

impl<'a> CompilerResources<'a> {
    pub(super) const fn new(
        programs: &'a BTreeMap<&'a str, &'a str>,
        environment: &'a [(OsString, OsString)],
    ) -> Self {
        Self {
            programs,
            environment,
        }
    }

    pub(super) fn program(&self, name: &str) -> Option<&'a str> {
        self.programs.get(name).copied()
    }

    pub(super) const fn environment(&self) -> &'a [(OsString, OsString)] {
        self.environment
    }
}

/// Selected Cargo inputs captured at one tree instant.
#[derive(Clone, Copy)]
pub(super) struct CargoSelection<'a> {
    root: &'a Path,
    base: Option<&'a Path>,
    changes: &'a [String],
}

impl<'a> CargoSelection<'a> {
    pub(super) const fn new(root: &'a Path, base: Option<&'a Path>, changes: &'a [String]) -> Self {
        Self {
            root,
            base,
            changes,
        }
    }
}

impl Analysis {
    /// A terminal state that carries neither findings nor an exit code.
    pub(super) fn terminal(outcome: ArmOutcome, reason: String) -> Self {
        Self {
            outcome,
            reason,
            findings: Vec::new(),
            exit_code: None,
            timing: None,
        }
    }

    /// A direct analysis process refused without naming anything it examined.
    fn refused(reason: String, code: i32) -> Self {
        Self {
            outcome: ArmOutcome::NonzeroAnalysis,
            reason,
            findings: Vec::new(),
            exit_code: Some(code),
            timing: None,
        }
    }

    /// The result of one completed scan over `examined` selected paths.
    fn scanned(id: GateId, examined: usize, findings: Vec<ArmFinding>) -> Self {
        if findings.is_empty() {
            return Self::terminal(
                ArmOutcome::Passed,
                format!(
                    "{} found nothing across {examined} selected paths",
                    id.as_str()
                ),
            );
        }
        Self {
            outcome: ArmOutcome::Findings,
            reason: format!(
                "{} found {} across {examined} selected paths",
                id.as_str(),
                plural(findings.len(), "finding"),
            ),
            findings,
            exit_code: None,
            timing: None,
        }
    }

    /// Attach the bounded process duration observed by a compiled runner.
    pub(super) fn timed(mut self, timing: ArmTiming) -> Self {
        self.timing = Some(timing);
        self
    }

    /// The declaration's own applicability predicate came out false.
    ///
    /// The sentence is read back off the declaration rather than repeated
    /// here, so the reason a tree was skipped is the text the report retains.
    fn not_applicable(declaration: ToolchainDeclaration) -> Self {
        Self::terminal(
            ArmOutcome::NotApplicable,
            format!(
                "{} requires {}",
                declaration.id.as_str(),
                declaration.jurisdiction.applicability
            ),
        )
    }
}

/// Settle declaration-owned applicability that precedes executable discovery.
///
/// A tool is required to perform an applicable analysis, not to prove that the
/// selected tree contains no subject for that analysis. Returning the absence
/// here keeps an inapplicable arm independent of workstation tool inventory and
/// ensures its retained tool evidence is empty.
pub(super) fn before_tools(
    declaration: ToolchainDeclaration,
    root: &Path,
    reached_paths: Option<&[String]>,
    analysis_paths: &[String],
) -> Option<Analysis> {
    match declaration.id {
        GateId::Formatters => match formatter::formatter_inputs(root, reached_paths) {
            Ok(sources) if sources.is_empty() => Some(Analysis::not_applicable(declaration)),
            Ok(_) => None,
            Err(reason) => Some(Analysis::terminal(ArmOutcome::ParseFailure, reason)),
        },
        GateId::AppWebTypecheck
            if !root.join("Cargo.toml").is_file() || !root.join("app/index.html").is_file() =>
        {
            Some(Analysis::not_applicable(declaration))
        }
        GateId::RustTypecheck if !root.join("Cargo.toml").is_file() => {
            Some(Analysis::not_applicable(declaration))
        }
        GateId::PythonTypecheck => match python_sources(root, analysis_paths) {
            Ok(true) => None,
            Ok(false) => Some(Analysis::not_applicable(declaration)),
            Err(reason) => Some(Analysis::terminal(ArmOutcome::ParseFailure, reason)),
        },
        _ => None,
    }
}

/// One compiled path-component exclusion and the sentence that records it.
///
/// [`super::PACKAGED_SCANNER_EXCLUDES`] is the retained prose form of exactly
/// this table, in this order, and the two are proven equal in tests.
#[derive(Clone, Copy)]
enum Exclusion {
    /// A path component equal to this name.
    Equals(&'static str),
    /// A path component beginning with this prefix.
    StartsWith(&'static str),
}

impl Exclusion {
    /// Whether this rule removes one path component.
    fn excludes(self, component: &str) -> bool {
        match self {
            Self::Equals(name) => component == name,
            Self::StartsWith(prefix) => component.starts_with(prefix),
        }
    }
}

/// Every scanner exclusion, in the order the declarations record them.
const SCANNER_EXCLUSIONS: &[Exclusion] = &[
    Exclusion::Equals(".git"),
    Exclusion::StartsWith(".spice"),
    Exclusion::Equals(".venv"),
    Exclusion::Equals(".ruff_cache"),
    Exclusion::Equals(".pytest_cache"),
    Exclusion::Equals("__pycache__"),
    Exclusion::Equals("node_modules"),
    Exclusion::Equals("site-packages"),
];

/// Every build-output exclusion, in the order the declarations record them.
///
/// The Formatters declaration excludes build output rather than scanner noise,
/// and the two tables are genuinely different rules: these are top-level
/// directories written as `name/**`, not components matched at any depth.
const BUILD_EXCLUSIONS: &[&str] = &[".git", "target", "dist", "node_modules", ".venv", "vendor"];

/// The compiled form of the JavaScript-unused include globs.
const JAVASCRIPT_EXTENSIONS: &[&str] = &["js", "mjs", "cjs"];

/// The compiled form of the Formatters Rust include glob.
const RUST_EXTENSION: &str = "rs";

/// Byte acquisition used by the source-decoding analyses.
///
/// Production always supplies [`fs::read`]. Keeping the operation explicit
/// lets tests prove read-refusal behavior without depending on effective-user
/// permission semantics.
type SourceReader = fn(&Path) -> io::Result<Vec<u8>>;

fn read_source(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}

/// The formatter whose canonical bytes every Formatters finding is measured against.
const FORMATTER: &str = "rustfmt";

/// The declared program the formatter check is invoked through.
const CARGO: &str = "cargo";

/// The declared program that validates the app asset graph.
const TRUNK: &str = "trunk";

/// Cargo's external build directory, constructed so the workspace ledger does
/// not mistake a third-party process variable for a Spice-owned name.
const CARGO_TARGET_DIRECTORY: &str = concat!("CARGO_", "TARGET_DIR");

/// Cargo's external build-artifact directory under the same ownership rule.
const CARGO_BUILD_DIRECTORY: &str = concat!("CARGO_", "BUILD_BUILD_DIR");

/// An ambient compiler cache must not sit outside this bounded process group.
const RUSTC_WRAPPER: &str = concat!("RUSTC_", "WRAPPER");

/// A workspace compiler wrapper is the same uncontained execution path.
const RUSTC_WORKSPACE_WRAPPER: &str = concat!("RUSTC_", "WORKSPACE_WRAPPER");

/// Cargo's incremental-compilation switch under the same ownership rule. A
/// gate compiles each selected tree once, so incremental state only doubles
/// the output it keeps warm.
const CARGO_INCREMENTAL: &str = concat!("CARGO_", "INCREMENTAL");

/// Trunk's external distribution directory under the same rule.
const TRUNK_DISTRIBUTION_DIRECTORY: &str = concat!("TRUNK_", "BUILD_DIST");

/// The first app analysis, kept in its declaration order.
const CARGO_WASM_ARGUMENTS: &[&str] = &[
    "clippy",
    "--locked",
    "--package",
    "app",
    "--target",
    "wasm32-unknown-unknown",
    "--",
    "-D",
    "warnings",
];

/// The second app analysis, kept in its declaration order: a development
/// bundle, so no proof pays for the `wasm-opt` pass only releases ship.
const TRUNK_BUILD_ARGUMENTS: &[&str] = &["build", "index.html", "--locked", "--filehash=false"];

/// The flag that makes the formatter name differing files instead of diffing.
///
/// A diff carries every differing region, so its volume grows with how far the
/// tree has drifted while the capture reading it stays bounded. A tree drifted
/// far enough truncates that capture, and the arm then refuses that it could
/// not read its own output rather than naming one file, which loses the report
/// exactly when there is most to report. A list costs one line per file, so the
/// worst tree is as reportable as the best one.
const FILES_WITH_DIFF: &str = "--files-with-diff";

/// Whether one selected-tree path sits under an excluded build directory.
fn built(path: &str) -> bool {
    path.split_once('/')
        .is_some_and(|(top, _)| BUILD_EXCLUSIONS.contains(&top))
}

/// Whether any component of one selected-tree path is excluded from scanning.
fn excluded(path: &str) -> bool {
    path.split('/').any(|component| {
        SCANNER_EXCLUSIONS
            .iter()
            .any(|exclusion| exclusion.excludes(component))
    })
}

/// The lowercase extension of one selected-tree path, when it has one.
fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let dot = name.rfind('.')?;
    (dot > 0).then(|| name[dot + 1..].to_ascii_lowercase())
}

/// One count with the unit it counts, so a reason never reads "1 findings".
fn plural(count: usize, unit: &str) -> String {
    if count == 1 {
        format!("{count} {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// Scan every selected change for the workstation path marker.
///
/// One finding per occurrence rather than per line, each carrying the column it
/// opens at. Two markers on one line are two paths that have to be fixed, and
/// the column is what keeps the second from arriving as a duplicate of the first
/// and being refused.
///
/// The include is every regular file, decoded as UTF-8 with replacement, so a
/// binary file is read and simply yields no marker rather than being skipped
/// for a reason the declaration never names. A file that cannot be read is a
/// different thing and terminates the arm, because a scan reporting no marker
/// in a file it never opened would name nothing and mean nothing.
pub(super) fn local_paths(
    declaration: ToolchainDeclaration,
    root: &Path,
    changes: &[String],
) -> Analysis {
    local_paths_with(declaration, root, changes, read_source)
}

fn local_paths_with(
    declaration: ToolchainDeclaration,
    root: &Path,
    changes: &[String],
    read_source: SourceReader,
) -> Analysis {
    let selected = selected_changes(root, changes, |_| true);
    if selected.is_empty() {
        return Analysis::not_applicable(declaration);
    }

    let mut findings = Vec::new();
    for path in &selected {
        let lines = match numbered_lines(root, path, read_source) {
            Ok(lines) => lines,
            Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
        };
        for (line, text) in lines {
            for (start, _) in text.match_indices(LOCAL_PATH_MARKER) {
                findings.push(ArmFinding {
                    path: Some(path.clone()),
                    line: Some(line),
                    column: column_of(&text, start),
                    detail: FindingDetail::LocalPath,
                });
            }
        }
    }
    Analysis::scanned(declaration.id, selected.len(), findings)
}

/// Scan every selected prose change for the compiled discouraged vocabulary.
///
/// One finding per occurrence rather than per word, each carrying the column it
/// opens at. A word written twice on one line is two places the prose has to be
/// rewritten, and the column is what keeps the second from arriving as a
/// duplicate of the first and being refused.
pub(super) fn taste(
    declaration: ToolchainDeclaration,
    root: &Path,
    changes: &[String],
) -> Analysis {
    taste_with(declaration, root, changes, read_source)
}

fn taste_with(
    declaration: ToolchainDeclaration,
    root: &Path,
    changes: &[String],
    read_source: SourceReader,
) -> Analysis {
    let selected = selected_changes(root, changes, |path| {
        extension(path).is_some_and(|found| TASTE_EXTENSIONS.contains(&found.as_str()))
    });
    if selected.is_empty() {
        return Analysis::not_applicable(declaration);
    }

    let mut findings = Vec::new();
    for path in &selected {
        let lines = match numbered_lines(root, path, read_source) {
            Ok(lines) => lines,
            Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
        };
        for (line, text) in lines {
            for found in discouraged(&text) {
                findings.push(ArmFinding {
                    path: Some(path.clone()),
                    line: Some(line),
                    column: Some(found.column),
                    detail: FindingDetail::Taste {
                        word: found.word,
                        replacement: found.replacement,
                    },
                });
            }
        }
    }
    Analysis::scanned(declaration.id, selected.len(), findings)
}

/// Scan every selected change for an unwaived environment access.
///
/// One finding per access rather than per line, each carrying the column it
/// begins at. Two reads on one line are two places the environment shapes
/// behavior, and a column is what keeps the second from arriving as a duplicate
/// of the first and being refused.
pub(super) fn env_policy(
    declaration: ToolchainDeclaration,
    root: &Path,
    changes: &[String],
) -> Analysis {
    env_policy_with(declaration, root, changes, read_source)
}

fn env_policy_with(
    declaration: ToolchainDeclaration,
    root: &Path,
    changes: &[String],
    read_source: SourceReader,
) -> Analysis {
    let selected = selected_changes(root, changes, env_language);
    if selected.is_empty() {
        return Analysis::not_applicable(declaration);
    }

    let mut findings = Vec::new();
    for path in &selected {
        let family = extension(path).and_then(|found| environment::family(&found));
        let text = match decoded(root, path, read_source) {
            Ok(text) => text,
            Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
        };
        let waived = environment::waived_lines(&text);
        for (line, source) in numbered(&text) {
            if waived.contains(&line) {
                continue;
            }
            for access in environment::accesses(family, &source) {
                findings.push(ArmFinding {
                    path: Some(path.clone()),
                    line: Some(line),
                    column: Some(access.column),
                    detail: FindingDetail::EnvironmentAccess {
                        name: access.name,
                        source: source.clone(),
                    },
                });
            }
        }
    }
    Analysis::scanned(declaration.id, selected.len(), findings)
}

/// Account every environment name the complete selected tree references.
///
/// A name the tree references and the profile does not declare is unaccounted
/// and carries every authored locus that references it. Inherited merge-parent
/// loci do not charge the pending commit. A name the profile declares and the
/// complete tree never references is stale and carries none, because there is
/// no place to point at. Both directions are reported, so the ledger cannot
/// drift in either one without saying so.
///
/// The guidance names both waiver spellings, because they are two vocabularies
/// over one line. `env-policy: not-an-env NAME` is a current-port assertion the
/// installed hook has never heard of, and the hook's own `env-policy: allow`
/// reads here as environment context, which returns the literal as a reference
/// instead of exempting it. Recommending the first alone steers an author into
/// a refusal from the second, and recommending the pair steers them back here.
pub(super) fn env_name_ledger(
    declaration: ToolchainDeclaration,
    root: &Path,
    reached_paths: &[String],
    declared_names: &[String],
    chargeable: impl Fn(&str) -> bool,
) -> Analysis {
    env_name_ledger_with(
        declaration,
        root,
        reached_paths,
        declared_names,
        read_source,
        chargeable,
    )
}

fn env_name_ledger_with(
    declaration: ToolchainDeclaration,
    root: &Path,
    reached_paths: &[String],
    declared_names: &[String],
    read_source: SourceReader,
    chargeable: impl Fn(&str) -> bool,
) -> Analysis {
    let sources = selected_changes(root, reached_paths, env_language);
    if sources.is_empty() {
        return Analysis::not_applicable(declaration);
    }

    let mut referenced: BTreeMap<String, Vec<SourceLocus>> = BTreeMap::new();
    for path in &sources {
        let extension = extension(path);
        let family = extension.as_deref().and_then(environment::family);
        let text = match decoded(root, path, read_source) {
            Ok(text) => text,
            Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
        };
        for (line, name) in environment::references_in(extension.as_deref(), family, &text) {
            referenced.entry(name).or_default().push(SourceLocus {
                path: path.clone(),
                line,
            });
        }
    }

    let declared: BTreeSet<&str> = declared_names.iter().map(String::as_str).collect();
    let referenced_paths: BTreeSet<&str> = referenced
        .iter()
        .filter(|(name, _)| !declared.contains(name.as_str()))
        .flat_map(|(_, loci)| loci.iter().map(|locus| locus.path.as_str()))
        .collect();
    let chargeable_paths: BTreeSet<&str> = referenced_paths
        .into_iter()
        .filter(|path| chargeable(path))
        .collect();
    let mut findings: Vec<ArmFinding> = referenced
        .iter()
        .filter(|(name, _)| !declared.contains(name.as_str()))
        .filter_map(|(name, loci)| {
            let loci = loci
                .iter()
                .filter(|locus| chargeable_paths.contains(locus.path.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            (!loci.is_empty()).then(|| ledger(EnvironmentLedgerKind::Unaccounted, name, loci))
        })
        .collect();
    findings.extend(
        declared
            .iter()
            .filter(|name| !referenced.contains_key(**name))
            .map(|name| ledger(EnvironmentLedgerKind::Stale, name, Vec::new())),
    );
    let has_findings = !findings.is_empty();
    let mut analysis = Analysis::scanned(declaration.id, sources.len(), findings);
    if has_findings {
        analysis.reason.push_str(&format!(
            concat!(
                "; mark a quoted non-environment literal as ",
                "`{marker} NAME` on its line, which only this ledger reads. ",
                "The commit hook waives by `{allow}` alone, and that marker ",
                "makes every quoted literal on its line a reference here, so ",
                "a literal both gates flag wants listing in `policy.env_names`"
            ),
            marker = ENV_NAME_LITERAL_MARKER,
            allow = PACKAGED_ENV_PROFILE.allow_marker,
        ));
    }
    analysis
}

/// One repository-wide ledger finding, which carries loci instead of a locus.
fn ledger(mismatch: EnvironmentLedgerKind, name: &str, references: Vec<SourceLocus>) -> ArmFinding {
    ArmFinding {
        path: None,
        line: None,
        column: None,
        detail: FindingDetail::EnvironmentLedger {
            mismatch,
            name: name.to_owned(),
            references,
        },
    }
}

/// Whether one selected-tree path is a packaged environment-language source.
fn env_language(path: &str) -> bool {
    extension(path).is_some_and(|found| environment::ENV_EXTENSIONS.contains(&found.as_str()))
}

mod javascript;
pub(in crate::toolchain) use javascript::*;

fn complete_output(name: &str, captured: &Captured<'_>) -> Result<String, String> {
    complete_compiler_output(name, captured, false)
}

fn complete_compiler_output(
    name: &str,
    captured: &Captured<'_>,
    retain_tail: bool,
) -> Result<String, String> {
    if let Some(failure) = captured.observation_failure {
        return Err(format!("{name} output observation failed: {failure}"));
    }
    let Some(stdout) = captured.stdout else {
        return Err(format!("{name} captured no stdout to read"));
    };
    let Some(stderr) = captured.stderr else {
        return Err(format!("{name} captured no stderr to read"));
    };
    if (!retain_tail && stdout.truncated)
        || stderr.truncated
        || !stdout.complete
        || !stderr.complete
    {
        return Err(format!(
            "{name} output was truncated or incomplete before it could be read"
        ));
    }

    let stdout = if retain_tail && stdout.truncated {
        stdout
            .bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |end| &stdout.bytes[end + 1..])
    } else {
        stdout.bytes
    };
    let mut printed = rendered_compiler_output(stdout);
    if !printed.is_empty() && !printed.ends_with('\n') {
        printed.push('\n');
    }
    printed.push_str(&String::from_utf8_lossy(stderr.bytes));
    Ok(printed)
}

#[derive(serde::Deserialize)]
struct CargoOutputLine {
    reason: Option<String>,
    message: Option<CargoDiagnosticMessage>,
}

#[derive(serde::Deserialize)]
struct CargoDiagnosticMessage {
    rendered: Option<String>,
}

fn rendered_compiler_output(stdout: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(stdout);
    let mut rendered = String::new();
    for line in decoded.lines() {
        match serde_json::from_str::<CargoOutputLine>(line) {
            Ok(message) if message.reason.as_deref() == Some("compiler-message") => {
                if let Some(diagnostic) = message.message.and_then(|message| message.rendered) {
                    rendered.push_str(&diagnostic);
                    if !diagnostic.ends_with('\n') {
                        rendered.push('\n');
                    }
                }
            }
            Ok(message) if message.reason.is_some() => {}
            _ => {
                rendered.push_str(line);
                rendered.push('\n');
            }
        }
    }
    rendered
}

/// Every located `rustc` diagnostic in one program's output.
///
/// The parse is the compiler's own, so both compile arms share it; they differ
/// only in the [`FindingDetail`] they publish, which is what keeps
/// [`FindingDetail::gate`] able to answer which arm a finding came from.
fn compile_findings(
    root: &Path,
    cwd: &Path,
    printed: &str,
    detail: fn(String, String) -> FindingDetail,
) -> Vec<ArmFinding> {
    let mut pending: Option<(String, String)> = None;
    let mut findings = BTreeSet::new();
    for raw in printed.lines() {
        let line = raw.trim_start();
        if let Some(diagnostic) = app_diagnostic(line) {
            pending = Some(diagnostic);
            continue;
        }
        let Some(location) = line.strip_prefix("--> ") else {
            continue;
        };
        let Some((code, message)) = pending.take() else {
            continue;
        };
        let Some((path, line, column)) = app_locus(root, cwd, location) else {
            continue;
        };
        findings.insert(ArmFinding {
            path: Some(path),
            line: Some(line),
            column: Some(column),
            detail: detail(code, message),
        });
    }
    findings.into_iter().collect()
}

fn app_diagnostic(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("error[")?;
    let (code, message) = rest.split_once("]: ")?;
    (!code.is_empty() && !message.is_empty()).then(|| (code.to_owned(), message.to_owned()))
}

fn app_locus(root: &Path, cwd: &Path, location: &str) -> Option<(String, u32, u32)> {
    let (path_and_line, column) = location.rsplit_once(':')?;
    let (reported, line) = path_and_line.rsplit_once(':')?;
    let line = line.parse::<u32>().ok().filter(|line| *line > 0)?;
    let column = column.parse::<u32>().ok().filter(|column| *column > 0)?;
    let path = if Path::new(reported).is_absolute() {
        relative_to(&reported_roots(root), reported)?
    } else {
        cwd.strip_prefix(root)
            .ok()?
            .join(reported)
            .to_str()?
            .replace('\\', "/")
    };
    super::valid_relative_path(&path).then_some((path, line, column))
}

/// A hermetic search path containing only the resolved analysis programs.
///
/// Cargo subcommands and the tools they dispatch are resolved through `PATH`
/// even when Cargo itself was invoked by absolute path. Preserve the empty
/// process environment everywhere else while making those declared programs
/// mutually discoverable.
fn tool_environment(label: &str, programs: &[&str]) -> Result<Environment, String> {
    tool_environment_with(label, programs, &[])
}

fn tool_environment_with(
    label: &str,
    programs: &[&str],
    inherited: &[(OsString, OsString)],
) -> Result<Environment, String> {
    let path = tool_search_path(label, programs)?;
    let mut entries = inherited
        .iter()
        .cloned()
        .collect::<BTreeMap<OsString, OsString>>();
    entries.insert(OsString::from("PATH"), path);
    Environment::new(entries)
        .map_err(|error| format!("{label} tool environment was refused: {error}"))
}

pub(crate) fn tool_search_path<P: AsRef<Path>>(
    label: &str,
    programs: &[P],
) -> Result<std::ffi::OsString, String> {
    let mut directories = Vec::new();
    for program in programs {
        let program = program.as_ref();
        let Some(parent) = program.parent() else {
            return Err(format!(
                "{label} tool {} has no parent directory",
                program.display()
            ));
        };
        if !directories.contains(&parent) {
            directories.push(parent);
        }
    }
    std::env::join_paths(directories)
        .map_err(|error| format!("{label} tool search path is invalid: {error}"))
}

/// Whether the selected tree satisfies the Python declaration's applicability.
fn python_sources(root: &Path, analysis_paths: &[String]) -> Result<bool, String> {
    if !root.join("pyproject.toml").is_file() {
        return Ok(false);
    }
    Ok(analysis_paths.iter().any(|path| {
        root.join(path).is_file()
            && !built(path)
            && extension(path).is_some_and(|found| found == "py" || found == "pyi")
    }))
}

/// The selected changes one scanner admits, sorted and deduplicated.
///
/// A captured change that is no longer a regular file in the selected tree is
/// dropped rather than reported: a deletion is a change the scanners have
/// nothing to read and nothing to say about.
///
/// Every path is held to [`super::valid_relative_path`], the same rule the
/// report holds findings to. That is not defensiveness about a malformed rim:
/// [`Path::join`] discards the root when given an absolute path, so admitting
/// one would read a file outside the captured tree and report it as though it
/// were inside.
fn selected_changes(
    root: &Path,
    changes: &[String],
    include: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut selected: Vec<String> = changes
        .iter()
        .filter(|path| super::valid_relative_path(path))
        .filter(|path| !excluded(path) && include(path))
        .filter(|path| root.join(path).is_file())
        .cloned()
        .collect();
    selected.sort();
    selected.dedup();
    selected
}

/// One selected file decoded with replacement, or why it could not be read.
///
/// Decoding is lossy so a binary file that reads is scanned and simply yields
/// nothing, which is what the path scanner's include promises. A read that
/// fails is a different thing entirely and terminates the arm that asked: an
/// analysis answering from empty text would report a clean file it never opened.
fn decoded(root: &Path, path: &str, read_source: SourceReader) -> Result<String, String> {
    read_source(&root.join(path))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| format!("the selected source {path} could not be read: {error}"))
}

/// One decoded file as one-based numbered lines.
fn numbered(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            u32::try_from(index + 1)
                .ok()
                .map(|number| (number, line.to_owned()))
        })
        .collect()
}

/// One selected file decoded with replacement, as one-based numbered lines.
fn numbered_lines(
    root: &Path,
    path: &str,
    read_source: SourceReader,
) -> Result<Vec<(u32, String)>, String> {
    Ok(numbered(&decoded(root, path, read_source)?))
}

/// One discouraged word, where it sits on its line, and what to write instead.
struct Discouraged {
    column: u32,
    word: String,
    replacement: String,
}

/// Every discouraged word one line of prose contains, with its replacement.
///
/// A trailing `*` in the compiled vocabulary matches any continuation of the
/// same word, and the reported word is the text that actually appeared rather
/// than the pattern that caught it.
///
/// One word repeated on one line is reported once per occurrence, because each
/// is a separate place the prose has to be rewritten. The column is what keeps
/// the second from arriving as a finding identical to the first, which the
/// report refuses as a duplicate rather than retains as evidence.
fn discouraged(text: &str) -> Vec<Discouraged> {
    words(text)
        .into_iter()
        .filter_map(|(column, word)| {
            PACKAGED_TASTE_WORDS
                .iter()
                .find(|(pattern, _)| matches_pattern(pattern, &word))
                .map(|(_, replacement)| Discouraged {
                    column,
                    word,
                    replacement: (*replacement).to_owned(),
                })
        })
        .collect()
}

/// Whether one compiled pattern catches one whole word, ignoring case.
///
/// A trailing `*` is the glob it looks like and matches any continuation
/// including none, so `orphan*` catches both `orphan` and `orphaned`.
fn matches_pattern(pattern: &str, word: &str) -> bool {
    let lowered = word.to_ascii_lowercase();
    pattern
        .strip_suffix('*')
        .map_or_else(|| lowered == pattern, |stem| lowered.starts_with(stem))
}

/// Every whole word in one line, each with the column it opens at, in order.
fn words(text: &str) -> Vec<(u32, String)> {
    let mut found: Vec<(u32, String)> = Vec::new();
    let mut opened: Option<usize> = None;
    for (offset, character) in text.char_indices() {
        match (is_word_character(character), opened) {
            (true, None) => opened = Some(offset),
            (false, Some(start)) => {
                found.extend(word_at(text, start, offset));
                opened = None;
            }
            (true, Some(_)) | (false, None) => {}
        }
    }
    if let Some(start) = opened {
        found.extend(word_at(text, start, text.len()));
    }
    found
}

/// Whether one character continues a word rather than ending it.
fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// One word taken from a line, at the column it opens at.
///
/// A line too long to name a column in is dropped rather than reported at an
/// invented one, which is what [`numbered`] already does with its line numbers.
fn word_at(text: &str, start: usize, end: usize) -> Option<(u32, String)> {
    column_of(text, start).map(|column| (column, text[start..end].to_owned()))
}

#[cfg(test)]
mod cargo_output_tests {
    use super::rendered_compiler_output;

    #[test]
    fn cargo_output_retains_rendered_diagnostics_without_artifact_records() {
        let output = concat!(
            "{\"reason\":\"compiler-artifact\",\"package_id\":\"fixture\"}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"rendered\":\"error: fixture failed\\n\"}}\n",
            "ordinary compiler output\n",
            "{\"reason\":\"build-finished\",\"success\":false}\n",
        );

        assert_eq!(
            rendered_compiler_output(output.as_bytes()),
            "error: fixture failed\nordinary compiler output\n",
        );
    }
}

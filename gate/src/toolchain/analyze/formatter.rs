//! Formatter analysis for selected source paths and declared build exclusions.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::measure::spawn;
use sill::{Argv, Completion};

use super::*;

/// The formatter invocation for one selected tree, or the refusal to build it.
///
/// Every selected path is named on the command line rather than left to a
/// crate-root walk. rustfmt reaches a file by walking the `mod` declarations
/// down from each crate root, and a file reached through `include!` is named by
/// a macro instead -- `proof/src/container.rs` reaches `container/tests.rs` that
/// way, one of seven such files left here -- so it belongs to no module path.
/// `cargo fmt --all` opens the including file and never the included one,
/// which leaves those files counted as selected, measured by every other arm,
/// and never handed to the formatter at all. Naming the paths is the one way
/// rustfmt will open them, and it is what makes the arm's own count and its
/// coverage the same set.
fn formatter_invocation(
    root: &Path,
    sources: &[String],
    programs: &BTreeMap<&str, &str>,
) -> Result<(Argv, Environment), Analysis> {
    let Some(program) = programs.get(FORMATTER) else {
        return Err(Analysis::terminal(
            ArmOutcome::ToolUnavailable,
            format!("formatters declares no {FORMATTER} to invoke the check through"),
        ));
    };
    let edition = match formatter_edition(root) {
        Ok(edition) => edition,
        Err(reason) => return Err(Analysis::terminal(ArmOutcome::ParseFailure, reason)),
    };
    let arguments: Vec<String> = [
        "--edition".to_owned(),
        edition,
        "--check".to_owned(),
        FILES_WITH_DIFF.to_owned(),
    ]
    .into_iter()
    .chain(sources.iter().cloned())
    .collect();
    let Ok(argv) = Argv::new(program, arguments.iter().map(OsStr::new)) else {
        return Err(Analysis::terminal(
            ArmOutcome::ParseFailure,
            format!("{FORMATTER} argument vector was refused"),
        ));
    };
    match tool_environment("formatters", &[*program]) {
        Ok(environment) => Ok((argv, environment)),
        Err(reason) => Err(Analysis::terminal(ArmOutcome::ParseFailure, reason)),
    }
}

/// Run the declared formatter check over its authorized source inventory.
///
/// A validated focused arm supplies exact authored paths; an exhaustive
/// capture supplies no narrowing and therefore walks the complete selected
/// tree.
///
/// One finding per differing file and no locus within it, because
/// [`FindingDetail::Formatting`] records that the file is not canonical and no
/// single region is more the cause than any other.
pub(in crate::toolchain) fn formatters(
    declaration: ToolchainDeclaration,
    root: &Path,
    reached: Option<&[String]>,
    bounds: &Bounds,
    programs: &BTreeMap<&str, &str>,
) -> Analysis {
    let sources = match formatter_inputs(root, reached) {
        Ok(sources) => sources,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    // A tree with nothing to format runs no formatter at all, rather than one
    // named an empty file list: rustfmt reads stdin when it is named no path,
    // and would sit on it until the bounds killed the arm.
    if sources.is_empty() {
        return Analysis::not_applicable(declaration);
    }
    let (argv, environment) = match formatter_invocation(root, &sources, programs) {
        Ok(invocation) => invocation,
        Err(analysis) => return analysis,
    };
    let mut spec = bounds.spec(argv, root);
    spec.environment = environment;
    let running = match spawn(spec) {
        Ok(running) => running,
        Err(error) => {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("{FORMATTER} did not start: {error}"),
            );
        }
    };
    let outcome = match running.wait() {
        Ok(outcome) => outcome,
        Err(error) => {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("{FORMATTER} could not be waited on: {error}"),
            );
        }
    };
    formatter_reply(declaration, root, sources.len(), &Captured::of(&outcome))
}

/// What one reaped formatter check says about the selected sources.
fn formatter_reply(
    declaration: ToolchainDeclaration,
    root: &Path,
    sources: usize,
    captured: &Captured<'_>,
) -> Analysis {
    let status = match captured.completion {
        Completion::Exited(status) => status,
        Completion::TimedOut(_) => {
            return Analysis::terminal(
                ArmOutcome::Timeout,
                format!("{FORMATTER} outlived the bounds it was given"),
            );
        }
        Completion::Cancelled(_) => {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("{FORMATTER} was cancelled before it reported"),
            );
        }
    };
    let Some(stdout) = captured.stdout else {
        return Analysis::terminal(
            ArmOutcome::ParseFailure,
            format!("{FORMATTER} captured no stdout to read"),
        );
    };
    if !stdout.whole() {
        return Analysis::terminal(
            ArmOutcome::ParseFailure,
            format!("{FORMATTER} output was truncated before it could be read"),
        );
    }

    let printed = String::from_utf8_lossy(stdout.bytes);
    let findings = differing_files(root, &printed);
    if findings.is_empty() && !status.success() {
        let Some(code) = status.code() else {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("{FORMATTER} was signalled without naming a differing file"),
            );
        };
        return Analysis::refused(
            format!("{FORMATTER} refused with {code} and named no differing file"),
            code,
        );
    }
    Analysis::scanned(declaration.id, sources, findings)
}

pub(super) fn formatter_inputs(
    root: &Path,
    reached: Option<&[String]>,
) -> Result<Vec<String>, String> {
    Ok(match reached {
        Some(reached) => reached
            .iter()
            .filter(|path| !built(path))
            .filter(|path| extension(path).is_some_and(|found| found == RUST_EXTENSION))
            .filter(|path| root.join(path).is_file())
            .cloned()
            .collect::<Vec<_>>(),
        None => formatter_sources(root)?,
    })
}

/// One finding per file the formatter named as differing from canonical form.
///
/// The file is the whole locus a formatting refusal has: the fix is to run the
/// formatter over it, and no region within it is more the cause than another.
pub(super) fn differing_files(root: &Path, printed: &str) -> Vec<ArmFinding> {
    let roots = reported_roots(root);
    let named: BTreeSet<String> = printed
        .lines()
        .filter_map(|line| relative_to(&roots, line.trim_end()))
        .collect();
    named
        .into_iter()
        .map(|path| ArmFinding {
            path: Some(path),
            line: None,
            column: None,
            detail: FindingDetail::Formatting {
                formatter: FORMATTER.to_owned(),
            },
        })
        .collect()
}

/// Every spelling the formatter may report the selected tree under.
///
/// A tree reached through a symlinked ancestor is reported canonically by the
/// formatter and handed here uncanonically, so both spellings are offered and a
/// path matching neither is genuinely outside the tree. Offering only one would
/// silently discard every finding and call an unformatted tree clean.
pub(super) fn reported_roots(root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![root.to_owned()];
    if let Ok(canonical) = fs::canonicalize(root)
        && canonical != root
    {
        roots.push(canonical);
    }
    roots
}

/// Re-express one absolute reported path relative to the selected tree.
pub(super) fn relative_to(roots: &[PathBuf], reported: &str) -> Option<String> {
    let reported = Path::new(reported);
    let relative = roots
        .iter()
        .find_map(|root| reported.strip_prefix(root).ok())?
        .to_str()?;
    (!relative.is_empty()).then(|| relative.replace('\\', "/"))
}

/// Count the selected Rust sources the formatter check applies to.
pub(in crate::toolchain) fn formatter_sources(root: &Path) -> Result<Vec<String>, String> {
    let walked = Jurisdiction::walk(root, &Ignore::default())
        .map_err(|error| format!("the selected tree could not be walked: {error}"))?;
    Ok(walked
        .measurable()
        .filter(|entry| !built(entry.path()))
        .filter(|entry| extension(entry.path()).is_some_and(|found| found == RUST_EXTENSION))
        .map(|entry| entry.path().to_owned())
        .collect())
}

/// The edition the root manifest pins, which the formatter must be told.
///
/// `cargo fmt` reads the edition out of the manifest of the package owning each
/// target it drives, before it invokes the formatter. Handing the formatter its
/// paths directly skips that read, and an included file has no package behind it
/// to read one from in the first place, since no cargo target claims it. The
/// formatter's own default is the 2015 edition, and a wrong edition is not a
/// formatting difference: rustfmt refuses to parse the file at all. Refusing is
/// the alternative to guessing one.
///
/// Both spellings are read because a root manifest carries the edition in
/// either: a workspace declares one its members inherit, and a single-package
/// repository declares its own. The workspace default is preferred because it
/// is the one that governs more than the root package.
///
/// What this cannot see is a member overriding the inherited edition, which
/// would be measured against the root's grammar instead of its own. This
/// repository has no such member and its own arm would report the drift, so the
/// bound is stated rather than guarded.
///
/// The refusal is flattened onto one line because a TOML error prints a
/// multiline snippet and the executor refuses to publish a multiline reason at
/// all, so an unflattened diagnosis would be replaced by the refusal to carry
/// it rather than reaching the reader.
pub(in crate::toolchain) fn formatter_edition(root: &Path) -> Result<String, String> {
    let manifest = root.join("Cargo.toml");
    let text = fs::read_to_string(&manifest)
        .map_err(|error| format!("the root manifest could not be read: {error}"))?;
    let document: toml::Value = toml::from_str(&text).map_err(|error| {
        format!(
            "the root manifest is not valid TOML: {}",
            error.to_string().replace('\n', " ")
        )
    })?;
    let workspace = document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"));
    [workspace, document.get("package")]
        .into_iter()
        .flatten()
        .find_map(|table| table.get("edition").and_then(toml::Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| {
            "the root manifest declares neither workspace.package.edition nor package.edition"
                .to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::TOOLCHAIN_ARMS;
    use crate::toolchain::captured::Stream;
    use crate::toolchain::exit_status;

    fn reply(completion: &Completion, stdout: Option<Stream<'_>>) -> Analysis {
        formatter_reply(
            TOOLCHAIN_ARMS[0],
            Path::new("/fixture/tree"),
            2,
            &Captured {
                completion,
                observation_failure: None,
                stdout,
                stderr: None,
            },
        )
    }

    fn whole(bytes: &[u8]) -> Option<Stream<'_>> {
        Some(Stream {
            bytes,
            truncated: false,
            complete: true,
        })
    }

    fn exited(code: i32) -> Completion {
        Completion::Exited(exit_status::exited(code))
    }

    /// A differing file is the finding whatever the exit status says, a path
    /// outside the selected tree is never one, and a refusal naming nothing
    /// keeps its exit code instead of reading as clean.
    #[test]
    fn the_formatter_names_each_differing_selected_file_once() {
        assert_eq!(TOOLCHAIN_ARMS[0].id, GateId::Formatters);
        let printed =
            b"/fixture/tree/src/main.rs\n/elsewhere/src/lib.rs\n/fixture/tree/src/main.rs\n";

        let differing = reply(&exited(1), whole(printed));
        assert_eq!(differing.outcome, ArmOutcome::Findings);
        assert_eq!(
            differing.findings,
            [ArmFinding {
                path: Some("src/main.rs".to_owned()),
                line: None,
                column: None,
                detail: FindingDetail::Formatting {
                    formatter: FORMATTER.to_owned(),
                },
            }]
        );

        let clean = reply(&exited(0), whole(b""));
        assert_eq!(clean.outcome, ArmOutcome::Passed);
        assert!(clean.findings.is_empty());

        let refused = reply(&exited(2), whole(b"/elsewhere/src/lib.rs\n"));
        assert_eq!(refused.outcome, ArmOutcome::NonzeroAnalysis);
        assert_eq!(refused.exit_code, Some(2));
    }

    /// Output the formatter did not finish writing cannot prove a tree clean.
    #[test]
    fn an_unread_or_interrupted_formatter_never_passes() {
        let truncated = Stream {
            bytes: b"",
            truncated: true,
            complete: true,
        };
        let killed = exit_status::terminated();
        // Only Unix reports an exit without a code, which is termination by
        // signal.
        let signaled = cfg!(unix).then_some((
            Completion::Exited(killed),
            whole(b""),
            ArmOutcome::ParseFailure,
        ));
        for (completion, stdout, outcome) in [
            (exited(0), Some(truncated), ArmOutcome::ParseFailure),
            (exited(0), None, ArmOutcome::ParseFailure),
            (Completion::TimedOut(killed), None, ArmOutcome::Timeout),
            (
                Completion::Cancelled(killed),
                None,
                ArmOutcome::ParseFailure,
            ),
        ]
        .into_iter()
        .chain(signaled)
        {
            assert_eq!(reply(&completion, stdout).outcome, outcome);
        }
    }
}

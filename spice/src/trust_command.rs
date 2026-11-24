//! Operator trust reporting and exact-digest rulings.

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::config::{LoadError, Loaded};
use spice::repository::Repository;
use std::path::PathBuf;
use wire::intent::{CommandAnswer, TrustAnswer, TrustShowAnswer};
use wire::output::{Export, INTERACTIVE};
use wire::{ConfigSource, Digest, Domain, FaultClass, Outcome, Refusal, Reply, SpiceCommand};

use crate::cli::Contract;
use crate::command_fault as fault;
use crate::read_page::Page;

pub(crate) const SURFACE_NAME: &str = "configuration";

/// One securely acquired executable configuration surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Surface {
    digest: Digest,
    tracked: Option<String>,
    local: Option<String>,
    generation: spice::config::CaptureGeneration,
}

impl Surface {
    /// Acquire all configuration and digest only executable tracked and local policy.
    pub(crate) fn load(repository: &Repository) -> Result<Self, LoadError> {
        let loaded = spice::config::capture_repository(repository, &[])?;
        Ok(Self::from_loaded(&loaded))
    }

    fn scoped(
        repository: &Repository,
        configuration: Option<&spice::config::RepositoryCapture>,
    ) -> Result<Self, LoadError> {
        let loaded = spice::config::capture_repository_scoped(repository, &[], configuration)?;
        Ok(Self::from_loaded(&loaded))
    }

    /// Bind trust identity to one already admitted configuration capture.
    pub(crate) fn from_loaded(loaded: &Loaded) -> Self {
        let tracked = loaded.tracked().map(ToOwned::to_owned);
        let local = loaded.local().map(ToOwned::to_owned);
        Self {
            digest: surface_digest(tracked.as_deref(), local.as_deref()),
            tracked,
            local,
            generation: loaded.generation().clone(),
        }
    }

    /// Identity of the exact executable tracked plus local bytes.
    pub(crate) const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// The exact documents the digest frames, as a pend records them.
    pub(crate) fn documents(&self) -> wire::body::TrustDocuments {
        wire::body::TrustDocuments {
            tracked: self.tracked.clone(),
            local: self.local.clone(),
        }
    }

    /// Reacquire the named paths and require the exact captured generation.
    pub(crate) fn is_current(&self, repository: &Repository) -> Result<bool, LoadError> {
        Self::load(repository).map(|current| current == *self)
    }
}

fn surface_digest(tracked: Option<&str>, local: Option<&str>) -> Digest {
    let mut framed = Vec::new();
    frame_document(&mut framed, ConfigSource::Tracked, tracked);
    frame_document(&mut framed, ConfigSource::Local, local);
    Digest::compute(Domain::ConfigExecutableSurface, &framed)
}

/// The digest recorded documents frame, which names their trust identity.
pub(crate) fn documents_digest(documents: &wire::body::TrustDocuments) -> Digest {
    surface_digest(documents.tracked.as_deref(), documents.local.as_deref())
}

fn frame_document(framed: &mut Vec<u8>, source: ConfigSource, text: Option<&str>) {
    framed.extend_from_slice(source.as_str().as_bytes());
    framed.push(0);
    match text {
        None => framed.push(0),
        Some(text) => {
            framed.push(1);
            let length = u64::try_from(text.len()).expect("configuration text length fits u64");
            framed.extend_from_slice(&length.to_be_bytes());
            framed.extend_from_slice(text.as_bytes());
        }
    }
}

/// Bare `spice trust` answer, read and bounded by the repository resident.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct TrustReport(wire::intent::TrustReportAnswer);

impl std::fmt::Display for TrustReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("trust configuration rulings")?;
        for ruling in &self.0.rulings {
            write!(
                formatter,
                "\n{} {}",
                ruling.digest,
                ruling.standing.as_str()
            )?;
            if let Some(differential) = &ruling.differential {
                write_differential(formatter, differential)?;
            } else if let Some(show) = &ruling.show {
                write!(formatter, "\ndifferential withheld; {show}")?;
            }
        }
        if let Some(next) = &self.0.next {
            write!(
                formatter,
                "\n{} bytes shown; next ruling {} bytes; {}",
                self.0.shown_bytes, next.next_bytes, next.continuation
            )?;
        }
        Ok(())
    }
}

/// Bounded terminal presentation or exact file export of one trust differential.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TrustShowReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    differential: Option<TrustShowAnswer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    withheld: Option<TrustShowWithheld>,
    #[serde(skip_serializing_if = "Option::is_none")]
    export: Option<Export>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TrustShowWithheld {
    digest: Digest,
    standing: wire::TrustState,
    encoded_bytes: usize,
    export: SpiceCommand,
}

impl std::fmt::Display for TrustShowReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(differential) = &self.differential {
            write!(
                formatter,
                "trust {SURFACE_NAME} {} {}",
                differential.digest,
                differential.standing.as_str()
            )?;
            write_differential(formatter, differential)?;
        }
        if let Some(withheld) = &self.withheld {
            write!(
                formatter,
                "trust {SURFACE_NAME} {} {}; {} differential bytes withheld; {}",
                withheld.digest,
                withheld.standing.as_str(),
                withheld.encoded_bytes,
                withheld.export
            )?;
        }
        if let Some(export) = &self.export {
            write!(formatter, "{export}")?;
        }
        Ok(())
    }
}

/// The recorded documents against the baseline they would replace, one
/// `+`, `-`, or ` ` marked line each, with elided unchanged runs counted.
fn write_differential(
    formatter: &mut std::fmt::Formatter<'_>,
    differential: &TrustShowAnswer,
) -> std::fmt::Result {
    use wire::intent::TrustDiffLine;

    match &differential.baseline {
        Some(baseline) if baseline.recorded => {
            write!(formatter, "\ndiff against granted {}", baseline.digest)?;
        }
        Some(baseline) => write!(
            formatter,
            "\nfull text; granted {} recorded no text to compare",
            baseline.digest
        )?,
        None => write!(formatter, "\nfull text; no other digest is granted")?,
    }
    let Some(documents) = &differential.documents else {
        return write!(formatter, "\nthis digest's pend recorded no document text");
    };
    for document in documents {
        write!(
            formatter,
            "\n--- {}{}",
            document.source.as_str(),
            if document.present { "" } else { " absent" }
        )?;
        for line in &document.lines {
            let (marker, text, ending) = match line {
                TrustDiffLine::Kept { text, ending } => (' ', text, *ending),
                TrustDiffLine::Added { text, ending } => ('+', text, *ending),
                TrustDiffLine::Removed { text, ending } => ('-', text, *ending),
                TrustDiffLine::Elided { count } => {
                    write!(formatter, "\n@@ {count} unchanged")?;
                    continue;
                }
            };
            write!(formatter, "\n{marker}{text}")?;
            if let Some(note) = ending.note() {
                write!(formatter, "\n\\ {note}")?;
            }
        }
    }
    Ok(())
}

pub(crate) fn report(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<TrustReport> {
    let page = match Page::from_matches(matches, &["trust"]) {
        Ok(page) => page,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    let after = match page.after() {
        Some(after) => match after.parse::<wire::intent::TrustReportCursor>() {
            Ok(cursor) => Some(cursor),
            Err(error) => return Outcome::Refusal(page.cursor_refusal(error)),
        },
        None => None,
    };
    let limits = page.limits();
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(outcome) => return outcome,
    };
    let host = crate::command_client::Host {
        repository: &repository,
        ambient,
    };
    match crate::command_client::execute_typed(
        host,
        contract,
        wire::CommandKind::TrustReport,
        Vec::new(),
        &wire::intent::TrustReportRead { after, limits },
    ) {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::TrustReport(answer),
        }) => Outcome::Reply(Reply {
            position,
            body: TrustReport(*answer),
        }),
        Outcome::Reply(reply) => fault(
            FaultClass::Protocol,
            "trust::answer-shape",
            format!("expected a trust report answer, received {:?}", reply.body),
        ),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

/// Render one recorded digest from the repository's resident journal alone.
pub(crate) fn show_report(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<TrustShowReport> {
    let digest = match digest_argument(matches) {
        Ok(digest) => digest,
        Err(outcome) => return outcome,
    };
    let max_bytes = match show_max_bytes(matches) {
        Ok(max_bytes) => max_bytes,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    let export = matches
        .try_get_one::<String>("export")
        .ok()
        .flatten()
        .map(PathBuf::from);
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(outcome) => return outcome,
    };
    let host = crate::command_client::Host {
        repository: &repository,
        ambient,
    };
    let answer = match show(host, contract, &digest) {
        Ok(answer) => answer,
        Err(outcome) => return outcome,
    };
    let encoded = match wire::encode(&answer.body) {
        Ok(encoded) => encoded,
        Err(error) => {
            return fault(
                FaultClass::Protocol,
                "trust::answer-unencodable",
                error.to_string(),
            );
        }
    };
    let body = match export {
        Some(path) => match spice::export::export(&path, &encoded) {
            Ok(export) => TrustShowReport {
                differential: None,
                withheld: None,
                export: Some(export),
            },
            Err(refusal) => return Outcome::Refusal(refusal),
        },
        None if encoded.len() <= max_bytes => TrustShowReport {
            differential: Some(answer.body),
            withheld: None,
            export: None,
        },
        None => {
            let path = format!("spice-trust-show-{digest}.json");
            let command = SpiceCommand::try_from(vec![
                "trust".to_owned(),
                "show".to_owned(),
                digest.to_string(),
                "--export".to_owned(),
                path,
            ])
            .expect("trust show export is one canonical command");
            TrustShowReport {
                differential: None,
                withheld: Some(TrustShowWithheld {
                    digest,
                    standing: answer.body.standing,
                    encoded_bytes: encoded.len(),
                    export: command,
                }),
                export: None,
            }
        }
    };
    Outcome::Reply(Reply {
        position: answer.position,
        body,
    })
}

fn show_max_bytes(matches: &ArgMatches) -> Result<usize, Refusal> {
    let Some(value) = matches.try_get_one::<String>("max-bytes").ok().flatten() else {
        return Ok(INTERACTIVE.default_limits().bytes);
    };
    let parsed = value.parse::<usize>().map_err(|error| {
        refusal_value(
            "trust::bound-invalid",
            Some("a positive byte count".to_owned()),
            Some(format!("{value}: {error}")),
            "pass --max-bytes within the interactive trust-show ceiling",
        )
    })?;
    let maximum = INTERACTIVE.maximum().bytes;
    if parsed == 0 || parsed > maximum {
        return Err(refusal_value(
            "trust::bound-exceeded",
            Some(format!("1..={maximum}")),
            Some(parsed.to_string()),
            "pass --max-bytes within the interactive trust-show ceiling",
        ));
    }
    Ok(parsed)
}

/// Read one digest's recorded documents against the latest other grant.
fn show<B>(
    host: crate::command_client::Host<'_>,
    contract: &Contract,
    digest: &Digest,
) -> Result<Reply<TrustShowAnswer>, Outcome<B>> {
    match crate::command_client::execute_typed(
        host,
        contract,
        wire::CommandKind::TrustShow,
        Vec::new(),
        &wire::intent::TrustRef {
            digest: digest.clone(),
            worktree: None,
        },
    ) {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::TrustShow(answer),
        }) => Ok(Reply {
            position,
            body: *answer,
        }),
        Outcome::Reply(reply) => Err(fault(
            FaultClass::Protocol,
            "trust::answer-shape",
            format!("expected a trust show answer, received {:?}", reply.body),
        )),
        Outcome::Refusal(refusal) => Err(Outcome::Refusal(refusal)),
        Outcome::Fault(fault) => Err(Outcome::Fault(fault)),
    }
}

/// Pend the exact executable surface currently present in this worktree.
pub(crate) fn pend(
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Outcome<TrustAnswer> {
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(outcome) => return outcome,
    };
    let surface = match Surface::scoped(&repository, configuration) {
        Ok(surface) => surface,
        Err(error) => return load_failure(&error),
    };
    let state = match repository.admit_state() {
        Ok(state) => state,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let Some(worktree) = repository.worktree_root().to_str() else {
        return fault(
            FaultClass::Environment,
            "trust::worktree-unrepresentable",
            format!(
                "worktree root is not UTF-8: {}",
                repository.worktree_root().display()
            ),
        );
    };
    trust_answer(crate::command_client::execute(
        state.serve(),
        contract,
        crate::command_client::SelectedCommand::TrustPend {
            digest: surface.digest(),
            worktree,
        },
        ambient,
    ))
}

/// Apply one exact grant or revoke through the resident command path.
pub(crate) fn rule(
    matches: &ArgMatches,
    contract: &Contract,
    grant: bool,
    ambient: &Ambient,
) -> Outcome<TrustAnswer> {
    let digest = match digest_argument(matches) {
        Ok(digest) => digest,
        Err(outcome) => return outcome,
    };
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(outcome) => return outcome,
    };
    let state = match repository.admit_state() {
        Ok(state) => state,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let selected = if grant {
        crate::command_client::SelectedCommand::TrustGrant { digest: &digest }
    } else {
        // Revoke rejects a pended request or withdraws a grant, so it
        // witnesses whichever standing the digest holds now.
        let host = crate::command_client::Host {
            repository: &repository,
            ambient,
        };
        let board = match crate::board_command::current_board(host, wire::intent::BoardRead::Trust)
        {
            Ok(board) => board,
            Err(outcome) => return crate::typed_outcome(outcome),
        };
        let Some(standing) = board
            .trust
            .iter()
            .find(|head| head.body.digest == digest)
            .map(|head| head.body.state)
        else {
            return refusal(
                "trust::digest-unknown",
                Some(digest.to_string()),
                None,
                "copy the exact digest from spice trust output",
            );
        };
        crate::command_client::SelectedCommand::TrustRevoke {
            digest: &digest,
            standing,
        }
    };
    trust_answer(crate::command_client::execute(
        state.serve(),
        contract,
        selected,
        ambient,
    ))
}

fn digest_argument<B>(matches: &ArgMatches) -> Result<Digest, Outcome<B>> {
    matches
        .get_one::<String>("DIGEST")
        .expect("the generated trust syntax requires DIGEST")
        .parse::<Digest>()
        .map_err(|error| {
            refusal(
                "trust::digest-invalid",
                Some("one canonical lowercase 64-character digest".to_owned()),
                Some(error.to_string()),
                "copy the exact digest from spice trust output",
            )
        })
}

fn trust_answer(outcome: Outcome<CommandAnswer>) -> Outcome<TrustAnswer> {
    match outcome {
        Outcome::Reply(reply) => match reply.body {
            CommandAnswer::Trust(answer) => Outcome::Reply(Reply {
                position: reply.position,
                body: answer,
            }),
            answer => fault(
                FaultClass::Protocol,
                "trust::answer-shape",
                format!("expected a trust answer, received {answer:?}"),
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn current_repository<B>() -> Result<Repository, Outcome<B>> {
    let current = std::env::current_dir().map_err(|error| {
        fault(
            FaultClass::Environment,
            "trust::working-directory-unavailable",
            error.to_string(),
        )
    })?;
    Repository::discover(&current).map_err(|error| {
        fault(
            FaultClass::Environment,
            spice::repository::ResolveError::REASON,
            error.to_string(),
        )
    })
}

pub(crate) fn load_failure<B>(error: &LoadError) -> Outcome<B> {
    if let Some(repository) = error.repository() {
        return fault(
            FaultClass::Environment,
            spice::repository::ResolveError::REASON,
            repository.to_string(),
        );
    }
    let detail = error.diagnostic();
    if error.unreadable() {
        fault(FaultClass::Environment, "trust::policy-unreadable", detail)
    } else {
        refusal(
            "trust::policy-refused",
            Some("tracked and local values admitted by the typed registry".to_owned()),
            Some(detail),
            "correct the named configuration inputs and retry",
        )
    }
}

fn refusal<B>(
    reason: &str,
    expected: Option<String>,
    observed: Option<String>,
    detail: &str,
) -> Outcome<B> {
    Outcome::Refusal(refusal_value(reason, expected, observed, detail))
}

fn refusal_value(
    reason: &str,
    expected: Option<String>,
    observed: Option<String>,
    detail: &str,
) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("executable-policy".to_owned()),
            expected,
            observed,
        )
        .with_detail(detail),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_identity_distinguishes_source_presence_and_bytes() {
        let absent = surface_digest(None, None);
        let tracked_empty = surface_digest(Some(""), None);
        let local_empty = surface_digest(None, Some(""));
        let tracked = surface_digest(Some("policy = 1"), None);
        let local = surface_digest(None, Some("policy = 1"));

        assert_ne!(absent, tracked_empty);
        assert_ne!(tracked_empty, local_empty);
        assert_ne!(tracked, local);
    }
}

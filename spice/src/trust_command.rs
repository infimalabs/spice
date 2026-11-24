//! Operator trust reporting and exact-digest rulings.

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::config::{LoadError, Loaded};
use spice::repository::Repository;
use wire::intent::{CommandAnswer, TrustAnswer, TrustShowAnswer};
use wire::{
    ConfigSource, Digest, Domain, FaultClass, Outcome, Refusal, Reply, ReplyPosition, TrustState,
};

use crate::cli::Contract;
use crate::command_fault as fault;
use crate::read_page::Page;
use wire::output::Bounded;

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

/// Bare `spice trust` answer, read from the repository's resident journal
/// alone: ruling counts, then a bounded page of rulings, pending decisions
/// first, each pended one carrying its recorded configuration as a
/// differential.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TrustReport {
    counts: RulingCounts,
    rulings: Bounded<TrustRuling>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
struct RulingCounts {
    pended: usize,
    granted: usize,
    revoked: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TrustRuling {
    digest: Digest,
    standing: TrustState,
    #[serde(skip_serializing_if = "Option::is_none")]
    differential: Option<TrustShowAnswer>,
}

impl std::fmt::Display for TrustReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "trust {SURFACE_NAME} rulings pended={} granted={} revoked={}",
            self.counts.pended, self.counts.granted, self.counts.revoked
        )?;
        for ruling in &self.rulings.rows {
            write!(
                formatter,
                "\n{} {}",
                ruling.digest,
                ruling.standing.as_str()
            )?;
            if let Some(differential) = &ruling.differential {
                write_differential(formatter, differential)?;
            }
        }
        match &self.rulings.omission {
            Some(omission) => write!(formatter, "\n{omission}"),
            None => Ok(()),
        }
    }
}

/// `spice trust show DIGEST` answer: one recorded digest's standing and its
/// configuration as a differential against the latest other grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct TrustShowReport(TrustShowAnswer);

impl std::fmt::Display for TrustShowReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "trust {SURFACE_NAME} {} {}",
            self.0.digest,
            self.0.standing.as_str()
        )?;
        write_differential(formatter, &self.0)
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

/// Authenticate through the current resident and render the repository's
/// durable trust heads; no worktree configuration is read.
pub(crate) fn report(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<TrustReport> {
    let page = match Page::from_matches(matches, &["trust"]) {
        Ok(page) => page,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(outcome) => return outcome,
    };
    let state = match repository.admit_state() {
        Ok(state) => state,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let epoch = match crate::command_client::authenticate(state.serve(), contract, ambient) {
        Ok(epoch) => epoch,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let host = crate::command_client::Host {
        repository: &repository,
        ambient,
    };
    let board = match crate::board_command::current_board(host, wire::intent::BoardRead::Trust) {
        Ok(board) => board,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    if board.residency.body.epoch != epoch {
        return refusal(
            "trust::epoch-moved",
            Some(epoch.to_string()),
            Some(board.residency.body.epoch.to_string()),
            "reconnect through the current resident discovery and retry",
        );
    }
    let (counts, rulings) = ordered_rulings(board.trust.into_iter().map(|head| head.body));
    let mut rulings = match page.listed(
        rulings,
        |ruling| ruling.digest.to_string(),
        "DIGEST of a recorded ruling",
    ) {
        Ok(rulings) => rulings,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    let show_contract =
        crate::cli::contract(&["trust", "show"]).expect("trust show contract is registered");
    for ruling in &mut rulings.rows {
        if ruling.standing == TrustState::Pended {
            match show(host, show_contract, &ruling.digest) {
                Ok(differential) => ruling.differential = Some(differential.body),
                Err(outcome) => return outcome,
            }
        }
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(board.cursor),
        body: TrustReport { counts, rulings },
    })
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
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(outcome) => return outcome,
    };
    let host = crate::command_client::Host {
        repository: &repository,
        ambient,
    };
    match show(host, contract, &digest) {
        Ok(answer) => Outcome::Reply(Reply {
            position: answer.position,
            body: TrustShowReport(answer.body),
        }),
        Err(outcome) => outcome,
    }
}

/// Count every ruling by standing and order them: decisions awaiting the
/// operator, then the standing grants and revocations; a digest breaks every
/// tie.
fn ordered_rulings(
    heads: impl Iterator<Item = wire::TrustHead>,
) -> (RulingCounts, Vec<TrustRuling>) {
    let mut counts = RulingCounts::default();
    let mut rulings = heads
        .map(|head| {
            *match head.state {
                TrustState::Pended => &mut counts.pended,
                TrustState::Granted => &mut counts.granted,
                TrustState::Revoked => &mut counts.revoked,
            } += 1;
            TrustRuling {
                digest: head.digest,
                standing: head.state,
                differential: None,
            }
        })
        .collect::<Vec<_>>();
    rulings.sort_by(|left, right| {
        left.standing
            .cmp(&right.standing)
            .then_with(|| left.digest.cmp(&right.digest))
    });
    (counts, rulings)
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
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("executable-policy".to_owned()),
            expected,
            observed,
        )
        .with_detail(detail),
    ))
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

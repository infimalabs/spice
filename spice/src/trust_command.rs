//! Operator trust reporting and exact-digest rulings.

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::config::{LoadError, Loaded};
use spice::repository::Repository;
use wire::intent::{CommandAnswer, TrustAnswer};
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
    tracked: Document,
    local: Document,
    generation: spice::config::CaptureGeneration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Document {
    source: ConfigSource,
    location: String,
    text: Option<String>,
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
        let tracked = Document {
            source: ConfigSource::Tracked,
            location: loaded.locations().tracked().display().to_string(),
            text: loaded.tracked().map(ToOwned::to_owned),
        };
        let local = Document {
            source: ConfigSource::Local,
            location: loaded.locations().local().display().to_string(),
            text: loaded.local().map(ToOwned::to_owned),
        };
        let digest = surface_digest(tracked.text.as_deref(), local.text.as_deref());
        Self {
            digest,
            tracked,
            local,
            generation: loaded.generation().clone(),
        }
    }

    /// Identity of the exact executable tracked plus local bytes.
    pub(crate) const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Reacquire the named paths and require the exact captured generation.
    pub(crate) fn is_current(&self, repository: &Repository) -> Result<bool, LoadError> {
        Self::load(repository).map(|current| current == *self)
    }

    fn provenance(&self) -> Vec<PolicyProvenance> {
        [&self.tracked, &self.local]
            .into_iter()
            .map(|document| PolicyProvenance {
                source: document.source,
                location: document.location.clone(),
                present: document.text.is_some(),
                digest: document.text.as_ref().map(|text| {
                    let mut framed = Vec::new();
                    frame_document(&mut framed, document.source, Some(text));
                    Digest::compute(Domain::ConfigExecutableSurface, &framed)
                }),
            })
            .collect()
    }
}

fn surface_digest(tracked: Option<&str>, local: Option<&str>) -> Digest {
    let mut framed = Vec::new();
    frame_document(&mut framed, ConfigSource::Tracked, tracked);
    frame_document(&mut framed, ConfigSource::Local, local);
    Digest::compute(Domain::ConfigExecutableSurface, &framed)
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

/// Bare `spice trust` answer: the current surface's standing and provenance,
/// ruling counts, then a bounded page of rulings, the current digest and
/// pending decisions first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TrustReport {
    surface: String,
    digest: Digest,
    standing: TrustState,
    provenance: Vec<PolicyProvenance>,
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
struct PolicyProvenance {
    source: ConfigSource,
    location: String,
    present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TrustRuling {
    digest: Digest,
    standing: TrustState,
    current: bool,
}

impl std::fmt::Display for TrustReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "trust {} {} {}",
            self.surface,
            self.digest,
            self.standing.as_str()
        )?;
        for document in &self.provenance {
            write!(
                formatter,
                "\n{} {} {}",
                document.source.as_str(),
                document.location,
                document
                    .digest
                    .as_ref()
                    .map_or_else(|| "absent".to_owned(), ToString::to_string)
            )?;
        }
        write!(
            formatter,
            "\nrulings pended={} granted={} revoked={}",
            self.counts.pended, self.counts.granted, self.counts.revoked
        )?;
        for ruling in &self.rulings.rows {
            write!(
                formatter,
                "\n{} {}{}",
                ruling.digest,
                ruling.standing.as_str(),
                if ruling.current { " current" } else { "" }
            )?;
        }
        match &self.rulings.omission {
            Some(omission) => write!(formatter, "\n{omission}"),
            None => Ok(()),
        }
    }
}

/// Authenticate through the current resident and render its durable trust heads.
pub(crate) fn report(
    matches: &ArgMatches,
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
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
    let surface = match Surface::scoped(&repository, configuration) {
        Ok(surface) => surface,
        Err(error) => return load_failure(&error),
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
    let standing = board
        .trust
        .iter()
        .find(|head| head.body.digest == *surface.digest())
        .map(|head| head.body.state);
    let Some(standing) = standing else {
        return refusal(
            "trust::digest-unknown",
            Some(surface.digest().to_string()),
            None,
            &format!(
                "run `spice trust pend` from worktree {} before ruling",
                repository.worktree_root().display(),
            ),
        );
    };
    let current = match surface.is_current(&repository) {
        Ok(current) => current,
        Err(error) => return load_failure(&error),
    };
    if !current {
        return refusal(
            "trust::surface-moved",
            Some(surface.digest().to_string()),
            None,
            "retry against the current tracked and local policy documents",
        );
    }
    let mut counts = RulingCounts::default();
    let mut rulings = board
        .trust
        .into_iter()
        .map(|head| {
            *match head.body.state {
                TrustState::Pended => &mut counts.pended,
                TrustState::Granted => &mut counts.granted,
                TrustState::Revoked => &mut counts.revoked,
            } += 1;
            TrustRuling {
                current: head.body.digest == *surface.digest(),
                digest: head.body.digest,
                standing: head.body.state,
            }
        })
        .collect::<Vec<_>>();
    // The current digest, then decisions awaiting the operator, then the
    // standing grants and revocations; a digest breaks every tie.
    rulings.sort_by(|left, right| {
        right
            .current
            .cmp(&left.current)
            .then(left.standing.cmp(&right.standing))
            .then_with(|| left.digest.cmp(&right.digest))
    });
    let rulings = match page.listed(
        rulings,
        |ruling| ruling.digest.to_string(),
        "DIGEST of a recorded ruling",
    ) {
        Ok(rulings) => rulings,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(board.cursor),
        body: TrustReport {
            surface: SURFACE_NAME.to_owned(),
            digest: surface.digest().clone(),
            standing,
            provenance: surface.provenance(),
            counts,
            rulings,
        },
    })
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
    let digest = match matches
        .get_one::<String>("DIGEST")
        .expect("the generated trust syntax requires DIGEST")
        .parse::<Digest>()
    {
        Ok(digest) => digest,
        Err(error) => {
            return refusal(
                "trust::digest-invalid",
                Some("one canonical lowercase 64-character digest".to_owned()),
                Some(error.to_string()),
                "copy the exact digest from spice trust output",
            );
        }
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
        crate::command_client::SelectedCommand::TrustRevoke { digest: &digest }
    };
    trust_answer(crate::command_client::execute(
        state.serve(),
        contract,
        selected,
        ambient,
    ))
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

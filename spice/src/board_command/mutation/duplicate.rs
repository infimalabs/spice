//! Refuse or warn on a creation whose statement repeats an independent
//! sibling's.

use std::collections::BTreeSet;
use std::sync::Arc;

use board::duplicate::{DuplicateKinship, DuplicateProbe, DuplicateScore, DuplicateStanding};
use board::similarity::FREQUENCY_WINDOW;

use super::*;

/// One creation's screen against the board: the relations it declares, the
/// warnings its write carries, and the guard that refuses a sibling committed
/// after the screen read.
pub(super) struct DuplicateScreen {
    probe: DuplicateProbe,
    created: wire::TaskStamp,
    exempt: Arc<Exemption>,
    cursor: Option<wire::Seq>,
    pub(super) related: Vec<wire::TaskStamp>,
    pub(super) warnings: Vec<wire::intent::DuplicateWarning>,
}

struct Match<'a> {
    statement: &'a TaskHead,
    phase: Phase,
    creator: Option<&'a wire::Actor>,
    score: DuplicateScore,
    standing: DuplicateStanding,
}

/// Admit the relations `statement`, about to be created by `command`, names
/// in `related`, then screen it against every independent sibling: unsettled
/// tasks in any phase, and settled tasks among the [`FREQUENCY_WINDOW`] most
/// recently revised statements. Deleted tasks never match. A sibling
/// `command`'s own actor created, one on the new task's parent, preparation,
/// or dependency chain, or one prepared under the same parent or controller
/// is exempt; one the creation relates to is exempt unless it is fresh.
pub(super) fn screen(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    statement: &TaskHead,
    related: &[TaskHandle],
) -> Result<DuplicateScreen, Outcome<CommandAnswer>> {
    let related = super::related::admitted_relations(bus, &statement.born, related)?;
    let observed = bus
        .task_snapshot()
        .map_err(|error| store_fault("board::heads-unreadable", error.to_string()))?;
    let exempt = Arc::new(Exemption::of(&observed, statement));
    let live = observed
        .tasks()
        .iter()
        .filter(|heads| !heads.workflow().body().status.is_deleted())
        .collect::<Vec<_>>();
    let mut recent = live
        .iter()
        .map(|heads| heads.statement().body())
        .collect::<Vec<_>>();
    recent.sort_by_key(|statement| std::cmp::Reverse(statement.revision));
    let window = recent
        .iter()
        .take(FREQUENCY_WINDOW)
        .map(|statement| &statement.born)
        .collect::<BTreeSet<_>>();
    let probe = DuplicateProbe::new(statement, recent.iter().copied());
    let mut matches = live
        .iter()
        .filter(|heads| {
            let sibling = heads.statement().body();
            sibling.born != statement.born
                && (heads.workflow().body().status.is_pending() || window.contains(&sibling.born))
                && observed.facts().task_creator(&sibling.born) != Some(command.actor())
        })
        .filter_map(|heads| {
            let sibling = heads.statement().body();
            let kinship = exempt.kinship(sibling)?;
            let score = probe.score(sibling)?;
            let standing =
                DuplicateStanding::of(&statement.born, &sibling.born, score.cosine, kinship)?;
            // A declared relation admits any sibling but a fresh one.
            if standing != DuplicateStanding::Refused && related.contains(&sibling.born) {
                return None;
            }
            Some(Match {
                statement: sibling,
                phase: heads.workflow().body().phase,
                creator: observed.facts().task_creator(&sibling.born),
                score,
                standing,
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| right.score.cosine.total_cmp(&left.score.cosine));
    let fresh = matches
        .iter()
        .filter(|sibling| sibling.standing == DuplicateStanding::Refused)
        .collect::<Vec<_>>();
    if let Some(first) = fresh.first() {
        let sibling = first.statement.handle(first.phase);
        return Err(refusal(
            "task::duplicate-fresh",
            Some(described(&statement.born, &fresh)),
            format!(
                "another lane just created this work: ask the holder of {sibling} for an invitation, or post what this statement adds with `spice room post {sibling} TEXT`; no relation admits a duplicate within a minute of its sibling"
            ),
        ));
    }
    let unrelated = matches
        .iter()
        .filter(|sibling| sibling.standing == DuplicateStanding::Unrelated)
        .collect::<Vec<_>>();
    if let Some(first) = unrelated.first() {
        let sibling = first.statement.handle(first.phase);
        return Err(refusal(
            "task::duplicate-unrelated",
            Some(described(&statement.born, &unrelated)),
            format!(
                "reuse {sibling} by asking its holder for an invitation or posting what this statement adds with `spice room post {sibling} TEXT`, or retry with --related naming each sibling when this is distinct related work"
            ),
        ));
    }
    let warnings = matches
        .iter()
        .map(|sibling| wire::intent::DuplicateWarning {
            task: sibling.statement.handle(sibling.phase),
            title: sibling.statement.title.clone(),
            creator: sibling.creator.cloned(),
            cosine: format!("{:.2}", sibling.score.cosine).into_boxed_str(),
            surface: sibling.score.surface.clone(),
        })
        .collect();
    Ok(DuplicateScreen {
        probe,
        created: statement.born.clone(),
        exempt,
        cursor: observed.cursor(),
        related,
        warnings,
    })
}

impl DuplicateScreen {
    /// Retry the screen when any sibling born after its read, outside the
    /// exempt chain, now matches. The retry must apply the same warning,
    /// relation requirement, or fresh refusal as an initially visible sibling.
    pub(super) fn guard(&self, write: &mut JournalWrite) {
        let probe = self.probe.clone();
        let created = self.created.clone();
        let exempt = Arc::clone(&self.exempt);
        let screened = self.cursor;
        write.guard(JournalGuard::new(move |read| {
            let Some(current) = read.cursor()? else {
                return Ok(());
            };
            let first = screened.map_or(Some(current), |screened| {
                wire::Seq::new(screened.get().saturating_add(1)).filter(|first| *first <= current)
            });
            let Some(first) = first else {
                return Ok(());
            };
            for heads in read.changed_task_heads(first, current)? {
                let sibling = heads.statement().body();
                if sibling.born == created
                    || sibling.revision < first
                    || heads.workflow().body().status.is_deleted()
                {
                    continue;
                }
                let Some(kinship) = exempt.kinship(sibling) else {
                    continue;
                };
                if let Some(score) = probe.score(sibling)
                    && DuplicateStanding::of(&created, &sibling.born, score.cosine, kinship)
                        .is_some()
                {
                    return Err(GuardError::refused(format!(
                        "task::duplicate-screen-moved: {} {:?} committed after duplicate screening with cosine {:.2}",
                        sibling.handle(heads.workflow().body().phase),
                        sibling.title,
                        score.cosine,
                    )));
                }
            }
            Ok(())
        }));
    }
}

/// How a creation's lineage bears on each sibling: its parent and that
/// parent's ancestors, its preparation, and its dependencies are exempt, and
/// outputs prepared beside it, answering the same parent or controller, are
/// judged as [`DuplicateKinship::Prepared`].
struct Exemption {
    chain: BTreeSet<wire::TaskStamp>,
    preparation: Option<wire::TaskPreparation>,
}

impl Exemption {
    fn of(observed: &bus::TaskSnapshot, statement: &TaskHead) -> Self {
        let mut chain = statement
            .dependencies
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(preparation) = &statement.preparation {
            chain.extend([
                preparation.parent.clone(),
                preparation.controller.clone(),
                preparation.construction.clone(),
            ]);
        }
        let mut parent = statement.provenance.parent().cloned();
        while let Some(task) = parent {
            if !chain.insert(task.clone()) {
                break;
            }
            parent = observed
                .task_heads(&task)
                .and_then(|heads| heads.statement().body().provenance.parent().cloned());
        }
        Self {
            chain,
            preparation: statement.preparation.clone(),
        }
    }

    /// `None` when `sibling` is on the creation's own chain.
    fn kinship(&self, sibling: &TaskHead) -> Option<DuplicateKinship> {
        if self.chain.contains(&sibling.born) {
            return None;
        }
        let beside = self
            .preparation
            .as_ref()
            .zip(sibling.preparation.as_ref())
            .is_some_and(|(own, theirs)| {
                own.parent == theirs.parent || own.controller == theirs.controller
            });
        Some(if beside {
            DuplicateKinship::Prepared
        } else {
            DuplicateKinship::Independent
        })
    }
}

/// Each sibling's handle, title, creator, cosine, age, and shared terms.
fn described(created: &wire::TaskStamp, siblings: &[&Match<'_>]) -> String {
    siblings
        .iter()
        .map(|sibling| {
            let age = created
                .micros()
                .saturating_sub(sibling.statement.born.micros())
                .max(0)
                / 1_000_000;
            format!(
                "{} {:?} by {} cosine {:.2} age {age}s shares {}",
                sibling.statement.handle(sibling.phase),
                sibling.statement.title,
                sibling
                    .creator
                    .map_or_else(|| "unknown".to_owned(), ToString::to_string),
                sibling.score.cosine,
                sibling.score.surface.join(","),
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

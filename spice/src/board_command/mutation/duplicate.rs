//! Refuse or warn on a creation whose statement repeats an independent
//! sibling's.

use std::collections::BTreeSet;

use board::duplicate::{DuplicateKinship, DuplicatePreview, DuplicateProbe, DuplicateStanding};
use board::similarity::FREQUENCY_WINDOW;

use super::*;

/// One creation's screen against the board: the relations it declares, the
/// warnings its write carries, and the cursor that invalidates the fitted
/// duplicate corpus when another task head changes.
pub(super) struct DuplicateScreen {
    cursor: Option<wire::Seq>,
    pub(super) related: Vec<wire::TaskStamp>,
    pub(super) warnings: Vec<wire::intent::DuplicateWarning>,
    pub(super) warnings_omitted: u32,
}

/// One band's preview of matching siblings.
type Band<'a> = DuplicatePreview<&'a journal::TaskHeads>;

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
    let exempt = Exemption::of(&observed, statement);
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
    let (mut fresh, mut unrelated, mut warned) =
        (Band::default(), Band::default(), Band::default());
    for heads in &live {
        let sibling = heads.statement().body();
        if sibling.born == statement.born
            || !(heads.workflow().body().status.is_pending() || window.contains(&sibling.born))
            || observed.facts().task_creator(&sibling.born) == Some(command.actor())
        {
            continue;
        }
        let Some(kinship) = exempt.kinship(sibling) else {
            continue;
        };
        let Some(cosine) = probe.cosine(sibling) else {
            continue;
        };
        let Some(standing) = DuplicateStanding::of(&statement.born, &sibling.born, cosine, kinship)
        else {
            continue;
        };
        // A declared relation admits any sibling but a fresh one.
        match standing {
            DuplicateStanding::Refused => fresh.admit(cosine, *heads),
            _ if related.contains(&sibling.born) => {}
            DuplicateStanding::Unrelated => unrelated.admit(cosine, *heads),
            DuplicateStanding::Warned => warned.admit(cosine, *heads),
        }
    }
    let described = |band: &Band<'_>| described(&probe, &observed, &statement.born, band);
    if let Some((_, first)) = fresh.shown().first() {
        let sibling = handle(first);
        return Err(refusal(
            "task::duplicate-fresh",
            Some(described(&fresh)),
            format!(
                "another lane just created this work: ask the holder of {sibling} for an invitation, or post what this statement adds with `spice room post {sibling} TEXT`; no relation admits a duplicate within a minute of its sibling"
            ),
        ));
    }
    if let Some((_, first)) = unrelated.shown().first() {
        let sibling = handle(first);
        return Err(refusal(
            "task::duplicate-unrelated",
            Some(described(&unrelated)),
            format!(
                "reuse {sibling} by asking its holder for an invitation or posting what this statement adds with `spice room post {sibling} TEXT`, or retry with --related naming each sibling when this is distinct related work"
            ),
        ));
    }
    let warnings = warned
        .shown()
        .iter()
        .map(|(cosine, heads)| {
            let sibling = heads.statement().body();
            wire::intent::DuplicateWarning {
                task: handle(heads),
                title: sibling.title.clone(),
                creator: observed.facts().task_creator(&sibling.born).cloned(),
                cosine: format!("{cosine:.2}").into_boxed_str(),
                surface: probe.surface(sibling),
            }
        })
        .collect();
    Ok(DuplicateScreen {
        cursor: observed.cursor(),
        related,
        warnings,
        warnings_omitted: u32::try_from(warned.omitted()).unwrap_or(u32::MAX),
    })
}

fn handle(heads: &journal::TaskHeads) -> TaskHandle {
    heads
        .statement()
        .body()
        .handle(heads.workflow().body().phase)
}

impl DuplicateScreen {
    /// Retry the screen when any task head changes after its read. Task-head
    /// changes can alter corpus membership, window order, or candidate status,
    /// so the old fitted profile cannot decide whether a retry is unnecessary.
    pub(super) fn guard(&self, write: &mut JournalWrite) {
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
            let Some(heads) = read.changed_task_heads(first, current)?.into_iter().next() else {
                return Ok(());
            };
            let statement = heads.statement().body();
            Err(GuardError::refused(format!(
                "task::duplicate-screen-moved: {} {:?} changed after duplicate screening",
                statement.handle(heads.workflow().body().phase),
                statement.title,
            )))
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

/// Each previewed sibling's handle, title, creator, cosine, age, and shared
/// terms, then how many more matched.
fn described(
    probe: &DuplicateProbe,
    observed: &bus::TaskSnapshot,
    created: &wire::TaskStamp,
    band: &Band<'_>,
) -> String {
    let mut described = band
        .shown()
        .iter()
        .map(|(cosine, heads)| {
            let sibling = heads.statement().body();
            let age = created
                .micros()
                .saturating_sub(sibling.born.micros())
                .max(0)
                / 1_000_000;
            format!(
                "{} {:?} by {} cosine {cosine:.2} age {age}s shares {}",
                handle(heads),
                sibling.title,
                observed
                    .facts()
                    .task_creator(&sibling.born)
                    .map_or_else(|| "unknown".to_owned(), ToString::to_string),
                probe.surface(sibling).join(","),
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    if band.omitted() > 0 {
        described.push_str(&format!("; and {} more", band.omitted()));
    }
    described
}

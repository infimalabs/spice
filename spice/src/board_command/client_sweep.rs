//! `spice sweep`: preview the latch catalogue's uncovered debt and the slow
//! gate arms as one Plan, or file exactly that preview with `--apply=<digest>`.
//!
//! The preview reads the catalogue, the arm-time ledger, the cover ledger,
//! and the board, and writes nothing. An apply holds the cover lock from that
//! read through the filing command, so two sweeps never cover one subject
//! twice. `--split ARM` runs each target in one previewed slow Cargo test arm
//! once under the harness's per-test report and attaches every target-qualified
//! test wall time to that child; the preview never runs tests.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use spice::gate::sweep::{self, Cover, SweepDraft};
use wire::intent::{SweepSplit, TaskSweep, TextAnswer};

mod split;

use super::*;

pub(super) fn run_sweep(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    match sweep_outcome(repository, contract, matches) {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

fn sweep_outcome(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Result<Outcome<CommandAnswer>, Outcome<CommandAnswer>> {
    let until = operand::<wire::Instant>(matches, "until", "sweep::until-invalid")?;
    let apply = operand::<Digest>(matches, "apply", "sweep::apply-digest-invalid")?;
    let splits: Vec<&String> = matches
        .get_many::<String>("split")
        .map(Iterator::collect)
        .unwrap_or_default();
    if apply.is_none() && !splits.is_empty() {
        return Err(refusal(
            "sweep::split-without-apply",
            None,
            "a preview never runs tests; name --split with --apply",
        ));
    }
    let state = repository
        .repository
        .admit_state()
        .map_err(|error| crate::state_failure(&error).outcome())?;
    // Apply owns gate-state publication, then arm timing publication, before
    // it owns covers. Writers take only their own earlier lock; no inverse
    // order exists. A preview writes nothing and therefore takes none.
    let _possession = apply.as_ref().map(|_| possess_apply(&state)).transpose()?;
    let ledger = crate::gate_command::catalogue(repository.repository.common_dir())?;
    let arms = spice::gate::arms::read_arms(state.gate_arms()).map_err(|error| {
        refusal(
            "sweep::arms-refused",
            Some(error.to_string()),
            "restore the gate arm ledger to the form spice gate prove writes",
        )
    })?;
    let covers = sweep::read_covers(state.gate_sweep()).map_err(|error| {
        refusal(
            "sweep::covers-refused",
            Some(error.to_string()),
            "restore the sweep cover ledger to the form spice sweep --apply writes",
        )
    })?;
    let pending = if covers.is_empty() {
        BTreeSet::new()
    } else {
        sweep::pending_tasks(state.journal(), &covers)
            .map_err(|error| environment_fault("sweep::journal-unavailable", &error))?
    };
    let held = sweep::held(&covers, |task| pending.contains(task), None);
    let draft = sweep::draft(&ledger, &arms, &held, until);
    // Filing witnesses only the global board barrier and fresh births, which
    // the server's Absence witness guards; no task row is needed.
    let board = current_board(
        repository,
        BoardRead::Tasks {
            tasks: Vec::new(),
            dependencies: false,
            claims: false,
            history: false,
        },
    )?;
    let digest = draft.content.digest();
    let Some(apply) = apply else {
        return Ok(Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(board.cursor),
            body: CommandAnswer::Text(TextAnswer {
                text: preview_text(&draft, &digest, matches.get_one::<String>("until")),
            }),
        }));
    };
    if draft.content.children.is_empty() {
        return Err(refusal(
            "sweep::nothing-to-file",
            Some(format!("{} held", draft.held)),
            "every latched fingerprint and slow arm is already held by a pending sweep task",
        ));
    }
    require_current_preview(&apply, &digest)?;
    require_unique_splits(&splits)?;
    let splits = splits
        .into_iter()
        .map(|arm| split::run(repository, &draft, arm))
        .collect::<Result<Vec<_>, _>>()?;
    // Covers name their tasks before the tasks exist: a cover whose filing
    // never commits names no pending task, holds nothing, and is pruned here.
    let retained = covers
        .into_iter()
        .filter(|cover| pending.contains(&cover.task))
        .collect();
    file_sweep(
        repository, contract, &state, &board, draft, digest, splits, retained,
    )
}

/// Parse one optional operand, refusing a misspelling under `reason`.
fn operand<T: std::str::FromStr<Err: std::fmt::Display>>(
    matches: &ArgMatches,
    name: &str,
    reason: &'static str,
) -> Result<Option<T>, Outcome<CommandAnswer>> {
    matches
        .get_one::<String>(name)
        .map(|text| {
            text.parse::<T>()
                .map_err(|error| refusal(reason, Some(text.clone()), error.to_string()))
        })
        .transpose()
}
/// Refuse an apply whose recomputed preview no longer matches what the
/// operator selected.
fn require_current_preview(apply: &Digest, current: &Digest) -> Result<(), Outcome<CommandAnswer>> {
    if apply == current {
        return Ok(());
    }
    Err(refusal(
        "sweep::preview-moved",
        Some(apply.to_string()),
        format!("preview the sweep again and apply the current digest {current}"),
    ))
}

/// Refuse a repeated selection before any split can admit or run Cargo.
fn require_unique_splits(splits: &[&String]) -> Result<(), Outcome<CommandAnswer>> {
    for (index, arm) in splits.iter().enumerate() {
        if splits[..index].contains(arm) {
            return Err(refusal(
                "sweep::split-repeated",
                Some(arm.to_string()),
                "name each --split arm once; no split target was run",
            ));
        }
    }
    Ok(())
}

/// Publish the children's covers beside the `retained` pending ones, then
/// file the previewed Plan and its children against the witnessed board.
#[expect(
    clippy::too_many_arguments,
    reason = "one filing names every authority the apply witnessed"
)]
fn file_sweep(
    repository: Host<'_>,
    contract: &Contract,
    state: &spice::repository::StatePaths,
    board: &BoardAnswer,
    draft: SweepDraft,
    digest: Digest,
    splits: Vec<SweepSplit>,
    mut retained: Vec<Cover>,
) -> Result<Outcome<CommandAnswer>, Outcome<CommandAnswer>> {
    let mut births = allocate_task_stamps(
        board,
        jiff::Timestamp::now().as_microsecond(),
        1 + draft.content.children.len(),
    )?;
    let children = births.split_off(1);
    let plan = births.pop().expect("one Plan birth is allocated");
    retained.extend(children.iter().zip(draft.covers).map(|(born, subjects)| {
        Cover::new(TaskHandle::for_topic(&sweep::sweep_topic(), born), subjects)
    }));
    sweep::publish_covers(state.gate_sweep(), retained)
        .map_err(|error| environment_fault("sweep::covers-unpublished", error.to_string()))?;
    let mut expected = vec![
        Witness::Preview {
            digest: digest.clone(),
        },
        Witness::Barrier {
            name: "board".parse().expect("board is a wire name"),
            digest: board.task_barrier.clone(),
        },
        Witness::Absence { task: plan.clone() },
    ];
    expected.extend(
        children
            .iter()
            .map(|task| Witness::Absence { task: task.clone() }),
    );
    Ok(crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::TaskSweep,
        expected,
        &TaskSweep {
            preview: digest,
            plan,
            children,
            content: draft.content,
            splits,
        },
    ))
}

/// The three authorities an apply must keep stable, in acquisition order.
#[derive(Debug)]
struct ApplyPossession {
    _state: sill::Possession,
    _arms: sill::Possession,
    _covers: sill::Possession,
}

fn possess_apply(
    state: &spice::repository::StatePaths,
) -> Result<ApplyPossession, Outcome<CommandAnswer>> {
    Ok(ApplyPossession {
        _state: possess(
            "gate-state",
            state.gate_state_lock(),
            (
                "sweep::gate-state-contended",
                "a gate run is publishing the latch catalogue; retry the sweep apply",
            ),
            "sweep::gate-state-lock-unavailable",
        )?,
        _arms: possess(
            "gate-arms",
            state.gate_arms_lock(),
            (
                "sweep::gate-arms-contended",
                "a gate run is publishing arm timings; retry the sweep apply",
            ),
            "sweep::gate-arms-lock-unavailable",
        )?,
        _covers: possess(
            "sweep",
            state.gate_sweep_lock(),
            (
                "sweep::contended",
                "another sweep is being filed; preview again once it finishes",
            ),
            "sweep::lock-unavailable",
        )?,
    })
}

/// Take one fixed lock without waiting: contention refuses with `contended`'s
/// reason and advice, and an unopenable lock faults with `unavailable`.
fn possess(
    name: &str,
    path: &std::path::Path,
    contended: (&'static str, &'static str),
    unavailable: &'static str,
) -> Result<sill::Possession, Outcome<CommandAnswer>> {
    let name = sill::LockName::new(name).expect("a fixed lock name is one normal component");
    match sill::LockFile::new(name, path).try_acquire() {
        Ok(sill::Acquisition::Acquired(possession)) => Ok(possession),
        Ok(sill::Acquisition::Contended) => Err(refusal(contended.0, None, contended.1)),
        Err(error) => Err(environment_fault(unavailable, error.to_string())),
    }
}

/// The Plan the apply would file, each child's fingerprints with value, bound,
/// and provenance, and the exact command that files it.
fn preview_text(draft: &SweepDraft, digest: &Digest, until: Option<&String>) -> String {
    let mut text = String::new();
    if draft.content.children.is_empty() {
        text.push_str("sweep: nothing to file");
    } else {
        let _ = write!(text, "sweep: {}", draft.content.plan.title);
        match &draft.content.until {
            Some(until) if until.is_indefinite() => text.push_str(", deferred until woken"),
            Some(until) => {
                let _ = write!(text, ", deferred until {until}");
            }
            None => {}
        }
        for child in &draft.content.children {
            let _ = write!(text, "\n\nchild: {}\n{}", child.title, child.description);
        }
    }
    if draft.held > 0 {
        let _ = write!(
            text,
            "\n\n{} latched fingerprints are already held by pending sweep tasks",
            draft.held
        );
    }
    if !draft.content.children.is_empty() {
        let until = until.map_or_else(String::new, |until| format!(" --until {until}"));
        let _ = write!(text, "\n\nnext: spice sweep{until} --apply={digest}");
    }
    text
}

#[cfg(test)]
mod tests {
    use spice::gate::arms::ArmTiming;
    use spice::gate::sweep::Held;

    use super::*;

    #[test]
    fn repeated_split_selections_refuse_but_distinct_arms_remain_admissible() {
        let app = "cargo:test:app::lib:app".to_owned();
        let same_app = app.clone();
        let wire = "cargo:test:wire::lib:wire".to_owned();
        for (selection, repeated) in [
            (&[&app, &wire][..], false),
            (&[&app, &same_app][..], true),
            (&[&app, &wire, &same_app][..], true),
        ] {
            let refused = match require_unique_splits(selection) {
                Ok(()) => false,
                Err(Outcome::Refusal(ref refusal))
                    if refusal.reason() == "sweep::split-repeated" =>
                {
                    true
                }
                other => panic!("unexpected selection outcome: {other:?}"),
            };
            assert_eq!(refused, repeated, "selection: {selection:?}");
        }
    }

    #[test]
    fn moved_arm_timings_refuse_the_previewed_apply() {
        let ledger: gate::StateLedger = serde_json::from_value(serde_json::json!({
            "version": gate::state::STATE_LEDGER_VERSION,
            "latches": [],
            "origins": [],
            "claims": [],
        }))
        .expect("empty gate state admits");
        let previewed = sweep::draft(
            &ledger,
            &[ArmTiming {
                arm: "cargo:test:app::lib:app".to_owned(),
                millis: vec![61_000],
            }],
            &Held::default(),
            None,
        )
        .content
        .digest();
        let current = sweep::draft(
            &ledger,
            &[ArmTiming {
                arm: "cargo:test:app::lib:app".to_owned(),
                millis: vec![59_000],
            }],
            &Held::default(),
            None,
        )
        .content
        .digest();

        let refusal =
            require_current_preview(&previewed, &current).expect_err("moved timing refuses apply");
        assert!(
            matches!(
                refusal,
                Outcome::Refusal(ref refusal) if refusal.reason() == "sweep::preview-moved"
            ),
            "{refusal:?}"
        );
    }
}

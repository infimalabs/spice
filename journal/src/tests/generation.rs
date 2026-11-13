//! The task generation the journal settles at every commit, the claim
//! acknowledgement settled beside it, and the index of every advance.

use super::claim_index::{add_claimed_task, query_plan};
use super::*;

use crate::{FactCause, JournalWrite};
use wire::body::{ClaimFreed, TaskWoken};
use wire::{Producer, TaskStamp};

const HOLDER: &str = "018f82d4-6f59-7d06-92e3-bce913116360";

#[test]
fn mutations_advance_once_per_transaction_and_only_the_holder_keeps_pace() {
    let scratch = Scratch::new();
    let path = scratch.path().join("generation.sqlite3");
    let mut journal = authority_file_fixture(&path);
    let holder = Actor::Lane(uuid(HOLDER));
    let task = add_claimed_task(&mut journal, 1, holder.clone());
    let transition = workflow(&journal, &task).position();
    assert_eq!(settled(&journal, &task), (0, 0));

    let first = note(&mut journal, &holder, &task, 1);
    assert_eq!(
        settled(&journal, &task),
        (1, 1),
        "a holder sees its own change"
    );

    let applied = note(&mut journal, &Actor::TEST_PRINCIPAL, &task, 2);
    assert_eq!(
        settled(&journal, &task),
        (2, 1),
        "two notes, one transaction"
    );
    let changed = journal
        .read(|read| read.changed_task_heads(applied.first(), applied.last()))
        .expect("changed heads read");
    assert_eq!(
        changed
            .iter()
            .map(|heads| heads.statement().body().born.clone())
            .collect::<Vec<_>>(),
        vec![task.clone()],
        "an in-place revision is a change a position-following reader sees"
    );

    let third = note(&mut journal, &holder, &task, 1);
    assert_eq!(
        settled(&journal, &task),
        (3, 1),
        "a trailing claim stays behind"
    );
    free(&mut journal, &task);
    assert_eq!(
        settled(&journal, &task),
        (3, 1),
        "claim maintenance is no mutation"
    );
    assert_eq!(
        workflow(&journal, &task).position(),
        transition,
        "no mutation here moved the workflow transition"
    );
    let recorded = [(1, first.last()), (2, applied.last()), (3, third.last())];
    assert_eq!(advances(&journal, &task, 0, 8), recorded);
    assert_eq!(
        advances(&journal, &task, 1, 1),
        recorded[1..2],
        "the read resumes past a generation and honors its bound"
    );
    let plan = query_plan(
        &journal,
        "SELECT generation, seq FROM task_generations \
         WHERE stamp = 'x' AND generation > 0 ORDER BY generation LIMIT 8",
    );
    assert!(
        plan.contains("SEARCH task_generations USING PRIMARY KEY") && !plan.contains("SCAN"),
        "one task's advances are a key range, not a scan: {plan}"
    );

    let before = heads(&journal, &task);
    drop(journal);
    let journal = Journal::open(&path).expect("generation authority reopens");
    assert_eq!(heads(&journal, &task), before);
    assert_eq!(advances(&journal, &task, 0, 8), recorded);
    journal.verify().expect("generation authority verifies");
}

#[test]
fn verification_names_an_index_that_disagrees_with_its_task() {
    for (corruption, subject) in [
        (
            "DELETE FROM task_generations WHERE generation = 1",
            "task_generations:{task}",
        ),
        (
            "DELETE FROM task_generations WHERE generation = 2",
            "task_generations:{task}",
        ),
        (
            "UPDATE task_generations SET baseline = 1 WHERE generation = 1",
            "task_generations:{task}",
        ),
        (
            "UPDATE task_generations SET seq = (SELECT min(seq) FROM fact_envelopes) \
             WHERE generation = 2",
            "task_generations:{task}:2",
        ),
    ] {
        let mut journal = authority_fixture();
        let task = add_claimed_task(&mut journal, 1, Actor::Lane(uuid(HOLDER)));
        note(&mut journal, &Actor::TEST_PRINCIPAL, &task, 1);
        note(&mut journal, &Actor::TEST_PRINCIPAL, &task, 1);
        journal.verify().expect("an intact index verifies");
        journal
            .connection
            .execute_batch(corruption)
            .expect("corrupt the index");
        let Err(crate::VerifyError::Violations(violations)) = journal.verify() else {
            panic!("verification accepted {corruption}");
        };
        let subject = subject.replace("{task}", task.as_str());
        assert!(
            violations
                .iter()
                .any(|violation| violation.subject == subject),
            "{corruption} was not named at {subject}: {violations:?}"
        );
    }
}

/// One task's recorded advances past `after`, at most `limit`.
fn advances(journal: &Journal, task: &TaskStamp, after: u64, limit: usize) -> Vec<(u64, Seq)> {
    journal
        .read(|read| read.task_generations_after(task, after, limit))
        .expect("generation index reads")
        .into_iter()
        .map(|advance| (advance.generation, advance.position))
        .collect()
}

/// Commit `count` wakes of `task` from `actor` in one transaction.
fn note(
    journal: &mut Journal,
    actor: &Actor,
    task: &TaskStamp,
    count: usize,
) -> crate::guard::Applied {
    let mut write = JournalWrite::new(
        actor.clone(),
        "task:claim-index-peer".parse().expect("claim origin"),
        instant(),
        Producer::CommandDoor,
    );
    for _ in 0..count {
        write
            .fact(TaskWoken { task: task.clone() }, FactCause::Root)
            .expect("wake fact builds");
    }
    journal.apply(write).expect("wakes commit")
}

fn free(journal: &mut Journal, task: &TaskStamp) {
    let mut head = heads(journal, task)
        .latest_claim()
        .expect("claim exists")
        .claim()
        .clone()
        .into_body();
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:claim-index-peer".parse().expect("claim origin"),
        instant(),
        Producer::CommandDoor,
    );
    let fact = write
        .fact(
            ClaimFreed {
                task: task.clone(),
                actor: Actor::TEST_PRINCIPAL,
                parked: None,
            },
            FactCause::Existing(head.claimed),
        )
        .expect("release fact builds");
    head.disposition = ClaimDisposition::Freed;
    write.claim_head(task.clone(), fact, move |position| {
        head.fact = position;
        head
    });
    journal.apply(write).expect("release commits");
}

fn workflow(journal: &Journal, task: &TaskStamp) -> crate::guard::JournalHead<WorkflowHead> {
    journal
        .read(|read| read.workflow_head(task))
        .expect("workflow reads")
        .expect("workflow exists")
}

/// The task generation and its claim's acknowledgement.
fn settled(journal: &Journal, task: &TaskStamp) -> (u64, u64) {
    let bodies = heads(journal, task).into_bodies();
    (
        bodies.workflow.generation,
        bodies.claims.last().expect("claim exists").0.acknowledged,
    )
}

fn heads(journal: &Journal, task: &TaskStamp) -> crate::guard::TaskHeads {
    journal
        .read(|read| read.task_heads_for(task))
        .expect("task heads read")
        .expect("task exists")
}

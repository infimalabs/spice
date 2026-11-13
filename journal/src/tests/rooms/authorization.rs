//! Preparation decisions and execution authorization.

use super::*;

#[test]
fn authorization_matches_one_exact_selected_decision() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = Preparation::new(&mut journal);
    adopt(
        &mut journal,
        &preparation.spare,
        prepared_by(&preparation.controller),
        prepared_by(&preparation.controller),
    )
    .expect("spare output adopted");
    let output = vec![preparation.output.clone()];
    let result = authorize(&mut journal, preparation.authorization(1, output.clone()));
    refused(&journal, result);

    commit(
        &mut journal,
        preparation.decision(1, selected(output.clone())),
    )
    .expect("selected decision");
    for inexact in [
        preparation.authorization(1, vec![preparation.spare.clone()]),
        preparation.authorization(
            1,
            vec![preparation.output.clone(), preparation.spare.clone()],
        ),
    ] {
        let result = authorize(&mut journal, inexact);
        refused(&journal, result);
    }
    authorize(&mut journal, preparation.authorization(1, output.clone()))
        .expect("exact selection authorized");

    // A sealed selection admits no second authorization of the same output.
    let result = authorize(&mut journal, preparation.authorization(1, output.clone()));
    refused(&journal, result);
    let result = authorize(&mut journal, preparation.authorization(2, output));
    refused(&journal, result);
    journal.verify().expect("exact authority verifies");
}

#[test]
fn a_collapse_authorizes_nothing() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = Preparation::new(&mut journal);
    commit(
        &mut journal,
        preparation.decision(
            1,
            DecisionOutcome::Collapsed {
                reason: "the premise did not hold".to_owned(),
            },
        ),
    )
    .expect("collapse records");
    let result = authorize(
        &mut journal,
        preparation.authorization(1, vec![preparation.output.clone()]),
    );
    refused(&journal, result);
    let granted = journal
        .read(|read| read.authorization_head(&preparation.output))
        .expect("authorization reads");
    assert_eq!(granted, None);
    journal.verify().expect("collapse authority verifies");
}

#[test]
fn only_the_operator_decides_and_authorizes() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = Preparation::new(&mut journal);
    let author = uuid("019fe57c-a453-7ec1-a400-26e6901aaeb1");
    let selected = preparation.decision(1, selected(vec![preparation.output.clone()]));
    let result = commit_as(&mut journal, Actor::Lane(author), selected.clone());
    refused(&journal, result);
    commit(&mut journal, selected).expect("the operator decides");

    let exact = preparation.authorization(1, vec![preparation.output.clone()]);
    let write = authorization_write(Actor::Lane(author), exact.clone());
    let result = journal.apply(write);
    refused(&journal, result);
    authorize(&mut journal, exact).expect("the operator authorizes the exact selection");

    journal
        .verify()
        .expect("principal-bound selection verifies");
}

#[test]
fn a_controller_concluded_without_authorization_never_authorizes() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = Preparation::new(&mut journal);
    commit(
        &mut journal,
        preparation.decision(1, selected(vec![preparation.output.clone()])),
    )
    .expect("selected decision");

    let controller = preparation.controller.clone();
    let workflow = journal
        .read(|read| read.workflow_head(&controller))
        .expect("controller workflow reads")
        .expect("controller exists")
        .into_body();
    let mut write = write();
    let done = write
        .fact(
            wire::body::TaskDone {
                task: controller.clone(),
                validation: "ordinary completion".to_owned(),
                disposition: None,
                terminal: None,
                git: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("completion builds");
    write.workflow_head(controller, done, move |_| WorkflowHead {
        status: wire::WorkflowStatus::completed(instant()),
        ..workflow
    });
    journal.apply(write).expect("ordinary completion commits");

    let result = authorize(
        &mut journal,
        preparation.authorization(1, vec![preparation.output.clone()]),
    );
    assert!(
        matches!(&result, Err(CommitError::Refused(_))),
        "authorization cannot follow completion: {result:?}"
    );
    refused(&journal, result);
    assert!(
        statement(&journal, &preparation.output)
            .dependencies
            .contains(&preparation.controller)
    );
    journal.verify().expect("unauthorized completion verifies");
}

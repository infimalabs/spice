use super::*;

#[test]
fn active_claim_refuses_a_lease_whose_start_has_not_arrived() {
    let entry = claimed_entry("BOARD-a1", "todo");

    // The sibling case bounds the lease from above. This one bounds it from
    // below, and the two are separate guards rather than one interval test: a
    // claim written ahead of its start is a lease nobody holds yet, and the
    // refusal carries the instant it begins so the caller need not read it back
    // off a head it was just refused possession of.
    assert!(matches!(
        Active::admit(&entry, &Actor::Spice, instant("2026-08-01T05:59:59.999Z")),
        Err(ClaimRefusal::NotStarted { start }) if start == instant("2026-08-01T06:00:00.000Z")
    ));

    // The boundary is inclusive at the start instant, which fixes the
    // comparison in both directions. A guard reading `now <= lease.start` would
    // refuse this admission, and one reading `now < lease.renewal` would refuse
    // it as well.
    Active::admit(&entry, &Actor::Spice, instant("2026-08-01T06:00:00.000Z"))
        .expect("a lease is held from its start instant");
}

#[test]
fn active_claim_refuses_a_split_epoch_and_each_out_of_order_lease_bound() {
    let live = instant("2026-08-01T06:05:00.000Z");

    // The epoch pair is what binds a lease to the claim that opened it. A lease
    // renewed under one epoch cannot vouch for a claim recorded under another,
    // so the refusal names both sides rather than reporting a bare disagreement.
    let mut divergent = claim(ClaimDisposition::Active);
    divergent.epoch = uuid("019fbbde-5105-75a3-a76a-722afde1e411");
    let split = admitted(
        statement("BOARD-a1"),
        workflow("todo"),
        Some(divergent),
        Some(lease()),
    )
    .expect("valid heads");
    assert!(matches!(
        Active::admit(&split, &Actor::Spice, live),
        Err(ClaimRefusal::Epoch {
            claim: recorded,
            lease: vouching,
        }) if recorded == uuid("019fbbde-5105-75a3-a76a-722afde1e411")
            && vouching == uuid("019fbbde-5105-75a3-a76a-722afde1e410")
    ));

    // Coherence is a chain of three inequalities, and one case can only prove
    // one link. Each lease below breaks exactly one and satisfies the other two,
    // so no surviving comparison can carry the refusal for its neighbours.
    for (broken, incoherent) in [
        (
            "renewal before start",
            LeaseHead {
                renewal: instant("2026-08-01T05:59:00.000Z"),
                ..lease()
            },
        ),
        (
            "renewal after expiry",
            LeaseHead {
                renewal: instant("2026-08-01T06:11:00.000Z"),
                ..lease()
            },
        ),
        (
            "expiry past maximum",
            LeaseHead {
                expiry: instant("2026-08-01T06:21:00.000Z"),
                ..lease()
            },
        ),
    ] {
        let entry = admitted(
            statement("BOARD-a1"),
            workflow("todo"),
            Some(claim(ClaimDisposition::Active)),
            Some(incoherent),
        )
        .expect("valid heads");
        assert!(
            matches!(
                Active::admit(&entry, &Actor::Spice, live),
                Err(ClaimRefusal::IncoherentLease)
            ),
            "{broken} leaves the lease incoherent",
        );
    }
}

#[test]
fn workflow_advances_only_one_declared_station_and_completes_only_the_last() {
    let todo = claimed_entry("BOARD-a1", "todo");
    let active = Active::admit(&todo, &Actor::Spice, instant("2026-08-01T06:05:00.000Z"))
        .expect("active claim");
    let advance = Transition::advance(&todo, &active, phase("review")).expect("next station");
    assert_eq!(advance.next().phase, phase("review"));
    assert!(matches!(
        Transition::advance(&todo, &active, phase("todo")),
        Err(PhaseRefusal::Illegal { .. })
    ));
    assert!(matches!(
        Transition::complete(&todo, &active, instant("2026-08-01T06:06:00.000Z")),
        Err(PhaseRefusal::NotFinal(_))
    ));

    let review = claimed_entry("BOARD-a1", "review");
    let active = Active::admit(&review, &Actor::Spice, instant("2026-08-01T06:05:00.000Z"))
        .expect("active claim");
    let complete = Transition::complete(&review, &active, instant("2026-08-01T06:06:00.000Z"))
        .expect("final station completes");
    assert!(complete.next().status.is_completed());
    assert_eq!(
        complete.next().status.completion(),
        Some(instant("2026-08-01T06:06:00.000Z"))
    );
}

#[test]
fn active_claim_for_another_task_cannot_advance_workflow() {
    let owned = claimed_entry("BOARD-a1", "todo");
    let active = Active::admit(&owned, &Actor::Spice, instant("2026-08-01T06:05:00.000Z"))
        .expect("active claim");
    let other = claimed_entry("BOARD-b2", "todo");

    assert_eq!(
        Transition::advance(&other, &active, phase("review")).expect_err("claim is task-bound"),
        PhaseRefusal::ClaimForAnotherTask
    );
}

#[test]
fn completed_workflow_refuses_both_advance_and_completion() {
    let completed = completed_entry("BOARD-a1");
    let active = Active::admit(
        &completed,
        &Actor::Spice,
        instant("2026-08-01T06:05:00.000Z"),
    )
    .expect("active claim");

    assert_eq!(
        Transition::advance(&completed, &active, phase("todo"))
            .expect_err("completed workflow cannot advance"),
        PhaseRefusal::Completed
    );
    assert_eq!(
        Transition::complete(&completed, &active, instant("2026-08-01T06:07:00.000Z"))
            .expect_err("completed workflow cannot complete again"),
        PhaseRefusal::Completed
    );
}

#[test]
fn advance_target_outside_declared_flow_is_named() {
    let entry = claimed_entry("BOARD-a1", "todo");
    let active = Active::admit(&entry, &Actor::Spice, instant("2026-08-01T06:05:00.000Z"))
        .expect("active claim");
    let outside = phase("design");

    assert_eq!(
        Transition::advance(&entry, &active, outside).expect_err("outside target refuses"),
        PhaseRefusal::TargetOutsideFlow(outside)
    );
}

#[test]
fn urgency_is_durable_topic_age_and_ignores_unrelated_edits() {
    let plain = entry("BOARD-a1");
    let mut edited_head = statement("BOARD-b2");
    edited_head.topic_origin = plain.statement().topic_origin;
    edited_head.revision = seq(99);
    edited_head.priority = Some(wire::TaskPriority::High);
    edited_head.dependencies.push(stamp("BOARD-d4"));
    let edited = admitted(edited_head, workflow("review"), None, None).expect("valid heads");
    let mut newer_head = statement("BOARD-c3");
    newer_head.topic_origin = seq(2);
    let newer = admitted(newer_head, workflow("todo"), None, None).expect("valid heads");

    assert_eq!(
        Urgency::from_heads(&plain),
        Urgency::from_heads(&edited),
        "statement, dependency, revision and phase changes do not reset age",
    );
    assert!(Urgency::from_heads(&plain) > Urgency::from_heads(&newer));
}

#[test]
fn independent_handoff_prefers_a_peer_then_returns_to_its_prior_author() {
    let mut independent_head = statement("BOARD-a1");
    independent_head.priority = Some(wire::TaskPriority::Critical);
    let prior = Actor::Spice;
    let independent = admitted(
        independent_head,
        workflow("todo"),
        Some(claim(ClaimDisposition::Freed)),
        Some(lease()),
    )
    .expect("independent phase")
    .with_phase_handoff(Some(board::PhaseHandoff::new(
        prior.clone(),
        instant("2026-08-01T06:00:03.000Z"),
        instant("2026-08-01T06:00:30.000Z"),
    )));
    let peer = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e411"));
    let before = instant("2026-08-01T06:00:02.000Z");
    assert!(
        board::select_at(
            std::slice::from_ref(&independent),
            &prior,
            &all_topics(),
            board::NextConfig::default(),
            &environment(),
            before,
        )
        .is_none(),
        "the prior author waits through the independent handoff opportunity",
    );
    assert_eq!(
        board::blocked_handoff_until(
            std::slice::from_ref(&independent),
            &board::resolved_tasks(std::slice::from_ref(&independent)),
            &prior,
            &all_topics(),
            &environment(),
            before,
        ),
        Some(instant("2026-08-01T06:00:03.000Z")),
        "lifecycle can arm the prior author's fallback deadline",
    );
    assert_eq!(
        board::select_at(
            std::slice::from_ref(&independent),
            &peer,
            &all_topics(),
            board::NextConfig::default(),
            &environment(),
            before,
        )
        .expect("an eligible peer receives the independent phase")
        .entry()
        .handle(),
        independent.handle(),
    );

    let peer_work = entry("BOARD-z1");
    let entries = [independent.clone(), peer_work];
    let after = instant("2026-08-01T06:00:03.000Z");
    let selected = board::select_at(
        &entries,
        &prior,
        &all_topics(),
        board::NextConfig::default(),
        &environment(),
        after,
    )
    .expect("ordinary work remains eligible");
    assert_eq!(selected.entry().handle(), &handle("BOARD-z1"));
    let returned = board::select_at(
        std::slice::from_ref(&independent),
        &prior,
        &all_topics(),
        board::NextConfig::default(),
        &environment(),
        after,
    )
    .expect("the prior author can progress when no peer takes the phase");
    assert_eq!(returned.entry().handle(), &handle("BOARD-a1"));
    assert_eq!(
        board::blocked_handoff_until(
            std::slice::from_ref(&independent),
            &board::resolved_tasks(std::slice::from_ref(&independent)),
            &prior,
            &all_topics(),
            &environment(),
            after,
        ),
        None,
    );
}
fn peer_decision(
    entry: &Entry,
    prior: &Actor,
    satisfied: &board::ResolvedTasks,
    peer: board::HandoffPeer<'_>,
    now: Instant,
) -> Option<Instant> {
    board::independent_handoff_awaits_peer(
        entry,
        prior,
        satisfied,
        || Ok::<_, std::convert::Infallible>([peer]),
        now,
    )
    .expect("fixture peer evidence is infallible")
}

#[test]
fn independent_handoff_waits_only_for_an_eligible_unclaimed_peer_and_only_until_its_bound() {
    let prior = Actor::Spice;
    let peer = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e411"));
    let required_schema = journal::SCHEMA_VERSION
        .checked_add(1)
        .expect("fixture schema advances");
    let mut head = statement("BOARD-peer-handoff");
    head.requirements = wire::EnvironmentRequirements::one(
        wire::EnvironmentRequirement::journal_schema(required_schema)
            .expect("future schema is positive"),
    );
    let entry = admitted(head, workflow("review"), None, None)
        .expect("peer handoff entry")
        .with_phase_handoff(Some(board::PhaseHandoff::new(
            prior.clone(),
            instant("2026-08-01T06:00:03.000Z"),
            instant("2026-08-01T06:00:30.000Z"),
        )));
    let entries = [entry.clone()];
    let satisfied = board::resolved_tasks(&entries);
    let subscription = all_topics();
    let eligible_environment =
        wire::Environment::journal_schema(required_schema).expect("future schema is positive");
    let ineligible_environment = environment();
    let now = instant("2026-08-01T06:00:04.000Z");
    let evidence =
        |holds_live_claim, subscription: &board::Allocation, environment| board::HandoffPeer {
            actor: peer.clone(),
            subscription: subscription.clone(),
            environment,
            holds_live_claim,
        };

    assert_eq!(
        peer_decision(
            &entry,
            &prior,
            &satisfied,
            evidence(false, &subscription, &eligible_environment),
            now,
        ),
        Some(instant("2026-08-01T06:00:30.000Z")),
    );
    assert_eq!(
        peer_decision(
            &entry,
            &prior,
            &satisfied,
            evidence(true, &subscription, &eligible_environment),
            now,
        ),
        None,
        "a peer already holding live work cannot withhold the handoff",
    );
    let wrong_team = support::allocation(uuid("019fbbde-5105-75a3-a76a-722afde1e408"), false);
    assert_eq!(
        peer_decision(
            &entry,
            &prior,
            &satisfied,
            evidence(false, &wrong_team, &eligible_environment),
            now,
        ),
        None,
        "a non-admitting subscription cannot withhold the handoff",
    );
    assert_eq!(
        peer_decision(
            &entry,
            &prior,
            &satisfied,
            evidence(false, &subscription, &ineligible_environment),
            now,
        ),
        None,
        "a peer whose environment refuses the row cannot withhold it",
    );
    assert_eq!(
        peer_decision(
            &entry,
            &prior,
            &satisfied,
            evidence(false, &subscription, &eligible_environment),
            instant("2026-08-01T06:00:30.000Z"),
        ),
        None,
        "the peer-first interval is bounded from the phase transition",
    );
    let shared = entry
        .clone()
        .with_phase_handoff(Some(board::PhaseHandoff::from_authors(
            [prior.clone(), peer.clone()],
            instant("2026-08-01T06:00:03.000Z"),
            instant("2026-08-01T06:00:30.000Z"),
        )));
    assert_eq!(
        peer_decision(
            &shared,
            &prior,
            &satisfied,
            evidence(false, &subscription, &eligible_environment),
            now,
        ),
        None,
        "a co-author of the handed-off phase is no independent peer",
    );
}

#[test]
fn allocation_is_deterministic_and_has_one_eligibility_path() {
    let first = entry("BOARD-a1");
    let second = entry("BOARD-b2");

    let mut blocked_head = statement("BOARD-z9");
    blocked_head.priority = Some(wire::TaskPriority::Critical);
    blocked_head.dependencies.push(stamp("WIRE-missing"));
    let blocked =
        admitted(blocked_head, workflow("todo"), None, None).expect("valid blocked heads");

    let active = claimed_entry("BOARD-x7", "todo");
    let mut deferred_workflow = workflow("todo");
    deferred_workflow.deferral = Some(instant("2026-08-01T07:00:00.000Z"));
    let mut deferred_head = statement("BOARD-w6");
    deferred_head.priority = Some(wire::TaskPriority::Critical);
    let deferred =
        admitted(deferred_head, deferred_workflow, None, None).expect("valid deferred heads");

    let mut completed_workflow = workflow("todo");
    completed_workflow.status =
        wire::WorkflowStatus::completed(instant("2026-08-01T06:30:00.000Z"));
    let mut completed_head = statement("BOARD-v5");
    completed_head.priority = Some(wire::TaskPriority::Critical);
    let completed =
        admitted(completed_head, completed_workflow, None, None).expect("valid completed heads");
    let ordered = [
        blocked.clone(),
        second.clone(),
        first.clone(),
        active.clone(),
        deferred.clone(),
        completed.clone(),
    ];
    let reversed = [completed, deferred, active, first, second, blocked];

    let selected = select(&ordered, &Actor::Spice, &all_topics()).expect("eligible task");
    let selected_again = select(&reversed, &Actor::Spice, &all_topics()).expect("eligible task");
    assert_eq!(selected.entry().handle(), &handle("BOARD-a1"));
    assert_eq!(selected_again.entry().handle(), selected.entry().handle());
    assert_eq!(selected_again.urgency(), selected.urgency());
}

#[test]
fn prerequisite_inherits_the_critical_priority_it_directly_unblocks() {
    let prerequisite = entry("BOARD-z1");
    let mut critical_head = statement("BOARD-c1");
    critical_head.priority = Some(wire::TaskPriority::Critical);
    critical_head.dependencies.push(stamp("BOARD-z1"));
    let critical = admitted(critical_head, workflow("todo"), None, None).expect("critical task");
    let mut medium_head = statement("BOARD-m1");
    medium_head.priority = Some(wire::TaskPriority::Medium);
    let medium = admitted(medium_head, workflow("todo"), None, None).expect("medium task");
    let ordered = [prerequisite.clone(), critical.clone(), medium.clone()];
    let reversed = [medium, critical, prerequisite];

    let selected = select(&ordered, &Actor::Spice, &all_topics()).expect("eligible task");
    let selected_again = select(&reversed, &Actor::Spice, &all_topics()).expect("eligible task");
    assert_eq!(selected.entry().handle(), &handle("BOARD-z1"));
    assert_eq!(selected_again.entry().handle(), selected.entry().handle(),);
    assert_eq!(selected_again.urgency(), selected.urgency());
}

#[test]
fn submitted_draft_dependencies_join_the_priority_graph() {
    for (submitted, expected) in [(false, handle("BOARD-m1")), (true, handle("BOARD-z1"))] {
        let prerequisite = entry("BOARD-z1");
        let mut critical_head = statement("BOARD-c1");
        critical_head.priority = Some(wire::TaskPriority::Critical);
        critical_head.dependencies.push(stamp("BOARD-z1"));
        let mut draft = workflow("draft");
        draft.submitted = submitted;
        let critical = admitted(critical_head, draft, None, None).expect("valid draft");
        let mut medium_head = statement("BOARD-m1");
        medium_head.priority = Some(wire::TaskPriority::Medium);
        let medium =
            admitted(medium_head, workflow("review"), None, None).expect("valid review task");
        let entries = [prerequisite, critical, medium];

        let selected =
            select(&entries, &Actor::Spice, &all_topics()).expect("one eligible task remains");
        assert_eq!(selected.entry().handle(), &expected);
    }
}

#[test]
fn prerequisite_inherits_priority_through_a_two_edge_chain() {
    let first = entry("BOARD-z1");
    let mut second_head = statement("BOARD-z2");
    second_head.dependencies.push(stamp("BOARD-z1"));
    let second = admitted(second_head, workflow("todo"), None, None).expect("second prerequisite");
    let mut critical_head = statement("BOARD-c1");
    critical_head.priority = Some(wire::TaskPriority::Critical);
    critical_head.dependencies.push(stamp("BOARD-z2"));
    let critical = admitted(critical_head, workflow("todo"), None, None).expect("critical task");
    let mut medium_head = statement("BOARD-m1");
    medium_head.priority = Some(wire::TaskPriority::Medium);
    let medium = admitted(medium_head, workflow("todo"), None, None).expect("medium task");
    let entries = [first, second, critical, medium];

    let selected = select(&entries, &Actor::Spice, &all_topics()).expect("eligible task");
    assert_eq!(selected.entry().handle(), &handle("BOARD-z1"));
}

#[test]
fn priority_inheritance_terminates_on_a_dependency_cycle() {
    let mut first_head = statement("BOARD-z1");
    first_head.priority = Some(wire::TaskPriority::Critical);
    first_head.dependencies.push(stamp("BOARD-z2"));
    let first = admitted(first_head, workflow("todo"), None, None).expect("first cycle entry");
    let mut second_head = statement("BOARD-z2");
    second_head.dependencies.push(stamp("BOARD-z1"));
    let second = admitted(second_head, workflow("todo"), None, None).expect("second cycle entry");
    let medium = entry("BOARD-m1");

    let entries = [first, second, medium];
    let selected = select(&entries, &Actor::Spice, &all_topics())
        .expect("cycle leaves the independent task eligible");
    assert_eq!(selected.entry().handle(), &handle("BOARD-m1"));
}

#[test]
fn downstream_weight_precedes_native_urgency_and_handle_order() {
    let eager = entry("BOARD-a1");
    let mut weighted_head = statement("BOARD-z1");
    weighted_head.topic_origin = seq(10);
    let weighted =
        admitted(weighted_head, workflow("todo"), None, None).expect("weighted candidate");
    let mut direct_head = statement("BOARD-z2");
    direct_head.dependencies.push(stamp("BOARD-z1"));
    let direct = admitted(direct_head, workflow("todo"), None, None).expect("direct dependent");
    let mut transitive_head = statement("BOARD-z3");
    transitive_head.dependencies.push(stamp("BOARD-z2"));
    let transitive =
        admitted(transitive_head, workflow("todo"), None, None).expect("transitive dependent");

    assert!(
        Urgency::from_heads(&eager) > Urgency::from_heads(&weighted),
        "native urgency favors the unweighted candidate",
    );
    let ordered = [
        eager.clone(),
        weighted.clone(),
        direct.clone(),
        transitive.clone(),
    ];
    let reversed = [transitive, direct, weighted, eager];

    // Removing the downstream rung makes urgency choose BOARD-a1; retaining
    // the rung but moving it above effective priority is caught separately.
    let selected =
        select(&ordered, &Actor::Spice, &all_topics()).expect("weighted candidate is ready");
    let selected_again = select(&reversed, &Actor::Spice, &all_topics())
        .expect("reversed weighted candidate is ready");
    assert_eq!(selected.entry().handle(), &handle("BOARD-z1"));
    assert_eq!(selected.entry_index(), 1);
    assert_eq!(selected_again.entry_index(), 2);
    assert_eq!(selected.downstream_weight(), 2);
    assert_eq!(selected_again.entry().handle(), selected.entry().handle(),);
    assert_eq!(
        selected_again.downstream_weight(),
        selected.downstream_weight(),
    );
    assert_eq!(selected_again.urgency(), selected.urgency());
}

#[test]
fn effective_priority_precedes_greater_downstream_weight() {
    let mut priority_head = statement("BOARD-z9");
    priority_head.priority = Some(wire::TaskPriority::Medium);
    let priority =
        admitted(priority_head, workflow("todo"), None, None).expect("higher-priority candidate");
    let weighted = entry("BOARD-a1");
    let mut direct_head = statement("BOARD-a2");
    direct_head.dependencies.push(stamp("BOARD-a1"));
    let direct = admitted(direct_head, workflow("todo"), None, None).expect("direct dependent");
    let mut transitive_head = statement("BOARD-a3");
    transitive_head.dependencies.push(stamp("BOARD-a2"));
    let transitive =
        admitted(transitive_head, workflow("todo"), None, None).expect("transitive dependent");
    let entries = [weighted, direct, transitive, priority];

    // Moving downstream weight above effective priority makes BOARD-a1 win.
    let selected =
        select(&entries, &Actor::Spice, &all_topics()).expect("priority candidate is ready");
    assert_eq!(selected.entry().handle(), &handle("BOARD-z9"));
    assert_eq!(selected.downstream_weight(), 0);
}

#[test]
fn downstream_weight_ignores_draft_and_settled_branches() {
    let root = entry("BOARD-a1");
    let mut draft_head = statement("BOARD-h1");
    draft_head.topic = "/board/correctness".parse().expect("fixture topic");
    let hidden = admitted(draft_head, workflow("draft"), None, None).expect("draft dependent");
    let mut after_hidden_head = statement("BOARD-z1");
    after_hidden_head.dependencies.push(stamp("BOARD-h1"));
    let after_hidden = admitted(after_hidden_head, workflow("todo"), None, None)
        .expect("public task beyond hidden branch");

    let mut settled_head = statement("BOARD-s1");
    settled_head.dependencies.push(stamp("BOARD-a1"));
    let mut settled_workflow = workflow("review");
    settled_workflow.status = wire::WorkflowStatus::completed(instant("2026-08-01T06:06:00.000Z"));
    let settled = admitted(
        settled_head,
        settled_workflow,
        Some(claim(ClaimDisposition::Active)),
        Some(lease()),
    )
    .expect("settled dependent");
    let mut after_settled_head = statement("BOARD-z2");
    after_settled_head.dependencies.push(stamp("BOARD-s1"));
    let after_settled = admitted(after_settled_head, workflow("todo"), None, None)
        .expect("public task beyond settled branch");
    let ordered = [
        root.clone(),
        hidden.clone(),
        after_hidden.clone(),
        settled.clone(),
        after_settled.clone(),
    ];
    let reversed = [after_settled, settled, after_hidden, hidden, root];

    let selected = select(&ordered, &Actor::Spice, &all_topics())
        .expect("root remains an unweighted candidate");
    let selected_again = select(&reversed, &Actor::Spice, &all_topics())
        .expect("reversed root remains an unweighted candidate");
    assert_eq!(selected.entry().handle(), &handle("BOARD-a1"));
    assert_eq!(selected.downstream_weight(), 0);
    assert_eq!(selected_again.entry().handle(), selected.entry().handle(),);
    assert_eq!(selected_again.downstream_weight(), 0);
}

#[test]
fn downstream_weight_terminates_on_a_dependency_cycle() {
    let root = entry("BOARD-z1");
    let mut first_head = statement("BOARD-z2");
    first_head
        .dependencies
        .extend([stamp("BOARD-z1"), stamp("BOARD-z3")]);
    let first = admitted(first_head, workflow("todo"), None, None).expect("first cycle entry");
    let mut second_head = statement("BOARD-z3");
    second_head.dependencies.push(stamp("BOARD-z2"));
    let second = admitted(second_head, workflow("todo"), None, None).expect("second cycle entry");
    let ordered = [root.clone(), first.clone(), second.clone()];
    let reversed = [second, first, root];

    let selected = select(&ordered, &Actor::Spice, &all_topics()).expect("cycle root is ready");
    let selected_again =
        select(&reversed, &Actor::Spice, &all_topics()).expect("reversed cycle root is ready");
    assert_eq!(selected.entry().handle(), &handle("BOARD-z1"));
    assert_eq!(selected.downstream_weight(), 2);
    assert_eq!(selected_again.entry().handle(), selected.entry().handle(),);
    assert_eq!(selected_again.downstream_weight(), 2);
}

#[test]
fn diagrams_and_handout_are_stable_across_input_order() {
    let prerequisite = entry("BOARD-a1");
    let mut dependent_head = statement("BOARD-b2");
    dependent_head.dependencies.push(stamp("BOARD-a1"));
    let dependent = admitted(dependent_head, workflow("todo"), None, None).expect("valid heads");
    let expected_edge = format!("{} -> {}", dependent.handle(), prerequisite.handle());
    let first = [dependent.clone(), prerequisite.clone()];
    let second = [prerequisite, dependent];

    for view in View::ALL {
        assert_eq!(
            graph::render(view, &first, &[]),
            graph::render(view, &second, &[])
        );
    }
    assert_eq!(
        handout::compose(&first, &[]),
        handout::compose(&second, &[])
    );
    assert!(handout::compose(&first, &[]).contains(&expected_edge));
}

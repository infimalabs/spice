use wire::body::{ConsentAsked, ConsentQueueCause};

use crate::{
    Actuator, Answer, Authority, Budgets, OPERATOR, Outcome, POSTURE, Pending, Policy, PolicyError,
    Reversibility, Rule, Ruling, Spend,
};

fn diagnostic(refusal: &wire::Refusal) -> &wire::RefusalDiagnostic {
    refusal
        .diagnostic()
        .expect("consent refusals carry predicate diagnostics")
}

#[test]
fn the_first_matching_rule_answers_and_later_matches_do_not() {
    let policy = table(vec![
        rule("reads", "read", Answer::Allow, None),
        rule("pushes", "git push*", Answer::Queue, None),
        // A rule that would also match `git push --force`, placed after the
        // one that answers, so a first-match failure shows up as this name.
        rule("everythinggit", "git *", Answer::Allow, None),
    ]);

    assert_eq!(
        policy.decide("git push --force").authority(),
        &Authority::Rule("pushes".to_owned()),
    );
    assert_eq!(policy.decide("git push --force").answer(), Answer::Queue);
    assert_eq!(
        policy.decide("git status").authority(),
        &Authority::Rule("everythinggit".to_owned()),
    );
}

#[test]
fn an_unmatched_operation_is_answered_by_the_posture_under_its_own_name() {
    let policy = Policy::new(
        Answer::Queue,
        vec![rule("reads", "read", Answer::Allow, None)],
    )
    .expect("valid table");
    let decision = policy.decide("filesystem.write");

    assert_eq!(decision.authority(), &Authority::Posture);
    assert_eq!(decision.authority().name(), POSTURE);
    assert_eq!(decision.answer(), Answer::Queue);
}

#[test]
fn a_rule_cannot_take_the_name_the_posture_answers_to() {
    assert_eq!(
        Rule::new(POSTURE, "read", Answer::Allow, None),
        Err(PolicyError::ReservedName),
    );
}

/// A rule cannot sign a fact as the operator.
///
/// A released request cites `operator` as the authority that answered it. If a
/// rule could take that spelling, a table entry would be indistinguishable in
/// the journal from the operator deciding a held request by hand.
#[test]
fn a_rule_cannot_take_the_name_the_operator_answers_to() {
    assert_eq!(
        Rule::new(OPERATOR, "read", Answer::Allow, None),
        Err(PolicyError::ReservedName),
    );
    assert_eq!(Authority::Operator.name(), OPERATOR);
}

#[test]
fn two_rules_cannot_share_the_identity_a_fact_cites() {
    let duplicate = Policy::new(
        Answer::Queue,
        vec![
            rule("reads", "read", Answer::Allow, None),
            rule("reads", "cat", Answer::Deny, None),
        ],
    );

    assert_eq!(
        duplicate,
        Err(PolicyError::DuplicateName {
            rule: "reads".to_owned()
        }),
    );
}

#[test]
fn a_rule_that_never_grants_cannot_carry_a_grant_budget() {
    assert_eq!(
        Rule::new("pushes", "git push*", Answer::Queue, Some(3)),
        Err(PolicyError::UngrantableBudget {
            rule: "pushes".to_owned()
        }),
    );
    assert_eq!(
        Rule::new("blocked", "rm *", Answer::Deny, Some(3)),
        Err(PolicyError::UngrantableBudget {
            rule: "blocked".to_owned()
        }),
    );
}

#[test]
fn every_admission_refusal_names_diagnostic_detail_in_the_bare_shape() {
    let refused = [
        Rule::new("", "read", Answer::Allow, None).expect_err("unnamed rule"),
        Rule::new(POSTURE, "read", Answer::Allow, None).expect_err("reserved name"),
        Rule::new("reads", "", Answer::Allow, None).expect_err("empty pattern"),
        Rule::new("pushes", "git push*", Answer::Deny, Some(2)).expect_err("ungrantable budget"),
        Rule::new("reads", "read", Answer::Allow, Some(0)).expect_err("zero budget"),
        Policy::new(
            Answer::Queue,
            vec![
                rule("reads", "read", Answer::Allow, None),
                rule("reads", "cat", Answer::Allow, None),
            ],
        )
        .expect_err("duplicate name"),
    ];

    for error in refused {
        let refusal = error.refusal();
        assert!(!refusal.reason().is_empty(), "{error:?} states no reason");
        let detail = diagnostic(&refusal)
            .detail()
            .expect("policy errors carry explanatory detail");
        assert!(!detail.is_empty(), "{error:?} offers no detail");
        assert_eq!(
            diagnostic(&refusal).witness(),
            error.rule(),
            "{error:?} disagrees with its own refusal about the rule at fault",
        );
        assert!(
            error.to_string().contains(detail),
            "{error:?} displays without its detail",
        );
    }
}

#[test]
fn a_glob_covers_a_prefix_and_an_exact_pattern_covers_only_itself() {
    let policy = table(vec![
        rule("pushes", "git push*", Answer::Allow, None),
        rule("reads", "read", Answer::Allow, None),
    ]);

    assert!(policy.rules()[0].matches("git push"));
    assert!(policy.rules()[0].matches("git push --force"));
    assert!(!policy.rules()[0].matches("git pull"));
    assert!(policy.rules()[1].matches("read"));
    assert!(!policy.rules()[1].matches("readme"));
    assert!(!policy.rules()[1].matches("re"));
}

#[test]
fn a_glob_backtracks_rather_than_stopping_at_its_first_candidate() {
    // `*` must be able to give back what it took: the first `.txt` it can
    // reach is not the one that lets the rest of the pattern match.
    let pattern = rule("archives", "tar *.txt.gz", Answer::Allow, None);

    assert!(pattern.matches("tar notes.txt.gz"));
    assert!(pattern.matches("tar a.txt.gz.txt.gz"));
    assert!(!pattern.matches("tar notes.txt.bz2"));
}

#[test]
fn a_bounded_rule_grants_to_its_bound_and_then_queues_for_renewal() {
    let mut actuator = Actuator::new(table(vec![rule("reads", "read", Answer::Allow, Some(2))]));

    let first = decide(&mut actuator, &asked("read"), Reversibility::Reversible);
    let second = decide(&mut actuator, &asked("read"), Reversibility::Reversible);
    let third_request = asked("read");
    let third = decide(&mut actuator, &third_request, Reversibility::Reversible);

    assert_eq!(granted_budget(&first), Some(Some(1)));
    assert_eq!(granted_budget(&second), Some(Some(0)));
    let Outcome::Queued(queued) = &third else {
        panic!("a spent budget queues for renewal, but the third request answered {third:?}");
    };
    assert_eq!(queued.rule, "reads");
    assert_eq!(
        queued.cause,
        ConsentQueueCause::BudgetExhausted { bound: 2 }
    );
    let held = actuator
        .pending()
        .get(third_request.request)
        .expect("the exhausted request remains held");
    assert_eq!(held.rule(), "reads");
    assert_eq!(
        held.cause(),
        ConsentQueueCause::BudgetExhausted { bound: 2 }
    );
    assert_eq!(actuator.budgets().remaining("reads", 2), 0);
}

#[test]
fn an_exhausted_budget_is_not_pushed_further_past_its_bound_by_asking_again() {
    let mut budgets = Budgets::new();

    assert_eq!(budgets.spend("reads", 1), Spend::Granted { remaining: 0 });
    assert_eq!(budgets.spend("reads", 1), Spend::Exhausted);
    assert_eq!(budgets.spend("reads", 1), Spend::Exhausted);
    // Had the refused spends been recorded, raising the bound by two would
    // show no grants left rather than the two the operator just added.
    assert_eq!(budgets.remaining("reads", 3), 2);
}

#[test]
fn renewal_uses_the_exact_held_request_and_changes_no_other_budget_or_hold() {
    let mut actuator = Actuator::new(table(vec![
        rule("reads", "read", Answer::Allow, Some(1)),
        rule("writes", "write", Answer::Allow, Some(1)),
    ]));

    assert_eq!(
        granted_budget(&decide(
            &mut actuator,
            &asked("read"),
            Reversibility::Reversible,
        )),
        Some(Some(0)),
    );
    assert_eq!(
        granted_budget(&decide(
            &mut actuator,
            &asked("write"),
            Reversibility::Reversible,
        )),
        Some(Some(0)),
    );

    let exhausted_read = asked("read");
    let exhausted_write = asked("write");
    assert!(matches!(
        decide(&mut actuator, &exhausted_read, Reversibility::Reversible),
        Outcome::Queued(_)
    ));
    assert!(matches!(
        decide(&mut actuator, &exhausted_write, Reversibility::Reversible),
        Outcome::Queued(_)
    ));
    assert_eq!(actuator.pending().len(), 2);
    assert_eq!(actuator.budgets().remaining("reads", 1), 0);
    assert_eq!(actuator.budgets().remaining("writes", 1), 0);

    let unknown = asked("read").request;
    let before_unknown = actuator.clone();
    let refused = actuator
        .renew(unknown)
        .expect_err("an unheld request cannot name a budget to renew");
    assert_eq!(refused.reason(), "consent::request-not-held");
    assert_eq!(
        diagnostic(&refused).witness(),
        Some(unknown.to_string().as_str())
    );
    assert_eq!(actuator, before_unknown, "a refusal changes no seat state");

    let renewed_write = actuator
        .renew(exhausted_write.request)
        .expect("the write request names the writes budget");
    assert!(
        matches!(&renewed_write, Outcome::Granted(granted)
            if granted.request == exhausted_write.request
                && granted.lane == exhausted_write.lane
                && granted.rule == OPERATOR
                && granted.budget.is_none()),
        "renewal records the operator grant that resolves the queued request",
    );
    assert_eq!(actuator.budgets().remaining("writes", 1), 1);
    assert_eq!(actuator.budgets().remaining("reads", 1), 0);
    assert!(!actuator.pending().holds(exhausted_write.request));
    assert!(!actuator.pending().refuses(exhausted_write.request));
    assert!(actuator.proceeds(exhausted_write.request, Reversibility::Irreversible));
    assert!(actuator.pending().holds(exhausted_read.request));
    assert_eq!(actuator.pending().len(), 1);

    let after_write_renewal = actuator.clone();
    let repeated = actuator
        .renew(exhausted_write.request)
        .expect_err("one request cannot renew its budget twice");
    assert_eq!(repeated.reason(), "consent::request-not-held");
    assert_eq!(
        actuator, after_write_renewal,
        "a repeated renewal neither restores the budget again nor mints another grant",
    );

    let renewed_read = actuator
        .renew(exhausted_read.request)
        .expect("the read request names the reads budget");
    assert!(matches!(&renewed_read, Outcome::Granted(granted)
        if granted.request == exhausted_read.request && granted.rule == OPERATOR));
    assert_eq!(actuator.budgets().remaining("reads", 1), 1);
    assert_eq!(actuator.budgets().remaining("writes", 1), 1);
    assert!(actuator.pending().is_empty());
}

#[test]
fn a_budget_meters_per_rule_name_rather_than_across_the_table() {
    let mut budgets = Budgets::new();

    assert_eq!(budgets.spend("reads", 1), Spend::Granted { remaining: 0 });

    assert_eq!(budgets.spend("writes", 1), Spend::Granted { remaining: 0 });
    assert_eq!(budgets.remaining("reads", 1), 0);
    assert_eq!(budgets.remaining("renamed", 1), 1);
}

#[test]
fn a_held_request_lets_reversible_work_continue_while_the_irreversible_waits() {
    let mut actuator = Actuator::new(table(vec![rule(
        "pushes",
        "git push*",
        Answer::Queue,
        None,
    )]));

    let request = asked("git push --force");

    assert!(actuator.proceeds(request.request, Reversibility::Irreversible));

    let outcome = decide(&mut actuator, &request, Reversibility::Irreversible);

    let Outcome::Queued(queued) = outcome else {
        panic!("the queue rule must pend its request");
    };
    assert_eq!(queued.rule, "pushes");
    assert_eq!(queued.cause, ConsentQueueCause::Policy {});
    assert!(actuator.pending().holds(request.request));
    assert!(
        actuator.proceeds(request.request, Reversibility::Reversible),
        "queueing must not stop reversible work; that is why it is not a denial",
    );
    assert!(
        !actuator.proceeds(request.request, Reversibility::Irreversible),
        "an irreversible action has no undo, so it waits for the operator",
    );
    let refused = actuator
        .renew(request.request)
        .expect_err("a policy queue is resolved directly rather than renewed");
    assert_eq!(refused.reason(), "consent::renewal-not-required");
    assert_eq!(diagnostic(&refused).witness(), Some("pushes"));
    assert!(actuator.pending().holds(request.request));

    let held = actuator
        .pending()
        .get(request.request)
        .expect("held request");

    assert_eq!(held.operation(), "git push --force");
    assert_eq!(held.reversibility(), Reversibility::Irreversible);

    let released = actuator
        .release(request.request, Ruling::Allow)
        .expect("held request");

    assert!(matches!(released, Outcome::Granted(_)));
    assert!(actuator.proceeds(request.request, Reversibility::Irreversible));
}

/// A held request governs its own action and no other.
///
/// The queue holds a decision about one named operation. A different operation
/// has its own rule and gets its own answer while that request waits. Gating on
/// whether anything at all is held would make the first queued request a halt
/// on unrelated work the operator already allowed by rule.
#[test]
fn a_held_request_leaves_a_distinct_irreversible_request_independently_decidable() {
    let mut actuator = Actuator::new(table(vec![
        rule("pushes", "git push*", Answer::Queue, None),
        rule("tags", "git tag*", Answer::Allow, None),
    ]));
    let push = asked("git push --force");
    let tag = asked("git tag v1.0.0");

    assert!(matches!(
        decide(&mut actuator, &push, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));

    assert!(
        !actuator.proceeds(push.request, Reversibility::Irreversible),
        "the request the operator was asked about is the one that waits",
    );
    assert!(
        matches!(
            decide(&mut actuator, &tag, Reversibility::Irreversible),
            Outcome::Granted(_)
        ),
        "a distinct operation is answered by its own rule while another request is held",
    );
    assert!(
        actuator.proceeds(tag.request, Reversibility::Irreversible),
        "an irreversible action no request holds proceeds, even while another one waits",
    );
    assert!(
        actuator.pending().holds(push.request),
        "deciding the second request must not answer the first",
    );
}

/// Releasing one request answers that request alone.
#[test]
fn releasing_one_held_request_leaves_every_other_request_held() {
    let mut actuator = Actuator::new(table(vec![rule(
        "pushes",
        "git push*",
        Answer::Queue,
        None,
    )]));
    let first = asked("git push --force");
    let second = asked("git push --mirror");

    assert!(matches!(
        decide(&mut actuator, &first, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));
    assert!(matches!(
        decide(&mut actuator, &second, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));

    assert_eq!(actuator.pending().len(), 2);
    assert!(
        actuator.proceeds(first.request, Reversibility::Reversible)
            && actuator.proceeds(second.request, Reversibility::Reversible),
        "reversible work continues with many requests held, not just one",
    );

    assert_eq!(
        actuator
            .release(first.request, Ruling::Allow)
            .map(|outcome| outcome.kind()),
        Some(wire::FactKind::ConsentGranted),
    );

    assert!(
        actuator.proceeds(first.request, Reversibility::Irreversible),
        "the answered request's own action proceeds once it is released",
    );
    assert!(
        !actuator.proceeds(second.request, Reversibility::Irreversible),
        "releasing one request must not release another",
    );
    assert!(actuator.pending().holds(second.request));
    assert_eq!(actuator.pending().len(), 1);
}

/// The answer that ends the wait also decides whether the action may be taken.
///
/// Releasing is not permission. A refused request leaves the queue exactly as a
/// granted one does, so a gate that only asked whether anything still held it
/// would let the operator's refusal open the very action it closed.
#[test]
fn a_request_the_operator_refused_never_proceeds_and_owes_its_refusal() {
    let mut actuator = Actuator::new(table(vec![rule(
        "pushes",
        "git push*",
        Answer::Queue,
        None,
    )]));
    let request = asked("git push --force");

    assert!(matches!(
        decide(&mut actuator, &request, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));

    let refused = actuator
        .release(request.request, Ruling::Deny)
        .expect("held request");

    assert_eq!(refused.kind(), wire::FactKind::ConsentDenied);
    assert!(
        matches!(&refused, Outcome::Denied(denied) if denied.rule == OPERATOR),
        "the operator answered, so the operator is the authority the fact cites",
    );
    assert!(
        refused
            .refusal()
            .is_some_and(|refusal| { diagnostic(&refusal).witness() == Some(OPERATOR) }),
        "the refusal identifies the operator who answered",
    );

    assert!(
        !actuator.pending().holds(request.request),
        "a refused request is answered, so it is no longer waiting",
    );
    assert!(
        actuator.pending().refuses(request.request),
        "the refusal outlives the wait",
    );
    assert!(
        !actuator.proceeds(request.request, Reversibility::Irreversible),
        "releasing a refusal must not open the action the operator closed",
    );
    assert!(
        actuator.proceeds(request.request, Reversibility::Reversible),
        "a refusal about one irreversible action does not stop reversible work",
    );
}

/// A granted request reaches its own terminal disposition.
#[test]
fn a_request_the_operator_granted_proceeds_and_mints_its_grant() {
    let mut actuator = Actuator::new(table(vec![rule(
        "pushes",
        "git push*",
        Answer::Queue,
        None,
    )]));
    let request = asked("git push --force");

    assert!(matches!(
        decide(&mut actuator, &request, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));

    let granted = actuator
        .release(request.request, Ruling::Allow)
        .expect("held request");

    assert!(
        matches!(&granted, Outcome::Granted(grant)
            if grant.rule == OPERATOR && grant.lane == request.lane && grant.budget.is_none()),
        "an operator grant spends no rule budget and cites the operator",
    );
    assert!(granted.refusal().is_none(), "a grant is not a refusal");
    assert!(
        !actuator.pending().refuses(request.request),
        "granting is not refusing",
    );
    assert!(actuator.proceeds(request.request, Reversibility::Irreversible));
}

#[test]
fn a_held_request_cannot_be_queued_twice_or_answered_both_ways() {
    let mut actuator = Actuator::new(table(vec![rule(
        "pushes",
        "git push*",
        Answer::Queue,
        None,
    )]));
    let request = asked("git push --force");

    assert!(matches!(
        decide(&mut actuator, &request, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));
    let held = actuator.clone();
    let replay = actuator
        .decide(&request, Reversibility::Irreversible)
        .expect_err("an identity already in the queue cannot queue again");
    assert_eq!(replay.reason(), "consent::request-already-known");
    assert_eq!(diagnostic(&replay).observed(), Some("held"));
    assert_eq!(actuator, held, "rejecting a replay changes no seat state");

    assert!(matches!(
        actuator.release(request.request, Ruling::Deny),
        Some(Outcome::Denied(_))
    ));
    let denied = actuator.clone();
    let replay = actuator
        .decide(&request, Reversibility::Irreversible)
        .expect_err("a denied identity must be asked again under a new UUID");
    assert_eq!(diagnostic(&replay).observed(), Some("denied"));
    assert!(
        actuator.release(request.request, Ruling::Allow).is_none(),
        "the denied identity cannot later mint a grant",
    );
    assert!(
        actuator.renew(request.request).is_err(),
        "the denied identity cannot reach a grant through renewal either",
    );
    assert_eq!(actuator, denied);
    assert!(!actuator.proceeds(request.request, Reversibility::Irreversible));
}

#[test]
fn an_operator_grant_cannot_be_replayed_into_a_denial() {
    let mut actuator = Actuator::new(table(vec![rule(
        "pushes",
        "git push*",
        Answer::Queue,
        None,
    )]));
    let request = asked("git push --force");

    assert!(matches!(
        decide(&mut actuator, &request, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));
    assert!(matches!(
        actuator.release(request.request, Ruling::Allow),
        Some(Outcome::Granted(_))
    ));
    let granted = actuator.clone();

    let replay = actuator
        .decide(&request, Reversibility::Irreversible)
        .expect_err("a granted identity must not be decided again");
    assert_eq!(diagnostic(&replay).observed(), Some("granted"));
    assert!(actuator.release(request.request, Ruling::Deny).is_none());
    assert!(actuator.renew(request.request).is_err());
    assert_eq!(actuator, granted);
    assert!(actuator.proceeds(request.request, Reversibility::Irreversible));
}

/// Answering one request answers only that request.
#[test]
fn a_refusal_stops_its_own_request_and_leaves_another_decidable() {
    let mut actuator = Actuator::new(table(vec![
        rule("pushes", "git push*", Answer::Queue, None),
        rule("tags", "git tag*", Answer::Allow, None),
    ]));
    let push = asked("git push --force");
    let tag = asked("git tag v1.0.0");

    assert!(matches!(
        decide(&mut actuator, &push, Reversibility::Irreversible),
        Outcome::Queued(_)
    ));
    assert!(
        actuator
            .release(push.request, Ruling::Deny)
            .is_some_and(|outcome| outcome.kind() == wire::FactKind::ConsentDenied)
    );

    assert!(
        matches!(
            decide(&mut actuator, &tag, Reversibility::Irreversible),
            Outcome::Granted(_)
        ),
        "one refused request does not answer an unrelated operation",
    );
    assert!(actuator.proceeds(tag.request, Reversibility::Irreversible));
    assert!(!actuator.proceeds(push.request, Reversibility::Irreversible));
}

/// Nothing is answered that was never asked.
#[test]
fn releasing_a_request_no_queue_holds_records_no_answer() {
    let mut actuator = Actuator::new(table(vec![rule(
        "pushes",
        "git push*",
        Answer::Queue,
        None,
    )]));
    let stranger = asked("git push --force");

    assert!(actuator.release(stranger.request, Ruling::Deny).is_none());
    assert!(
        !actuator.pending().refuses(stranger.request),
        "a refusal must answer a request that was actually held",
    );
    assert!(actuator.proceeds(stranger.request, Reversibility::Irreversible));
}

#[test]
fn a_queue_holds_each_request_under_its_own_identity() {
    let mut pending = Pending::new();
    let first = asked("git push");
    let second = asked("git push --force");

    pending
        .hold(
            &first,
            Reversibility::Irreversible,
            "pushes".to_owned(),
            ConsentQueueCause::Policy {},
        )
        .expect("fresh request");
    pending
        .hold(
            &second,
            Reversibility::Reversible,
            "writes".to_owned(),
            ConsentQueueCause::BudgetExhausted { bound: 4 },
        )
        .expect("fresh request");

    assert_eq!(pending.len(), 2);
    assert!(pending.holds(first.request));
    assert!(pending.holds(second.request));
    let two_held = pending.clone();
    let replay = pending
        .hold(
            &first,
            Reversibility::Irreversible,
            "pushes".to_owned(),
            ConsentQueueCause::Policy {},
        )
        .expect_err("a held identity cannot mint another queue fact");
    assert_eq!(diagnostic(&replay).observed(), Some("held"));
    assert_eq!(pending, two_held);
    let held = pending.get(second.request).expect("second request is held");
    assert_eq!(held.rule(), "writes");
    assert_eq!(
        held.cause(),
        ConsentQueueCause::BudgetExhausted { bound: 4 }
    );
    assert_eq!(
        pending
            .release(first.request, Ruling::Allow)
            .map(|held| held.lane()),
        Some(first.lane)
    );
    assert!(!pending.holds(first.request));
    assert!(!pending.is_empty());

    let replay = pending
        .hold(
            &first,
            Reversibility::Irreversible,
            "pushes".to_owned(),
            ConsentQueueCause::Policy {},
        )
        .expect_err("a granted identity cannot return to the queue");
    assert_eq!(diagnostic(&replay).observed(), Some("granted"));
    assert!(pending.proceeds(first.request, Reversibility::Irreversible));
}

#[test]
fn a_denial_refuses_loudly_and_a_grant_or_a_queue_does_not_refuse_at_all() {
    let mut actuator = Actuator::new(
        Policy::new(
            Answer::Queue,
            vec![
                rule("blocked", "rm *", Answer::Deny, None),
                rule("reads", "read", Answer::Allow, None),
            ],
        )
        .expect("valid table"),
    );

    let denied = decide(
        &mut actuator,
        &asked("rm -rf /"),
        Reversibility::Irreversible,
    );
    let refusal = denied.refusal().expect("a denial refuses");

    assert_eq!(diagnostic(&refusal).witness(), Some("blocked"));
    assert!(
        diagnostic(&refusal)
            .detail()
            .expect("denials carry detail")
            .contains("blocked"),
        "a refusal names the rule in its detail, not only in its witness: {refusal:?}",
    );
    assert!(!refusal.reason().is_empty());

    assert_eq!(
        decide(&mut actuator, &asked("read"), Reversibility::Reversible).refusal(),
        None,
        "a grant happened, so it cannot pretend to be a refusal",
    );
    let posture = decide(
        &mut actuator,
        &asked("anything else"),
        Reversibility::Reversible,
    );
    assert_eq!(
        posture.refusal(),
        None,
        "a queued request is not answered yet, so it is not a refusal either",
    );
    let Outcome::Queued(queued) = posture else {
        panic!("an unmatched request follows the queue posture");
    };
    assert_eq!(queued.rule, POSTURE);
    assert_eq!(queued.cause, ConsentQueueCause::Policy {});
}

#[test]
fn policy_answers_are_terminal_without_spending_twice_on_replay() {
    let mut actuator = Actuator::new(table(vec![
        rule("reads", "read", Answer::Allow, Some(2)),
        rule("blocked", "rm *", Answer::Deny, None),
    ]));
    let granted_request = asked("read");

    assert_eq!(
        granted_budget(&decide(
            &mut actuator,
            &granted_request,
            Reversibility::Irreversible,
        )),
        Some(Some(1)),
    );
    let granted = actuator.clone();
    let replay = actuator
        .decide(&granted_request, Reversibility::Irreversible)
        .expect_err("the rule already granted this identity");
    assert_eq!(diagnostic(&replay).observed(), Some("granted"));
    assert_eq!(
        actuator, granted,
        "the duplicate grant neither emits a fact nor spends the budget twice",
    );
    assert!(actuator.proceeds(granted_request.request, Reversibility::Irreversible));

    let denied_request = asked("rm -rf /");
    assert!(matches!(
        decide(&mut actuator, &denied_request, Reversibility::Irreversible),
        Outcome::Denied(_)
    ));
    let denied = actuator.clone();
    let replay = actuator
        .decide(&denied_request, Reversibility::Irreversible)
        .expect_err("the rule already denied this identity");
    assert_eq!(diagnostic(&replay).observed(), Some("denied"));
    assert_eq!(actuator, denied);
    assert!(!actuator.proceeds(denied_request.request, Reversibility::Irreversible));
}

#[test]
fn a_decided_request_keeps_the_identity_and_lane_it_arrived_with() {
    let mut actuator = Actuator::new(table(vec![
        rule("blocked", "rm *", Answer::Deny, None),
        rule("reads", "read", Answer::Allow, None),
    ]));
    let request = asked("read");

    match decide(&mut actuator, &request, Reversibility::Reversible) {
        Outcome::Granted(granted) => {
            assert_eq!(granted.request, request.request);
            assert_eq!(granted.lane, request.lane);
            assert_eq!(granted.rule, "reads");
        }
        other => panic!("an allow rule grants, but this answered {other:?}"),
    }
}

#[test]
fn every_outcome_this_seat_can_reach_is_a_consent_fact() {
    let mut actuator = Actuator::new(table(vec![
        rule("reads", "read", Answer::Allow, None),
        rule("blocked", "rm *", Answer::Deny, None),
        rule("pushes", "git push*", Answer::Queue, None),
    ]));

    for operation in ["read", "rm -rf /", "git push", "unmatched"] {
        let kind = actuator
            .decide(&asked(operation), Reversibility::Reversible)
            .expect("the sweep mints a fresh request for each operation")
            .kind();
        assert!(
            matches!(
                kind,
                wire::FactKind::ConsentGranted
                    | wire::FactKind::ConsentDenied
                    | wire::FactKind::ConsentQueued
            ),
            "deciding {operation:?} produced {kind:?}, which is not a consent fact",
        );
    }
}

#[test]
fn a_seated_table_and_its_queue_read_back_as_the_operator_configured_them() {
    // An operator has to be able to see the table that decides and the
    // requests waiting on them, so every accessor that renders either one is
    // exercised here rather than left as surface nothing calls.
    let mut actuator = Actuator::new(
        Policy::new(
            Answer::Deny,
            vec![
                rule("reads", "read", Answer::Allow, None),
                rule("pushes", "git push*", Answer::Queue, None),
            ],
        )
        .expect("valid table"),
    );

    assert_eq!(actuator.policy().posture(), Answer::Deny);
    assert_eq!(
        actuator
            .policy()
            .rules()
            .iter()
            .map(|rule| (rule.name(), rule.tool()))
            .collect::<Vec<_>>(),
        [("reads", "read"), ("pushes", "git push*")],
        "the table renders in the order it decides",
    );

    let request = asked("git push --force");
    decide(&mut actuator, &request, Reversibility::Irreversible);

    assert_eq!(
        actuator
            .pending()
            .iter()
            .map(|(identity, held)| (identity, held.operation().to_owned()))
            .collect::<Vec<_>>(),
        [(request.request, "git push --force".to_owned())],
        "a held request is listable, which is what pending it to an operator means",
    );
}

fn table(rules: Vec<Rule>) -> Policy {
    Policy::new(Answer::Queue, rules).expect("valid table")
}

fn rule(name: &str, tool: &str, answer: Answer, budget: Option<u32>) -> Rule {
    Rule::new(name, tool, answer, budget).expect("valid rule")
}

fn decide(actuator: &mut Actuator, asked: &ConsentAsked, reversibility: Reversibility) -> Outcome {
    actuator
        .decide(asked, reversibility)
        .expect("the fixture uses a fresh request identity")
}

/// Each call takes a fresh request identity so a fixture that holds two
/// requests really holds two, rather than overwriting one entry twice.
fn asked(operation: &str) -> ConsentAsked {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    ConsentAsked {
        request: format!("018f7777-7777-7777-8777-{ordinal:012x}")
            .parse()
            .expect("canonical request"),
        lane: "018f8888-8888-7888-8888-888888888888"
            .parse()
            .expect("canonical lane"),
        operation: operation.to_owned(),
    }
}

fn granted_budget(outcome: &Outcome) -> Option<Option<u32>> {
    match outcome {
        Outcome::Granted(granted) => Some(granted.budget),
        _ => None,
    }
}

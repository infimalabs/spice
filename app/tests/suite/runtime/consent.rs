use app::runtime::ConsentRuling;
use wire::intent::{ConsentAnswer, ConsentBudgetAnswer, ConsentPendingAnswer, ConsentQueueAnswer};
use wire::{Answer, Name, Witness};

use super::*;

const SECOND_REQUEST: &str = "018f82d4-6f59-7d06-92e3-bce9131163d1";

fn rule() -> Name {
    "publish".parse().expect("fixture rule name")
}

fn pending(request: &str, budget: Option<u32>) -> ConsentPendingAnswer {
    let rule = rule();
    ConsentPendingAnswer {
        request: uuid(request),
        lane: uuid(LANE),
        operation: "git push".to_owned(),
        rule: rule.clone(),
        cause: wire::body::ConsentQueueCause::Policy {},
        budget: budget.map(|remaining| ConsentBudgetAnswer {
            rule,
            bound: 3,
            remaining,
        }),
    }
}

fn queue(
    session: &Session,
    pending: Vec<ConsentPendingAnswer>,
) -> app::Admitted<Response<ConsentQueueAnswer>> {
    let bytes = support::response(
        WireVersion::CURRENT,
        uuid(REQUEST),
        uuid(EPOCH),
        Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(seq(7)),
            body: ConsentQueueAnswer {
                pending,
                budgets: Vec::new(),
            },
        }),
    );
    session
        .admit_frame(&bytes)
        .expect("queue answer is admitted")
}

fn ruling(
    session: &Session,
    outcome: Outcome<ConsentAnswer>,
) -> app::Admitted<Response<ConsentAnswer>> {
    let bytes = support::response(WireVersion::CURRENT, uuid(REQUEST), uuid(EPOCH), outcome);
    session
        .admit_frame(&bytes)
        .expect("ruling answer is admitted")
}

#[test]
fn an_operator_ruling_carries_the_queue_witnesses_and_the_ruled_request_leaves() {
    let session = session(OPERATOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    runtime.apply_consent_queue(&queue(
        &session,
        vec![pending(REQUEST, Some(0)), pending(SECOND_REQUEST, None)],
    ));

    let (decide, expected) = runtime
        .begin_consent_ruling(uuid(REQUEST), Answer::Granted)
        .expect("a queued request is decidable");
    assert_eq!(decide.answer, Answer::Granted);
    assert_eq!(
        expected,
        vec![
            Witness::Consent {
                request: uuid(REQUEST),
                answer: Answer::Queued,
            },
            Witness::Rule { name: rule() },
            Witness::Budget {
                rule: rule(),
                remaining: 0,
            },
        ],
        "a grant spends the budget the queue showed"
    );
    assert!(
        runtime
            .begin_consent_ruling(uuid(REQUEST), Answer::Denied)
            .is_none(),
        "one request carries at most one ruling in flight"
    );
    let (_, denial) = runtime
        .begin_consent_ruling(uuid(SECOND_REQUEST), Answer::Denied)
        .expect("a sibling request stays decidable");
    assert_eq!(denial.len(), 2, "a denial spends no budget");
    assert!(
        runtime
            .begin_consent_ruling(uuid(THIRD_LANE), Answer::Granted)
            .is_none(),
        "a request absent from the queue is not decidable"
    );

    runtime.apply_consent_ruling(
        uuid(REQUEST),
        &ruling(
            &session,
            Outcome::Reply(Reply {
                position: ReplyPosition::Committed(wire::SeqRange {
                    first: seq(8),
                    last: seq(8),
                }),
                body: ConsentAnswer {
                    request: uuid(REQUEST),
                    answer: Answer::Granted,
                },
            }),
        ),
    );
    runtime.apply_consent_ruling(
        uuid(SECOND_REQUEST),
        &ruling(
            &session,
            Outcome::Refusal(wire::Refusal::bare(wire::RefusalDiagnostic::new(
                "consent::witness-moved".to_owned(),
                None,
                None,
                None,
            ))),
        ),
    );
    assert_eq!(
        runtime.consent_ruling(uuid(REQUEST)),
        Some(&ConsentRuling::Ruled(Answer::Granted))
    );
    assert_eq!(
        runtime.consent_ruling(uuid(SECOND_REQUEST)),
        Some(&ConsentRuling::Refused("consent::witness-moved".to_owned()))
    );
    assert!(
        runtime
            .begin_consent_ruling(uuid(REQUEST), Answer::Denied)
            .is_none(),
        "a committed ruling is final before the queue is re-read"
    );
    assert!(
        runtime
            .begin_consent_ruling(uuid(SECOND_REQUEST), Answer::Denied)
            .is_some(),
        "a refused ruling leaves the request decidable"
    );

    runtime.apply_consent_queue(&queue(&session, vec![pending(SECOND_REQUEST, None)]));
    assert_eq!(runtime.consent_ruling(uuid(REQUEST)), None);
    assert!(
        runtime
            .consent_queue()
            .is_some_and(|queue| queue.pending.len() == 1)
    );
}

#[test]
fn a_lane_principal_cannot_rule_on_consent() {
    let session = session(LANE_CAPABILITY, EPOCH);
    let accepted = acceptance(&session, &format!("lane:{LANE}"), EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    runtime.apply_consent_queue(&queue(&session, vec![pending(REQUEST, None)]));

    assert!(
        runtime
            .begin_consent_ruling(uuid(REQUEST), Answer::Granted)
            .is_none()
    );
}

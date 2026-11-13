use super::*;

const DIRECTIVE: &str = "a0ae31a7-eb68-463a-b3c0-272bfeaaa5d1";
const KEY: &str = "1kJK6N3Rx";
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const EPOCH: &str = "e2e9f1f2-19b2-4ddc-83fa-340b02dee2c4";
const FIRST_EXPIRY: &str = "2026-08-06T04:00:00.000Z";

#[test]
fn terminal_history_retains_the_stable_root_stage_and_complete_audit() {
    let snapshot = terminal_snapshot();
    let ledger = steer::Ledger::from_snapshot(&snapshot).expect("stable steering replay");
    let mut roots = |key: &steer::Key| {
        Ok(ledger
            .directives()
            .into_iter()
            .find(|directive| directive.key() == key)
            .map(StableRoot::from))
    };
    let expected_escalations = [
        wire::body::SteeringTransition {
            stage: wire::body::SteeringStage::Reminded,
            at: "2026-08-06T03:00:15.000Z".parse().expect("reminded at"),
        },
        wire::body::SteeringTransition {
            stage: wire::body::SteeringStage::Escalated,
            at: "2026-08-06T03:01:00.000Z".parse().expect("escalated at"),
        },
        wire::body::SteeringTransition {
            stage: wire::body::SteeringStage::Overdue,
            at: "2026-08-06T03:05:00.000Z".parse().expect("overdue at"),
        },
    ];

    for (index, expected_disposition) in [
        (4, wire::Disposition::Lapsed),
        (5, wire::Disposition::Live),
        (6, wire::Disposition::Canceled),
    ] {
        let Some(RecallProvenance::Steering {
            directive,
            key,
            disposition,
            stage,
            escalations,
            ..
        }) = recall_provenance(&snapshot.facts()[index], &mut roots)
            .expect("terminal provenance projects")
        else {
            panic!("terminal fact must project stable steering provenance")
        };
        assert_eq!(directive.to_string(), DIRECTIVE);
        assert_eq!(key, KEY);
        assert_eq!(disposition, expected_disposition);
        assert_eq!(stage, Some(wire::body::SteeringStage::Overdue));
        assert_eq!(escalations.as_slice(), expected_escalations.as_slice());
    }
}

fn terminal_snapshot() -> journal::FactSnapshot {
    let facts = [
        envelope(
            1,
            "2026-08-06T03:00:00.000Z",
            &format!(
                r#"{{"kind":"steer::sent","directive":"{DIRECTIVE}","key":"{KEY}","recipient":"lane:{LANE}","text":"finish the task","sender":"spice","expires":"{FIRST_EXPIRY}"}}"#,
            ),
        ),
        escalation(2, "2026-08-06T03:00:15.000Z", "reminded"),
        escalation(3, "2026-08-06T03:01:00.000Z", "escalated"),
        escalation(4, "2026-08-06T03:05:00.000Z", "overdue"),
        envelope(
            5,
            FIRST_EXPIRY,
            &format!(
                r#"{{"kind":"steer::lapsed","key":"{KEY}","recipient":"lane:{LANE}","expires":"{FIRST_EXPIRY}"}}"#,
            ),
        ),
        envelope(
            6,
            "2026-08-06T04:00:01.000Z",
            &format!(
                r#"{{"kind":"steer::recovered","directive":"{DIRECTIVE}","key":"{KEY}","recipient":"lane:{LANE}","lapsed_at":"{FIRST_EXPIRY}","expires":"2026-08-07T04:00:01.000Z"}}"#,
            ),
        ),
        envelope(
            7,
            "2026-08-06T04:00:02.000Z",
            &format!(r#"{{"kind":"steer::canceled","key":"{KEY}","recipient":"lane:{LANE}"}}"#),
        ),
    ]
    .into_iter()
    .map(|text| wire::decode(text.as_bytes()).expect("registered steering envelope"))
    .collect();
    journal::FactSnapshot::from_facts(facts).expect("terminal steering snapshot")
}

fn escalation(seq: u64, at: &str, stage: &str) -> String {
    envelope(
        seq,
        at,
        &format!(
            r#"{{"kind":"steer::escalated","directive":"{DIRECTIVE}","key":"{KEY}","recipient":"lane:{LANE}","stage":"{stage}"}}"#,
        ),
    )
}

fn envelope(ordinal: u64, at: &str, fact: &str) -> String {
    let seq = wire::Seq::FIRST.get() + ordinal - 1;
    format!(
        r#"{{"v":1,"seq":{seq},"at":"{at}","actor":"spice","origin":"resident:{EPOCH}","fact":{fact}}}"#,
    )
}

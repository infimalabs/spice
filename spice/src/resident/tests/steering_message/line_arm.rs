//! A lane arms only the open lines it opened or received with its partners in
//! its claimed task phase round, refuses a whole set over any other root, and
//! journals one fact only when its set changes.

use super::*;

const STAMP: &str = "dVrD244Jd";

fn arm(lanes: &Lanes, lane: u8, roots: &[&str]) -> Outcome<CommandAnswer> {
    let request = wire::intent::LineArm {
        roots: roots.iter().map(|root| (*root).to_owned()).collect(),
    };
    let command = Command {
        v: wire::WireVersion::CURRENT,
        id: new_uuid(),
        epoch: lanes.epoch,
        kind: CommandKind::LineArm,
        expected: Vec::new(),
        body: serde_json::to_value(&request).expect("line arm encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::Lane(test_uuid(lane)),
            epoch: lanes.epoch,
            release: "1.2.3".parse().expect("release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .expect("lane command admits");
    steering::line_arm(&lanes.bus, lanes.epoch, &command, &request)
}

/// The armed roots the answer names, and whether the call wrote a fact.
fn armed(outcome: Outcome<CommandAnswer>) -> (Vec<String>, bool) {
    let Outcome::Reply(Reply {
        position,
        body: CommandAnswer::LineArm(answer),
    }) = outcome
    else {
        panic!("line arm answers its set: {outcome:?}");
    };
    (
        answer.roots,
        matches!(position, ReplyPosition::Committed(_)),
    )
}

fn refused(outcome: Outcome<CommandAnswer>) -> String {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("line arm refuses: {outcome:?}");
    };
    refusal.reason().to_owned()
}

fn facts(lanes: &Lanes) -> Vec<wire::body::LineArmed> {
    lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::LineArmed>())
        .collect()
}

/// Open one line from `from` to `to` and return its root key.
fn open(lanes: &Lanes, from: u8, to: u8) -> String {
    let before = lanes.pending(to).len();
    lanes.author(from, &format!("SYN {}: are you there?", lanes.address(to)));
    lanes.hear(from, test_uuid(from + 1));
    let pending = lanes.pending(to);
    assert_eq!(pending.len(), before + 1, "the SYN reached its recipient");
    pending.last().expect("one new input").key.clone()
}

#[test]
fn a_lane_arms_exactly_its_open_same_round_lines() {
    let lanes = Lanes::new("line-arm");
    lanes.seat(THIRD);
    lanes.seed_round(STAMP, &[SENDER, FIRST, THIRD]);
    let first = open(&lanes, SENDER, FIRST);
    let third = open(&lanes, THIRD, SENDER);
    let foreign = open(&lanes, SENDER, SECOND);
    let operator = lanes.direct(SENDER, 40, "keep going");

    // Any invalid root refuses the whole set and writes nothing.
    assert_eq!(
        refused(arm(&lanes, SENDER, &[&first, &foreign])),
        "line::root-foreign-round"
    );
    assert_eq!(
        refused(arm(&lanes, SENDER, &[&first, &operator])),
        "line::root-not-peer"
    );
    assert_eq!(
        refused(arm(&lanes, SENDER, &[&first, &first])),
        "line::root-duplicate"
    );
    assert_eq!(
        refused(arm(&lanes, SENDER, &["dVrD244Jz"])),
        "line::root-unknown"
    );
    assert_eq!(
        refused(arm(&lanes, SENDER, &[first.as_str(); 17])),
        "line::roots-over-bound"
    );
    assert_eq!(
        refused(arm(&lanes, SECOND, &[&foreign])),
        "line::claim-missing"
    );
    assert!(facts(&lanes).is_empty());

    // A set may mix partners; it binds the claim's round and returns the set.
    let mut both = vec![first.clone(), third.clone()];
    both.sort();
    assert_eq!(
        armed(arm(&lanes, SENDER, &[&third, &first])),
        (both.clone(), true)
    );
    let [fact] = facts(&lanes).try_into().expect("one arm fact");
    let heads = lanes
        .bus
        .read(|read| read.active_claim_for_actor(&Actor::Lane(test_uuid(SENDER))))
        .expect("claim reads")
        .expect("the lane holds its claim");
    assert_eq!(
        (
            fact.lane,
            &fact.task,
            &fact.phase,
            fact.round,
            fact.roots.len()
        ),
        (
            test_uuid(SENDER),
            &heads.statement().body().born,
            &heads.workflow().body().phase,
            heads.completion().body().round,
            2
        )
    );

    // An unchanged set writes nothing, and a refusal keeps the prior set.
    assert_eq!(armed(arm(&lanes, SENDER, &[&first, &third])), (both, false));
    lanes.answer(FIRST, &[&first], &format!("ACK {first}: Nearly."));
    let continuation = lanes
        .pending(SENDER)
        .into_iter()
        .find(|input| input.key != operator && input.key != third)
        .expect("the continuation reached the opener")
        .key;
    assert_eq!(
        refused(arm(&lanes, SENDER, &[&continuation])),
        "line::root-continuation"
    );
    lanes.answer(SENDER, &[&third], &format!("FIN {third}: Done."));
    assert_eq!(
        refused(arm(&lanes, SENDER, &[&first, &third])),
        "line::root-closed"
    );
    assert_eq!(facts(&lanes).len(), 1);

    // An empty arm clears the set once; clearing nothing writes nothing.
    assert_eq!(armed(arm(&lanes, SENDER, &[])), (Vec::new(), true));
    assert_eq!(armed(arm(&lanes, SENDER, &[])), (Vec::new(), false));
    let facts = facts(&lanes);
    assert_eq!(facts.len(), 2);
    assert!(facts[1].roots.is_empty());
    lanes.finish();
}

//! Fast checks for call thresholds, prose, turn completion, and occupant changes.

use super::*;

const BOUND: u32 = 3;

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
        .parse()
        .expect("fixture UUID")
}

fn record(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    turn: Option<listen::TurnMoment>,
    events: Vec<listen::Event>,
) -> lane::capture::CaptureRecord {
    lane::capture::CaptureRecord {
        lane,
        occupant,
        stream: vendor::RecordStream::Stdout,
        boundary: sill::RecordBoundary::Complete,
        locus: listen::Locus::new(7, 120, 64),
        declaration: None,
        decoded: listen::Decoded::Events {
            context: Box::new(listen::RecordContext {
                timestamp: Some("2030-04-12T18:44:09.271Z".to_owned()),
                turn,
                ..Default::default()
            }),
            events,
        },
    }
}

fn prompt_completed(lane: CanonicalUuid, occupant: CanonicalUuid) -> lane::capture::CaptureRecord {
    let mut record = record(lane, occupant, None, Vec::new());
    let listen::Decoded::Events { context, .. } = &mut record.decoded else {
        unreachable!("the fixture is decoded events");
    };
    context.prompt_outcome = Some(listen::PromptOutcome {
        agent_invoked: true,
        status: listen::PromptStatus::Completed,
        session_settled: true,
    });
    record
}

fn call(name: &str) -> listen::Event {
    listen::Event::Tool(listen::Tool {
        call_id: format!("call-{name}"),
        moment: listen::ToolMoment::Call {
            name: name.to_owned(),
            arguments: "{}".to_owned(),
            intent: None,
        },
        locus: listen::Locus::new(7, 120, 64),
    })
}

fn result() -> listen::Event {
    listen::Event::Tool(listen::Tool {
        call_id: "call-Bash".to_owned(),
        moment: listen::ToolMoment::Result(listen::ToolOutcome {
            result: listen::ToolResult::Succeeded,
            output: None,
            exit_code: None,
            media: Vec::new(),
            external_media: Vec::new(),
        }),
        locus: listen::Locus::new(7, 120, 64),
    })
}

fn reasoning() -> listen::Event {
    listen::Event::Reasoning(listen::Reasoning {
        content: listen::ReasoningContent::Plain(vec!["inspect".to_owned()]),
        locus: listen::Locus::new(7, 120, 64),
    })
}

fn prose(role: listen::ProseRole, text: &str) -> listen::Event {
    listen::Event::Prose(listen::Prose {
        role,
        text: text.to_owned(),
        phase: None,
        media: Vec::new(),
        external_media: Vec::new(),
        locus: listen::Locus::new(7, 120, 64),
    })
}

fn tools(lane: CanonicalUuid, occupant: CanonicalUuid, count: u32) -> lane::capture::CaptureRecord {
    record(
        lane,
        occupant,
        None,
        (0..count).map(|_| call("Bash")).collect(),
    )
}

#[test]
fn a_silent_lane_is_prodded_once_its_tool_calls_reach_the_bound() {
    let (lane, occupant) = (uuid(1), uuid(2));
    let mut streaks = Streaks::default();

    let started = record(
        lane,
        occupant,
        Some(listen::TurnMoment::Started),
        vec![prose(listen::ProseRole::Operator, "go")],
    );
    assert_eq!(streaks.observe(BOUND, &started), Vec::new());
    // Results, reasoning, and a second call in one record do not count as
    // more than the calls they carry.
    let two = record(
        lane,
        occupant,
        None,
        vec![reasoning(), call("Read"), result(), call("Bash")],
    );
    assert_eq!(streaks.observe(BOUND, &two), Vec::new());

    let third = tools(lane, occupant, 1);
    assert_eq!(
        streaks.observe(BOUND, &third),
        vec![Step::Prodded(NarrationProdded {
            lane,
            occupant,
            moments: 3,
            count: 1,
        })]
    );
}

#[test]
fn a_lane_that_speaks_before_the_bound_is_never_prodded() {
    let (lane, occupant) = (uuid(1), uuid(2));
    let mut streaks = Streaks::default();

    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 2)),
        Vec::new()
    );
    let spoke = record(
        lane,
        occupant,
        None,
        vec![
            prose(listen::ProseRole::Assistant, "ACK dStg3T0R4: on it."),
            call("Bash"),
        ],
    );
    // The prose resets the streak before the call that follows it counts.
    assert_eq!(streaks.observe(BOUND, &spoke), Vec::new());
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 1)),
        Vec::new()
    );
    // Blank prose is not speech; the third call after the reset prods.
    let blank = record(
        lane,
        occupant,
        None,
        vec![prose(listen::ProseRole::Assistant, "  \n")],
    );
    assert_eq!(streaks.observe(BOUND, &blank), Vec::new());
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 1)),
        vec![Step::Prodded(NarrationProdded {
            lane,
            occupant,
            moments: 3,
            count: 1,
        })]
    );
}

#[test]
fn a_lane_at_rest_between_turns_is_never_prodded_and_its_prod_clears() {
    let (lane, occupant) = (uuid(1), uuid(2));
    let mut streaks = Streaks::default();

    let prodded = streaks.observe(BOUND, &tools(lane, occupant, 3));
    assert_eq!(prodded.len(), 1);
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 1)),
        Vec::new(),
        "the fourth call remains below the next prod threshold"
    );
    let completed = prompt_completed(lane, occupant);
    // The typed prompt result clears the prod the streak drew.
    assert_eq!(
        streaks.observe(BOUND, &completed),
        vec![Step::Cleared(NarrationCleared {
            lane,
            occupant,
            moments: 4,
            prods: 1,
            relief: NarrationRelief::Turn,
        })]
    );
    // At rest nothing arrives to count; a residue record changes nothing.
    let residue = lane::capture::CaptureRecord {
        decoded: listen::Decoded::Residue(listen::Residue {
            dialect: listen::Dialect::Omp,
            locus: listen::Locus::new(7, 120, 64),
            kind: listen::ResidueKind::Unknown {
                discriminator: "progress".to_owned(),
            },
        }),
        ..tools(lane, occupant, 0)
    };
    assert_eq!(streaks.observe(BOUND, &residue), Vec::new());
    // The next turn starts from zero.
    let next = record(
        lane,
        occupant,
        Some(listen::TurnMoment::Started),
        vec![call("Bash")],
    );
    assert_eq!(streaks.observe(BOUND, &next), Vec::new());
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 1)),
        Vec::new()
    );
    assert_eq!(streaks.observe(BOUND, &tools(lane, occupant, 1)).len(), 1);
}

#[test]
fn continued_silence_prods_again_after_each_further_bound_until_prose_clears_it() {
    let (lane, occupant) = (uuid(1), uuid(2));
    let mut streaks = Streaks::default();

    assert_eq!(streaks.observe(BOUND, &tools(lane, occupant, 3)).len(), 1);
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 2)),
        Vec::new()
    );
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 1)),
        vec![Step::Prodded(NarrationProdded {
            lane,
            occupant,
            moments: 6,
            count: 2,
        })]
    );
    // Seven calls in one record cross two more bounds: one prod per bound.
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, occupant, 7)),
        vec![
            Step::Prodded(NarrationProdded {
                lane,
                occupant,
                moments: 9,
                count: 3,
            }),
            Step::Prodded(NarrationProdded {
                lane,
                occupant,
                moments: 12,
                count: 4,
            }),
        ]
    );
    let spoke = record(
        lane,
        occupant,
        None,
        vec![prose(
            listen::ProseRole::Assistant,
            "Gate is green; committing.",
        )],
    );
    assert_eq!(
        streaks.observe(BOUND, &spoke),
        vec![Step::Cleared(NarrationCleared {
            lane,
            occupant,
            moments: 13,
            prods: 4,
            relief: NarrationRelief::Prose,
        })]
    );
    // Prose with no prod outstanding clears nothing durable.
    assert_eq!(streaks.observe(BOUND, &spoke), Vec::new());
}

#[test]
fn a_new_occupant_or_a_closing_sweep_forgets_the_streak_without_a_clearance() {
    let (lane, occupant, successor) = (uuid(1), uuid(2), uuid(3));
    let mut streaks = Streaks::default();

    assert_eq!(streaks.observe(BOUND, &tools(lane, occupant, 3)).len(), 1);
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, successor, 2)),
        Vec::new()
    );
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, successor, 1)),
        vec![Step::Prodded(NarrationProdded {
            lane,
            occupant: successor,
            moments: 3,
            count: 1,
        })]
    );
    assert_eq!(streaks.observe(0, &tools(lane, successor, 1)), Vec::new());
    assert!(streaks.lanes.is_empty());
    assert_eq!(
        streaks.observe(BOUND, &tools(lane, successor, 2)),
        Vec::new()
    );
}

use super::*;

const DESIGN_STAMP: &str = "dM27Rjglh";
const DESIGN_TITLE: &str = "Choose status output delivery";
const FIRST_OUTPUT_STAMP: &str = "dM27bwP5r";
const FIRST_OUTPUT_TITLE: &str = "Add newline-delimited JSON status";
const SECOND_OUTPUT_STAMP: &str = "dM27m76S0";
const SECOND_OUTPUT_TITLE: &str = "Document status output compatibility";

pub(super) fn seed(journal: &mut journal::Journal) -> DemoResult<()> {
    let design: wire::TaskStamp = DESIGN_STAMP
        .parse()
        .expect("demo pass Design stamp is valid");
    let statement = seed_board_task(
        journal,
        DemoBoardTask {
            born: design.clone(),
            title: DESIGN_TITLE,
            topic: DEMO_TOPIC,
            term: &[],
            preparation: None,
            intent: Some(wire::TaskIntent::Design),
            dependencies: Vec::new(),
            flow: &["design"],
            at: "2026-07-11T05:13:00.000000Z",
        },
    )?;
    let first: wire::TaskStamp = FIRST_OUTPUT_STAMP
        .parse()
        .expect("demo first output stamp is valid");
    seed_output(
        journal,
        &design,
        &first,
        FIRST_OUTPUT_TITLE,
        &[
            "Each status record is one complete JSON object.",
            "Human-readable status output remains unchanged.",
        ],
        "2026-07-11T05:14:00.000000Z",
    )?;
    let second: wire::TaskStamp = SECOND_OUTPUT_STAMP
        .parse()
        .expect("demo second output stamp is valid");
    seed_output(
        journal,
        &design,
        &second,
        SECOND_OUTPUT_TITLE,
        &[
            "Compatibility guarantees name every stable field.",
            "Examples cover successful and failed lane records.",
        ],
        "2026-07-11T05:15:00.000000Z",
    )?;

    final_word(journal, &design, "2026-07-11T05:15:30.000000Z")?;
    complete_pass(journal, &design, statement, "2026-07-11T05:16:00.000000Z")?;
    Ok(())
}

/// The demo lane's room post and `task done` share that end `design`'s pass,
/// so the decision card shows the finishing lane's final word.
fn final_word(
    journal: &mut journal::Journal,
    design: &wire::TaskStamp,
    at: &str,
) -> DemoResult<()> {
    let at = instant(at);
    let mut write = JournalWrite::new(
        Actor::Lane(uuid(DEMO_LANE)),
        format!("task:{design}")
            .parse()
            .expect("demo Design origin is valid"),
        at,
        Producer::CommandDoor,
    );
    write
        .fact(
            wire::rooms::RoomPublished {
                room: design.clone(),
                publication: wire::rooms::RoomAuthorityStamp::for_micros(at.get().as_microsecond())
                    .expect("the final word instant names a publication"),
                content: wire::rooms::RoomPublicationContent::Authored {
                    text: "Both outputs together deliver the purpose: scripts get one JSON object \
                           per line, and the compatibility note keeps the text format stable."
                        .to_owned(),
                },
            },
            FactCause::Root,
        )
        .map_err(build_failure)?;
    write
        .fact(
            body::TaskDone {
                task: design.clone(),
                validation: "Charged every term, offered both outputs, and posted why they \
                             deliver the Design."
                    .to_owned(),
                terminal: None,
                git: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
                disposition: None,
            },
            FactCause::Root,
        )
        .map_err(build_failure)?;
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

/// End `design`'s pass in round `round` at `at`, leaving it awaiting the
/// operator's decision, and return the completion's position.
pub(super) fn complete_pass(
    journal: &mut journal::Journal,
    design: &wire::TaskStamp,
    round: wire::Seq,
    at: &str,
) -> DemoResult<wire::Seq> {
    let mut completed = JournalWrite::new(
        Actor::Spice,
        format!("task:{design}")
            .parse()
            .expect("demo Design origin is valid"),
        instant(at),
        Producer::CommandDoor,
    );
    completed
        .fact(
            body::TaskRoundCompleted {
                task: design.clone(),
                phase: "design".parse().expect("demo design phase is valid"),
                round,
                participants: Vec::new(),
                status: body::TaskCompletionStatus::Pending,
                next: None,
                disposition: None,
            },
            FactCause::Root,
        )
        .map_err(build_failure)?;
    Ok(journal.apply(completed).map_err(commit_failure)?.last())
}

fn seed_output(
    journal: &mut journal::Journal,
    design: &wire::TaskStamp,
    output: &wire::TaskStamp,
    title: &str,
    term: &[&str],
    at: &str,
) -> DemoResult<()> {
    seed_board_task(
        journal,
        DemoBoardTask {
            born: output.clone(),
            title,
            topic: DEMO_TOPIC,
            term,
            preparation: Some(wire::TaskPreparation {
                parent: design.clone(),
                controller: design.clone(),
                construction: output.clone(),
            }),
            intent: None,
            dependencies: Vec::new(),
            flow: &["todo", "review"],
            at,
        },
    )?;
    Ok(())
}

pub(super) fn activate_demo_output(
    decided: &mut JournalWrite,
    authorization: wire::rooms::ExecutionAuthorization,
    output: &wire::TaskStamp,
    decision_fact: journal::PendingFact,
) -> DemoResult<()> {
    let authorized = decided
        .fact(
            wire::rooms::ExecutionAuthorized { authorization },
            FactCause::Pending(decision_fact),
        )
        .map_err(build_failure)?;
    let advanced = decided
        .fact(
            body::TaskPhased {
                task: output.clone(),
                phase: wire::Phase::Todo,
            },
            FactCause::Pending(authorized),
        )
        .map_err(build_failure)?;
    decided.workflow_head(output.clone(), advanced, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, wire::Phase::Todo],
        phase: wire::Phase::Todo,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    Ok(())
}

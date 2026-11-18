use std::fs;
use std::path::{Path, PathBuf};

use board::allocate::{Mint, mint};
use journal::{FactCause, Journal, JournalWrite};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use vendor::omp::SESSION_ENVIRONMENT;
use wire::{
    Actor, CanonicalUuid, Disposition, Instant, LaneResidency, Origin, Phase, Producer,
    ResidencyHead, ResidentState, SteeringHead, TaskStamp, WorkflowHead, body,
};

use super::{
    Scratch, THREAD, omp_call, omp_compaction, omp_header, omp_result, omp_rows, omp_text,
    omp_user, reply_forms_in, strings, write_jsonl,
};

const SESSION_TOPIC: &str = "/spice/session";

fn fake_repository(root: &Path) {
    fs::create_dir(root.join(".git")).expect("fixture git directory is created");
    fs::write(root.join(wire::TRACKED_POLICY_FILE), "").expect("fixture tracked policy is written");
}

fn authority_path(root: &Path) -> PathBuf {
    root.join(".git/.spice/authority/journal/journal.sqlite3")
}

/// One transcript of `thread`, named by its path as forensic evidence.
fn selected_transcript(root: &Path, thread: &str) -> String {
    let transcript = root.join("selected.jsonl");
    write_jsonl(&transcript, &omp_rows(thread));
    transcript.to_string_lossy().into_owned()
}

fn uuid(tail: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{tail:02x}")
        .parse()
        .expect("fixture uuid")
}

fn instant(minute: u8) -> Instant {
    format!("2026-08-01T06:{minute:02}:00.000Z")
        .parse()
        .expect("fixture instant")
}

fn phase(name: &str) -> Phase {
    name.parse().expect("fixture phase")
}

fn stamp(text: &str) -> TaskStamp {
    let value = text.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 10_000_000_000_000
    });
    TaskStamp::for_micros(value).expect("fixture name fits the task stamp codec")
}

fn workflow(name: &str) -> WorkflowHead {
    WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase(name),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    }
}

fn task_added(
    write: &mut JournalWrite,
    task: TaskStamp,
    title: &str,
    phase: &str,
    intent: Option<wire::TaskIntent>,
    preparation: Option<wire::TaskPreparation>,
    positive: bool,
) {
    let mut workflow = workflow(phase);
    if positive {
        workflow.status = wire::WorkflowStatus::reviewed(instant(9));
    }
    let title = title.to_owned();
    let topic = SESSION_TOPIC.to_owned();
    let dependencies = Vec::new();
    let fact = write
        .fact(
            body::TaskAdded {
                preparation: preparation.clone(),
                intent,
                task: task.clone(),
                title: title.clone(),
                pitch: "Sessions need a task to stand on.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: topic.parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: dependencies.clone(),
                flow: workflow.flow.clone(),
                phase: workflow.phase,
                revision: 1,
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::SpiceActor,
                ),
            },
            FactCause::Root,
        )
        .expect("task fact builds");
    let statement_task = task.clone();
    write.task_head(fact, move |position| wire::TaskHead {
        preparation,
        intent,
        born: statement_task,
        topic_origin: position,
        title,
        pitch: "Sessions need a task to stand on.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: topic.parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies,
        provenance: wire::TaskProvenance::OperatorDraft,
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision: position,
    });
    write.workflow_head(task, fact, move |_| workflow);
}

fn seed_board_glance_tasks(write: &mut JournalWrite) {
    task_added(
        write,
        stamp("SESSION-ready"),
        "Ordinary unclaimed work",
        "todo",
        None,
        None,
        false,
    );
    let design = stamp("SESSION-design");
    task_added(
        write,
        design.clone(),
        "Speculative Design",
        "design",
        Some(wire::TaskIntent::Design),
        None,
        false,
    );
    task_added(
        write,
        stamp("SESSION-output"),
        "Unauthorized candidate",
        "draft",
        None,
        Some(wire::TaskPreparation {
            parent: design.clone(),
            controller: design,
            construction: stamp("SESSION-output"),
        }),
        false,
    );
    let released_design = stamp("SESSION-released-design");
    task_added(
        write,
        released_design.clone(),
        "Concluded Design",
        "review",
        Some(wire::TaskIntent::Design),
        None,
        true,
    );
    let released_plan = stamp("SESSION-released-plan");
    task_added(
        write,
        released_plan.clone(),
        "Concluded selected Plan",
        "review",
        Some(wire::TaskIntent::Plan),
        None,
        true,
    );
    task_added(
        write,
        stamp("SESSION-released-output"),
        "Released nested output",
        "todo",
        None,
        Some(wire::TaskPreparation {
            parent: released_plan,
            controller: released_design,
            construction: stamp("SESSION-released-output"),
        }),
        false,
    );
}

fn seed_claimed_tasks(write: &mut JournalWrite, held: &TaskStamp, peer: &TaskStamp) {
    task_added(
        write,
        held.clone(),
        "Compose Rust briefings",
        "review",
        None,
        None,
        false,
    );
    task_added(
        write,
        peer.clone(),
        "Peer active claim",
        "todo",
        None,
        None,
        false,
    );
}

fn seed_recall_authority(path: &Path, thread: &str) {
    fs::create_dir_all(path.parent().expect("authority path has a parent"))
        .expect("authority parent is created");
    let mut journal = Journal::open(path).expect("fixture journal opens");
    let lane = uuid(0xaa);
    let occupant = uuid(0xab);
    let epoch = uuid(0xac);
    let peer_lane = uuid(0xad);
    let peer_occupant = uuid(0xae);
    let origin = "task:SESSION-fixture"
        .parse::<Origin>()
        .expect("fixture origin");
    let held = stamp("SESSION-held");
    let peer = stamp("SESSION-peer");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin.clone(),
        instant(0),
        Producer::CommandDoor,
    );
    let seated = write
        .fact(body::UpSeated { epoch }, FactCause::Root)
        .expect("seating fact builds");
    let lanes = recall_lanes(lane, occupant, peer_lane, peer_occupant, epoch, thread);
    write.residency_head(seated, move |_| ResidencyHead {
        epoch,
        state: ResidentState::Seated,
        topology_revision: 0,
        lanes,
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    seed_claimed_tasks(&mut write, &held, &peer);
    seed_board_glance_tasks(&mut write);
    let steering = body::SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive: uuid(0xaf),
        fanout: None,
        recipients: Vec::new(),
        key: "1kBsk001x".to_owned(),
        recipient: Actor::Lane(lane),
        text: "Address this exact steering ask".to_owned(),
        control: None,
        attachments: vec![
            wire::Media::from_base64("operator-capture.png", "image/png", "iVBORw0KGgo=")
                .expect("fixture media admits"),
        ],
        expires: instant(30),
    };
    let minted_micros = steering
        .key
        .parse::<steer::Key>()
        .expect("current steering key")
        .minted_micros();
    let steering_fact = write
        .fact(steering.clone(), FactCause::Root)
        .expect("steering fact builds");
    let steering_origin = origin.clone();
    write.steering_head(steering_fact, move |created| SteeringHead {
        sender: steering.sender,
        room: steering.room.clone(),
        directive: steering.directive,
        fanout: steering.fanout,
        recipients: Vec::new(),
        key: steering.key,
        recipient: steering.recipient,
        text: steering.text,
        control: steering.control,
        attachments: steering.attachments,
        published: instant(0),
        expires: steering.expires,
        stage: body::SteeringStage::Published,
        escalations: Vec::new(),
        disposition: Disposition::Live,
        origin: steering_origin,
        created,
        minted_micros,
    });
    journal.apply(write).expect("fixture authority seeds");

    mint_claim(
        &mut journal,
        FixtureClaim {
            task: held,
            lane,
            occupant,
            epoch,
            phase: "review",
            at: instant(1),
            expiry: instant(2),
            maximum: instant(2),
        },
    );
    mint_claim(
        &mut journal,
        FixtureClaim {
            task: peer,
            lane: peer_lane,
            occupant: peer_occupant,
            epoch,
            phase: "todo",
            at: instant(2),
            expiry: instant(40),
            maximum: instant(50),
        },
    );
}

#[test]
fn briefing_board_counts_execution_without_hiding_designs_from_storage() {
    let scratch = Scratch::new("briefing-execution-board");
    let authority = authority_path(scratch.path());
    seed_recall_authority(&authority, &uuid(0xaa).to_string());
    let journal = Journal::open(&authority).expect("fixture journal reopens");

    let glance = journal
        .read(crate::session_command::glance::board)
        .expect("board glance reads");
    assert_eq!(glance.unclaimed, vec![("todo".to_owned(), 2)]);
    let retained = journal
        .read(|read| {
            Ok((
                read.task_head(&stamp("SESSION-design"))?.is_some(),
                read.task_head(&stamp("SESSION-output"))?.is_some(),
            ))
        })
        .expect("complete task heads read");
    assert_eq!(
        retained,
        (true, true),
        "dedicated reads retain the planning rows omitted from the briefing"
    );
    scratch.remove();
}

fn seed_retired_peer_steering(path: &Path) {
    let mut journal = Journal::open(path).expect("fixture journal reopens");
    let directive = uuid(0xb0);
    let lane = uuid(0xad);
    let origin = "task:SESSION-fixture"
        .parse::<Origin>()
        .expect("fixture origin");
    let mut sent = JournalWrite::new(
        Actor::Spice,
        origin.clone(),
        instant(3),
        Producer::CommandDoor,
    );
    let body = body::SteerSent {
        sender: Actor::Spice,
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: "1kRtxrxxx".to_owned(),
        recipient: Actor::Lane(lane),
        text: "unrelated retired steering".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: instant(30),
    };
    let minted_micros = body
        .key
        .parse::<steer::Key>()
        .expect("current steering key")
        .minted_micros();
    let fact = sent
        .fact(body.clone(), FactCause::Root)
        .expect("peer steering fact builds");
    let head_origin = origin.clone();
    sent.steering_head(fact, move |created| SteeringHead {
        sender: body.sender,
        room: body.room.clone(),
        directive: body.directive,
        fanout: body.fanout,
        recipients: Vec::new(),
        key: body.key,
        recipient: body.recipient,
        text: body.text,
        control: body.control,
        attachments: body.attachments,
        published: instant(3),
        expires: body.expires,
        stage: body::SteeringStage::Published,
        escalations: Vec::new(),
        disposition: Disposition::Live,
        origin: head_origin,
        created,
        minted_micros,
    });
    journal
        .apply(sent)
        .expect("peer steering publication commits");
    let current = journal
        .read(|read| read.steering_head(directive))
        .expect("peer steering head reads")
        .expect("peer steering head exists");
    let mut canceled = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin,
        instant(4),
        Producer::CommandDoor,
    );
    let cancellation = canceled
        .fact(
            body::SteerCanceled {
                key: current.body().key.clone(),
                recipient: Actor::Lane(lane),
                reason: None,
            },
            FactCause::Existing(current.body().created),
        )
        .expect("peer cancellation fact builds");
    let mut retired = current.into_body();
    retired.disposition = Disposition::Canceled;
    canceled.steering_head(cancellation, move |_| retired);
    journal
        .apply(canceled)
        .expect("peer steering retirement commits");
}

fn recall_lanes(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    peer_lane: CanonicalUuid,
    peer_occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    thread: &str,
) -> Vec<LaneResidency> {
    vec![
        LaneResidency {
            lane,
            team: lane,
            name: None,
            worktree: "/fixture".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(occupant),
            lineage: vec![occupant],
            capabilities: Vec::new(),
            session: Some(wire::LaneSession {
                lane,
                occupant,
                worktree: "/fixture".to_owned(),
                vendor: "omp".parse().expect("fixture vendor"),
                launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
                epoch,
                stream: "session".to_owned(),
                identity: thread.to_owned(),
                locus: wire::TapeLocus {
                    member: 0,
                    offset: 0,
                    length: thread.len() as u64,
                },
            }),
            disposition: "occupied".to_owned(),
        },
        LaneResidency {
            lane: peer_lane,
            team: peer_lane,
            name: None,
            worktree: "/fixture-peer".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(peer_occupant),
            lineage: vec![peer_occupant],
            capabilities: Vec::new(),
            session: None,
            disposition: "occupied".to_owned(),
        },
    ]
}

/// One lane's claim on a session-topic fixture task, minted by that lane.
struct FixtureClaim {
    task: TaskStamp,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    phase: &'static str,
    at: Instant,
    expiry: Instant,
    maximum: Instant,
}

fn mint_claim(journal: &mut Journal, claim: FixtureClaim) {
    let FixtureClaim {
        task,
        lane,
        occupant,
        epoch,
        phase,
        at,
        expiry,
        maximum,
    } = claim;
    let actor = Actor::Lane(lane);
    let mut write = JournalWrite::new(
        actor.clone(),
        "task:SESSION-fixture"
            .parse()
            .expect("fixture claim origin"),
        at,
        Producer::CommandDoor,
    );
    mint(
        &mut write,
        Mint {
            task: &task,
            actor: &actor,
            workflow: &workflow(phase),
            topic: Some(&SESSION_TOPIC.parse().expect("fixture topic")),
            lane,
            occupant,
            epoch,
            at,
            expiry,
            maximum,
            git: None,
            recovery: None,
            source: body::SourceBoundary::Proven { heads: Vec::new() },
            cause: None,
        },
    )
    .expect("claim mints");
    journal.apply(write).expect("claim commits");
}

fn claim_won_facts(path: &Path) -> i64 {
    Connection::open(path)
        .expect("fixture authority opens for claim fact count")
        .query_row(
            "SELECT COUNT(*) FROM fact_envelopes WHERE kind = 'claim::won'",
            [],
            |row| row.get(0),
        )
        .expect("claim fact count reads")
}

fn fact_count(path: &Path) -> i64 {
    Connection::open(path)
        .expect("fixture authority opens for total fact count")
        .query_row("SELECT COUNT(*) FROM fact_envelopes", [], |row| row.get(0))
        .expect("total fact count reads")
}

fn active_claims_for(path: &Path, actor: &Actor) -> i64 {
    Connection::open(path)
        .expect("fixture authority opens for active claim count")
        .query_row(
            "SELECT COUNT(*) FROM claim_index WHERE actor = ?1 AND disposition = 'active'",
            params![actor.to_string()],
            |row| row.get(0),
        )
        .expect("active claim count reads")
}

#[test]
fn preferred_vendor_without_identity_leaves_authority_unknown() {
    let scratch = Scratch::new("authority-missing-preferred-live-vendor");
    fake_repository(scratch.path());
    let selected = uuid(0xaa).to_string();
    let authority = authority_path(scratch.path());
    seed_recall_authority(&authority, &selected);
    let transcript = selected_transcript(scratch.path(), &selected);
    let environment = [("SPICE_AGENT_VENDOR", "omp".to_owned())]; // env-policy: allow

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript,
            "--deadline-seconds",
            "5",
        ]),
        &environment,
        "recall",
    );

    assert_eq!(recall["held_work"]["thread"], Value::Null);
    assert!(recall.get("asks").is_none());
    assert!(
        recall["unknowns"]
            .as_array()
            .expect("missing preferred identity is explicit")
            .iter()
            .any(|row| {
                row["probe"] == "live-thread-identity"
                    && row["detail"].as_str().is_some_and(|detail| {
                        detail.contains(&format!("{SESSION_ENVIRONMENT} is absent"))
                    })
            })
    );

    scratch.remove();
}

#[test]
fn malformed_live_identity_stays_unknown_instead_of_using_selected_transcript_identity() {
    let scratch = Scratch::new("authority-malformed-live");
    fake_repository(scratch.path());
    let selected = uuid(0xaa).to_string();
    let authority = authority_path(scratch.path());
    seed_recall_authority(&authority, &selected);
    let transcript = selected_transcript(scratch.path(), &selected);
    let environment = [(SESSION_ENVIRONMENT, "malformed-live-thread".to_owned())];
    let before = fact_count(&authority);

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript,
            "--deadline-seconds",
            "5",
        ]),
        &environment,
        "recall",
    );

    assert_eq!(recall["held_work"]["thread"], Value::Null);
    assert_eq!(recall["held_work"]["lane"], Value::Null);
    assert_eq!(recall["held_work"]["own_claim"], Value::Null);
    assert_eq!(recall["held_work"]["peer_claims"], Value::Null);
    assert!(recall.get("asks").is_none());
    assert!(
        recall["unknowns"]
            .as_array()
            .expect("malformed live identity is explicit")
            .iter()
            .any(|row| {
                row["authority"] == "environment" && row["probe"] == "live-thread-identity"
            })
    );
    assert_eq!(fact_count(&authority), before, "briefing appends no facts");

    scratch.remove();
}

#[test]
fn absent_live_identity_keeps_current_authority_unknown_during_historical_recovery() {
    let scratch = Scratch::new("authority-no-live");
    fake_repository(scratch.path());
    let selected = uuid(0xaa).to_string();
    let authority = authority_path(scratch.path());
    seed_recall_authority(&authority, &selected);
    let transcript = selected_transcript(scratch.path(), &selected);
    let before = fact_count(&authority);

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript,
            "--deadline-seconds",
            "5",
        ]),
        &[],
        "recall",
    );

    assert!(
        recall["live"]["turn"].is_object(),
        "historical transcript evidence remains in the live working set"
    );
    assert_eq!(recall["held_work"]["thread"], Value::Null);
    assert_eq!(recall["held_work"]["lane"], Value::Null);
    assert_eq!(recall["held_work"]["own_claim"], Value::Null);
    assert_eq!(recall["held_work"]["peer_claims"], Value::Null);
    assert!(recall.get("asks").is_none());
    let unknowns = recall["unknowns"]
        .as_array()
        .expect("missing live identity is explicit");
    assert!(unknowns.iter().any(|row| {
        row["authority"] == "environment" && row["probe"] == "live-thread-identity"
    }));
    assert!(
        unknowns
            .iter()
            .any(|row| { row["authority"] == "board" && row["probe"] == "exact-actor-claim" })
    );
    assert_eq!(fact_count(&authority), before, "briefing appends no facts");

    scratch.remove();
}

#[test]
fn recall_authority_probe_is_read_only_when_repository_state_is_absent() {
    let scratch = Scratch::new("authority-absent");
    fake_repository(scratch.path());
    let transcript = scratch.path().join("omp.jsonl");
    write_jsonl(&transcript, &omp_rows(THREAD));

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript.to_string_lossy(),
            "--deadline-seconds",
            "5",
        ]),
        &[],
        "recall",
    );

    assert!(
        recall.get("asks").is_none(),
        "unread steering is not rendered as an empty known inbox"
    );
    assert!(
        recall["unknowns"]
            .as_array()
            .expect("unknowns are explicit")
            .iter()
            .any(|row| {
                row["authority"] == "environment" && row["probe"] == "live-thread-identity"
            }),
        "missing live identity is reported before actor-bound journal probes"
    );
    assert!(
        !scratch.path().join(".git/.spice").exists(),
        "read-only observation does not initialize repository state"
    );

    scratch.remove();
}

#[test]
fn recall_authority_sources_asks_and_held_work_from_exact_state_authorities() {
    let scratch = Scratch::new("authority-present");
    fake_repository(scratch.path());
    let transcript = scratch.path().join("omp.jsonl");
    write_jsonl(&transcript, &omp_rows(THREAD));
    let thread = uuid(0xaa).to_string();
    seed_recall_authority(&authority_path(scratch.path()), &thread);
    let environment = [(SESSION_ENVIRONMENT, thread.clone())];

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript.to_string_lossy(),
            "--deadline-seconds",
            "5",
        ]),
        &environment,
        "recall",
    );

    let ask = &recall["asks"]["rows"][0];
    assert_eq!(ask["key"], "1kBsk001x");
    assert_eq!(ask["text"]["text"], "Address this exact steering ask");
    assert_eq!(ask["attachments"][0]["name"], "operator-capture.png");
    let expected_media =
        wire::Media::from_base64("operator-capture.png", "image/png", "iVBORw0KGgo=")
            .expect("fixture media admits");
    let expected_reference = wire::AgentMediaReference::from_media(uuid(0xaf), 0, &expected_media)
        .expect("fixture reference derives");
    assert_eq!(
        ask["attachments"][0],
        serde_json::to_value(expected_reference).expect("fixture reference encodes"),
    );
    assert_eq!(
        recall["held_work"]["own_claim"]["title"],
        "Compose Rust briefings"
    );
    assert_eq!(recall["held_work"]["peer_claims"], 1);

    scratch.remove();
}

/// Six compaction windows of tool traffic precede the live turn. The briefing
/// carries the one pending ask and the live turn, never the old traffic.
#[test]
fn a_long_briefing_keeps_the_pending_ask_and_drops_old_tool_traffic() {
    let scratch = Scratch::new("authority-long-briefing");
    fake_repository(scratch.path());
    let thread = uuid(0xaa).to_string();
    seed_recall_authority(&authority_path(scratch.path()), &thread);
    let mut rows = vec![omp_header(THREAD, "2026-03-01T00:00:00.000Z")];
    for window in 0..6_u32 {
        let at = |second: u32| format!("2026-03-01T00:{window:02}:{second:02}Z");
        if window > 0 {
            rows.push(omp_compaction(
                &at(0),
                &format!("c{window}"),
                "Earlier work summarized.",
                &format!("u{window}"),
            ));
        }
        rows.push(omp_user(&at(1), &format!("u{window}"), "old request"));
        for call in 0..20_u32 {
            let id = format!("call-{window}-{call}");
            rows.push(omp_call(
                &at(2 + call * 2),
                &format!("a{window}-{call}"),
                &id,
                "bash",
                json!({"command": format!("old-tool-traffic {window}-{call}")}),
            ));
            rows.push(omp_result(
                &at(3 + call * 2),
                &format!("r{window}-{call}"),
                &id,
                "bash",
                Some(0),
            ));
        }
    }
    rows.push(omp_user(
        "2026-03-01T00:06:00Z",
        "live",
        "Current direction.",
    ));
    rows.push(omp_text(
        "2026-03-01T00:06:01Z",
        "final",
        "Live work answered.",
        "stop",
    ));
    let transcript = scratch.path().join("long.jsonl");
    write_jsonl(&transcript, &rows);

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript.to_string_lossy(),
            "--deadline-seconds",
            "5",
        ]),
        &[(SESSION_ENVIRONMENT, thread)],
        "recall",
    );

    assert_eq!(recall["asks"]["rows"][0]["key"], "1kBsk001x");
    assert_eq!(
        recall["held_work"]["own_claim"]["title"],
        "Compose Rust briefings"
    );
    assert_eq!(
        recall["live"]["turn"]["prompt"]["text"],
        "Current direction."
    );
    assert_eq!(
        recall["live"]["latest_final"]["text"],
        "Live work answered."
    );
    assert_eq!(recall["live"]["compacted"], "2026-03-01T00:05:00.000Z");
    let body = recall.to_string();
    for old in ["old-tool-traffic", "old request", "Earlier work summarized"] {
        assert!(!body.contains(old), "the briefing replays {old:?}: {body}");
    }

    scratch.remove();
}

#[test]
fn creator_and_claim_briefings_do_not_append_claim_facts_or_duplicate_the_actor_claim() {
    let scratch = Scratch::new("authority-briefing-read-only");
    fake_repository(scratch.path());
    let creator_transcript = scratch.path().join("creator.jsonl");
    let claim_transcript = scratch.path().join("claim.jsonl");
    write_jsonl(&creator_transcript, &omp_rows(THREAD));
    write_jsonl(&claim_transcript, &omp_rows(THREAD));
    let thread = uuid(0xaa).to_string();
    let authority = authority_path(scratch.path());
    seed_recall_authority(&authority, &thread);
    let actor = Actor::Lane(uuid(0xaa));
    let environment = [(SESSION_ENVIRONMENT, thread.clone())];

    let before_claim_facts = claim_won_facts(&authority);
    assert_eq!(active_claims_for(&authority, &actor), 1);

    let creator = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &creator_transcript.to_string_lossy(),
            "--deadline-seconds",
            "5",
        ]),
        &environment,
        "recall",
    );
    assert_eq!(
        creator["held_work"]["own_claim"]["title"],
        "Compose Rust briefings"
    );
    assert_eq!(
        claim_won_facts(&authority),
        before_claim_facts,
        "creator briefing appended a claim fact"
    );

    let claim = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &claim_transcript.to_string_lossy(),
            "--deadline-seconds",
            "5",
        ]),
        &environment,
        "recall",
    );
    assert_eq!(
        claim["held_work"]["own_claim"]["title"],
        "Compose Rust briefings"
    );
    assert_eq!(
        claim_won_facts(&authority),
        before_claim_facts,
        "claim briefing appended a claim fact"
    );
    assert_eq!(
        active_claims_for(&authority, &actor),
        1,
        "the actor owns exactly one active claim after both briefings"
    );

    scratch.remove();
}

#[test]
fn recall_ignores_corrupt_fact_and_retired_steering_outside_the_exact_locus() {
    let scratch = Scratch::new("authority-bounded-corruption");
    fake_repository(scratch.path());
    let transcript = scratch.path().join("omp.jsonl");
    write_jsonl(&transcript, &omp_rows(THREAD));
    let thread = uuid(0xaa).to_string();
    let authority = authority_path(scratch.path());
    seed_recall_authority(&authority, &thread);
    seed_retired_peer_steering(&authority);
    let connection = Connection::open(&authority).expect("fixture authority opens for corruption");
    connection
        .execute(
            "UPDATE fact_envelopes SET envelope = ?1 \
             WHERE seq = (SELECT max(seq) FROM fact_envelopes WHERE kind = 'task::added')",
            params![b"corrupt unrelated fact".to_vec()],
        )
        .expect("corrupt one unrelated fact body");
    connection
        .execute(
            "UPDATE steering_heads SET body = ?1 WHERE disposition = 'canceled'",
            params![b"corrupt retired steering".to_vec()],
        )
        .expect("corrupt retired steering body");
    drop(connection);

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript.to_string_lossy(),
            "--deadline-seconds",
            "5",
        ]),
        &[(SESSION_ENVIRONMENT, thread)],
        "recall",
    );

    assert_eq!(recall["asks"]["rows"][0]["key"], "1kBsk001x");
    assert_eq!(
        recall["held_work"]["own_claim"]["title"],
        "Compose Rust briefings"
    );
    assert_eq!(recall["held_work"]["peer_claims"], 1);
    assert!(
        recall.get("unknowns").is_none(),
        "corruption outside every selected keyspace is not decoded"
    );

    scratch.remove();
}

#[test]
fn a_failed_steering_probe_does_not_hide_independent_board_answers() {
    let scratch = Scratch::new("authority-probe-isolation");
    fake_repository(scratch.path());
    let transcript = scratch.path().join("omp.jsonl");
    write_jsonl(&transcript, &omp_rows(THREAD));
    let thread = uuid(0xaa).to_string();
    let authority = authority_path(scratch.path());
    seed_recall_authority(&authority, &thread);
    let connection = Connection::open(&authority).expect("fixture authority opens for corruption");
    connection
        .execute(
            "UPDATE steering_heads SET body = ?1 WHERE disposition = 'live'",
            params![b"corrupt selected steering".to_vec()],
        )
        .expect("corrupt selected steering body");
    drop(connection);

    let recall = reply_forms_in(
        Some(scratch.path()),
        &strings(&[
            "session",
            "briefing",
            &transcript.to_string_lossy(),
            "--deadline-seconds",
            "5",
        ]),
        &[(SESSION_ENVIRONMENT, thread)],
        "recall",
    );

    assert!(
        recall.get("asks").is_none(),
        "failed selected steering is not rendered as an empty known inbox"
    );
    assert_eq!(
        recall["held_work"]["own_claim"]["title"],
        "Compose Rust briefings"
    );
    assert_eq!(recall["held_work"]["peer_claims"], 1);
    assert_eq!(
        recall["unknowns"]
            .as_array()
            .expect("failed probe is explicit")
            .iter()
            .filter(|row| row["authority"] == "steering")
            .count(),
        1
    );

    scratch.remove();
}

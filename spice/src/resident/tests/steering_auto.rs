use spice::rooms::evidence::{Conversation, Evidence};
use spice::rooms::profile::EvidenceKey;

use super::*;

#[test]
fn automatic_message_prefers_an_idle_running_agent_over_a_better_matching_vacant_lane() {
    let (root, paths) = resident_fixture("steer-auto-recipient");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let worktree = repository.worktree_root().to_string_lossy().into_owned();
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let bus = Bus::start(journal::Journal::open(&paths.journal).expect("fixture journal opens"))
        .expect("fixture bus starts");
    let epoch = test_uuid(1);
    let running = test_uuid(2);
    let running_occupant = test_uuid(3);
    let vacant = test_uuid(4);
    let retained_occupant = test_uuid(5);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    seed_lane_in_worktree(&bus, epoch, running, running_occupant, &worktree);
    seed_vacant_lane_in_worktree(&bus, epoch, vacant, &worktree);

    let running_session = declaration(running, running_occupant, epoch, &worktree, 1);
    let vacant_session = declaration(vacant, retained_occupant, epoch, &worktree, 2);
    attach_session(&bus, epoch, &running_session);
    attach_session(&bus, epoch, &vacant_session);

    let mut state = message_surface::MessageState::default();
    retain(
        &mut state,
        &running_session,
        "ledger migration schema durable storage journal authority transaction recovery indexing snapshot replication checksum manifest",
        10,
    );
    retain(
        &mut state,
        &vacant_session,
        "canvas renderer vector tessellation shader geometry viewport layer drawing pixels texture gradient bezier polygon raster animation",
        20,
    );
    let profiles = std::sync::Mutex::new(state);
    let send = wire::intent::SteerSend {
        fanout: test_uuid(31),
        recipients: Vec::new(),
        lane: None,
        message: "Please repair the canvas renderer tessellation".to_owned(),
        attachments: Vec::new(),
    };
    let command = Command {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(30),
        epoch,
        kind: CommandKind::SteerSend,
        expected: Vec::new(),
        body: serde_json::to_value(&send).expect("automatic send encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::TEST_PRINCIPAL,
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .expect("automatic send admits");
    let mut effects = CommandEffects::default();
    let outcome = steering::send(
        &bus,
        epoch,
        &steer::Config::default(),
        Some(&profiles),
        &command,
        &send,
        &mut effects,
    );
    let Outcome::Reply(Reply {
        body: CommandAnswer::Directive(answer),
        ..
    }) = outcome
    else {
        panic!("automatic message commits: {outcome:?}");
    };
    assert_eq!(answer.recipient, Actor::Lane(running));
    assert_eq!(effects.inbox_lanes, [running]);

    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

fn declaration(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    worktree: &str,
    member: u64,
) -> wire::body::LaneSessionDeclared {
    wire::body::LaneSessionDeclared {
        lane,
        occupant,
        worktree: worktree.to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("model", "effort"),
        epoch,
        stream: "session".to_owned(),
        source: test_uuid(80),
        identity: format!("session-{member}"),
        member,
        offset: member * 100,
        length: 50,
    }
}

fn attach_session(bus: &Bus, epoch: CanonicalUuid, declaration: &wire::body::LaneSessionDeclared) {
    let current = current_residency(bus, epoch).expect("current residency reads");
    let mut head = current.body().clone();
    let row = head
        .lanes
        .iter_mut()
        .find(|row| row.lane == declaration.lane)
        .expect("declared lane is seated");
    row.session = Some(wire::LaneSession {
        lane: declaration.lane,
        occupant: declaration.occupant,
        worktree: declaration.worktree.clone(),
        vendor: declaration.vendor.clone(),
        launch: declaration.launch.clone(),
        epoch: declaration.epoch,
        stream: declaration.stream.clone(),
        identity: declaration.identity.clone(),
        locus: wire::TapeLocus {
            member: declaration.member,
            offset: declaration.offset,
            length: declaration.length,
        },
    });
    let mut proposal =
        write(Actor::Spice, epoch, Producer::ResidentRecognition).expect("session write starts");
    let declared = proposal
        .fact(declaration.clone(), FactCause::Existing(current.position()))
        .expect("session declaration is valid");
    proposal.residency_head(declared, move |_| head);
    commit(bus, proposal).expect("session declaration commits");
}

fn retain(
    state: &mut message_surface::MessageState,
    declaration: &wire::body::LaneSessionDeclared,
    text: &str,
    offset: u64,
) {
    let conversation = Conversation::read(text).expect("fixture conversation has enough terms");
    state.profiles.declare(declaration);
    state.profiles.insert(
        declaration.lane,
        declaration.occupant,
        Evidence::conversation(
            EvidenceKey::Conversation {
                source: declaration.source,
                member: declaration.member,
                offset,
                length: u64::try_from(text.len()).expect("fixture text length"),
                caused_by: Vec::new(),
            },
            &conversation,
        ),
    );
}

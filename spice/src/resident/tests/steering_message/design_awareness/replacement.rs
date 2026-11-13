//! An owed Design request yields only to a strictly better match from the same
//! session's current evidence.

use std::fs;
use std::sync::{Arc, mpsc};
use std::thread;

use sill::{Acquisition, LockFile, LockName};
use wire::intent::CommandAnswer;
use wire::rooms::DesignReach;
use wire::{Actor, Outcome};

use super::*;

/// Retained conversation prose about Windows runner archives.
const WINDOWS_PROSE: &str = "Windows native runner validates archive compression headers \
                             libraries toolchain manifests receipts deployment replay smoke";
const WINDOWS_TITLE: &str = "Validate Windows native runner archives";
const WINDOWS_DESCRIPTION: &str =
    "Compression headers libraries toolchain manifests receipts deployment replay smoke.";
/// A Design sharing only its title's subject with [`WINDOWS_PROSE`].
const SIGNING_DESCRIPTION: &str = "Signing certificates notarization keychains entitlements.";

/// FIRST's accounted transcript source, the owner of retained conversation.
fn conversation_source(lanes: &Lanes) -> CanonicalUuid {
    lanes.account(FIRST);
    lanes
        .bus
        .read(|read| read.transcript_source_heads(&test_uuid(FIRST)))
        .expect("transcript sources read")
        .into_iter()
        .next()
        .expect("declared lane has a transcript source")
        .into_body()
        .source
}

/// Newer evidence that fits another Design strictly better replaces the owed
/// request: the replacement is recorded and told once, and `task next` then
/// presents only it.
#[test]
fn a_strictly_better_match_replaces_the_owed_request() {
    let lanes = Lanes::new("design-request-better-match");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut cursor = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("awareness cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    let source = conversation_source(&lanes);
    retain_conversation(&profiles, source, 0, WINDOWS_PROSE);
    let partial = lanes.design("dVrD245Kd", WINDOWS_TITLE, SIGNING_DESCRIPTION);
    let database = lanes.design(
        "dVrD245Km",
        "Repair database migration",
        "Indexes transactions snapshots rollback consistency schema storage locks recovery.",
    );
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    let owed = lanes
        .request(&config, FIRST)
        .expect("the partial match is owed first");
    assert_eq!(owed.design, partial);
    assert_eq!(lanes.noticed().len(), 1);

    retain_conversation(
        &profiles,
        source,
        1,
        "Repair database migration indexes transactions snapshots rollback consistency schema \
         storage locks recovery",
    );
    let told = lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    assert_eq!(
        told.iter()
            .map(|recorded| &recorded.request.design)
            .collect::<Vec<_>>(),
        [&database],
        "the replacement is told once"
    );
    let replacement = lanes
        .request(&config, FIRST)
        .expect("the strictly better match is owed");
    assert_eq!(replacement.design, database);
    let notices = lanes.noticed();
    assert_eq!(notices.len(), 2, "one replacing notice is recorded");
    assert_eq!(notices[1].design, stamp(&database));

    seated.release_all();
    lanes.finish();
}

/// A tied twin, a weaker Design, and newer evidence whose best fit is the owed
/// Design itself each leave the owed request in place and record nothing.
#[test]
fn a_match_no_better_than_the_owed_request_records_nothing() {
    let lanes = Lanes::new("design-request-no-better-match");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut cursor = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("awareness cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    let source = conversation_source(&lanes);
    retain_conversation(&profiles, source, 0, WINDOWS_PROSE);
    let owed = lanes.design("dVrD245Kd", WINDOWS_TITLE, WINDOWS_DESCRIPTION);
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .expect("the matching Design is owed")
            .design,
        owed
    );

    let _twin = lanes.design("dVrD245Km", WINDOWS_TITLE, WINDOWS_DESCRIPTION);
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    let _weaker = lanes.design("dVrD245Kn", WINDOWS_TITLE, SIGNING_DESCRIPTION);
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    retain_conversation(
        &profiles,
        source,
        1,
        "Windows native runner archive compression headers libraries toolchain manifests \
         receipts deployment replay smoke checks",
    );
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);

    assert_eq!(
        lanes
            .request(&config, FIRST)
            .expect("the owed request survives")
            .design,
        owed
    );
    assert_eq!(lanes.noticed().len(), 1, "no replacing notice is recorded");
    seated.release_all();
    lanes.finish();
}
#[test]
fn room_score_reports_current_session_without_changing_profile_state() {
    let lanes = Lanes::new("room-score-current-profile");
    align_completion_lane(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let _design = lanes.design(
        "dVrD245Kd",
        "Catchup pagination snapshot",
        "Lanes page retained publications through bounded cursors.",
    );
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    lanes.complete(
        &task,
        "Catchup pagination snapshot retained publication cursor.",
    );
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let address = lanes
        .bus
        .task_snapshot()
        .expect("snapshot reads")
        .facts()
        .occupant_address(test_uuid(FIRST), test_uuid(FIRST))
        .expect("current completion lane has an address");
    let inspect = || {
        let state = crate::resident::message_surface::lock_state(&profiles).expect("profiles lock");
        let profile = state
            .profiles
            .profile(test_uuid(FIRST))
            .expect("completion lane profile exists");
        let warm = profile.warm_state().clone();
        let evidence = profile
            .evidence()
            .evidence()
            .map(|card| (card.key().clone(), card.digest()))
            .collect::<Vec<_>>();
        let opportunities = state
            .profiles
            .opportunities(None, spice::rooms::profile::PROFILE_OPPORTUNITIES);
        (warm, evidence, opportunities)
    };
    let before = inspect();
    let notices = lanes.noticed().len();
    let request = wire::intent::RoomScore {
        address,
        text: "Catchup pagination snapshot".to_owned(),
    };
    let score = || {
        let Outcome::Reply(Reply {
            body: wire::intent::CommandAnswer::RoomScore(answer),
            ..
        }) = crate::resident::design_awareness::score(&lanes.bus, &profiles, &request)
        else {
            panic!("room score returns its structured answer");
        };
        *answer
    };
    for answer in [score(), score()] {
        assert_eq!(answer.address, address);
        assert_eq!(answer.lane, test_uuid(FIRST));
        assert_eq!(answer.session.vendor.as_str(), "omp");
        assert_eq!(
            answer.session.identity,
            test_uuid(FIRST.saturating_add(20)).to_string()
        );
        assert_eq!(
            answer
                .qualifications
                .iter()
                .map(|qualification| qualification.reach)
                .collect::<Vec<_>>(),
            [DesignReach::Wide, DesignReach::Narrow, DesignReach::Item]
        );
    }
    let unweightable = crate::resident::design_awareness::score(
        &lanes.bus,
        &profiles,
        &wire::intent::RoomScore {
            address,
            text: "the and or".to_owned(),
        },
    );
    assert!(
        matches!(
            &unweightable,
            Outcome::Refusal(refusal)
                if refusal.reason() == "room::score-text-unweightable"
        ),
        "{unweightable:?}"
    );

    assert_eq!(inspect(), before, "scoring moves no profile state");
    assert_eq!(
        lanes.noticed().len(),
        notices,
        "scoring records no Design notice"
    );
    seated.release_all();
    lanes.finish();
}

#[test]
fn room_score_ignores_stale_evidence_without_removing_it() {
    let lanes = Lanes::new("room-score-current-evidence");
    align_completion_lane(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let _design = lanes.design("dVrD245Kd", WINDOWS_TITLE, WINDOWS_DESCRIPTION);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);

    let stale_source = conversation_source(&lanes);
    retain_conversation(&profiles, stale_source, 0, WINDOWS_PROSE);
    lanes.complete(
        &task,
        "Windows native runner archives retain compression headers and manifests.",
    );
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let address = lanes
        .bus
        .task_snapshot()
        .expect("snapshot reads")
        .facts()
        .occupant_address(test_uuid(FIRST), test_uuid(FIRST))
        .expect("current completion lane has an address");
    let request = wire::intent::RoomScore {
        address,
        text: WINDOWS_PROSE.to_owned(),
    };
    let inspect = || {
        let state = crate::resident::message_surface::lock_state(&profiles).expect("profiles lock");
        let session = state
            .profiles
            .session(test_uuid(FIRST), test_uuid(FIRST))
            .expect("completion lane session is bound");
        let profile = state
            .profiles
            .profile(test_uuid(FIRST))
            .expect("completion lane profile exists");
        (
            session.clone(),
            profile.warm_state().clone(),
            profile
                .evidence()
                .evidence()
                .map(|evidence| (evidence.key().clone(), evidence.digest()))
                .collect::<Vec<_>>(),
            state
                .profiles
                .opportunities(None, spice::rooms::profile::PROFILE_OPPORTUNITIES),
        )
    };
    let before = inspect();
    let notices = lanes.noticed().len();
    let score = || {
        let Outcome::Reply(Reply {
            body: wire::intent::CommandAnswer::RoomScore(answer),
            ..
        }) = crate::resident::design_awareness::score(&lanes.bus, &profiles, &request)
        else {
            panic!("room score returns its current-evidence measurement");
        };
        *answer
    };
    for current_source in [score(), score()] {
        let wire::rooms::DesignNoticeEvidence::Conversation { source } = &current_source.evidence
        else {
            panic!("the exact-text conversation card wins while its source is current");
        };
        assert_eq!(
            source.locus.source,
            Some(stale_source),
            "the exact-text card is the source-current winner"
        );
    }

    lanes.replace_transcript_source(FIRST);
    for filtered in [score(), score()] {
        assert!(
            matches!(
                &filtered.evidence,
                wire::rooms::DesignNoticeEvidence::Task { .. }
            ),
            "production-current task evidence wins after the stale card is excluded"
        );
    }
    assert_eq!(
        inspect(),
        before,
        "diagnostic filtering preserves session, warm cursors, retained evidence and opportunities"
    );
    assert_eq!(
        lanes.noticed().len(),
        notices,
        "diagnostic filtering records no Design notice"
    );

    seated.release_all();
    lanes.finish();
}

#[test]
fn room_score_crosses_authenticated_rpc_without_changing_profile_state() {
    let lanes = Lanes::new("room-score-managed-rpc");
    align_completion_lane(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let _design = lanes.design(
        "dVrD245Kd",
        "Catchup pagination snapshot",
        "Lanes page retained publications through bounded cursors.",
    );
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = Arc::new(std::sync::Mutex::new(
        crate::resident::message_surface::MessageState::default(),
    ));
    lanes.complete(
        &task,
        "Catchup pagination snapshot retained publication cursor.",
    );
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let address = lanes
        .bus
        .task_snapshot()
        .expect("snapshot reads")
        .facts()
        .occupant_address(test_uuid(FIRST), test_uuid(FIRST))
        .expect("current completion lane has an address");
    let inspect = || {
        let state = crate::resident::message_surface::lock_state(&profiles).expect("profiles lock");
        let profile = state
            .profiles
            .profile(test_uuid(FIRST))
            .expect("completion lane profile exists");
        (
            profile.warm_state().clone(),
            profile
                .evidence()
                .evidence()
                .map(|card| (card.key().clone(), card.digest()))
                .collect::<Vec<_>>(),
            state
                .profiles
                .opportunities(None, spice::rooms::profile::PROFILE_OPPORTUNITIES),
        )
    };
    let before = inspect();
    let notices = lanes.noticed().len();

    let (outcome, operator_outcome) = run_score_rpc(
        &lanes,
        Arc::clone(&profiles),
        address,
        "Catchup pagination snapshot",
    );
    assert_eq!(
        operator_outcome, outcome,
        "operator and lane callers receive the same measurement"
    );
    let Outcome::Reply(Reply {
        body: CommandAnswer::RoomScore(answer),
        ..
    }) = outcome
    else {
        panic!("managed room score returns its structured answer");
    };
    assert_eq!(answer.address, address);
    assert_eq!(answer.lane, test_uuid(FIRST));
    assert!(answer.score.get().is_finite());
    assert!(answer.text.contains("do not predict admission"));
    assert_eq!(inspect(), before, "managed scoring moves no profile state");
    assert_eq!(
        lanes.noticed().len(),
        notices,
        "managed scoring records no Design notice"
    );

    seated.release_all();
    lanes.finish();
}

fn run_score_rpc(
    lanes: &Lanes,
    profiles: Arc<std::sync::Mutex<crate::resident::message_surface::MessageState>>,
    address: wire::OccupantAddress,
    text: &str,
) -> (Outcome<CommandAnswer>, Outcome<CommandAnswer>) {
    let repository = Repository::discover(&lanes.root).expect("fixture repository resolves");
    let state = repository.admit_state().expect("fixture state admits");
    fs::create_dir_all(
        state
            .resident()
            .parent()
            .expect("resident possession has a runtime parent"),
    )
    .expect("fixture runtime exists");
    fs::create_dir_all(state.lane_serve()).expect("lane discovery directory exists");
    let lock = LockFile::new(
        LockName::new("resident").expect("resident is a valid lock name"),
        state.resident(),
    );
    let Acquisition::Acquired(publisher) = lock
        .try_acquire()
        .expect("fixture resident possession opens")
    else {
        panic!("a private score fixture cannot have a resident contender");
    };
    publisher
        .rewrite_record(
            ::spice::resident_lock::holder_record(std::process::id(), Some(lanes.epoch)).as_bytes(),
        )
        .expect("fixture resident generation records");

    let lane_capability = test_uuid(90);
    let operator_capability = test_uuid(91);
    let roles = crate::resident::principal_surface::roles(&lanes.bus)
        .expect("fixture principal roles read");
    let principal = roles
        .bootstrap()
        .expect("fixture bootstrap principal is born");
    let capabilities = serve::axum::Capabilities::owned_by(operator_capability, principal)
        .expect("fixture operator capability registers");
    capabilities
        .publish(roles)
        .expect("fixture principal roles publish");
    capabilities
        .insert(lane_capability, Actor::Lane(test_uuid(FIRST)))
        .expect("fixture lane capability registers");
    let (app, version) = fixture_app(&lanes.root);
    let generation = serve::axum::Generation::new(lanes.epoch, capabilities, version, app);
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let (stop_sender, stop_receiver) = tokio::sync::oneshot::channel();

    let outcome = thread::scope(|scope| {
        let worker = thread::Builder::new()
            .name("spice-room-score-rpc-test".to_owned())
            .spawn_scoped(scope, || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("score fixture runtime builds");
                runtime.block_on(async {
                    let (server, mut control) = serve::axum::Server::bind(generation)
                        .await
                        .expect("score fixture server binds");
                    ready_sender
                        .send(format!(
                            "http://{}",
                            server.local_addr().expect("bound address reads")
                        ))
                        .expect("score fixture endpoint is observed");
                    let serving = tokio::spawn(server.up_until(async move {
                        let _ = stop_receiver.await;
                    }));
                    while let Some(request) = control.recv().await {
                        let wire::intent::Intent::RoomScore(body) = request.command().body() else {
                            panic!("score fixture receives only room score");
                        };
                        let body = body.clone();
                        let outcome =
                            crate::resident::design_awareness::score(&lanes.bus, &profiles, &body);
                        request
                            .respond(outcome)
                            .expect("score fixture client awaits its response")
                            .wait()
                            .await;
                    }
                    serving
                        .await
                        .expect("score fixture server task joins")
                        .expect("score fixture server stops cleanly");
                });
            })
            .expect("score fixture server thread starts");
        let endpoint = ready_receiver
            .recv()
            .expect("score fixture server publishes its endpoint");
        serve::Discovery {
            endpoint: endpoint.clone(),
            capability: lane_capability,
            epoch: lanes.epoch,
        }
        .publish(state.lane_serve().join(test_uuid(FIRST).to_string()))
        .expect("fixture lane discovery publishes");
        serve::Discovery {
            endpoint,
            capability: operator_capability,
            epoch: lanes.epoch,
        }
        .publish(state.serve())
        .expect("fixture operator discovery publishes");

        let outcomes = score_clients(&repository, address, text);
        let _ = stop_sender.send(());
        worker.join().expect("score fixture server thread joins");
        outcomes
    });
    drop(publisher);
    outcome
}

fn score_clients(
    repository: &Repository,
    address: wire::OccupantAddress,
    text: &str,
) -> (Outcome<CommandAnswer>, Outcome<CommandAnswer>) {
    let address_argument = address.to_string();
    let matches = crate::cli::command()
        .try_get_matches_from(["spice", "room", "score", address_argument.as_str(), text])
        .expect("room score leaf parses");
    let contract =
        crate::cli::selected_contract(&matches).expect("room score contract is selected");
    let leaf = crate::cli::selected_matches(&matches);
    let ambient = spice::ambient::Ambient::default();
    let lane_outcome = crate::command_client::as_lane(test_uuid(FIRST), || {
        crate::board_command::run_at(
            crate::command_client::Host {
                repository,
                ambient: &ambient,
            },
            leaf,
            contract,
        )
    });
    let operator_outcome = crate::board_command::run_at(
        crate::command_client::Host {
            repository,
            ambient: &spice::ambient::Ambient::default(),
        },
        leaf,
        contract,
    );
    (lane_outcome, operator_outcome)
}

fn fixture_app(root: &std::path::Path) -> (serve::canvas::CanvasAssets, wire::Version) {
    const GENERATION: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let stage = root.join(serve::APP_STAGE);
    let generation = stage.join(serve::APP_STAGE_GENERATIONS).join(GENERATION);
    fs::create_dir_all(&generation).expect("fixture app generation exists");
    for asset in serve::APP.assets() {
        fs::write(
            generation.join(asset.file()),
            format!("fixture {}", asset.file()),
        )
        .expect("fixture app asset writes");
    }
    fs::write(
        stage.join(serve::APP_STAGE_CURRENT),
        format!("{GENERATION}\n"),
    )
    .expect("fixture app current pointer writes");
    let app = serve::canvas::CanvasAssets::DevelopmentDirectory(
        serve::AppSource::resolve(root).expect("fixture app resolves"),
    );
    let version = wire::Version {
        product: "1.2.3".parse().expect("fixture product version"),
        binary: wire::Digest::compute(wire::Domain::DistArtifact, b"fixture native"),
        wire: wire::WireVersion::CURRENT,
        journal: journal::SCHEMA_VERSION,
        app_protocol: wire::AppProtocolVersion::CURRENT,
        app: app.generation().expect("fixture app generation reads"),
    };
    (app, version)
}

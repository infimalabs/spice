//! Resident media admission and durable steering replay regressions.

use super::*;
use base64::Engine as _;

#[test]
fn direct_steer_send_refuses_unjournalable_content_before_mutation() {
    let (root, paths) = resident_fixture("steer-media-admission");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let worktree = repository.worktree_root().to_string_lossy().into_owned();
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = test_uuid(1);
    let lane = test_uuid(2);
    let occupant = test_uuid(3);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    seed_lane_in_worktree(&bus, epoch, lane, occupant, &worktree);
    let media = large_media();
    let malformed = wire::Media {
        name: "malformed.png".to_owned(),
        content_type: "image/png".to_owned(),
        data_url: "not-a-data-url".to_owned(),
    };
    let cases = [
        (
            "inspect".to_owned(),
            vec![malformed],
            "steer::media-invalid",
        ),
        (
            "inspect".to_owned(),
            vec![media.clone(), media.clone()],
            "steer::media-invalid",
        ),
        (
            "\0".repeat(journal::MAX_DURABLE_FACT_ENVELOPE_BYTES / 6),
            Vec::new(),
            "steer::envelope-too-large",
        ),
        (
            "x".repeat(2 * 1024 * 1024),
            vec![media],
            "steer::envelope-too-large",
        ),
    ];
    let before = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("journal reads before refusals");
    for (message, attachments, reason) in cases {
        let request = wire::intent::SteerSend {
            fanout: test_uuid(90),
            recipients: Vec::new(),
            lane: Some(lane),
            message,
            attachments,
        };
        let command = Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(30),
            epoch,
            kind: CommandKind::SteerSend,
            expected: vec![Witness::Residency {
                lane,
                occupant: Some(occupant),
                epoch,
            }],
            body: serde_json::to_value(&request).expect("steer request encodes"),
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
        .expect("resident content admission remains authoritative");
        let mut effects = CommandEffects::default();
        let outcome = steering::send(
            &bus,
            epoch,
            &steer::Config::default(),
            None,
            &command,
            &request,
            &mut effects,
        );
        let Outcome::Refusal(refusal) = outcome else {
            panic!("unjournalable direction must refuse at admission: {outcome:?}");
        };
        assert_eq!(refusal.reason(), reason);
        let maximum = if reason == "steer::media-invalid" {
            wire::media::MAX_MEDIA_DATA_URL_BYTES
        } else {
            journal::MAX_DURABLE_FACT_ENVELOPE_BYTES
        };
        assert!(
            refusal
                .diagnostic()
                .unwrap()
                .expected()
                .unwrap()
                .contains(&maximum.to_string()),
            "operator refusal must name the violated byte bound"
        );
        assert!(effects.inbox_lanes.is_empty());
        let after = bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("journal reads after refusal");
        assert_eq!(after.horizon(), before.horizon());
        assert_eq!(after.facts(), before.facts());
    }
    bus.verify().expect("refused authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn steer_send_receipts_retain_content_digests_and_replay_exactly() {
    let (root, paths) = resident_fixture("steer-media-receipt");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let worktree = repository.worktree_root().to_string_lossy().into_owned();
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = test_uuid(11);
    let lane = test_uuid(12);
    let occupant = test_uuid(13);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    seed_lane_in_worktree(&bus, epoch, lane, occupant, &worktree);

    let media = wire::Media::from_bytes("capture.png", "image/png", b"\x89PNG\r\n\x1a\n\0")
        .expect("fixture image admits");
    let message = "inspect the private capture";
    let request = wire::intent::SteerSend {
        fanout: test_uuid(19),
        recipients: Vec::new(),
        lane: Some(lane),
        message: message.to_owned(),
        attachments: vec![media.clone()],
    };
    let command = admitted_send(epoch, lane, occupant, test_uuid(20), &request);
    let mut effects = CommandEffects::default();
    let first = steering::send(
        &bus,
        epoch,
        &steer::Config::default(),
        None,
        &command,
        &request,
        &mut effects,
    );
    assert!(matches!(first, Outcome::Reply(_)), "{first:?}");

    let exact = command.clone();
    assert_eq!(
        bus.read(move |read| read.command_replay(&exact))
            .expect("exact receipt reads"),
        Some(journal::CommandReplay::Exact(first.clone()))
    );
    let changed_request = wire::intent::SteerSend {
        message: "inspect another capture".to_owned(),
        ..request.clone()
    };
    let changed = admitted_send(epoch, lane, occupant, test_uuid(20), &changed_request);
    assert_eq!(
        bus.read(move |read| read.command_replay(&changed))
            .expect("conflicting receipt reads"),
        Some(journal::CommandReplay::Conflict)
    );
    bus.verify()
        .expect("digest receipt verifies against its fact");
    bus.shutdown().expect("fixture bus stops");

    assert_stored_receipt_shapes(&paths.journal, test_uuid(20), message, &media);
    fs::remove_dir_all(root).expect("fixture cleanup");
}

fn assert_stored_receipt_shapes(
    authority: &std::path::Path,
    id: CanonicalUuid,
    message: &str,
    media: &wire::Media,
) {
    let connection =
        rusqlite::Connection::open(authority).expect("receipt authority opens directly");
    let stored = connection
        .query_row(
            "SELECT request FROM command_receipts WHERE command_id = ?1",
            [id.to_string()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .expect("stored request reads");
    let mut document: serde_json::Value =
        serde_json::from_slice(&stored).expect("stored request is JSON");
    assert_eq!(
        document["body"]["message"]["digest"],
        wire::QuoteTarget::text_digest(message).to_string()
    );
    assert_eq!(
        document["body"]["attachments"][0]["data_url"]["digest"],
        media
            .identity()
            .expect("fixture media identity")
            .to_string()
    );
    let stored_text = String::from_utf8(stored).expect("canonical request is UTF-8");
    assert!(!stored_text.contains(message));
    assert!(!stored_text.contains(&media.data_url));

    document["body"]["message"]["digest"] = wire::QuoteTarget::text_digest("different content")
        .to_string()
        .into();
    let tampered: Command<serde_json::Value> =
        serde_json::from_value(document).expect("tampered request decodes");
    let tampered = tampered
        .canonical_json()
        .expect("tampered request canonicalizes")
        .into_bytes();
    connection
        .execute(
            "UPDATE command_receipts SET request = ?1 WHERE command_id = ?2",
            rusqlite::params![tampered, id.to_string()],
        )
        .expect("mismatched digest plants");
    drop(connection);
    let journal = journal::Journal::open(authority).expect("tampered authority reopens");
    let failure = journal
        .verify()
        .expect_err("a digest that disagrees with the fact must fail verification");
    assert!(
        failure.violations().iter().any(|violation| {
            violation.class == journal::ViolationClass::Receipt
                && violation.detail.contains("steer::sent fact does not match")
        }),
        "semantic steer::send corruption was not reported: {failure}"
    );
}

fn admitted_send(
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    id: CanonicalUuid,
    request: &wire::intent::SteerSend,
) -> wire::AdmittedCommand {
    Command {
        v: wire::WireVersion::CURRENT,
        id,
        epoch,
        kind: CommandKind::SteerSend,
        expected: vec![Witness::Residency {
            lane,
            occupant: Some(occupant),
            epoch,
        }],
        body: serde_json::to_value(request).expect("steer request encodes"),
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
    .expect("steer command admits")
}

fn large_media() -> wire::Media {
    let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
    let payload_bytes =
        (wire::media::MAX_MEDIA_DATA_URL_BYTES - "data:image/webp;base64,".len()) / 4 * 3;
    bytes.resize(payload_bytes, 0);
    wire::Media::from_base64(
        "operator-capture.webp",
        "image/webp",
        &base64::engine::general_purpose::STANDARD.encode(bytes),
    )
    .expect("operator image validates")
}

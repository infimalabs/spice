//! An invitation's message is authored from the captured invite write on the
//! inviter's tape and settles as one peer message in the task's room.

use wire::intent::{TaskInvite, TaskInviteAnswer};

use super::*;

const TITLE: &str = "Keep one lane busy";

/// Land `lane`'s write of `text` to the invite URL of `task`, addressed when
/// `recipient` names a lane, and name the captured record.
fn written(
    lanes: &Lanes,
    lane: u8,
    task: &wire::TaskHandle,
    recipient: Option<OccupantAddress>,
    text: &str,
) -> Carrier {
    let url = recipient.map_or_else(
        || format!("spice://task/{task}/invite"),
        |address| format!("spice://task/{task}/invite/{address}"),
    );
    let record = serde_json::json!({
        "type": "host_uri_request", "id": format!("invite-{}", lanes.cursor()),
        "operation": "write", "url": url, "content": text,
    });
    let locus = lanes
        .tape(lane)
        .tee(format!("{record}\n").as_bytes())
        .expect("complete host operation reaches tape");
    Carrier {
        occupant: test_uuid(lane + 1),
        locus: wire::TapeLocus {
            member: locus.member,
            offset: locus.offset,
            length: locus.length,
        },
    }
}

/// `lane` invites `recipient` to `task` as its client would: under the
/// witnesses of the board it read, carrying the captured write.
fn invite(
    lanes: &Lanes,
    lane: u8,
    task: &wire::TaskHandle,
    recipient: OccupantAddress,
    message: Carrier,
) -> Outcome<CommandAnswer> {
    let read = wire::intent::BoardRead::All { family: None };
    let board = match crate::board_command::dispatch(
        &lanes.bus,
        &command(lanes, lane, CommandKind::BoardRead, &read),
    ) {
        Some(Outcome::Reply(Reply {
            body: CommandAnswer::Board(board),
            ..
        })) => *board,
        other => panic!("the board reads: {other:?}"),
    };
    let (kind, expected, body) = match crate::board_command::invitation_offer(
        &board,
        Some(task),
        Some(recipient),
        Some(message),
    ) {
        Ok(offer) => offer,
        Err(outcome) => return outcome,
    };
    let intent: TaskInvite = serde_json::from_value(body).expect("invitation intent decodes");
    crate::resident::command_surface::invite(
        &lanes.bus,
        &lanes.paths,
        (lanes.epoch, lanes.joining.joining()),
        &steer::Config::default(),
        None,
        (&witnessed(lanes, lane, (kind, expected), &intent), &intent),
    )
    .0
}

fn invited(outcome: Outcome<CommandAnswer>) -> TaskInviteAnswer {
    match outcome {
        Outcome::Reply(Reply {
            body: CommandAnswer::TaskInvite(answer),
            ..
        }) => *answer,
        other => panic!("the invitation commits: {other:?}"),
    }
}

fn refused(outcome: &Outcome<CommandAnswer>) -> &str {
    match outcome {
        Outcome::Refusal(refusal) => refusal.reason(),
        other => panic!("the invitation refuses: {other:?}"),
    }
}

/// Every pending input of `lane`, as its text.
fn told(lanes: &Lanes, lane: u8) -> Vec<String> {
    lanes
        .pending(lane)
        .into_iter()
        .map(|head| head.text)
        .collect()
}

fn admissions(lanes: &Lanes) -> usize {
    lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .filter(|envelope| envelope.fact.kind() == wire::FactKind::OutboxAccepted)
        .count()
}

fn room_records(lanes: &Lanes, task: &wire::TaskHandle) -> Vec<wire::rooms::RoomMessaged> {
    let room = task.born().expect("fixture task has a birth stamp");
    lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::rooms::RoomMessaged>())
        .filter(|messaged| messaged.room == room)
        .collect()
}

#[test]
fn an_invitation_message_is_authored_from_the_inviters_tape_and_delivered_once() {
    let lanes = Lanes::new("invite-message");
    lanes.declare(SENDER);
    let task = lanes.seed_task("dVrD245Kc", TITLE, None, None, Some(test_uuid(SENDER)));
    let recipient = lanes.address(FIRST);
    // The URL alone addresses the message: a header inside it opens nothing.
    let words = format!(
        "Review the cut first.\n\nSYN {}: quoted, not sent.",
        lanes.address(SECOND)
    );
    let body = format!("\n{words}\n");
    let message = written(&lanes, SENDER, &task, Some(recipient), &body);

    let answer = invited(invite(&lanes, SENDER, &task, recipient, message));
    assert_eq!(answer.lane, test_uuid(FIRST));
    let submission = answer.message.expect("the invitation names its message");
    assert_eq!(
        told(&lanes, FIRST),
        [format!("Join {task} in todo: {TITLE}"), words.clone()],
        "the invitation keeps its own line and the words arrive once, as a peer message"
    );
    assert!(told(&lanes, SECOND).is_empty());
    let [recorded] = room_records(&lanes, &task)
        .try_into()
        .unwrap_or_else(|records| panic!("one room record: {records:?}"));
    assert_eq!(recorded.author, test_uuid(SENDER + 1));
    assert_eq!(recorded.directed.len(), 1);

    let inspection = receipt(inspect(&lanes, SENDER, submission));
    assert_eq!(inspection.text.as_deref(), Some(body.as_str()));
    assert!(inspection.unresolved.is_empty());
    assert!(
        matches!(&inspection.decided[..], [decided]
            if matches!(&decided.outcome, PacketOutcome::Messaged { lanes: delivered, .. }
                if delivered == &[test_uuid(FIRST)])),
        "{:?}",
        inspection.decided
    );
    let reader = lanes.bus.reader();
    let resolver = spice::quote::Resolver::from_tape(&lanes.paths.tape, &reader);
    let quote = resolver.resolve(&inspection.quote, 0);
    assert!(
        matches!(&quote, wire::QuoteResolution::Resolved(resolved) if resolved.text == body),
        "{quote:?}"
    );
    // The author's card leads the whole message with its invitation's
    // addressing, the lane it reached and the task's room, as a SYN's
    // outgoing quote does; the header inside it addresses nothing.
    let card = presented(&lanes, false);
    let [send] = card.sends.as_slice() else {
        panic!("one outgoing send: {card:?}");
    };
    assert_eq!(send.lanes, [test_uuid(FIRST)]);
    assert_eq!(send.contexts, std::slice::from_ref(&task));
    assert!(send.recipients.is_empty() && !send.operator, "{send:?}");
    assert_eq!(
        (send.from, send.sender),
        (test_uuid(SENDER), test_uuid(SENDER + 1))
    );
    assert_eq!(send.body, words);
    assert!(wire::valid_message_sends(
        &card.text,
        &card.sends,
        &card.directive_contexts
    ));
    assert_eq!(card, presented(&lanes, true));

    // One captured record admits one message.
    assert_eq!(
        refused(&invite(&lanes, SENDER, &task, recipient, message)),
        "outbox::source-spent"
    );
    assert_eq!(admissions(&lanes), 1);
    lanes.finish();
}

#[test]
fn a_refused_or_unproved_invitation_admits_and_delivers_nothing() {
    let lanes = Lanes::new("invite-message-refused");
    lanes.declare(SENDER);
    let task = lanes.seed_task("dVrD245Kc", TITLE, None, None, Some(test_uuid(SENDER)));
    lanes.seed_task(
        "dVrD245Kp",
        "Hold another",
        None,
        None,
        Some(test_uuid(SECOND)),
    );
    let words = "Review the cut first.";

    // Proved words do not outlive the invitation that carried them: a busy
    // recipient refuses it.
    let busy = lanes.address(SECOND);
    let message = written(&lanes, SENDER, &task, Some(busy), words);
    assert_eq!(
        refused(&invite(&lanes, SENDER, &task, busy, message)),
        "task::invite-busy"
    );

    // Words addressed elsewhere, or written to the outbox, prove nothing for
    // this recipient, and no invitation is offered without them.
    let recipient = lanes.address(FIRST);
    let elsewhere = written(&lanes, SENDER, &task, Some(busy), words);
    let unaddressed = written(&lanes, SENDER, &task, None, words);
    let outbox = captured(&lanes, SENDER, words).carrier;
    for carrier in [elsewhere, unaddressed, outbox] {
        assert_eq!(
            refused(&invite(&lanes, SENDER, &task, recipient, carrier)),
            "outbox::source-unproved"
        );
    }

    assert_eq!(admissions(&lanes), 0);
    assert!(told(&lanes, FIRST).is_empty());
    assert!(told(&lanes, SECOND).is_empty());
    assert!(room_records(&lanes, &task).is_empty());
    lanes.finish();
}

#[test]
fn an_interrupted_invitation_message_settles_once_after_restart() {
    let mut lanes = Lanes::new("invite-message-recovery");
    lanes.declare(SENDER);
    let task = lanes.seed_task("dVrD245Kc", TITLE, None, None, Some(test_uuid(SENDER)));
    let recipient = lanes.address(FIRST);
    let words = "Review the cut first.";
    let message = written(&lanes, SENDER, &task, Some(recipient), words);
    let connection = rusqlite::Connection::open(&lanes.paths.journal).expect("isolated authority");
    connection
        .execute_batch(
            "CREATE TRIGGER interrupt_invitation BEFORE INSERT ON fact_envelopes
         WHEN NEW.kind = 'outbox::judged'
         BEGIN SELECT RAISE(ABORT, 'interrupted packet transaction'); END;",
        )
        .expect("deterministic interruption is armed");

    // The invitation stands with its message admitted and nothing delivered.
    let answer = invited(invite(&lanes, SENDER, &task, recipient, message));
    let submission = answer.message.expect("the invitation names its message");
    assert_eq!(
        told(&lanes, FIRST),
        [format!("Join {task} in todo: {TITLE}")]
    );
    assert_eq!(receipt(inspect(&lanes, SENDER, submission)).unresolved, [0]);
    assert!(room_records(&lanes, &task).is_empty());

    connection
        .execute_batch("DROP TRIGGER interrupt_invitation;")
        .expect("storage is available again");
    drop(connection);
    lanes.bus.shutdown().expect("old resident stops");
    lanes.bus =
        Bus::start(journal::Journal::open(&lanes.paths.journal).expect("durable journal reopens"))
            .expect("fresh bus rebuilds its indexes");
    lanes.epoch = test_uuid(81);
    let repository = Repository::discover(&lanes.root).expect("same repository");
    let surface = crate::trust_command::Surface::load(&repository).expect("same trust surface");
    commit_startup(&lanes.bus, &repository, &surface, lanes.epoch, None)
        .expect("new resident adopts the exact existing occupants");
    for _ in 0..2 {
        steering::outbox::recover(
            &lanes.bus,
            &lanes.paths,
            (lanes.epoch, lanes.joining.joining()),
            &steer::Config::default(),
        )
        .expect("the admitted message resumes from its own addressing");
    }

    let recovered = receipt(inspect(&lanes, SENDER, submission));
    assert!(recovered.unresolved.is_empty());
    assert!(
        matches!(&recovered.decided[..], [decided]
            if matches!(&decided.outcome, PacketOutcome::Messaged { lanes: delivered, .. }
                if delivered == &[test_uuid(FIRST)])),
        "{:?}",
        recovered.decided
    );
    assert_eq!(
        told(&lanes, FIRST)
            .iter()
            .filter(|text| text.as_str() == words)
            .count(),
        1
    );
    assert_eq!(room_records(&lanes, &task).len(), 1);
    lanes.finish();
}

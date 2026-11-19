//! Tape-backed outbox behavior through real native admission and packet effects.

use super::*;
use wire::outbox::{BodyFate, Carrier, PacketOutcome, Receipt, Submit};

mod invitation;

fn captured(lanes: &Lanes, lane: u8, text: &str) -> Submit {
    let record = serde_json::json!({
        "type": "host_uri_request", "id": format!("outbox-{}", lanes.cursor()),
        "operation": "write", "url": "spice://outbox", "content": text,
    });
    let locus = lanes
        .tape(lane)
        .tee(format!("{record}\n").as_bytes())
        .expect("complete host operation reaches tape");
    let current = current_residency(&lanes.bus, lanes.epoch).expect("current author");
    let occupant = current
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == test_uuid(lane))
        .and_then(|row| row.occupant)
        .expect("occupied author");
    Submit {
        carrier: Carrier {
            occupant,
            locus: wire::TapeLocus {
                member: locus.member,
                offset: locus.offset,
                length: locus.length,
            },
        },
        text: text.to_owned(),
    }
}

fn command(
    lanes: &Lanes,
    lane: u8,
    kind: CommandKind,
    body: impl serde::Serialize,
) -> wire::AdmittedCommand {
    witnessed(lanes, lane, (kind, Vec::new()), body)
}

/// Admit `lane`'s command under the witnesses its client read.
fn witnessed(
    lanes: &Lanes,
    lane: u8,
    (kind, expected): (CommandKind, Vec<wire::Witness>),
    body: impl serde::Serialize,
) -> wire::AdmittedCommand {
    Command {
        v: wire::WireVersion::CURRENT,
        id: new_uuid(),
        epoch: lanes.epoch,
        kind,
        expected,
        body: serde_json::to_value(body).expect("native body"),
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
    .expect("authenticated lane resource command")
}

fn submit(lanes: &Lanes, lane: u8, request: &Submit) -> Outcome<CommandAnswer> {
    steering::outbox::submit(
        &lanes.bus,
        &lanes.paths,
        (lanes.epoch, lanes.joining.joining()),
        &command(lanes, lane, CommandKind::OutboxSubmit, request),
        request,
        &steer::Config::default(),
        &mut CommandEffects::default(),
    )
}

fn receipt(outcome: Outcome<CommandAnswer>) -> Receipt {
    match outcome {
        Outcome::Reply(Reply {
            body: CommandAnswer::Outbox(receipt),
            ..
        }) => *receipt,
        other => panic!("accepted submission has a truthful receipt: {other:?}"),
    }
}

fn inspect(lanes: &Lanes, lane: u8, id: CanonicalUuid) -> Outcome<CommandAnswer> {
    let current = current_residency(&lanes.bus, lanes.epoch).expect("current reader");
    let occupant = current
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == test_uuid(lane))
        .and_then(|row| row.occupant)
        .expect("occupied reader");
    let request = wire::outbox::Read {
        submission: id,
        occupant,
    };
    steering::outbox::inspect(
        &lanes.bus,
        &lanes.paths,
        lanes.epoch,
        &command(lanes, lane, CommandKind::OutboxRead, &request),
        &request,
    )
}

fn presented_messages(lanes: &Lanes, replay: bool) -> Vec<wire::AgentMessage> {
    let source = message_source(lanes.bus.reader(), lanes.epoch);
    let publisher = source.message_publisher();
    if replay {
        let residency = current_residency(&lanes.bus, lanes.epoch).expect("seated author");
        message_surface::restore_agent_messages_indexed(
            &lanes.bus.reader(),
            &lanes.paths,
            residency.body(),
            &publisher,
            &mut message_surface::BacklogReader::indexed(),
            &mut Vec::new(),
        )
        .expect("admitted outbox restores from tape");
    } else {
        message_surface::outbox::advance(
            &lanes.bus.reader(),
            &lanes.paths,
            &publisher,
            &std::sync::Mutex::new(MessageState::default()),
        )
        .expect("admitted outbox publishes live");
    }
    let accepted = wire::Accepted {
        wire: wire::WireVersion::CURRENT,
        actor: Actor::TEST_PRINCIPAL,
        epoch: lanes.epoch,
        release: "1.2.3".parse().expect("fixture release"),
    };
    let request = wire::StreamRequest::Subscribe {
        v: wire::WireVersion::CURRENT,
        id: new_uuid(),
        epoch: lanes.epoch,
        subjects: vec![wire::ViewSubject::Messages],
        after: None,
    };
    let (_, frames) = source.begin(&accepted, &request).expect("app subscribes");
    frames
        .into_iter()
        .flat_map(|frame| match frame {
            wire::StreamFrame::Messages { generation, .. } => generation.messages,
            _ => Vec::new(),
        })
        .collect()
}

fn presented(lanes: &Lanes, replay: bool) -> wire::AgentMessage {
    presented_messages(lanes, replay)
        .into_iter()
        .find(|message| message.activity.is_prose() && message.lane == test_uuid(SENDER))
        .expect("the authored card reaches the app")
}

#[test]
fn outbox_preserves_unicode_and_independent_native_outcomes() {
    let lanes = Lanes::new("outbox-mixed");
    lanes.declare(SENDER);
    let ack = lanes.direct(SENDER, 40, "first direction");
    let nak = lanes.direct(SENDER, 42, "second direction");
    let foreign = lanes.direct(FIRST, 44, "not this lane's direction");
    let first = lanes.address(FIRST);
    let text = format!(
        "Ordinary prose — Καλημέρα 🦀.\r\n\nACK {ack}: earlier answer\n\n\
        NAK {ack}: decisive later answer\n\nACK {nak}: settled independently\n\n\
        ACK {foreign}: cannot retire another lane\n\nACK ZZZZZZZZZ: unknown token\n\n\
        SYN operator: malformed neighbor\n\n\
        ```text\nSYN {first}: example, not a second delivery\nACK {foreign}: example\n```\n\n\
        SYN {first}: Exact Unicode — 日本語.\n"
    );
    let request = captured(&lanes, SENDER, &text);
    let result = receipt(submit(&lanes, SENDER, &request));
    assert!(result.unresolved.is_empty());
    assert_eq!(result.decided.len(), 6);
    assert!(matches!(&result.decided[0].outcome, PacketOutcome::Superseded { key } if key == &ack));
    assert!(matches!(&result.decided[1].outcome,
        PacketOutcome::Acknowledged { key, disposition: wire::Disposition::Declined, body: None, .. }
        if key == &ack));
    assert!(matches!(&result.decided[2].outcome,
        PacketOutcome::Acknowledged { key, disposition: wire::Disposition::Acked, body: None, .. }
        if key == &nak));
    assert_eq!(
        result.note, None,
        "operator direction has no peer its answer's text could miss"
    );
    assert!(
        matches!(&result.decided[3].outcome, PacketOutcome::Refused { reason, .. }
        if reason == "steer::ack-missed")
    );
    assert!(matches!(
        &result.decided[4].outcome,
        PacketOutcome::Refused { .. }
    ));
    assert!(
        matches!(&result.decided[5].outcome, PacketOutcome::Messaged { lanes: delivered, .. }
        if delivered == &[test_uuid(FIRST)])
    );
    assert!(
        lanes
            .pending(FIRST)
            .iter()
            .any(|head| head.text == "Exact Unicode — 日本語.")
    );
    assert_eq!(
        lanes
            .pending(FIRST)
            .iter()
            .filter(|head| head.key == foreign)
            .count(),
        1
    );
    let before = lanes.cursor();
    let inspection = receipt(inspect(&lanes, SENDER, result.submission));
    assert_eq!(inspection.text.as_deref(), Some(text.as_str()));
    assert_eq!(inspection.decided, result.decided);
    assert_eq!(lanes.cursor(), before, "inspection has no packet effects");
    let reader = lanes.bus.reader();
    let resolver = spice::quote::Resolver::from_tape(&lanes.paths.tape, &reader);
    let quote = resolver.resolve(&inspection.quote, 0);
    assert!(
        matches!(&quote, wire::QuoteResolution::Resolved(resolved) if resolved.text == text),
        "{quote:?}"
    );
    for replay in [false, true] {
        let card = presented(&lanes, replay);
        assert_eq!(
            card.acknowledgement,
            Some(wire::MessageAcknowledgement::Declined),
            "native mixed ACK/NAK disposition survives live and history presentation"
        );
        let keys = card
            .directive_contexts
            .iter()
            .filter_map(|context| match &context.quote_target {
                wire::QuoteTarget::Steering { key, .. } => Some(key.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert!(
            keys.contains(ack.as_str()) && keys.contains(nak.as_str()),
            "real acknowledgments carry quoted direction context: {card:?}"
        );
        assert!(
            !keys.contains("ZZZZZZZZZ"),
            "unknown headers acquire no source context"
        );
        assert!(wire::valid_message_directives(
            &card.text,
            &card.directive_contexts
        ));
        assert_eq!(
            card.quote_target.as_ref().map(ToString::to_string),
            Some(inspection.quote.clone()),
            "presentation keeps the exact canonical tape quote"
        );
    }
    lanes.finish();
}

#[test]
fn outbox_deduplicates_complete_bytes_but_not_distinct_bodies_or_successors() {
    let lanes = Lanes::new("outbox-dedup");
    lanes.declare(SENDER);
    let text = format!("SYN {}: one exact body", lanes.address(FIRST));
    let original = captured(&lanes, SENDER, &text);
    let first = receipt(submit(&lanes, SENDER, &original));
    let first_json = serde_json::to_value(&first).expect("write receipt serializes");
    assert_eq!(first_json["reused"], false, "fresh admission is disclosed");
    let retry = captured(&lanes, SENDER, &text);
    assert_ne!(retry.carrier.locus, original.carrier.locus);
    let duplicate = receipt(submit(&lanes, SENDER, &retry));
    assert_eq!(
        serde_json::to_value(&duplicate).expect("reused receipt serializes")["reused"],
        true,
        "identical bytes disclose reuse"
    );
    assert_eq!(duplicate.submission, first.submission);
    assert_eq!(duplicate.accepted, first.accepted);
    assert_eq!(duplicate.decided, first.decided);
    let inspection = receipt(inspect(&lanes, SENDER, first.submission));
    assert!(
        serde_json::to_value(&inspection)
            .expect("inspection serializes")
            .get("reused")
            .is_none(),
        "inspection makes no write-reuse claim"
    );
    assert_eq!(
        lanes.messages().len(),
        1,
        "a different captured invocation does not repeat the effect"
    );
    let changed = captured(&lanes, SENDER, &(text.clone() + "!"));
    let changed = receipt(submit(&lanes, SENDER, &changed));
    assert_ne!(changed.submission, first.submission);
    assert!(
        lanes
            .pending(FIRST)
            .iter()
            .any(|head| head.text == "one exact body!")
    );
    lanes.vacate(SENDER);
    lanes.renew(SENDER, test_uuid(90));
    lanes.declare(SENDER);
    assert!(
        matches!(&submit(&lanes, SENDER, &original), Outcome::Refusal(refusal)
        if refusal.reason() == "outbox::occupant-moved")
    );
    assert!(matches!(
        inspect(&lanes, SENDER, first.submission),
        Outcome::Refusal(_)
    ));
    let next = captured(&lanes, SENDER, &text);
    let next = receipt(submit(&lanes, SENDER, &next));
    assert_ne!(next.submission, first.submission);
    assert_eq!(next.occupant, test_uuid(90));
    assert_eq!(lanes.messages().len(), 3);
    let retired_reader = wire::outbox::Read {
        submission: next.submission,
        occupant: original.carrier.occupant,
    };
    let outcome = steering::outbox::inspect(
        &lanes.bus,
        &lanes.paths,
        lanes.epoch,
        &command(&lanes, SENDER, CommandKind::OutboxRead, &retired_reader),
        &retired_reader,
    );
    assert!(
        matches!(&outcome, Outcome::Refusal(refusal)
        if refusal.reason() == "outbox::occupant-moved"),
        "a predecessor helper cannot read a successor submission: {outcome:?}"
    );
    lanes.finish();
}

#[test]
fn outbox_refuses_unproved_text_and_cross_lane_carriers_before_any_effect() {
    let lanes = Lanes::new("outbox-authority");
    lanes.declare(SENDER);
    let mut request = captured(
        &lanes,
        SENDER,
        &format!("SYN {}: original", lanes.address(FIRST)),
    );
    request.text.push_str(" forged suffix");
    assert!(
        matches!(&submit(&lanes, SENDER, &request), Outcome::Refusal(refusal)
        if refusal.reason() == "outbox::source-unproved")
    );
    assert!(lanes.messages().is_empty());
    let admitted = lanes
        .bus
        .reader()
        .outbox(bus::OutboxQuery::Pending { limit: 1 })
        .expect("outbox reads");
    assert!(admitted.is_empty());
    let request = captured(&lanes, SENDER, "ordinary prose only");
    assert!(matches!(
        submit(&lanes, FIRST, &request),
        Outcome::Refusal(_)
    ));
    lanes.finish();
}

#[test]
fn outbox_recovers_only_unjudged_packets_after_transaction_failure_and_restart() {
    let mut lanes = Lanes::new("outbox-recovery");
    lanes.declare(SENDER);
    let key = lanes.direct(SENDER, 80, "settle only after recovery");
    let body = format!(
        "ACK ZZZZZZZZZ: unknown key\n\nSYN {}: first committed effect\n\n\
         SYN {}: interrupted effect\n\nACK {key}: recovered answer",
        lanes.address(FIRST),
        lanes.address(SECOND),
    );
    let request = captured(&lanes, SENDER, &body);
    let connection = rusqlite::Connection::open(&lanes.paths.journal).expect("isolated authority");
    connection
        .execute_batch(
            "CREATE TRIGGER interrupt_outbox BEFORE INSERT ON fact_envelopes
         WHEN NEW.kind = 'outbox::judged'
           AND (SELECT count(*) FROM fact_envelopes WHERE kind = 'outbox::judged') = 2
         BEGIN SELECT RAISE(ABORT, 'interrupted packet transaction'); END;",
        )
        .expect("deterministic interruption is armed");
    let interrupted = receipt(submit(&lanes, SENDER, &request));
    assert!(matches!(
        interrupted.interruption,
        Some(wire::outbox::Interruption::Fault(_))
    ));
    assert_eq!(interrupted.unresolved, [2, 3]);
    assert!(matches!(
        interrupted.decided[0].outcome,
        PacketOutcome::Refused { .. }
    ));
    assert!(matches!(
        interrupted.decided[1].outcome,
        PacketOutcome::Messaged { .. }
    ));
    assert_eq!(delivered(&lanes, FIRST).text, "first committed effect");
    assert!(
        lanes.pending(SECOND).is_empty(),
        "effect and judgment roll back together"
    );
    assert!(lanes.pending(SENDER).iter().any(|head| head.key == key));
    let inspection = receipt(inspect(&lanes, SENDER, interrupted.submission));
    assert_eq!(inspection.unresolved, interrupted.unresolved);
    assert_eq!(inspection.text.as_deref(), Some(body.as_str()));
    connection
        .execute_batch("DROP TRIGGER interrupt_outbox;")
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
    steering::outbox::recover(
        &lanes.bus,
        &lanes.paths,
        (lanes.epoch, lanes.joining.joining()),
        &steer::Config::default(),
    )
    .expect("only unjudged work resumes");
    let recovered = receipt(inspect(&lanes, SENDER, interrupted.submission));
    assert_eq!(&recovered.decided[..2], interrupted.decided.as_slice());
    assert!(recovered.unresolved.is_empty());
    assert_eq!(delivered(&lanes, FIRST).text, "first committed effect");
    assert_eq!(delivered(&lanes, SECOND).text, "interrupted effect");
    assert!(lanes.pending(SENDER).is_empty());
    let duplicate = receipt(submit(&lanes, SENDER, &request));
    assert_eq!(duplicate.submission, recovered.submission);
    assert_eq!(duplicate.decided, recovered.decided);
    assert_eq!(
        lanes.messages().len(),
        2,
        "restart and resubmission repeat neither native effect"
    );
    lanes.finish();
}

#[test]
fn outbox_status_requires_exact_native_body_and_author_not_tool_metadata() {
    let lanes = Lanes::new("outbox-status-authority");
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let text = "Outbox status — Καλημέρα 🦀.";
    let append = |lane, message: serde_json::Value| {
        let record = serde_json::json!({"type":"message_end", "message":message});
        lanes
            .tape(lane)
            .tee(format!("{record}\n").as_bytes())
            .expect("tool record reaches tape")
    };
    let call = |lane, id, name, path, body| {
        append(
            lane,
            serde_json::json!({
                "role":"assistant", "content":[{"type":"toolCall", "id":id, "name":name,
                "arguments":{"path":path, "content":body}}], "stopReason":"toolUse"
            }),
        )
    };
    let result = |lane, id, output: &str| {
        append(
            lane,
            serde_json::json!({
                "role":"toolResult", "toolCallId":id, "toolName":"write",
                "content":[{"type":"text", "text":output}], "isError":false
            }),
        )
    };
    call(SENDER, "accepted", "write", "spice://outbox", text);
    let request = captured(&lanes, SENDER, text);
    let accepted = receipt(submit(&lanes, SENDER, &request));
    let output = serde_json::to_string(&accepted).expect("receipt text");
    let admitted = result(SENDER, "accepted", &output);
    let at = |lane, locus: listen::Locus| {
        presented_messages(&lanes, true)
            .into_iter()
            .find(|message| {
                message.lane == test_uuid(lane)
                    && message.locus.member == locus.member
                    && message.locus.offset == locus.offset
            })
            .expect("latest transport activity retains its exact source")
    };
    let activity = at(SENDER, admitted);
    assert_eq!(activity.activity, wire::AgentActivityKind::OutboxTransport);
    assert_eq!(activity.author, request.carrier.occupant);
    assert_eq!(activity.text, text);
    assert_eq!(
        activity.quote_target.as_ref().map(ToString::to_string),
        Some(accepted.quote.clone())
    );
    assert!(!activity.activity.is_visible());
    assert!(activity.automatic_speech.is_none());
    let repeated = call(SENDER, "repeated", "write", "spice://outbox", text);
    assert_eq!(
        at(SENDER, repeated).activity,
        wire::AgentActivityKind::OutboxTransport
    );
    result(SENDER, "repeated", &output);
    for (id, name, path, body) in [
        (
            "different-body",
            "write",
            "spice://outbox",
            "Never admitted",
        ),
        ("different-path", "write", "notes.txt", text),
        ("different-tool", "read", "spice://outbox", text),
    ] {
        call(SENDER, id, name, path, body);
        let ordinary = result(SENDER, id, &output);
        assert_eq!(at(SENDER, ordinary).activity, wire::AgentActivityKind::Tool);
    }
    call(FIRST, "other-author", "write", "spice://outbox", text);
    let other_author = result(FIRST, "other-author", &output);
    assert_eq!(
        at(FIRST, other_author).activity,
        wire::AgentActivityKind::Tool
    );
    let messages = presented_messages(&lanes, true);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.activity.is_prose() && message.text == text)
            .count(),
        1
    );
    lanes.finish();
}

#[test]
fn a_messaged_receipt_counts_every_body_line_across_an_interior_blank_line() {
    let lanes = Lanes::new("outbox-lines");
    lanes.declare(SENDER);
    let first = lanes.address(FIRST);
    let text = format!("SYN {first}: one\ntwo\nthree\n\nSYN {first}: one\ntwo\n\nthree\n");
    let result = receipt(submit(&lanes, SENDER, &captured(&lanes, SENDER, &text)));
    let lines: Vec<_> = result
        .decided
        .iter()
        .map(|decided| match &decided.outcome {
            PacketOutcome::Messaged { lines, .. } => *lines,
            other => panic!("each packet messages its recipient: {other:?}"),
        })
        .collect();
    assert_eq!(
        lines,
        [Some(3), Some(4)],
        "a body ends at the next placed header, and an interior blank line stays inside it"
    );
    lanes.finish();
}

/// The captured host record carries no vendor time, so the card's display
/// time is its admission. Live publication and replay must name the same
/// instant: the store refuses one identity with two contents, and a
/// surrogate time would sort the card behind the lane's whole history.
#[test]
fn an_outbox_card_is_observed_at_admission_identically_live_and_replayed() {
    let lanes = Lanes::new("outbox-observed");
    lanes.declare(SENDER);
    let result = receipt(submit(
        &lanes,
        SENDER,
        &captured(&lanes, SENDER, "plain outbox prose\n"),
    ));
    let admitted = lanes
        .bus
        .reader()
        .outbox(bus::OutboxQuery::Submission(result.submission))
        .expect("outbox index")
        .pop()
        .expect("admitted submission")
        .at;
    let live = presented(&lanes, false);
    assert_eq!(live.observed, Some(admitted));
    assert_eq!(live, presented(&lanes, true));
    lanes.finish();
}

fn steering_head(lanes: &Lanes, key: &str) -> wire::SteeringHead {
    let lookup = key.to_owned();
    lanes
        .bus
        .read(move |read| read.steering_key(&lookup))
        .expect("steering index reads")
        .expect("directive is indexed")
        .body()
        .clone()
}

/// Stage `FIRST`'s typed task invitation to `SENDER` under `claim`.
fn invitation(lanes: &Lanes, claim: u64) -> String {
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("invitation instant");
    let origin: wire::Origin = "task:1kKRRsDPx".parse().expect("invitation origin");
    let mut write = JournalWrite::new(
        Actor::Lane(test_uuid(FIRST)),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let residency = lanes
        .bus
        .task_snapshot()
        .expect("snapshot reads")
        .residency()
        .expect("residency is seeded")
        .position();
    let (key, _, _) = crate::resident::append_task_invitation(
        &lanes.bus,
        origin,
        &mut write,
        crate::resident::TaskInvitation {
            task: &"1kKRRsDPx".parse().expect("task stamp"),
            phase: &"todo".parse().expect("phase"),
            generation: 0,
            inviter: test_uuid(FIRST),
            inviter_address: lanes.address(FIRST),
            inviter_claim: wire::Seq::new(claim).expect("claim position"),
            recipient: test_uuid(SENDER),
            recipient_occupant: test_uuid(SENDER + 1),
            residency,
            epoch: lanes.epoch,
            text: "Join this task".to_owned(),
        },
        at,
    )
    .expect("invitation stages");
    assert!(matches!(
        lanes.bus.commit(write).expect("invitation reaches writer"),
        bus::CommitOutcome::Committed(_)
    ));
    key
}

fn cancel(lanes: &Lanes, key: &str) {
    lanes.withdraw(steering_head(lanes, key).directive);
}

#[test]
fn a_late_ack_of_a_retired_invitation_learns_it_expired() {
    let lanes = Lanes::new("outbox-late-invitation");
    lanes.declare(SENDER);
    let lapsed = invitation(&lanes, wire::Seq::FIRST.get());
    let head = steering_head(&lanes, &lapsed);
    assert!(
        steering::apply_deadline(
            &lanes.bus,
            &steering::SteeringDeadline {
                directive: head.directive,
                key: lapsed.parse().expect("invitation key"),
                recipient: head.recipient,
                expires: head.expires,
                transition: steering::DeadlineTransition::Lapse,
            },
            head.expires,
        )
        .expect("the deadline applies")
    );
    let canceled = invitation(&lanes, wire::Seq::FIRST.get() + 1);
    cancel(&lanes, &canceled);
    let withdrawn = lanes.direct(SENDER, 40, "withdrawn direction");
    cancel(&lanes, &withdrawn);

    let text = format!(
        "ACK {lapsed}: too late\n\nACK {canceled}: too late\n\nACK {withdrawn}: too late\n"
    );
    let result = receipt(submit(&lanes, SENDER, &captured(&lanes, SENDER, &text)));
    let reasons = result
        .decided
        .iter()
        .map(|decided| match &decided.outcome {
            PacketOutcome::Refused { reason, .. } => reason.as_str(),
            other => panic!("a late ACK settles nothing: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        [
            "task::invite-expired",
            "task::invite-expired",
            "steer::canceled"
        ]
    );
    assert_eq!(
        [&lapsed, &canceled, &withdrawn].map(|key| steering_head(&lanes, key).disposition),
        [
            wire::Disposition::Lapsed,
            wire::Disposition::Canceled,
            wire::Disposition::Canceled
        ]
    );
    lanes.finish();
}

/// RST cancels each live send its own lane made and keeps its reason for the
/// recipient's one notice. Every other key it names is refused alone, and the
/// recipient's late answer learns the send was canceled.
#[test]
fn rst_cancels_only_its_own_live_sends_and_tells_the_recipient_why() {
    let lanes = Lanes::new("outbox-rst");
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let first = lanes.address(FIRST);
    let said = |lane, text: String| receipt(submit(&lanes, lane, &captured(&lanes, lane, &text)));
    said(SENDER, format!("SYN {first}: wrong question"));
    let wrong = pending_key(&lanes, FIRST);
    said(SENDER, format!("SYN {first}: settled question"));
    let settled = lanes
        .pending(FIRST)
        .into_iter()
        .map(|input| input.key)
        .find(|key| *key != wrong)
        .expect("the second input is pending");
    said(FIRST, format!("ACK {settled}"));
    let foreign = lanes.direct(FIRST, 40, "operator direction");
    let before = lanes.cursor();

    let result = said(
        SENDER,
        format!("RST {wrong} {settled} {foreign}: I asked the wrong lane."),
    );
    let [canceled, retired, unsent] = <[_; 3]>::try_from(result.decided)
        .unwrap_or_else(|decided| panic!("each key is judged alone: {decided:?}"))
        .map(|decided| decided.outcome);
    assert_eq!(
        canceled,
        PacketOutcome::Acknowledged {
            key: wrong.clone(),
            disposition: wire::Disposition::Canceled,
            body: None,
            line: None,
        }
    );
    for refused in [retired, unsent] {
        assert!(
            matches!(&refused, PacketOutcome::Refused { reason, .. }
                if reason == "steer::rst-unavailable"),
            "{refused:?}"
        );
    }
    assert_eq!(
        [&wrong, &settled, &foreign].map(|key| steering_head(&lanes, key).disposition),
        [
            wire::Disposition::Canceled,
            wire::Disposition::Acked,
            wire::Disposition::Live
        ]
    );

    let told = steering::line_outcomes(
        &lanes.bus,
        wire::Seq::new(before + 1).expect("a later position is positive"),
        wire::Seq::new(lanes.cursor()).expect("the journal has facts"),
        |lane| (lane == test_uuid(FIRST)).then_some(test_uuid(FIRST + 1)),
    )
    .expect("outcomes read");
    let [(lane, notice)] = <[_; 1]>::try_from(told)
        .unwrap_or_else(|told| panic!("only the recipient is told: {told:?}"));
    assert_eq!(lane, test_uuid(FIRST));
    assert_eq!(notice.peer, Some(lanes.address(SENDER).to_string()));
    assert_eq!(
        notice.outcome,
        steering::LineOutcome::Canceled {
            reason: Some("I asked the wrong lane.".to_owned())
        }
    );

    let late = said(FIRST, format!("ACK {wrong}: here is my answer"));
    assert!(
        matches!(&late.decided[0].outcome, PacketOutcome::Refused { reason, .. }
            if reason == "steer::canceled"),
        "{late:?}"
    );
    lanes.finish();
}

/// Each keyed line input one header answers is judged alone. A shared
/// substantive ACK body recursively continues every selected peer line, and
/// an exact retry of the settled body returns those judgments without writing.
#[test]
fn an_acknowledgment_receipt_names_each_keys_retirement_and_its_texts_fate() {
    let lanes = Lanes::new("outbox-body-fate");
    lanes.declare(SENDER);
    lanes.author(
        SENDER,
        &format!("SYN {}: is main green?", lanes.address(SECOND)),
    );
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    let asked = delivered(&lanes, SECOND);
    lanes.declare(SECOND);
    lanes.answer(SECOND, &[&asked.key], &format!("ACK {}: Green.", asked.key));
    lanes.author(
        FIRST,
        &format!("SYN {}: hold the merge", lanes.address(SENDER)),
    );
    lanes.hear(FIRST, test_uuid(FIRST + 1));
    let held = lanes.pending(SENDER);
    let from = |lane: u8| {
        held.iter()
            .find(|head| head.from == Some(test_uuid(lane)))
            .unwrap_or_else(|| panic!("lane {lane} reached the sender: {held:?}"))
            .key
            .clone()
    };
    let (note, reply) = (from(FIRST), from(SECOND));
    let text = format!("ACK {note} {reply}: Holding until main is green.\n");
    let result = receipt(submit(&lanes, SENDER, &captured(&lanes, SENDER, &text)));
    let judged = result
        .decided
        .iter()
        .map(|decided| match &decided.outcome {
            PacketOutcome::Acknowledged {
                key,
                disposition: wire::Disposition::Acked,
                body,
                ..
            } => (key.as_str(), *body),
            PacketOutcome::Refused { reason, .. } => (reason.as_str(), None),
            other => panic!("each key is acknowledged or refused alone: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        judged,
        [
            (note.as_str(), Some(BodyFate::Sent)),
            (reply.as_str(), Some(BodyFate::Sent)),
        ],
        "each selected peer line receives the substantive continuation"
    );
    assert!(result.note.is_none(), "every body route was admitted");
    assert!(
        lanes.pending(SENDER).is_empty(),
        "both exact inputs retired"
    );
    let [returned_to_first] = lanes
        .pending(FIRST)
        .try_into()
        .unwrap_or_else(|pending| panic!("the first line continues once: {pending:?}"));
    let [returned_to_second] = lanes
        .pending(SECOND)
        .try_into()
        .unwrap_or_else(|pending| panic!("the second line continues once: {pending:?}"));
    assert_eq!(returned_to_first.text, "Holding until main is green.");
    assert_eq!(returned_to_second.text, "Holding until main is green.");
    let inspection = receipt(inspect(&lanes, SENDER, result.submission));
    assert_eq!(
        inspection.text.as_deref(),
        Some(text.as_str()),
        "the authored text remains inspectable"
    );

    let retry = captured(&lanes, SENDER, &text);
    let before = lanes.cursor();
    let repeated = receipt(submit(&lanes, SENDER, &retry));
    assert_eq!(
        (&repeated.decided, &repeated.note),
        (&result.decided, &result.note),
        "a settled retry returns its original judgment"
    );
    assert_eq!(lanes.cursor(), before, "a settled retry writes no fact");
    assert_eq!(lanes.pending(FIRST), [returned_to_first]);
    assert_eq!(lanes.pending(SECOND), [returned_to_second]);
    lanes.finish();
}

/// The key of the one input pending for `lane`.
fn pending_key(lanes: &Lanes, lane: u8) -> String {
    let [input] = lanes
        .pending(lane)
        .try_into()
        .unwrap_or_else(|pending| panic!("one input is pending: {pending:?}"));
    input.key
}

/// A receipt states each key's effect on its line apart from the fate of the
/// text it carried: a nod is seen and sends nothing, an admitted body names
/// the input it became and why it was admitted, and a refused body leaves its
/// control recorded without ever reading as delivered.
#[test]
fn a_line_receipt_separates_each_keys_effect_from_its_bodys_fate() {
    let lanes = Lanes::new("outbox-line-effects");
    let (sender, first) = (lanes.address(SENDER), lanes.address(FIRST));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let said = |lane: u8, text: String| {
        let result = receipt(submit(&lanes, lane, &captured(&lanes, lane, &text)));
        assert!(result.unresolved.is_empty(), "{result:?}");
        result
    };
    let only = |lane: u8| pending_key(&lanes, lane);

    let opened = said(SENDER, format!("SYN {first}: seen this?"));
    let nodded = only(FIRST);
    assert_eq!(
        opened.effects,
        [format!("{nodded} delivered to active recipient {first}")]
    );
    let nod = said(FIRST, format!("ACK {nodded}: Thanks"));
    assert_eq!(nod.effects, [format!("{nodded} on line {nodded}: seen")]);
    assert_eq!(nod.note, None, "a nod carries no unsent body");
    assert!(lanes.pending(SENDER).is_empty(), "a nod opens no input");

    said(SENDER, format!("SYN {first}: is the parser settled?"));
    let root = only(FIRST);
    let continued = said(FIRST, format!("ACK {root}: Nearly."));
    let returned = only(SENDER);
    assert_eq!(
        continued.effects,
        [format!(
            "{root} on line {root}: acknowledged; continuation {returned} delivered to active \
             recipient {sender}"
        )]
    );
    let closed = said(SENDER, format!("FIN {returned}: Ship it."));
    let last = only(FIRST);
    assert_eq!(
        closed.effects,
        [format!(
            "{returned} on line {root}: closed; final body {last} delivered to active recipient \
             {first}, still owed a seen after the close"
        )]
    );
    let late = said(FIRST, format!("ACK {last}: One more question."));
    assert_eq!(
        late.effects,
        [format!(
            "{last} on line {root}: acknowledged; line already closed; no body delivered \
             (refused before peer input: steer::line-closed). A reply on this closed line reaches \
             nobody. Build on this message in a new linked line with `SYN #{last}: BODY`"
        )]
    );
    assert!(late.note.is_some(), "the refused body is reported unsent");
    assert!(
        lanes.pending(SENDER).is_empty(),
        "a closed line gains no input"
    );
    lanes.finish();
}

/// A valid FIN stays recorded when its final body is refused: the receipt
/// says the line closed and that nothing was delivered.
#[test]
fn a_refused_final_body_leaves_its_close_recorded_on_the_receipt() {
    let lanes = Lanes::new("outbox-line-refused-final");
    let first = lanes.address(FIRST);
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let said =
        |lane: u8, text: String| receipt(submit(&lanes, lane, &captured(&lanes, lane, &text)));
    let only = |lane: u8| pending_key(&lanes, lane);
    said(SENDER, format!("SYN {first}: is the parser settled?"));
    let root = only(FIRST);
    said(FIRST, format!("ACK {root}: Nearly."));
    said(SENDER, format!("ACK {}: What is left?", only(SENDER)));
    let question = only(FIRST);
    lanes.vacate(SENDER);
    lanes.spend_starts(FIRST, SENDER);

    let closed = said(FIRST, format!("FIN {question}: Only the lexer."));
    assert_eq!(
        closed.effects,
        [format!(
            "{question} on line {root}: closed; no final body delivered (refused before peer \
             input: steer::pair-starts-spent)"
        )]
    );
    assert!(
        lanes.pending(SENDER).is_empty(),
        "the refused body is no peer input"
    );
    lanes.finish();
}

/// An exact prior receipt of a peer input remains authority for a later
/// control, not another retirement. Other lanes and unknown keys gain no
/// authority.
#[test]
fn an_acknowledged_peer_input_can_continue_and_finish_its_open_line() {
    for (marker, terminal) in [
        ("FIN", wire::body::PeerLineAction::Finish),
        ("NAK", wire::body::PeerLineAction::Decline),
    ] {
        let lanes = Lanes::new("outbox-prior-peer-receipt");
        lanes.declare(SENDER);
        lanes.declare(FIRST);
        let first = lanes.address(FIRST);
        let said =
            |lane, text: String| receipt(submit(&lanes, lane, &captured(&lanes, lane, &text)));
        said(SENDER, format!("SYN {first}: initial question"));
        let root = pending_key(&lanes, FIRST);
        said(FIRST, format!("ACK {root}: first answer"));
        let head = || {
            let key = root.clone();
            lanes
                .bus
                .read(move |read| read.steering_key(&key))
                .expect("the exact input reads")
                .expect("the receipt remains")
        };
        let acknowledged = head();
        assert_eq!(acknowledged.body().disposition, wire::Disposition::Acked);

        let continued = said(FIRST, format!("ACK {root}: additional detail"));
        assert!(matches!(&continued.decided[0].outcome,
            PacketOutcome::Acknowledged { line: Some(effect), .. }
            if effect.action == Some(wire::body::PeerLineAction::Continue)
                && matches!(effect.body, Some(wire::outbox::LineBody::Admitted(_)))));
        assert!(
            lanes
                .pending(SENDER)
                .iter()
                .any(|input| input.text == "additional detail")
        );
        assert_eq!(
            head().position(),
            acknowledged.position(),
            "a new control never retires the same input again"
        );

        for (lane, key) in [(SENDER, root.clone()), (FIRST, "ZZZZZZZZZ".to_owned())] {
            let refused = said(lane, format!("ACK {key}: no authority"));
            assert!(matches!(&refused.decided[0].outcome,
                PacketOutcome::Refused { reason, .. } if reason == "steer::ack-missed"));
        }

        let request = captured(&lanes, FIRST, &format!("{marker} {root}: final answer"));
        let closed = receipt(submit(&lanes, FIRST, &request));
        assert!(matches!(&closed.decided[0].outcome,
            PacketOutcome::Acknowledged { disposition: wire::Disposition::Acked,
                line: Some(effect), .. }
            if effect.action == Some(terminal)
                && matches!(effect.body, Some(wire::outbox::LineBody::Admitted(_)))));
        assert!(
            lanes
                .pending(SENDER)
                .iter()
                .any(|input| input.text == "final answer")
        );
        assert_eq!(head().position(), acknowledged.position());
        let before = lanes.cursor();
        let replayed = receipt(submit(&lanes, FIRST, &request));
        assert_eq!(replayed.decided, closed.decided);
        assert_eq!(lanes.cursor(), before, "settled replay writes nothing");

        let late = said(FIRST, format!("ACK {root}: too late"));
        assert!(matches!(&late.decided[0].outcome,
            PacketOutcome::Acknowledged { line: Some(effect), .. }
            if effect.action.is_none()
                && matches!(&effect.body, Some(wire::outbox::LineBody::Refused { reason, .. })
                    if reason == "steer::line-closed")));
        assert!(
            !lanes
                .pending(SENDER)
                .iter()
                .any(|input| input.text == "too late")
        );
        assert!(
            lanes.pending(FIRST).is_empty(),
            "a post-close body opens no source-bound correction"
        );
        assert_eq!(head().position(), acknowledged.position());
        lanes.finish();
    }
}

/// An acknowledged operator direction grants its recipient exactly one later
/// FIN answer, recorded without retiring the direction again.
#[test]
fn an_acknowledged_operator_direction_admits_one_later_answer() {
    let lanes = Lanes::new("outbox-direction-answer");
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let said = |lane, text: String| receipt(submit(&lanes, lane, &captured(&lanes, lane, &text)));
    let refused = |outcome: &PacketOutcome| matches!(outcome, PacketOutcome::Refused { reason, .. } if reason == "steer::ack-missed");
    let operator = lanes.direct(FIRST, 40, "operator direction");
    said(FIRST, format!("ACK {operator}"));
    let direction = || {
        let key = operator.clone();
        lanes
            .bus
            .read(move |read| read.steering_key(&key))
            .expect("the direction reads")
            .expect("the direction remains")
            .position()
    };
    let nodded = direction();
    let answer_used = || {
        lanes
            .bus
            .read(move |read| read.steering_answered(nodded))
            .expect("answer evidence reads")
    };
    for wrong in ["ACK", "NAK"] {
        let wrong = said(FIRST, format!("{wrong} {operator}: not a final answer"));
        assert!(refused(&wrong.decided[0].outcome));
        assert!(
            !answer_used(),
            "a refused control spends no answer authority"
        );
    }
    let foreign = said(SENDER, format!("FIN {operator}: not mine"));
    assert!(refused(&foreign.decided[0].outcome));
    assert!(
        !answer_used(),
        "another lane's FIN spends no answer authority"
    );

    let answer = captured(&lanes, FIRST, &format!("FIN {operator}: the answer"));
    let answered = receipt(submit(&lanes, FIRST, &answer));
    assert!(matches!(
        &answered.decided[0].outcome,
        PacketOutcome::Acknowledged {
            disposition: wire::Disposition::Acked,
            line: None,
            ..
        }
    ));
    assert!(answer_used(), "the FIN records durable answer evidence");
    assert_eq!(
        direction(),
        nodded,
        "an answer after the nod never retires the direction again"
    );
    let before = lanes.cursor();
    let replayed = receipt(submit(&lanes, FIRST, &answer));
    assert_eq!(replayed.decided, answered.decided);
    assert_eq!(lanes.cursor(), before, "a replayed answer writes nothing");
    let repeated = said(FIRST, format!("FIN {operator}: a second answer"));
    assert!(refused(&repeated.decided[0].outcome));
    assert!(answer_used(), "a refused repeat preserves the first answer");
    lanes.finish();
}

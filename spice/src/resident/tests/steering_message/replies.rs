//! Each peer line is a duty of its recipient's logical lane: an answer retires
//! its own input once and carries the line through vacancy, renewal, restart,
//! unreadable transcripts, and lane removal.

use super::*;

/// Every fanout recipient owns its key: each answer retires only its own
/// directive and relays its clause once to the sending lane, whose answer to
/// that reply sends nothing back.
#[test]
fn fanout_inputs_continue_or_decline_their_independent_lines() {
    let lanes = Lanes::new("syn-answer");
    let (first, second) = (lanes.address(FIRST), lanes.address(SECOND));
    lanes.author(SENDER, &format!("SYN {first} {second}: rebase onto main"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    let (to_first, to_second) = (delivered(&lanes, FIRST), delivered(&lanes, SECOND));
    assert_eq!(to_first.text, "rebase onto main");
    assert_ne!(to_first.key, to_second.key);
    lanes.declare(FIRST);
    lanes.declare(SECOND);

    lanes.answer(
        FIRST,
        &[&to_first.key],
        &format!("ACK {}: Rebased.", to_first.key),
    );
    assert!(lanes.pending(FIRST).is_empty());
    assert_eq!(
        lanes.pending(SECOND),
        std::slice::from_ref(&to_second),
        "a sibling recipient's key stays live"
    );
    let [ack] = lanes
        .pending(SENDER)
        .try_into()
        .unwrap_or_else(|pending| panic!("one reply reaches the sender: {pending:?}"));
    assert_eq!(ack.from, Some(test_uuid(FIRST)));
    assert_eq!(ack.text, "Rebased.");
    assert!(matches!(
        ack.control.as_deref().and_then(wire::parse_peer_line_control),
        Some((_, wire::body::PeerBodyKind::Continue, covered))
            if covered == [to_first.directive]
    ));
    assert!(
        ack.wakes,
        "an active reply keeps the wake its admission committed"
    );
    assert_eq!(
        ack.sender,
        Some(wire::intent::PeerSender {
            occupant: test_uuid(FIRST + 1),
            address: Some(first),
        }),
        "a reply names the exact occupant that answered"
    );

    lanes.answer(
        SECOND,
        &[&to_second.key],
        &format!("NAK {}: Main is red.", to_second.key),
    );
    assert!(lanes.pending(SECOND).is_empty());
    let replies = lanes.pending(SENDER);
    let nak = replies
        .iter()
        .find(|reply| reply.from == Some(test_uuid(SECOND)))
        .unwrap_or_else(|| panic!("the refusal replies too: {replies:?}"));
    assert_eq!(nak.text, "Main is red.");
    assert_eq!(
        nak.sender.map(|sender| sender.address),
        Some(Some(second)),
        "each reply names its own answerer"
    );
    assert!(matches!(
        nak.control.as_deref().and_then(wire::parse_peer_line_control),
        Some((_, wire::body::PeerBodyKind::Decline, covered))
            if covered == [to_second.directive]
    ));

    let transitions = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("authority reads")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
        .collect::<Vec<_>>();
    assert!(transitions.iter().any(|transition| {
        transition.input == to_first.directive
            && transition.action == wire::body::PeerLineAction::Continue
    }));
    assert!(transitions.iter().any(|transition| {
        transition.input == to_second.directive
            && transition.action == wire::body::PeerLineAction::Decline
    }));
    lanes.finish();
}

/// A bare ACK is a seen reaction. It retires the exact input without opening
/// another duty, even when the opening sender is vacant.
#[test]
fn a_bare_ack_to_a_departed_sender_creates_no_reply_duty() {
    let lanes = Lanes::new("line-reaction-vacant");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.vacate(SENDER);
    let message = delivered(&lanes, FIRST);

    lanes.answer(FIRST, &[&message.key], &format!("ACK {}", message.key));
    assert!(lanes.pending(FIRST).is_empty(), "the exact input retires");
    assert!(
        lanes.pending(SENDER).is_empty(),
        "a reaction creates no recursive input"
    );
    let reactions = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("authority reads")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
        .filter(|transition| transition.action == wire::body::PeerLineAction::Reaction)
        .count();
    assert_eq!(reactions, 1);
    lanes.finish();
}

/// A blank line after a payload-free control returns the following paragraph
/// to ordinary assistant prose. Open and closed lines share that authored
/// boundary, and a bodyless FIN closes without sending the paragraph.
#[test]
fn payload_free_line_controls_do_not_claim_the_following_paragraph() {
    let lanes = Lanes::new("line-payload-free-boundary");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: acknowledge this"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);

    let opening = delivered(&lanes, FIRST);
    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("ACK {}: noted.!;,\n\nOperator report.", opening.key),
    );
    assert!(
        lanes.pending(SENDER).is_empty(),
        "the paragraph after an ACK reaction is not peer payload"
    );

    lanes
        .say(SENDER, &format!("SYN {first}: close without a body"))
        .expect("a second line opens");
    let bodyless = delivered(&lanes, FIRST);
    lanes.answer(
        FIRST,
        &[&bodyless.key],
        &format!("FIN {}\n\nOperator report.", bodyless.key),
    );
    assert!(
        lanes.pending(SENDER).is_empty(),
        "the paragraph after a bodyless FIN is not a final body"
    );

    lanes
        .say(SENDER, &format!("SYN {first}: close with a body"))
        .expect("a third line opens");
    let closing = delivered(&lanes, FIRST);
    lanes.answer(
        FIRST,
        &[&closing.key],
        &format!("FIN {}: final result", closing.key),
    );
    let [final_body] = lanes
        .pending(SENDER)
        .try_into()
        .unwrap_or_else(|pending| panic!("one final body reaches the sender: {pending:?}"));
    lanes.answer(
        SENDER,
        &[&final_body.key],
        &format!("ACK {}: Thanks.,:;!\n\nOperator report.", final_body.key),
    );
    assert!(
        lanes.pending(SENDER).is_empty() && lanes.pending(FIRST).is_empty(),
        "the closed-line receipt creates neither a peer body nor a correction"
    );

    let snapshot = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("line transitions read");
    let reactions = snapshot
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
        .filter(|transition| transition.action == wire::body::PeerLineAction::Reaction)
        .count();
    assert_eq!(reactions, 2, "both ACK controls are seen reactions");
    assert!(
        snapshot
            .facts()
            .iter()
            .all(|envelope| envelope.fact.read::<wire::body::SteerWithheld>().is_none()),
        "ordinary prose after the blank line is never withheld as a peer body"
    );
    lanes.finish();
}
fn final_body_on_closed_line(lanes: &Lanes) -> wire::intent::PendingDirective {
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: final answer?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    let opening = delivered(lanes, FIRST);
    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("FIN {}: The result is 42.", opening.key),
    );
    let [final_body] = lanes
        .pending(SENDER)
        .try_into()
        .unwrap_or_else(|pending| panic!("one final body is pending: {pending:?}"));
    let final_control = lanes
        .bus
        .read(move |read| read.steering_head(final_body.directive))
        .expect("final body head reads")
        .expect("final body has an authority head")
        .body()
        .control
        .clone();
    assert!(
        final_control
            .as_deref()
            .and_then(wire::parse_peer_line_control)
            .is_some(),
        "final body retains its line control: {final_control:?}"
    );
    final_body
}

/// A terminal line accepts the exact receipt owed by its final body, but a new
/// body on that control is retained as one source-bound refusal, never appears
/// to the closed peer, and opens no author correction.
#[test]
fn a_post_close_body_is_withheld_without_reopening_the_line() {
    let lanes = Lanes::new("line-post-close-body");
    let final_body = final_body_on_closed_line(&lanes);
    let parent = final_body
        .line
        .as_ref()
        .expect("a final body retains its line standing")
        .root
        .clone();
    let door = door::Door::bind(
        &crate::resident::supervisor::command::kick_path(&lanes.paths, test_uuid(SENDER)),
        crate::resident::supervisor::command::KICK_BYTES,
    )
    .expect("the transcript author's kick door binds");
    lanes.answer(
        SENDER,
        &[&final_body.key],
        &format!("ACK {} one more question", final_body.key),
    );
    let notices = std::iter::from_fn(|| door.recv(Some(std::time::Duration::ZERO)).ok())
        .filter_map(|kicked| {
            match serde_json::from_slice(&kicked).expect("the private delivery decodes") {
                crate::resident::supervisor::command::Kick::MessageRefused(notice) => Some(notice),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    let [notice] = notices.as_slice() else {
        panic!("the refused transcript reply earns one private notice: {notices:?}");
    };
    assert_eq!(notice.closed_line.as_deref(), Some(final_body.key.as_str()));
    assert_eq!(
        notice.text(),
        format!(
            "<system-notice>\nA reply on this closed line reaches nobody. Build on this message \
             in a new linked line with `SYN #{}: BODY`.\n</system-notice>",
            final_body.key
        )
    );
    assert!(
        lanes.pending(SENDER).is_empty(),
        "the final-body input retires without waking its author"
    );

    assert!(
        lanes.pending(FIRST).is_empty(),
        "a body after terminal authority never reaches the peer"
    );
    lanes.restart();
    assert!(
        lanes.pending(SENDER).is_empty() && lanes.pending(FIRST).is_empty(),
        "rereading the refused body creates no input or correction"
    );
    let replayed_refusal = std::iter::from_fn(|| {
        door.recv(Some(std::time::Duration::ZERO))
            .ok()
            .map(|kicked| serde_json::from_slice(&kicked).expect("the private delivery decodes"))
    })
    .any(|kick| {
        matches!(
            kick,
            crate::resident::supervisor::command::Kick::MessageRefused(_)
        )
    });
    assert!(!replayed_refusal, "startup sends no second refusal notice");
    lanes
        .say(
            SENDER,
            &format!(
                "SYN #{}: retry the refused question on a linked line",
                final_body.key
            ),
        )
        .expect("the settled input whose body was refused remains linkable");
    let linked = delivered(&lanes, FIRST);
    assert_eq!(
        linked
            .line
            .as_ref()
            .and_then(|standing| standing.parent.as_deref()),
        Some(parent.as_str()),
        "the fresh line links from the closed line whose input it names"
    );
    let snapshot = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("withheld judgment reads");
    let withheld: Vec<_> = snapshot
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerWithheld>())
        .collect();
    let transitions: Vec<_> = snapshot
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
        .collect();
    assert_eq!(
        withheld
            .iter()
            .filter(|judgment| judgment.reason == "steer::line-closed")
            .count(),
        1,
        "withheld={withheld:?}; transitions={transitions:?}"
    );
    assert_eq!(withheld[0].from, test_uuid(SENDER));
    assert_eq!(
        transitions
            .iter()
            .filter(|transition| transition.input == final_body.directive)
            .map(|transition| (transition.action, transition.body))
            .collect::<Vec<_>>(),
        [(wire::body::PeerLineAction::Reaction, None)],
        "the late control settles its input without changing the closed line"
    );
    lanes.finish();
}

/// The one pending input on `lane`.
fn only(lanes: &Lanes, lane: u8) -> wire::intent::PendingDirective {
    let [input] = lanes
        .pending(lane)
        .try_into()
        .unwrap_or_else(|pending| panic!("one line input is pending: {pending:?}"));
    input
}

/// Open a line from SENDER to FIRST and carry it to FIRST's pending
/// question, which FIRST answers with a later body back to SENDER.
fn line_question(lanes: &Lanes) -> wire::intent::PendingDirective {
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let opening = only(lanes, FIRST);
    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("ACK {}: Nearly.", opening.key),
    );
    let returned = only(lanes, SENDER);
    lanes.answer(
        SENDER,
        &[&returned.key],
        &format!("ACK {}: What is left?", returned.key),
    );
    only(lanes, FIRST)
}

/// A later body to a vacant recipient whose residency has no launch capability
/// is refused before any peer input exists. Its header still retires the key
/// and records the line effect, one judgment withholds the body, and its author
/// holds one correction no replay repeats.
#[test]
fn an_unadmitted_line_body_is_withheld_and_corrected_once() {
    let lanes = Lanes::new("line-body-unadmitted");
    let question = line_question(&lanes);
    lanes.vacate_unlaunchable(SENDER);

    lanes.answer(
        FIRST,
        &[&question.key],
        &format!("ACK {}: Only the lexer.", question.key),
    );
    assert!(
        lanes.pending(SENDER).is_empty(),
        "an unlaunchable vacant recipient gains no peer input"
    );
    let correction = only(&lanes, FIRST);
    assert!(
        correction.from.is_none()
            && correction.text.contains("steer::recipient-unlaunchable")
            && correction.text.contains("Only the lexer."),
        "the answered key retires and the correction repeats the refused body: {correction:?}"
    );
    let judged = lanes.withheld();
    let [judgment] = judged.as_slice() else {
        panic!("one judgment withholds the body: {judged:?}");
    };
    assert_eq!(
        (judgment.from, judgment.sender),
        (test_uuid(FIRST), test_uuid(FIRST + 1))
    );
    let opened = correction.directive;
    let created = lanes
        .bus
        .read(move |read| read.steering_head(opened))
        .expect("correction head reads")
        .expect("correction has an authority head")
        .body()
        .created;
    let snapshot = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("authority reads");
    let withheld_at = snapshot
        .facts()
        .iter()
        .find(|envelope| envelope.fact.read::<wire::body::SteerWithheld>().is_some())
        .map(|envelope| (envelope.seq, envelope.at))
        .expect("the judgment is journaled");
    let opened_at = snapshot
        .facts()
        .iter()
        .find(|envelope| envelope.seq == created)
        .map(|envelope| (envelope.cause, envelope.at))
        .expect("the correction is journaled");
    assert_eq!(
        opened_at,
        (Some(withheld_at.0), withheld_at.1),
        "the correction commits in the write that judges its body"
    );
    let effects = || {
        lanes
            .bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("authority reads")
            .facts()
            .iter()
            .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
            .filter(|transition| transition.input == question.directive)
            .map(|transition| (transition.action, transition.body))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        effects(),
        [(wire::body::PeerLineAction::Continue, None)],
        "the header's line effect commits with its body unsent"
    );

    lanes.restart();
    lanes.restart();
    assert_eq!(lanes.pending(FIRST), [correction], "a replay opens nothing");
    assert_eq!(lanes.withheld().len(), 1, "a replay judges nothing");
    assert_eq!(effects().len(), 1, "a replay records no second effect");
    lanes.finish();
}

/// A later body to an idle recipient outside every round, past the first
/// return and with no start left in its pair, is refused before any peer
/// input exists and corrects its author.
#[test]
fn a_line_body_to_an_idle_lane_whose_pair_is_spent_is_withheld() {
    let lanes = Lanes::new("line-body-spent");
    let question = line_question(&lanes);
    lanes.vacate(SENDER);
    lanes.spend_starts(FIRST, SENDER);

    lanes.answer(
        FIRST,
        &[&question.key],
        &format!("ACK {}: Only the lexer.", question.key),
    );
    assert!(
        lanes.pending(SENDER).is_empty(),
        "a spent pair gives its idle recipient no peer input"
    );
    let correction = only(&lanes, FIRST);
    assert!(
        correction.text.contains("steer::pair-starts-spent")
            && correction.text.contains("Only the lexer."),
        "{correction:?}"
    );
    lanes.finish();
}

/// A peer line is bound to its recipient's logical lane: it keeps its one key,
/// body, and deadline through that lane's vacancy and renewal, the successor
/// answers it, and an answered input never returns at a later renewal. Its
/// quote still names the occupant that wrote it after that sender leaves.
#[test]
fn a_syn_follows_its_lane_to_a_renewed_occupant_with_one_key_and_deadline() {
    let lanes = Lanes::new("syn-lane-bound");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: hold the merge"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.vacate(SENDER);
    let message = delivered(&lanes, FIRST);
    let resolver_source = lanes.bus.reader();
    let resolver = spice::quote::Resolver::from_tape(&lanes.paths.tape, &resolver_source);
    let target = wire::QuoteTarget::Steering {
        key: message.key.clone(),
        digest: wire::QuoteTarget::text_digest(&message.text),
    };
    assert!(
        matches!(
            resolver.resolve(&target.to_string(), 0),
            wire::QuoteResolution::Resolved(wire::ResolvedQuote {
                author: wire::QuoteAuthor::Agent { lane, occupant },
                ..
            }) if lane == test_uuid(SENDER) && occupant == test_uuid(SENDER + 1)
        ),
        "a quote names the occupant that wrote the SYN"
    );
    drop(resolver_source);

    lanes.vacate(FIRST);
    assert_eq!(
        lanes.pending(FIRST),
        std::slice::from_ref(&message),
        "a vacant lane keeps its line input"
    );
    let successor = test_uuid(41);
    lanes.renew(FIRST, successor);
    let before = lanes.cursor();
    lanes.dead_letter_removed_lanes();
    assert_eq!(
        lanes.cursor(),
        before,
        "a lane still resident parks nothing"
    );
    lanes.restart();
    lanes.restart();
    assert_eq!(
        lanes.pending(FIRST),
        std::slice::from_ref(&message),
        "the successor inherits the same key, body, and deadline through every replay"
    );
    assert_eq!(lanes.messages().len(), 1, "replay publishes nothing again");

    lanes.declare(FIRST);
    lanes.answer(
        FIRST,
        &[&message.key],
        &format!("ACK {}: Holding.", message.key),
    );
    assert!(
        lanes.pending(FIRST).is_empty(),
        "the successor's answer retires the inherited key"
    );
    let [reply] = lanes
        .pending(SENDER)
        .try_into()
        .unwrap_or_else(|pending| panic!("one reply reaches the sending lane: {pending:?}"));
    let answerer = lanes
        .bus
        .task_snapshot()
        .expect("snapshot reads")
        .facts()
        .occupant_address(test_uuid(FIRST), successor)
        .expect("the successor has one birth");
    assert_ne!(answerer, first);
    assert_eq!(
        reply.sender,
        Some(wire::intent::PeerSender {
            occupant: successor,
            address: Some(answerer),
        }),
        "the reply names the occupant that answered, not the one first addressed"
    );

    lanes.vacate(FIRST);
    lanes.renew(FIRST, test_uuid(43));
    assert!(
        lanes.pending(FIRST).is_empty(),
        "an answered input never returns to the lane"
    );
    lanes.finish();
}

/// A predecessor transcript that cannot be read at exit settles nothing and
/// says so; once readable, the answer it holds retires the key exactly once.
#[test]
fn an_unreadable_predecessor_transcript_settles_nothing_at_exit() {
    let lanes = Lanes::new("syn-unreadable-predecessor");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: hold the merge"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    let message = delivered(&lanes, FIRST);
    lanes.declare(FIRST);
    lanes.answer(FIRST, &[], &format!("ACK {}: Holding.", message.key));
    lanes.vacate(FIRST);
    let tape = lanes.paths.lane_tape(test_uuid(FIRST)).join("omp");
    let hidden = tape.with_extension("hidden");
    fs::rename(&tape, &hidden).expect("the tape moves aside");
    fs::write(&tape, b"not a tape").expect("an unreadable tape stands in");
    let vendor: wire::Name = "omp".parse().expect("fixture vendor");
    let exit = |diagnostics: &mut Vec<u8>| {
        steering::dispose_at_exit(
            &lanes.bus,
            &lanes.paths,
            &steer::Config::default(),
            (lanes.epoch, lanes.joining.joining()),
            test_uuid(FIRST),
            &vendor,
            diagnostics,
        );
    };

    let mut diagnostics = Vec::new();
    exit(&mut diagnostics);

    assert_eq!(
        lanes.pending(FIRST),
        std::slice::from_ref(&message),
        "an unreadable predecessor settles nothing"
    );
    assert!(lanes.pending(SENDER).is_empty(), "no reply is owed yet");
    assert!(
        String::from_utf8_lossy(&diagnostics).contains(&test_uuid(FIRST).to_string()),
        "the failure is visible"
    );

    fs::remove_file(&tape).expect("the stand-in leaves");
    fs::rename(&hidden, &tape).expect("the tape returns");
    exit(&mut Vec::new());
    exit(&mut Vec::new());
    assert!(
        lanes.pending(FIRST).is_empty(),
        "the readable answer retires"
    );
    assert_eq!(
        lanes.pending(SENDER).len(),
        1,
        "one reply reaches the sender"
    );
    lanes.finish();
}

/// Removing a lane ends its peer duties' destination: each live one parks
/// once with the lane-removed reason and corrects its seated sender once, a
/// lane that takes its place inherits nothing, and recovery refuses to revive
/// the duty.
#[test]
fn a_removed_lane_parks_its_syn_and_a_replacement_lane_inherits_nothing() {
    let lanes = Lanes::new("syn-lane-removed");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: hold the merge"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    let message = delivered(&lanes, FIRST);

    lanes.vacate(FIRST);
    lanes.remove(FIRST);
    lanes.seat(THIRD);
    let before = lanes.cursor();
    let log = lanes.dead_letter_removed_lanes();
    assert!(log.contains(&message.key.to_string()), "{log}");
    assert!(log.contains(steering::LANE_REMOVED), "{log}");
    let told = lanes.pending(SENDER);
    let [correction] = told.as_slice() else {
        panic!("the sender hears one correction: {told:?}");
    };
    assert!(
        correction.from.is_none()
            && correction.text.contains(&message.key)
            && correction.text.contains(steering::LANE_REMOVED)
            && correction.text.contains("\nhold the merge\n"),
        "{correction:?}"
    );
    assert_eq!(
        lanes.messages().len(),
        1,
        "a correction publishes no message"
    );
    let head = lanes
        .bus
        .read(move |read| read.steering_head(message.directive))
        .expect("steering reads")
        .expect("the peer message stays durable");
    assert_eq!(head.body().disposition, wire::Disposition::DeadLettered);
    assert_eq!(head.body().key, message.key, "parking never re-keys");
    let reasons = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("authority reads")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::steering::SteerDeadLettered>())
        .map(|parked| (parked.directive, parked.reason))
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        [(message.directive, steering::LANE_REMOVED.to_owned())]
    );
    assert!(
        lanes.inbox(SENDER).lines.is_empty(),
        "parking the input ends its line"
    );
    assert!(
        lanes
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("transitions read")
            .facts()
            .iter()
            .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
            .any(|ended| ended.input == message.directive
                && ended.action == wire::body::PeerLineAction::Unanswered),
        "the line ends unanswered"
    );
    let parked = lanes.cursor();
    assert!(parked > before);
    assert_eq!(
        lanes.dead_letter_removed_lanes(),
        "",
        "a parked peer message parks once"
    );
    assert_eq!(lanes.cursor(), parked, "a parked peer message parks once");
    assert!(
        lanes.pending(THIRD).is_empty(),
        "a replacement lane inherits nothing"
    );

    let refused = lanes.recover_directive(message.directive);
    assert!(
        matches!(&refused, Outcome::Refusal(refusal) if refusal.reason() == steering::LANE_REMOVED),
        "{refused:?}"
    );
    lanes.finish();
}

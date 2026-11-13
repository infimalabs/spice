use app::speech_queue::{
    Advance, AutomaticChange, Enqueue, MAX_NARRATION_CURSORS, NarrationCursors, NarrationMessage,
    SpeechControl, SpeechMode, SpeechQueue, SpeechRequest,
};

fn playback(lane: &str) -> wire::SpeechPlayback {
    wire::SpeechPlayback::System {
        voices: vec![format!("voice-{lane}")],
        words_per_minute: 175,
    }
}

fn worktree(lane: &str) -> String {
    format!("/worktrees/{lane}")
}

fn narration(lane: &str, text: &str) -> SpeechRequest {
    SpeechRequest::narration(
        worktree(lane),
        playback(lane),
        lane.to_owned(),
        text.to_owned(),
        text.to_owned(),
    )
}

fn manual(lane: &str, key: &str, text: &str) -> SpeechRequest {
    manual_control(lane, key, SpeechControl::default(), text)
}

fn manual_control(lane: &str, key: &str, control: SpeechControl, text: &str) -> SpeechRequest {
    SpeechRequest::message(
        worktree(lane),
        playback(lane),
        lane.to_owned(),
        key.to_owned(),
        control,
        text.to_owned(),
    )
}

fn started(result: Enqueue) -> app::speech_queue::SpeechEntry {
    match result {
        Enqueue::Start(entry) => entry,
        Enqueue::Queued => panic!("an idle queue must start its head"),
        Enqueue::Suppressed => panic!("an enabled team must admit automatic speech"),
    }
}

fn enable(queue: &mut SpeechQueue, team: &str) {
    assert_eq!(
        queue.set_mode(team, SpeechMode::Speak),
        AutomaticChange::Preserved
    );
}

#[test]
fn active_owner_distinguishes_control_hunks_inside_one_message() {
    let mut queue = SpeechQueue::default();
    let message = "shared-card";
    let quote = SpeechControl::new(1, 0);
    let second_answer = SpeechControl::new(0, 1);

    let quoted = started(queue.enqueue(manual_control("lane", message, quote, "quoted words")));
    let owner = queue.active_owner().expect("quoted hunk owns playback");
    assert!(owner.is_control(message, quote));
    assert!(!owner.is_control(message, SpeechControl::default()));
    assert!(!owner.is_control(message, second_answer));
    queue.stop();
    assert_eq!(queue.finish(quoted.id()), Advance::Ignored);

    started(queue.enqueue(manual_control(
        "lane",
        message,
        second_answer,
        "second answer",
    )));
    let owner = queue.active_owner().expect("second answer owns playback");
    assert!(owner.is_control(message, second_answer));
    assert!(!owner.is_control(message, quote));

    queue.stop();
    enable(&mut queue, "lane");
    started(queue.enqueue(narration("lane", message)));
    assert!(
        queue
            .active_owner()
            .expect("automatic speech owns its message")
            .is_control(message, SpeechControl::default())
    );
}

#[test]
fn one_global_head_survives_a_burst_and_each_team_keeps_one_fresh_tail() {
    let mut queue = SpeechQueue::default();
    enable(&mut queue, "team-a");
    enable(&mut queue, "team-b");
    let head = started(queue.enqueue(narration("team-a", "a1")));
    assert_eq!(head.message_key(), Some("a1"));

    assert_eq!(
        queue.enqueue(narration("team-b", "stale b")),
        Enqueue::Queued
    );
    assert_eq!(
        queue.enqueue(narration("team-b", "fresh b")),
        Enqueue::Queued
    );
    assert_eq!(
        queue.enqueue(narration("team-a", "fresh a")),
        Enqueue::Queued
    );
    assert!(queue.is_active(head.id()));
    assert_eq!(queue.pending(), 2);

    let b = match queue.finish(head.id()) {
        Advance::Start(entry) => entry,
        other => panic!("head did not advance to the oldest fresh tail: {other:?}"),
    };
    assert_eq!(b.lane(), Some("team-b"));
    assert_eq!(b.message_key(), Some("fresh b"));
    assert_eq!(b.text(), "fresh b");
    assert_eq!(b.worktree(), "/worktrees/team-b");
    assert_eq!(b.playback(), &playback("team-b"));
    let a = match queue.finish(b.id()) {
        Advance::Start(entry) => entry,
        other => panic!("second team did not advance: {other:?}"),
    };
    assert_eq!(a.lane(), Some("team-a"));
    assert_eq!(a.text(), "fresh a");
    assert_eq!(a.worktree(), "/worktrees/team-a");
    assert_eq!(a.playback(), &playback("team-a"));
    assert_eq!(queue.finish(a.id()), Advance::Idle);
}

#[test]
fn stop_and_lane_abandonment_cannot_revive_superseded_playback() {
    let mut queue = SpeechQueue::default();
    enable(&mut queue, "removed");
    enable(&mut queue, "survivor");
    let head = started(queue.enqueue(manual("team-head", "surface", "surface")));
    assert_eq!(
        queue.enqueue(narration("removed", "drop me")),
        Enqueue::Queued
    );
    assert_eq!(
        queue.enqueue(narration("survivor", "keep me")),
        Enqueue::Queued
    );

    queue.abandon_lane("removed");
    assert_eq!(
        queue.enqueue(narration("removed", "late stale work")),
        Enqueue::Suppressed
    );
    let survivor = match queue.finish(head.id()) {
        Advance::Start(entry) => entry,
        other => panic!("abandonment removed another team: {other:?}"),
    };
    assert_eq!(survivor.lane(), Some("survivor"));
    queue.stop();
    queue.stop();
    assert!(!queue.active());
    assert_eq!(queue.pending(), 0);
    assert_eq!(queue.finish(survivor.id()), Advance::Ignored);
}

#[test]
fn quiet_is_a_default_closed_team_policy_that_preserves_explicit_playback() {
    let mut queue = SpeechQueue::default();
    assert_eq!(
        queue.enqueue(narration("team-a", "default quiet")),
        Enqueue::Suppressed
    );

    let manual_head =
        started(queue.enqueue(manual("team-a", "manual", "explicit playback while quiet")));
    enable(&mut queue, "team-a");
    assert_eq!(
        queue.enqueue(narration("team-a", "queued automatic")),
        Enqueue::Queued
    );
    assert_eq!(
        queue.set_mode("team-a", SpeechMode::Quiet),
        AutomaticChange::Preserved
    );
    assert!(queue.is_active(manual_head.id()));
    assert_eq!(queue.pending(), 0);
    assert_eq!(
        queue.enqueue(narration("team-a", "late stale automatic")),
        Enqueue::Suppressed
    );

    queue.stop();
    enable(&mut queue, "team-a");
    enable(&mut queue, "team-b");
    let automatic = started(queue.enqueue(narration("team-a", "automatic head")));
    assert_eq!(
        queue.enqueue(narration("team-b", "enabled team successor")),
        Enqueue::Queued
    );
    assert_eq!(
        queue.enqueue(manual("team-a", "manual", "queued explicit playback")),
        Enqueue::Queued
    );
    let enabled_team = match queue.set_mode("team-a", SpeechMode::Quiet) {
        AutomaticChange::Start(entry) => entry,
        other => panic!("quiet did not preserve another enabled team: {other:?}"),
    };
    assert_eq!(enabled_team.lane(), Some("team-b"));
    assert_eq!(
        queue.enqueue(narration("team-a", "stale after suppression")),
        Enqueue::Suppressed
    );
    assert_eq!(queue.pending(), 1);
    let manual = match queue.finish(enabled_team.id()) {
        Advance::Start(entry) => entry,
        other => panic!("enabled team did not preserve manual successor: {other:?}"),
    };
    assert_eq!(manual.message_key(), Some("manual"));
    assert_eq!(queue.finish(automatic.id()), Advance::Ignored);
}

#[test]
fn only_narrate_keeps_one_session_active_across_teams_and_idle_queue_gaps() {
    let mut queue = SpeechQueue::default();
    assert!(!queue.narration_session_active());

    queue.set_mode("team-a", SpeechMode::Speak);
    assert!(!queue.narration_session_active());
    queue.set_mode("team-a", SpeechMode::Narrate);
    assert!(queue.narration_session_active());
    queue.set_mode("team-b", SpeechMode::Narrate);
    queue.stop();
    assert!(queue.narration_session_active());

    queue.set_mode("team-a", SpeechMode::Speak);
    assert!(queue.narration_session_active());
    queue.abandon_lane("team-b");
    assert!(!queue.narration_session_active());

    queue.set_mode("team-a", SpeechMode::Narrate);
    assert!(queue.narration_session_active());
    queue.set_mode("team-a", SpeechMode::Quiet);
    assert!(!queue.narration_session_active());
}

#[test]
fn narration_cursors_suppress_materialized_history_and_admit_only_live_prefixes() {
    let mut cursors = NarrationCursors::default();
    let history = vec![
        message("agent-a", "a1", "old a"),
        message("agent-b", "b1", "old b"),
    ];
    assert!(cursors.observe(true, &history).is_empty());

    let live = vec![
        message("agent-b", "b2", "live b"),
        history[1].clone(),
        automatic_message("agent-a", "a2", "live a", "selected a"),
        history[0].clone(),
    ];
    assert_eq!(
        cursors.observe(false, &live),
        [
            app::speech_queue::NarrationLine {
                lane: "agent-b".to_owned(),
                worktree: worktree("agent-b"),
                playback: playback("agent-b"),
                key: "b2".to_owned(),
                narration: Some("live b".to_owned()),
                automatic_speech: None,
            },
            app::speech_queue::NarrationLine {
                lane: "agent-a".to_owned(),
                worktree: worktree("agent-a"),
                playback: playback("agent-a"),
                key: "a2".to_owned(),
                narration: Some("live a".to_owned()),
                automatic_speech: Some("selected a".to_owned()),
            },
        ]
    );

    let restored = NarrationCursors::restore(cursors.rows());
    let old_reconnect = vec![
        message("agent-a", "a3", "arrived while disconnected"),
        live[2].clone(),
        live[0].clone(),
    ];
    let mut restored = restored;
    assert!(restored.observe(true, &old_reconnect).is_empty());
}

#[test]
fn a_two_message_narration_batch_admits_only_its_newest_message() {
    let mut cursors = NarrationCursors::default();
    let history = message("agent-a", "a1", "history");
    assert!(
        cursors
            .observe(true, std::slice::from_ref(&history))
            .is_empty()
    );

    let lines = cursors.observe(
        false,
        &[
            message("agent-a", "a3", "newest unseen"),
            message("agent-a", "a2", "older unseen"),
            history,
        ],
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line.narration.as_deref().expect("a narrated line speaks"))
            .collect::<Vec<_>>(),
        ["newest unseen"]
    );

    let mut busy_queue = SpeechQueue::default();
    enable(&mut busy_queue, "team-b");
    enable(&mut busy_queue, "agent-a");
    let busy_head = started(busy_queue.enqueue(narration("team-b", "active head")));
    for line in lines.clone() {
        assert_eq!(
            busy_queue.enqueue(narration(&line.lane, line.narration.as_deref().unwrap())),
            Enqueue::Queued
        );
    }
    let newest = match busy_queue.finish(busy_head.id()) {
        Advance::Start(entry) => entry,
        other => panic!("busy queue did not advance to the newest batched line: {other:?}"),
    };
    assert_eq!(newest.text(), "newest unseen");

    let mut queue = SpeechQueue::default();
    enable(&mut queue, "agent-a");
    let first = lines
        .into_iter()
        .find_map(|line| {
            match queue.enqueue(narration(&line.lane, line.narration.as_deref().unwrap())) {
                Enqueue::Start(entry) => Some(entry),
                Enqueue::Queued | Enqueue::Suppressed => None,
            }
        })
        .expect("an idle queue commits the first narration line");
    assert_eq!(first.text(), "newest unseen");
}

/// A turn's final answer revises the prose card already shown under the same
/// key. Speak hears that revision once; Narrate already spoke the card.
#[test]
fn a_final_revision_of_a_seen_card_is_admitted_once_for_speak_only() {
    let mut cursors = NarrationCursors::default();
    let history = message("agent-a", "a1", "history");
    assert!(
        cursors
            .observe(true, std::slice::from_ref(&history))
            .is_empty()
    );
    let prose = message("agent-a", "a2", "streamed answer");
    let first = cursors.observe(false, &[prose, history.clone()]);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].automatic_speech, None);

    let terminal = automatic_message("agent-a", "a2", "streamed answer", "final edges");
    let revised = cursors.observe(false, &[terminal.clone(), history.clone()]);
    assert_eq!(
        revised
            .iter()
            .map(|line| (
                line.key.as_str(),
                line.narration.as_deref(),
                line.automatic_speech.as_deref()
            ))
            .collect::<Vec<_>>(),
        [("a2", None, Some("final edges"))]
    );

    assert!(
        cursors
            .observe(false, &[terminal.clone(), history.clone()])
            .is_empty()
    );
    let mut restored = NarrationCursors::restore(cursors.rows());
    assert!(restored.observe(false, &[terminal, history]).is_empty());
}

#[test]
fn cursor_storage_is_bounded_and_a_missing_cursor_rebases_without_reading_history() {
    let mut cursors = NarrationCursors::default();
    for index in 0..=MAX_NARRATION_CURSORS {
        let lane = format!("agent-{index}");
        let key = format!("message-{index}");
        assert!(
            cursors
                .observe(false, &[message(&lane, &key, "history")])
                .is_empty()
        );
    }
    assert_eq!(cursors.len(), MAX_NARRATION_CURSORS);
    assert_eq!(cursors.rows().first().unwrap().0, "agent-1");

    assert!(
        cursors
            .observe(
                false,
                &[
                    message("agent-1", "newest", "also old"),
                    message("agent-1", "history-not-containing-cursor", "old"),
                ],
            )
            .is_empty()
    );
}

fn message(lane: &str, key: &str, text: &str) -> NarrationMessage {
    NarrationMessage {
        lane: lane.to_owned(),
        worktree: worktree(lane),
        playback: playback(lane),
        key: key.to_owned(),
        narration: Some(text.to_owned()),
        automatic_speech: None,
    }
}

fn automatic_message(lane: &str, key: &str, text: &str, speech: &str) -> NarrationMessage {
    NarrationMessage {
        automatic_speech: Some(speech.to_owned()),
        ..message(lane, key, text)
    }
}

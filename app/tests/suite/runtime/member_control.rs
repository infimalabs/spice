use super::*;
use app::runtime::{MemberCommand, MemberControlState, MemberSettlement};

fn pending(runtime: &AppRuntime, lane: wire::CanonicalUuid) -> Option<&MemberCommand> {
    runtime
        .member_control(lane)
        .and_then(MemberControlState::pending)
        .map(|request| &request.command)
}

fn settled(runtime: &AppRuntime, lane: wire::CanonicalUuid) -> Option<&MemberSettlement> {
    runtime
        .member_control(lane)
        .and_then(MemberControlState::settled)
}

fn issue(runtime: &AppRuntime, lane: wire::CanonicalUuid) -> Option<&str> {
    settled(runtime, lane).and_then(|settled| settled.issue.as_deref())
}

#[test]
fn member_commands_settle_only_the_exact_reservation_they_answer() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let lane = uuid(THIRD_LANE);

    let drain = runtime
        .begin_member_command(lane, MemberCommand::Drain)
        .expect("an idle member reserves a command");
    assert!(
        runtime
            .begin_member_command(lane, MemberCommand::Remove)
            .is_none(),
        "a member runs one command at a time"
    );
    assert_eq!(pending(&runtime, lane), Some(&MemberCommand::Drain));

    runtime.settle_member_command(&drain, Some("Could not drain this agent: busy.".to_owned()));
    assert_eq!(pending(&runtime, lane), None);
    assert_eq!(
        issue(&runtime, lane),
        Some("Could not drain this agent: busy.")
    );

    let retry = runtime
        .begin_member_command(lane, MemberCommand::Drain)
        .expect("a refused member can retry");
    assert_eq!(
        settled(&runtime, lane),
        None,
        "a new command clears the last settlement"
    );
    runtime.settle_member_command(&drain, Some("late".to_owned()));
    assert_eq!(
        pending(&runtime, lane),
        Some(&MemberCommand::Drain),
        "a late answer to an earlier drain cannot settle its retry"
    );
    assert_eq!(settled(&runtime, lane), None);

    assert!(!runtime.reconnect(&accepted));
    assert_eq!(
        pending(&runtime, lane),
        None,
        "a reconnect abandons the command"
    );
    let abandoned = settled(&runtime, lane)
        .expect("an abandoned command still settles, so its progress report can end")
        .clone();
    assert_eq!(abandoned.request, retry);
    assert!(
        abandoned.issue.is_some(),
        "an unanswered command is not accepted"
    );
    runtime.settle_member_command(&retry, Some("after reconnect".to_owned()));
    assert_eq!(
        settled(&runtime, lane),
        Some(&abandoned),
        "an abandoned command's answer no longer lands on the member"
    );

    let wake = runtime
        .begin_member_command(lane, MemberCommand::Wake)
        .expect("the member is free again after reconnect");
    runtime.settle_member_command(&wake, None);
    assert_eq!(pending(&runtime, lane), None);
    assert_eq!(
        settled(&runtime, lane),
        Some(&MemberSettlement {
            request: wake,
            issue: None,
        }),
        "an accepted command settles without an issue"
    );
}

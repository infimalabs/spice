//! Steering-specific client preflight and command mapping.

use super::*;

/// Submit one steering command with the caller's optional activated lane.
pub(crate) fn execute_steering(
    discovery_path: &Path,
    contract: &Contract,
    selected: SelectedCommand<'_>,
    lane_context: Option<CanonicalUuid>,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    match selected {
        SelectedCommand::SteerSend { lane, message } => {
            let inbox = match read_inbox(discovery_path, Connection::Operator, Some(lane), ambient)
            {
                Ok(inbox) => inbox.answer,
                Err(outcome) => return outcome,
            };
            let Some(row) = inbox.lanes.iter().find(|row| row.lane == lane) else {
                return steering_refusal(
                    "steer::lane-unknown",
                    Some(lane.to_string()),
                    "seat the lane before sending it direction",
                );
            };
            let mapped = MappedCommand::new(
                contract,
                &["steer"],
                CommandKind::SteerSend,
                vec![Witness::Residency {
                    lane,
                    occupant: row.occupant,
                    epoch: inbox.epoch,
                }],
                &wire::intent::SteerSend {
                    fanout: CanonicalUuid::new(uuid::Uuid::new_v4()),
                    recipients: Vec::new(),
                    lane: Some(lane),
                    message: message.to_owned(),
                    attachments: Vec::new(),
                },
            );
            execute_mapped(
                discovery_path,
                contract,
                Connection::Operator,
                mapped,
                ambient,
            )
        }
        SelectedCommand::SteerInbox { lane } => {
            let connection = lane_context.map_or(Connection::Operator, Connection::Lane);
            match read_inbox(discovery_path, connection, lane, ambient) {
                Ok(inbox) => Outcome::Reply(Reply {
                    position: ReplyPosition::Snapshot(inbox.position),
                    body: CommandAnswer::Inbox(inbox.answer),
                }),
                Err(outcome) => outcome,
            }
        }
        selected => execute(discovery_path, contract, selected, ambient),
    }
}

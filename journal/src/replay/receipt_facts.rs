use super::*;
pub(super) fn verify_contribution_receipt(
    row: &ReceiptRow,
    facts: &BTreeMap<u64, FactRecord>,
    uncovered: &mut std::collections::BTreeSet<u64>,
    violations: &mut Vec<Violation>,
) {
    let Some(seq) = row
        .first
        .and_then(|value| u64::try_from(value).ok())
        .filter(|_| row.last == row.first)
    else {
        receipt_violation(
            row,
            violations,
            "room contribution receipt does not span exactly one fact",
        );
        return;
    };
    let Some(record) = facts.get(&seq) else {
        receipt_violation(
            row,
            violations,
            "room contribution receipt names a missing fact",
        );
        return;
    };
    let Some(envelope) = record.envelope.as_ref() else {
        receipt_violation(
            row,
            violations,
            "room contribution receipt spans an undecodable fact",
        );
        return;
    };
    let Some(contribution) = envelope.fact.read::<wire::rooms::RoomContributed>() else {
        receipt_violation(
            row,
            violations,
            "room contribution receipt spans another fact kind",
        );
        return;
    };
    let Ok(command) = wire::decode::<wire::Command<serde_json::Value>>(&row.request) else {
        return;
    };
    let Ok(request) = serde_json::from_value::<wire::intent::RoomContribute>(command.body) else {
        receipt_violation(
            row,
            violations,
            "room contribution request body does not decode",
        );
        return;
    };
    let Ok(response) = wire::decode::<wire::Response<serde_json::Value>>(&row.response) else {
        return;
    };
    let wire::Outcome::Reply(reply) = response.outcome else {
        return;
    };
    let Ok(answer) = serde_json::from_value::<wire::intent::RoomContributeAnswer>(reply.body)
    else {
        receipt_violation(
            row,
            violations,
            "room contribution answer body does not decode",
        );
        return;
    };
    let Ok(actor) = row.actor.parse::<wire::Actor>() else {
        return;
    };
    let matches = contribution.matches_command(&actor, &request, &command.expected, &answer);
    if actor != envelope.actor || !matches {
        receipt_violation(
            row,
            violations,
            "room contribution fact does not match its command receipt",
        );
        return;
    }
    if !uncovered.remove(&seq) {
        receipt_violation(
            row,
            violations,
            "room contribution fact has more than one command receipt",
        );
    }
}

pub(super) fn verify_steer_send_receipt(
    row: &ReceiptRow,
    facts: &BTreeMap<u64, FactRecord>,
    violations: &mut Vec<Violation>,
) {
    let Some(seq) = row
        .first
        .and_then(|value| u64::try_from(value).ok())
        .filter(|_| row.last == row.first)
    else {
        receipt_violation(
            row,
            violations,
            "steer::send receipt does not span exactly one fact",
        );
        return;
    };
    let Some(envelope) = facts.get(&seq).and_then(|record| record.envelope.as_ref()) else {
        receipt_violation(
            row,
            violations,
            "steer::send receipt spans a missing or undecodable fact",
        );
        return;
    };
    let Some(sent) = envelope.fact.read::<wire::body::SteerSent>() else {
        receipt_violation(
            row,
            violations,
            "steer::send receipt spans another fact kind",
        );
        return;
    };
    let Ok(command) = wire::decode::<wire::Command<serde_json::Value>>(&row.request) else {
        return;
    };
    let request = match crate::guard::decode_steer_send_receipt(command.body) {
        Ok(request) => request,
        Err(detail) => {
            receipt_violation(row, violations, detail);
            return;
        }
    };
    let Ok(response) = wire::decode::<wire::Response<serde_json::Value>>(&row.response) else {
        return;
    };
    let wire::Outcome::Reply(reply) = response.outcome else {
        return;
    };
    let answer = match serde_json::from_value::<wire::intent::DirectiveAnswer>(reply.body) {
        Ok(answer) => answer,
        Err(error) => {
            receipt_violation(
                row,
                violations,
                format!("steer::send receipt answer does not decode: {error}"),
            );
            return;
        }
    };
    let Ok(actor) = row.actor.parse::<wire::Actor>() else {
        return;
    };
    if actor != envelope.actor
        || command.id != sent.directive
        || !request.matches_sent(&sent)
        || answer.directive != sent.directive
        || answer.recipient != sent.recipient
        || answer.key != sent.key
        || answer.authored != envelope.at
        || answer.disposition != wire::Disposition::Live
    {
        receipt_violation(
            row,
            violations,
            "steer::sent fact does not match its command receipt",
        );
    }
}

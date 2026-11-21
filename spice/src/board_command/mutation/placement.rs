//! Ordered creation placement, retaining the router observation through commit.

use super::*;

pub(in crate::board_command) struct CreationPlacement {
    pub(in crate::board_command) topic: wire::TaskTopic,
    /// Router evidence; absent when the creation never consulted the router.
    pub(in crate::board_command) route: Option<wire::intent::TaskRoute>,
    pub(in crate::board_command) capture: Option<super::super::routing::Capture>,
}

pub(in crate::board_command) fn place_creation(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
    actor: &Actor,
    named: Option<&wire::TopicInput>,
    statement: super::super::routing::Statement<'_>,
    origin: Option<super::super::routing::OriginView>,
    held_dependency_topic: Option<&wire::TaskTopic>,
) -> Result<CreationPlacement, Outcome<CommandAnswer>> {
    let placement = place_oops_creation(
        bus,
        snapshot,
        actor,
        named,
        statement,
        origin,
        held_dependency_topic,
    )?;
    if let Some(topic) = &placement.topic.path {
        refuse_reserved_oops_topic(topic)?;
    }
    Ok(placement)
}

/// Only a violated source or policy assumption can refuse automatic placement.
/// The capture resolves an authored suffix once, leaving the task unplaced
/// with that tail when the router cannot decide; complete destinations and
/// inherited topics retain their explicit placement semantics.
pub(super) fn place_oops_creation(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
    actor: &Actor,
    named: Option<&wire::TopicInput>,
    statement: super::super::routing::Statement<'_>,
    origin: Option<super::super::routing::OriginView>,
    held_dependency_topic: Option<&wire::TaskTopic>,
) -> Result<CreationPlacement, Outcome<CommandAnswer>> {
    let explicit = match (named, held_dependency_topic) {
        (Some(wire::TopicInput::Absolute(topic) | wire::TopicInput::Private(topic)), _) => {
            Some(wire::TaskTopic::from(topic.clone()))
        }
        (None, Some(parent)) => Some(wire::TaskTopic {
            path: parent.path.clone(),
            placed: None,
            named: parent.tail().cloned().map(wire::TopicInput::Tail),
            unmatched: parent.unmatched,
            nodes: parent.nodes.map(|nodes| wire::topic::TopicNodes {
                node: nodes.node,
                placed: None,
            }),
        }),
        _ => None,
    };
    if let Some(topic) = explicit {
        return Ok(CreationPlacement {
            topic,
            route: None,
            capture: None,
        });
    }
    let capture = super::super::routing::Capture::new(
        bus,
        snapshot,
        actor,
        statement.with_topic(named),
        origin,
    )?;
    let routed = capture.restricted();
    let route = match &routed {
        Ok(routed) => routed.route.clone(),
        Err(error) => wire::intent::TaskRoute::Unavailable {
            reason: error.reason().to_owned(),
            detail: error.to_string(),
        },
    };
    let topic = routed.map(|routed| routed.topic).map_err(|error| {
        refusal(
            error.reason(),
            Some(error.to_string()),
            "correct the named team policy or name an explicitly admitted topic",
        )
    })?;
    Ok(CreationPlacement {
        topic,
        route: Some(route),
        capture: Some(capture),
    })
}

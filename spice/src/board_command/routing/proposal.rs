use super::*;
use crate::board_command::topic_tree::{TopicTree, seat};
use wire::Topic;

/// The tree proposes from the statement alone; what the filer may use is
/// checked against that proposal afterwards.
pub(super) fn choose(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
    authority: &authority::Authority<'_>,
    roots: &RoutingRoots,
    nodes: &[journal::TopicRoutingNode],
    statement: &Statement<'_>,
) -> Result<Routed, Outcome<CommandAnswer>> {
    let (observed, directory) = current_sources(bus, snapshot)?;
    let Some(tree) = TopicTree::for_route(&directory) else {
        return Ok(catching_up(statement));
    };
    let prepared = statement.prepare();
    let minimum = if statement.for_approval {
        0
    } else {
        authority.placement_min_lead()
    };
    let (seat, lag) = match tree.with_current(&observed, |state| {
        let authored = statement.authored();
        (
            seat::forecast(state, &observed, &prepared, &authored, nodes, minimum > 0),
            state.lag(&observed),
        )
    }) {
        Ok(seat) => seat,
        Err(Outcome::Refusal(refusal)) if refusal.reason() == "topic::preview-catching-up" => {
            return Ok(catching_up(statement));
        }
        Err(outcome) => return Err(outcome),
    };
    let lead = seat.lead;
    let chosen = decide(
        authority,
        roots,
        seat.destination,
        seat.companions,
        lag,
        statement.tail,
        nodes,
    );
    let inferred = matches!(
        chosen.route,
        TaskRoute::Pick {
            unmatched: false,
            ..
        }
    );
    if inferred && lead < minimum {
        return Ok(unavailable(
            statement.tail,
            "task::placement-weak-match",
            &format!("lead {lead} below required {minimum}; choose --topic or approve task place"),
        ));
    }
    Ok(chosen)
}

/// What filing does with the tree's proposal. One the filer may not use is
/// reported and files nothing. The team's unmatched topic receives only a
/// statement the tree could not name, or one it would seat beneath that topic.
pub(in crate::board_command) fn decide(
    authority: &authority::Authority<'_>,
    roots: &RoutingRoots,
    destination: Option<Topic>,
    companions: Vec<wire::TaskStamp>,
    lag: Option<wire::topic::TreeLag>,
    tail: Option<&wire::TopicTail>,
    nodes: &[journal::TopicRoutingNode],
) -> Routed {
    let unmatched = authority.unmatched();
    if let Some(destination) = destination {
        let chosen = routed(destination, companions, lag, false, tail, nodes);
        let TaskRoute::Pick { topic, .. } = &chosen.route else {
            unreachable!("a routed destination is a pick");
        };
        if !unmatched.is_some_and(|unmatched| unmatched.contains(topic)) {
            if allowed(authority, roots, topic) {
                return chosen;
            }
            return unavailable(
                tail,
                "task::route-not-admitted",
                &format!(
                    "the tree proposes {topic}, which this filer may not use; choose a topic with --topic"
                ),
            );
        }
    }
    if let Some(unmatched) = unmatched {
        let fallback = routed(unmatched.clone(), Vec::new(), None, true, tail, nodes);
        if fallback
            .topic
            .path
            .as_ref()
            .is_some_and(|p| roots.admits(p) && authority.admits(p))
        {
            return fallback;
        }
    }
    undecided(tail)
}

fn catching_up(statement: &Statement<'_>) -> Routed {
    unavailable(
        statement.tail,
        "topic::preview-catching-up",
        "no complete topic generation is available for this decision",
    )
}

fn current_sources(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
) -> Result<(bus::TaskSnapshot, std::path::PathBuf), Outcome<CommandAnswer>> {
    let observed = bus
        .task_snapshot()
        .map_err(|e| store_fault("board::heads-unreadable", e.to_string()))?;
    if read_guard::task_projection_barrier(observed.task_projection_cursor())
        != snapshot.answer.task_barrier
    {
        return Err(refusal(
            "task::routing-snapshot-moved",
            None,
            "capture the current topic tree",
        ));
    }
    let paths = snapshot
        .policy
        .repository()
        .admit_state()
        .map_err(|e| store_fault("topic::state-unavailable", e.to_string()))?;
    Ok((observed, paths.data().to_owned()))
}

fn allowed(authority: &authority::Authority<'_>, roots: &RoutingRoots, path: &Topic) -> bool {
    path.private_owner().is_none()
        && !wire::is_oops_topic(path)
        && roots.admits(path)
        && authority.admits(path)
}

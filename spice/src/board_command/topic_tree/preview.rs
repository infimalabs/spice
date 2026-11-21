use super::{
    Actor, Bus, CommandAnswer, Outcome, Repository,
    current::{Generation, TopicTree},
    read_guard, routing, seat,
};
use std::collections::BTreeMap;
use wire::{
    Reply, ReplyPosition,
    topic::{Group, GroupMember, Grouping, NextSeat, TopicAnswer},
};

pub(crate) fn preview(
    bus: &Bus,
    repository: &Repository,
    actor: &Actor,
    tree: &TopicTree,
    title: &str,
    pitch: &str,
) -> Outcome<CommandAnswer> {
    if title.trim().is_empty() || pitch.trim().is_empty() {
        return read_guard::refusal(
            "topic::invalid-placement-query",
            None,
            "supply a fresh title and pitch",
        );
    }
    let grouping = match forecast(bus, repository, actor, tree, [title, pitch]) {
        Ok(grouping) => grouping,
        Err(outcome) => return outcome,
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(grouping.cursor),
        body: CommandAnswer::Topic(Box::new(TopicAnswer {
            node: None,
            history: Vec::new(),
            next: None,
            preview: None,
            change: None,
            grouping: Some(grouping),
        })),
    })
}

fn forecast(
    bus: &Bus,
    repository: &Repository,
    actor: &Actor,
    tree: &TopicTree,
    authored: [&str; 2],
) -> Result<Grouping, Outcome<CommandAnswer>> {
    let observed = bus
        .task_snapshot()
        .map_err(|e| read_guard::store_fault("board::heads-unreadable", e.to_string()))?;
    let snapshot = read_guard::routing_snapshot(&observed, Some(repository), actor.clone())?;
    let authority = routing::authority::Authority::new(
        &snapshot,
        routing::creator(&snapshot, actor)?,
        &wire::Phase::Todo,
    )
    .map_err(|e| read_guard::refusal(e.reason(), None, e.to_string()))?;
    let roots = authority
        .roots()
        .map_err(|e| read_guard::refusal(e.reason(), None, e.to_string()))?;
    let (nodes, _) = routing::Capture::topology(bus, &snapshot)?;
    let [title, pitch] = authored;
    let fresh = topic::evidence::prepare_statement("", title, pitch, None, &[]);
    tree.with_current(&observed, |state| {
        let mut seat = seat::forecast(state, &observed, &fresh, &authored, &nodes, false);
        let proposal = routing::proposal::decide(
            &authority,
            &roots,
            seat.destination.take(),
            seat.companions.clone(),
            state.lag(&observed),
            None,
            &nodes,
        );
        describe(&observed, state, seat, proposal.route)
    })
}

fn describe(
    observed: &bus::TaskSnapshot,
    state: &Generation,
    seat: seat::Seat,
    proposal: wire::intent::TaskRoute,
) -> Grouping {
    let tree = state.tree();
    let held = tree.held();
    let rows = tree.rows();
    let source: Vec<_> = (0..rows.len())
        .map(|row| match state.member(row) {
            seat::Member::Fixed(node) => Shown::Fixed(&node.path),
            seat::Member::Task(head) => {
                let task = observed.task_heads(&head.id).expect("current window task");
                Shown::Task(&head.id, task.statement().body().title.as_str())
            }
        })
        .collect();
    let groups = visible_groups(&held, &source, |i| tree.row_index(i));
    let path = seat.path;
    let next_seat = if path.is_empty() {
        NextSeat::Alone
    } else {
        let companions = seat
            .companions
            .into_iter()
            .map(|id| {
                let title = observed
                    .task_heads(&id)
                    .expect("current companion")
                    .statement()
                    .body()
                    .title
                    .clone();
                GroupMember { id, title }
            })
            .collect();
        NextSeat::Group {
            path_existed_before: held.contains_key(&path),
            path,
            companions,
        }
    };
    Grouping {
        retained: state.inputs.len(),
        window: rows.len(),
        freshness: state.freshness(observed),
        cursor: observed.cursor().expect("validated snapshot"),
        proposal,
        groups,
        next_seat,
    }
}

type Held = BTreeMap<Vec<usize>, Vec<usize>>;

#[derive(Clone, Copy)]
enum Shown<'a> {
    Task(&'a wire::TaskStamp, &'a str),
    Fixed(&'a wire::Topic),
}

fn visible_groups(held: &Held, source: &[Shown<'_>], row: impl Fn(usize) -> usize) -> Vec<Group> {
    let mut groups = Vec::with_capacity(held.len());
    // The held map emits every ancestor before its children. Accumulate
    // subtree counts and an illustrative member on the return pass.
    let mut totals = BTreeMap::new();
    for (path, members) in held.iter().rev() {
        let value = totals.entry(path.as_slice()).or_insert((0usize, None));
        value.0 += members.len();
        if let Some(&first) = members.first() {
            value.1 = Some(first);
        }
        let (count, first) = *value;
        if let Some((_, parent)) = path.split_last() {
            let parent = totals.entry(parent).or_insert((0, None));
            parent.0 += count;
            if parent.1.is_none() {
                parent.1 = first;
            }
        }
    }
    for (path, members) in held {
        let (count, first) = totals[path.as_slice()];
        let mut direct = Vec::with_capacity(members.len());
        let mut fixed = Vec::new();
        for &i in members {
            match source[row(i)] {
                Shown::Fixed(topic) => fixed.push(topic.clone()),
                Shown::Task(id, title) => direct.push(GroupMember {
                    id: id.clone(),
                    title: title.to_owned(),
                }),
            }
        }
        let label = if path.is_empty() {
            "whole board".to_owned()
        } else {
            match source[row(first.expect("non-root groups have members"))] {
                Shown::Fixed(topic) => topic.to_string(),
                Shown::Task(_, title) => title.to_owned(),
            }
        };
        groups.push(Group {
            path: path.clone(),
            label,
            subtree: count,
            direct,
            fixed,
        });
    }
    groups
}

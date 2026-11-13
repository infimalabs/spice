use super::*;
use wire::rooms::{
    RoomAuthorityStamp, RoomCharge, RoomContributionKind, RoomSource, StatementItem,
    StatementItemPrefixError,
};

pub(super) fn dispatch_room_client(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Option<Outcome<CommandAnswer>> {
    let outcome = match contract.path {
        ["room", "post"] => client_room_post(repository, contract, matches),
        ["room", "score"] => client_room_score(repository, contract, matches),
        ["room", "leave"] => client_room_leave(repository, contract, matches),
        ["room", "contribute"] => client_room_contribute(repository, contract, matches),
        ["room", "ack" | "nak" | "fin"] => client_line_control(repository, contract, matches),
        _ => return None,
    };
    Some(outcome)
}

pub(super) fn client_room_post(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |_board, room, heads| {
        let text = matches
            .get_one::<String>("TEXT")
            .cloned()
            .unwrap_or_default();
        Ok((
            CommandKind::RoomPost,
            vec![statement_witness(heads)],
            serde_json::to_value(RoomPost { room, text }).expect("typed room post encodes"),
        ))
    })
}
fn client_room_score(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    let written = required(matches, "LANE");
    let address = match written.parse::<wire::OccupantAddress>() {
        Ok(address) => address,
        Err(error) => {
            return refusal(
                "room::score-lane-invalid",
                Some(written),
                format!("name the lane by any occupant code it has shown: {error}"),
            );
        }
    };
    crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::RoomScore,
        Vec::new(),
        &wire::intent::RoomScore {
            address,
            text: required(matches, "TEXT"),
        },
    )
}

/// Contribute one charge or question to an open Design's room as the
/// connected lane, witnessing the Design's statement and workflow as the board
/// reads and resolving each `--cite` key to the exact source it names.
fn client_room_contribute(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    mutate_task_from_board(repository, contract, matches, |board, room, heads| {
        let kind = match (matches.get_one::<String>("charge"), matches.get_flag("ask")) {
            (Some(written), _) => written
                .parse::<RoomCharge>()
                .map(RoomContributionKind::Charge)
                .map_err(|error| {
                    refusal(
                        "room::contribution-charge-invalid",
                        Some(written.clone()),
                        error.to_string(),
                    )
                })?,
            (None, true) => RoomContributionKind::Question,
            (None, false) => {
                return Err(refusal(
                    "room::contribution-kind-missing",
                    None,
                    "weigh one question item with --charge=C --question HEX, or pose an open \
                     question with --ask",
                ));
            }
        };
        let question = matches
            .get_one::<String>("question")
            .map(|written| question_item(written, &heads.statement.body.term))
            .transpose()?;
        let anchor = &heads.statement.body.born;
        let sources = matches
            .get_many::<String>("cite")
            .into_iter()
            .flatten()
            .map(|key| cite(board, anchor, key, &repository))
            .collect::<Result<Vec<_>, _>>()?;
        let mut expected = vec![statement_witness(heads), workflow_witness(heads)];
        expected.extend(sources.iter().map(|source| wire::Witness::Source {
            source: source.authority_digest(),
        }));
        Ok((
            CommandKind::RoomContribute,
            expected,
            serde_json::to_value(wire::intent::RoomContribute {
                room,
                kind,
                question,
                text: required(matches, "TEXT"),
                sources,
            })
            .expect("typed room contribution encodes"),
        ))
    })
}

/// The one current question item whose digest begins with `written`.
///
/// A handle no current item begins with is stale: the statement changed
/// since the lane read it. Either refusal lists the current items with their
/// handles so the lane can re-aim.
fn question_item(written: &str, term: &[String]) -> Result<StatementItem, Outcome<CommandAnswer>> {
    let current = StatementItem::each(term).collect::<Vec<_>>();
    StatementItem::resolve(written, &current)
        .cloned()
        .map_err(|error| {
            let reason = match error {
                StatementItemPrefixError::Malformed => "room::contribution-question-invalid",
                StatementItemPrefixError::Unknown => "room::contribution-question-stale",
                StatementItemPrefixError::Ambiguous => "room::contribution-question-ambiguous",
            };
            let hint = match term {
                [] => format!("{error}; the Design names no question"),
                _ => format!(
                    "{error}; current questions:\n{}",
                    StatementItem::bullets(term)
                ),
            };
            refusal(reason, Some(written.to_owned()), &hint)
        })
}

/// The exact source one `--cite` key names, resolved by its form the way
/// `spice://` resolves it: a key holding `/`, `.` or `:` cites repository
/// source as `[COMMIT:]PATH[:LINES]`, the path at the lane's HEAD unless a
/// commit is named; otherwise a task handle or stamp cites that task's
/// statement head fact, a hex digest an artifact, a publication stamp a
/// contribution in this room, and a base-52 journal code that fact. The
/// resident still refuses any source that does not exist before the
/// contribution.
pub(in crate::board_command) fn cite(
    board: &BoardAnswer,
    room: &wire::TaskStamp,
    key: &str,
    repository: &spice::repository::Repository,
) -> Result<RoomSource, Outcome<CommandAnswer>> {
    if key.contains(['/', '.', ':']) {
        return cite_source(repository, key).map(RoomSource::Git);
    }
    let statement = |task: &BoardTask| RoomSource::Journal {
        fact: wire::SequenceCode::new(task.statement.position.get()),
    };
    if let Ok(handle) = key.parse::<TaskHandle>() {
        return task_heads(board, &handle).map(statement).map_err(|_| {
            refusal(
                "room::contribution-cite-unknown",
                Some(key.to_owned()),
                "cite a task by a handle the board shows",
            )
        });
    }
    if let Ok(digest) = key.parse::<wire::Digest>() {
        return Ok(RoomSource::Artifact { digest });
    }
    if let Ok(stamp) = key.parse::<wire::TaskStamp>() {
        if let Some(task) = board
            .tasks
            .iter()
            .find(|task| task.statement.body.born == stamp)
        {
            return Ok(statement(task));
        }
        if let Ok(publication) = key.parse::<RoomAuthorityStamp>() {
            return Ok(RoomSource::Contribution {
                room: room.clone(),
                publication,
            });
        }
    }
    key.parse::<wire::SequenceCode>()
        .map(|fact| RoomSource::Journal { fact })
        .map_err(|_| {
            refusal(
                "room::contribution-cite-invalid",
                Some(key.to_owned()),
                format!("cite {}", spice::rooms::CITABLE_KEYS),
            )
        })
}

/// The tasks the `--cite` keys can name by handle or bare stamp, which a
/// board read must include for [`cite`] to resolve them to their statement
/// heads; a stamp no task carries then names a publication instead.
pub(in crate::board_command) fn cited_tasks<'a>(
    keys: impl IntoIterator<Item = &'a str>,
) -> impl Iterator<Item = wire::TaskStamp> {
    keys.into_iter()
        .filter_map(|key| match key.parse::<TaskHandle>() {
            Ok(handle) => Some(
                handle
                    .born()
                    .expect("a parsed handle carries a canonical task stamp"),
            ),
            Err(_) => key.parse::<wire::TaskStamp>().ok(),
        })
}

/// Repository source written `[COMMIT:]PATH[:LINES]`: `PATH` is relative to
/// the repository root, read at `COMMIT`, a full id or a prefix of at least
/// four hex digits naming one object, or else at the lane's HEAD, the code
/// the lane has checked out.
fn cite_source(
    repository: &spice::repository::Repository,
    key: &str,
) -> Result<wire::rooms::GitSource, Outcome<CommandAnswer>> {
    use crate::room_command::git_source;
    let unknown = |detail: &str| {
        refusal(
            "room::contribution-cite-unknown",
            Some(key.to_owned()),
            detail,
        )
    };
    let (commit, rest) = key
        .split_once(':')
        .and_then(|(commit, rest)| Some((gix::hash::Prefix::from_hex(commit).ok()?, rest)))
        .map_or((None, key), |(commit, rest)| (Some(commit), rest));
    let (path, lines) = match rest.rsplit_once(':') {
        Some((path, lines)) => {
            let lines = lines.parse::<wire::rooms::LineRange>().map_err(|error| {
                refusal(
                    "room::contribution-cite-invalid",
                    Some(key.to_owned()),
                    format!("{error}; write lines as L or L1-L2, counted from 1"),
                )
            })?;
            (path, Some(lines))
        }
        None => (rest, None),
    };
    let git = git_source::open(repository.worktree_root())
        .map_err(|_| unknown("cite source from a repository Git can open"))?;
    let commit = match commit {
        Some(prefix) => match git.objects.lookup_prefix(prefix, None) {
            Ok(Some(Ok(id))) => id
                .to_string()
                .parse()
                .map_err(|_| unknown("cite a commit whose id this build reads"))?,
            Ok(Some(Err(()))) => {
                return Err(unknown(&format!(
                    "{prefix} begins more than one object id; write more of the commit id"
                )));
            }
            Ok(None) => {
                return Err(unknown(&format!(
                    "no object in this repository has an id beginning {prefix}"
                )));
            }
            Err(_) => return Err(unknown("cite source from a repository Git can read")),
        },
        None => git
            .head_id()
            .ok()
            .and_then(|head| head.to_string().parse().ok())
            .ok_or_else(|| unknown("cite a path once the lane's HEAD names a commit"))?,
    };
    git_source::link(&git, commit, path, lines).map_err(|_| {
        unknown(
            "cite a commit the repository holds, a path from the repository root present at it, \
             and lines within that file",
        )
    })
}

/// Leave one room as the connected lane and say what still keeps it there.
fn client_room_leave(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    let written = required(matches, "HANDLE");
    let room = match written.parse::<TaskHandle>() {
        Ok(room) => room,
        Err(error) => {
            return refusal(
                "room::anchor-invalid",
                Some(written),
                format!("name the room's anchor task handle: {error}"),
            );
        }
    };
    let outcome = crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::RoomLeave,
        Vec::new(),
        &wire::intent::RoomLeave {
            room,
            clause: matches
                .get_one::<String>("clause")
                .cloned()
                .unwrap_or_default(),
        },
    );
    let Outcome::Reply(Reply {
        position,
        body: CommandAnswer::RoomLeave(answer),
    }) = outcome
    else {
        return outcome;
    };
    Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Text(TextAnswer {
            text: render_leave(&answer),
        }),
    })
}

fn render_leave(answer: &wire::intent::RoomLeaveAnswer) -> String {
    use wire::intent::RoomListeningDuty as Duty;
    let mut lines = vec![format!(
        "left {} publication {}",
        answer.room, answer.publication
    )];
    if answer.kept.is_empty() {
        lines.push(
            "no longer listening; a later room message or Design request restores listening"
                .to_owned(),
        );
        return lines.join("\n");
    }
    lines.push("still listening until these native duties end; leaving ends none:".to_owned());
    for duty in &answer.kept {
        lines.push(match duty {
            Duty::Work { task } => {
                format!("  work {task}: active claim on the anchor or related work")
            }
            Duty::Stop { key } => format!("  stop {key}: live stop request for this lane's claim"),
        });
    }
    lines.join("\n")
}

/// Submit one principal's ACK, NAK, or FIN of the exact input its key names.
fn client_line_control(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    use wire::intent::{SteerLineAction, SteerLineControl};
    let key = required(matches, "KEY");
    let action = match contract.path {
        ["room", "ack"] => SteerLineAction::Ack,
        ["room", "nak"] => SteerLineAction::Nak,
        _ => SteerLineAction::Fin,
    };
    let input = match crate::room_command::input::resolve(repository.repository, &key) {
        Ok(input) => input,
        Err(outcome) => return outcome,
    };
    let body = matches
        .get_one::<String>("BODY")
        .or_else(|| matches.get_one::<String>("REASON"))
        .cloned();
    render_line_control(
        crate::command_client::execute_typed(
            repository,
            contract,
            CommandKind::SteerLineControl,
            Vec::new(),
            &SteerLineControl {
                input,
                action,
                body,
            },
        ),
        &key,
    )
}

/// The control's effect on `key`'s line, then any body fate outside it.
fn render_line_control(outcome: Outcome<CommandAnswer>, key: &str) -> Outcome<CommandAnswer> {
    let Outcome::Reply(Reply {
        position,
        body: CommandAnswer::SteerLine(answer),
    }) = outcome
    else {
        return outcome;
    };
    let mut text = answer.effect.effect(key);
    match answer.body {
        Some(wire::outbox::BodyFate::Sent) | None => {}
        Some(wire::outbox::BodyFate::Unsent) => {
            text.push_str("; its body was not sent");
        }
    }
    Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Text(TextAnswer { text }),
    })
}

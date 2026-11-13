//! Bounded, relevance-ordered lane list projection.

use base64::Engine as _;
use std::collections::BTreeMap;
use std::fmt;

use clap::ArgMatches;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::intent::{BoardAnswer, BoardRead, CommandAnswer};
use wire::{Actor, CanonicalUuid, CommandKind, OccupantAddress, Outcome, Reply, SeatAddress};

use crate::cli::Contract;
use crate::command_client;
use crate::{command_refusal as refusal, numeric_option, protocol_fault};

pub(super) fn run(
    repository: &Repository,
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<LanePage> {
    let options = match LaneListOptions::from_matches(matches) {
        Ok(options) => options,
        Err(outcome) => return outcome,
    };
    match command_client::execute_typed(
        command_client::Host {
            repository,
            ambient,
        },
        contract,
        CommandKind::BoardRead,
        Vec::new(),
        // Active claims sit only on open tasks; terminal history is not read.
        &BoardRead::Open {
            recent_finished: 0,
            after: options.after.and_then(LaneListAfter::code),
        },
    ) {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Board(board),
        }) => {
            if let Some(LaneListAfter::Code(code)) = options.after
                && let Some(selection) = board.selected
                && let Some(notice) = crate::lane_selector::staleness(code, &selection)
            {
                eprintln!("{notice}");
            }
            match lane_list(&board, &options) {
                Ok(page) => Outcome::Reply(Reply {
                    position,
                    body: page,
                }),
                Err(outcome) => outcome,
            }
        }
        Outcome::Reply(_) => protocol_fault(
            "lane::list-answer-invalid",
            "lane list board read returned another answer shape",
        ),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

/// One bounded page of lanes, each with the peer address it offers.
///
/// Text form speaks one line per lane, or names the command that seats the
/// first lane; `--json` keeps only the typed rows.
#[derive(Debug, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct LanePage(wire::output::Bounded<LaneListRow>);

impl fmt::Display for LanePage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.rows.is_empty() && self.0.omission.is_none() {
            return formatter.write_str("no lanes are seated; next: spice lane add [WORKTREE]");
        }
        let mut lines = self
            .0
            .rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if let Some(omission) = &self.0.omission {
            lines.push(omission.to_string());
        }
        formatter.write_str(&lines.join("\n"))
    }
}

const LANE_CURSOR_PREFIX: &str = "lane.v1.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LaneListAfter {
    Code(OccupantAddress),
    Cursor(CanonicalUuid),
}

impl LaneListAfter {
    fn parse(written: &str) -> Result<Self, String> {
        if let Ok(code) = written.parse::<OccupantAddress>() {
            return Ok(Self::Code(code));
        }
        let Some(encoded) = written.strip_prefix(LANE_CURSOR_PREFIX) else {
            return Err("name an occupant code or use a cursor emitted by lane list".to_owned());
        };
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "lane list cursor is not canonical URL-safe base64")?;
        let canonical = format!(
            "{LANE_CURSOR_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
        );
        if canonical != written {
            return Err("lane list cursor is not canonical URL-safe base64".to_owned());
        }
        let lane = uuid::Uuid::from_slice(&bytes)
            .map_err(|_| "lane list cursor does not carry one lane identity")?;
        Ok(Self::Cursor(CanonicalUuid::new(lane)))
    }

    const fn code(self) -> Option<OccupantAddress> {
        match self {
            Self::Code(code) => Some(code),
            Self::Cursor(_) => None,
        }
    }
}

#[derive(Clone, Default)]
struct LaneListOptions {
    limit: Option<usize>,
    max_bytes: Option<usize>,
    after: Option<LaneListAfter>,
}

impl LaneListOptions {
    fn from_matches(matches: &ArgMatches) -> Result<Self, Outcome<LanePage>> {
        let limit = numeric_option(matches, "limit", "lane::list-limit")?;
        let max_bytes = numeric_option(matches, "max-bytes", "lane::list-max-bytes")?;
        let _ = lane_list_budget(limit, max_bytes)?;
        let after = matches
            .get_one::<String>("after")
            .map(|written| {
                LaneListAfter::parse(written)
                    .map_err(|error| refusal("lane::list-after", Some(written.clone()), &error))
            })
            .transpose()?;
        Ok(Self {
            limit,
            max_bytes,
            after,
        })
    }

    fn budget(&self) -> wire::output::Budget {
        lane_list_budget(self.limit, self.max_bytes)
            .expect("CLI parsing and test fixtures admit lane list limits")
    }
}

#[derive(Clone, Copy)]
struct LaneListCandidate<'a> {
    lane: CanonicalUuid,
    team: CanonicalUuid,
    name: Option<&'a str>,
    seated: bool,
    claim: Option<&'a str>,
    address: &'a SeatAddress,
    code: Option<OccupantAddress>,
}

/// One lane's public selector code, current team, display name, claim, and
/// exact peer address. The canonical lane UUID stays internal for sorting.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct LaneListRow {
    #[serde(skip)]
    lane: CanonicalUuid,
    team: CanonicalUuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    seated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    claim: Option<String>,
    address: SeatAddress,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<OccupantAddress>,
}

impl fmt::Display for LaneListRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lane code=")?;
        match self.code {
            Some(code) => code.fmt(formatter)?,
            None => formatter.write_str("-")?,
        }
        write!(formatter, " team={}", self.team)?;
        if let Some(name) = &self.name {
            write!(formatter, " name={name:?}")?;
        }
        write!(
            formatter,
            " seated={} claim={} address={}",
            self.seated,
            self.claim.as_deref().unwrap_or("-"),
            self.address
        )
    }
}

fn lane_list(
    board: &BoardAnswer,
    options: &LaneListOptions,
) -> Result<LanePage, Outcome<LanePage>> {
    let actor = match board.principal {
        Actor::Lane(lane) => Some(lane),
        Actor::Spice | Actor::Principal(_) => None,
    };
    let after = match options.after {
        None => None,
        Some(LaneListAfter::Cursor(lane)) => Some(lane),
        Some(LaneListAfter::Code(_)) => Some(
            board
                .selected
                .ok_or_else(|| {
                    protocol_fault(
                        "lane::selection-missing",
                        "the resident answered a lane list code cursor without its selection",
                    )
                })?
                .lane,
        ),
    };
    lane_list_page(lane_list_candidates(board)?, actor, after, options).map(LanePage)
}

fn lane_list_candidates(
    board: &BoardAnswer,
) -> Result<Vec<LaneListCandidate<'_>>, Outcome<LanePage>> {
    let mut claims = BTreeMap::new();
    for task in &board.tasks {
        for claim in task.active_claims().map(|held| &held.claim) {
            if claims
                .insert(claim.body.lane, task.statement.body.handle.as_str())
                .is_some()
            {
                return Err(protocol_fault(
                    "lane::active-claim-ambiguous",
                    "one lane has more than one active claim in the board snapshot",
                ));
            }
        }
    }
    board
        .residency
        .body
        .lanes
        .iter()
        .map(|row| {
            let address = board.addresses.get(&row.lane).ok_or_else(|| {
                protocol_fault(
                    "lane::list-address-missing",
                    "the board snapshot names no seat address for a residency lane",
                )
            })?;
            Ok(LaneListCandidate {
                lane: row.lane,
                team: row.team,
                name: row.name.as_deref(),
                seated: row.disposition == "occupied" && row.occupant.is_some(),
                claim: claims.get(&row.lane).copied(),
                address,
                code: board.codes.get(&row.lane).copied(),
            })
        })
        .collect()
}

fn lane_list_page(
    mut candidates: Vec<LaneListCandidate<'_>>,
    actor: Option<CanonicalUuid>,
    after: Option<CanonicalUuid>,
    options: &LaneListOptions,
) -> Result<wire::output::Bounded<LaneListRow>, Outcome<LanePage>> {
    candidates.sort_by(|left, right| {
        lane_relevance(*left, actor)
            .cmp(&lane_relevance(*right, actor))
            .then_with(|| left.lane.cmp(&right.lane))
    });
    if let Some(after) = after {
        let Some(index) = candidates.iter().position(|row| row.lane == after) else {
            return Err(refusal(
                "lane::list-after-unknown",
                Some(after.to_string()),
                "name a lane present in the current residency table",
            ));
        };
        candidates = candidates.split_off(index);
    }

    let mut admission = wire::output::Admission::new(options.budget());
    for (index, candidate) in candidates.iter().enumerate() {
        if admission.is_closed() {
            admission.withhold(candidates.len() - index);
            break;
        }
        admission
            .offer(LaneListRow {
                lane: candidate.lane,
                team: candidate.team,
                name: candidate.name.map(str::to_owned),
                seated: candidate.seated,
                claim: candidate.claim.map(str::to_owned),
                address: candidate.address.clone(),
                code: candidate.code,
            })
            .expect("a lane list row always serializes");
    }
    Ok(admission.finish(|resume| lane_list_continuation(options, resume)))
}

fn lane_relevance(candidate: LaneListCandidate<'_>, actor: Option<CanonicalUuid>) -> u8 {
    if Some(candidate.lane) == actor {
        0
    } else if candidate.seated || candidate.claim.is_some() {
        1
    } else {
        2
    }
}

fn lane_list_continuation(
    options: &LaneListOptions,
    resume: wire::output::Resume<'_, LaneListRow>,
) -> wire::SpiceCommand {
    let (row, widened) = match resume {
        wire::output::Resume::At(row) => (row, None),
        wire::output::Resume::Widened { record, bytes } => (record, Some(bytes)),
        wire::output::Resume::Export(row) => (row, Some(wire::output::INTERACTIVE.maximum().bytes)),
    };
    let after = row.code.map_or_else(
        || {
            format!(
                "{LANE_CURSOR_PREFIX}{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(row.lane.get().as_bytes())
            )
        },
        |code| code.to_string(),
    );
    let limits = options.budget().resume();
    wire::SpiceCommand::new(
        "lane",
        [
            "list".to_owned(),
            "--limit".to_owned(),
            limits.rows.to_string(),
            "--max-bytes".to_owned(),
            widened.unwrap_or(limits.bytes).to_string(),
            "--after".to_owned(),
            after,
        ],
    )
    .expect("lane list continuation has structural arguments")
}

fn lane_list_budget(
    rows: Option<usize>,
    bytes: Option<usize>,
) -> Result<wire::output::Budget, Outcome<LanePage>> {
    wire::output::INTERACTIVE
        .budget(wire::output::Request {
            rows,
            bytes,
            text: None,
        })
        .map_err(|error| {
            let detail = error.to_string();
            refusal("lane::list-limits", Some(detail.clone()), detail)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(value: u128) -> CanonicalUuid {
        CanonicalUuid::new(uuid::Uuid::from_u128(value))
    }

    static VACANT: SeatAddress = SeatAddress::Vacant;

    fn candidates() -> Vec<LaneListCandidate<'static>> {
        (1..=206)
            .map(|value| LaneListCandidate {
                lane: uuid(value),
                team: uuid(1_000 + value),
                name: None,
                seated: value % 2 == 0,
                claim: (value % 5 == 0).then_some("TASK-claim"),
                address: &VACANT,
                code: Some(OccupantAddress::new(
                    wire::Seq::new(value as u64).expect("fixture code position is nonzero"),
                )),
            })
            .collect()
    }

    #[test]
    fn hundreds_of_lanes_page_actor_then_active_rows_without_gaps() {
        let actor = uuid(206);
        let first = lane_list_page(candidates(), Some(actor), None, &LaneListOptions::default())
            .expect("default lane page renders");
        assert_eq!(first.rows.len(), 20);
        assert_eq!(first.rows[0].lane, actor, "actor lane is never displaced");
        assert_eq!(first.rows[1].lane, uuid(2));
        assert_eq!(first.rows[2].lane, uuid(4));
        assert_eq!(first.rows[3].lane, uuid(5));
        let omission = first.omission.expect("hundreds of lanes need another page");
        assert_eq!(
            (omission.population, omission.shown, omission.withheld),
            (206, 20, 186)
        );
        assert_eq!(
            omission.continuation.arguments()[..5],
            ["lane", "list", "--limit", "20", "--max-bytes"]
        );

        let mut options = LaneListOptions {
            limit: Some(17),
            max_bytes: Some(wire::output::INTERACTIVE.maximum().bytes),
            after: None,
        };
        let mut seen = Vec::new();
        loop {
            let candidates = candidates();
            let after = options.after.map(|after| match after {
                LaneListAfter::Code(code) => {
                    candidates
                        .iter()
                        .find(|row| row.code == Some(code))
                        .expect("continuation code stays in the residency table")
                        .lane
                }
                LaneListAfter::Cursor(lane) => lane,
            });
            let page = lane_list_page(candidates, Some(actor), after, &options)
                .expect("continuation cursor stays in the residency table");
            seen.extend(page.rows.into_iter().map(|row| row.lane));
            let Some(omission) = page.omission else {
                break;
            };
            let arguments = omission.continuation.arguments();
            options.after = Some(
                LaneListAfter::parse(
                    arguments
                        .last()
                        .expect("continuation carries a public selector"),
                )
                .expect("continuation selector is canonical"),
            );
        }

        let mut expected = vec![actor];
        expected.extend(
            (1..206)
                .filter(|value| value % 2 == 0 || value % 5 == 0)
                .map(uuid),
        );
        expected.extend(
            (1..206)
                .filter(|value| value % 2 != 0 && value % 5 != 0)
                .map(uuid),
        );
        assert_eq!(seen, expected);
    }

    #[test]
    fn lane_rows_follow_team_authority_without_losing_seat_state() {
        let occupied_lane = uuid(301);
        let vacant_lane = uuid(302);
        let first_team = uuid(401);
        let moved_team = uuid(402);
        let vacant_team = uuid(403);
        let occupied_address = SeatAddress::Seated {
            occupant: uuid(501),
            address: wire::OccupantAddress::new(
                wire::Seq::new(52).expect("fixture address position is nonzero"),
            ),
        };
        let vacant_code = wire::OccupantAddress::new(
            wire::Seq::new(53).expect("fixture address position is nonzero"),
        );
        let page = |team| {
            LanePage(
                lane_list_page(
                    vec![
                        LaneListCandidate {
                            lane: occupied_lane,
                            team,
                            name: Some("occupied"),
                            seated: true,
                            claim: Some("TASK-active"),
                            address: &occupied_address,
                            code: occupied_address.address(),
                        },
                        LaneListCandidate {
                            lane: vacant_lane,
                            team: vacant_team,
                            name: Some("vacant"),
                            seated: false,
                            claim: None,
                            address: &VACANT,
                            code: Some(vacant_code),
                        },
                    ],
                    None,
                    None,
                    &LaneListOptions::default(),
                )
                .expect("lane page renders"),
            )
        };

        let before = page(first_team);
        let text = before.to_string();
        assert!(text.contains(&format!(
            "lane code={} team={first_team} name=\"occupied\" seated=true claim=TASK-active",
            occupied_address.address().expect("occupied address is proved")
        )));
        assert!(text.contains(&format!(
            "lane code={vacant_code} team={vacant_team} name=\"vacant\" seated=false claim=- \
             address=unavailable:vacant"
        )));
        assert!(
            !text.contains(&occupied_lane.to_string()) && !text.contains(&vacant_lane.to_string()),
            "public rows do not expose canonical lane UUIDs"
        );
        let json = serde_json::to_value(&before).expect("lane page serializes");
        assert!(json["rows"][0].get("lane").is_none());
        assert!(json["rows"][1].get("lane").is_none());
        assert_eq!(json["rows"][0]["team"], first_team.to_string());
        assert_eq!(json["rows"][0]["address"]["state"], "seated");
        assert_eq!(
            json["rows"][0]["code"],
            occupied_address
                .address()
                .expect("occupied address is proved")
                .to_string()
        );
        assert_eq!(json["rows"][1]["team"], vacant_team.to_string());
        assert_eq!(json["rows"][1]["address"]["state"], "vacant");
        assert_eq!(json["rows"][1]["code"], vacant_code.to_string());

        let after = page(moved_team);
        let before_row = &before.0.rows[0];
        let after_row = &after.0.rows[0];
        assert_eq!(after_row.team, moved_team);
        assert_eq!(after_row.lane, before_row.lane);
        assert_eq!(after_row.name, before_row.name);
        assert_eq!(after_row.seated, before_row.seated);
        assert_eq!(after_row.claim, before_row.claim);
        assert_eq!(after_row.address, before_row.address);
        assert_eq!(after_row.code, before_row.code);
    }

    #[test]
    fn a_lane_without_a_birth_code_uses_an_opaque_continuation_cursor() {
        let coded = LaneListCandidate {
            lane: uuid(1),
            team: uuid(101),
            name: None,
            seated: false,
            claim: None,
            address: &VACANT,
            code: Some(OccupantAddress::new(
                wire::Seq::new(1).expect("fixture code position is nonzero"),
            )),
        };
        let unstarted = LaneListCandidate {
            lane: uuid(2),
            team: uuid(102),
            name: None,
            seated: false,
            claim: None,
            address: &VACANT,
            code: None,
        };
        let options = LaneListOptions {
            limit: Some(1),
            max_bytes: Some(wire::output::INTERACTIVE.maximum().bytes),
            after: None,
        };
        let page = lane_list_page(vec![coded, unstarted], None, None, &options)
            .expect("bounded lane page renders");
        let continuation = page
            .omission
            .expect("the unstarted lane is withheld")
            .continuation;
        let written = continuation
            .arguments()
            .last()
            .expect("continuation carries a selector");
        assert_eq!(
            LaneListAfter::parse(written),
            Ok(LaneListAfter::Cursor(unstarted.lane))
        );
        assert!(!written.contains(&unstarted.lane.to_string()));
    }

    #[test]
    fn lane_list_parser_owns_limits_and_cursor() {
        let after =
            OccupantAddress::new(wire::Seq::new(17).expect("fixture code position is nonzero"));
        let matches = crate::cli::command()
            .try_get_matches_from([
                "spice",
                "lane",
                "list",
                "--limit",
                "23",
                "--max-bytes",
                "4096",
                "--after",
                &after.to_string(),
            ])
            .expect("bounded lane list parses");
        let (_, lane) = matches.subcommand().expect("lane command");
        let (_, list) = lane.subcommand().expect("lane list command");
        let options = LaneListOptions::from_matches(list).expect("lane list limits are admitted");
        assert_eq!(options.limit, Some(23));
        assert_eq!(options.max_bytes, Some(4096));
        assert_eq!(options.after, Some(LaneListAfter::Code(after)));
        assert!(LaneListAfter::parse(&uuid(17).to_string()).is_err());

        let too_many = crate::cli::command()
            .try_get_matches_from(["spice", "lane", "list", "--limit", "501"])
            .expect("syntax accepts a numeric limit for semantic admission");
        let (_, lane) = too_many.subcommand().expect("lane command");
        let (_, list) = lane.subcommand().expect("lane list command");
        assert!(LaneListOptions::from_matches(list).is_err());
    }
}

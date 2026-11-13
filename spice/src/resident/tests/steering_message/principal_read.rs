//! A principal reads its pending inputs and a room's routes through the same
//! steering heads and line standing a lane does, attributed by its immutable
//! principal code, and a CLI control names exactly one principal input.

use serde_json::{Value, json};
use wire::principal::PrincipalCode;

use super::*;

const ROOM: &str = "dVrD244Jd";

impl Lanes {
    /// Author `text` on `lane`, account it, and hear its newest packet.
    fn publish_text(&self, lane: u8, text: &str) {
        self.author(lane, text);
        self.account(lane);
        let (_, outcomes) = self.hear_newest(lane);
        let [outcome] = outcomes.try_into().expect("one packet per block");
        outcome.expect("the packet publishes");
    }

    /// `spice room inbox WORDS...` as the local owner reads it.
    fn principal_inbox(&self, words: &[&str]) -> Outcome<crate::room_command::inbox::RoomInbox> {
        let matches = crate::cli::command()
            .try_get_matches_from(["spice", "room", "inbox"].iter().chain(words))
            .expect("room inbox parses");
        let contract = crate::cli::selected_contract(&matches).expect("room inbox is registered");
        let journal =
            journal::JournalReader::open(&self.paths.journal).expect("the journal opens to read");
        crate::room_command::inbox::answer(
            &journal,
            crate::cli::selected_matches(&matches),
            contract.path,
        )
    }

    /// Every pending input key the local owner's inbox lists, following each
    /// page's continuation from `words`.
    fn inbox_keys(&self, words: &[&str]) -> Vec<String> {
        let mut words = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let mut keys = Vec::new();
        for _ in 0..8 {
            let page = match self
                .principal_inbox(&words.iter().map(String::as_str).collect::<Vec<_>>())
            {
                Outcome::Reply(reply) => serde_json::to_value(&reply.body).expect("inbox encodes"),
                other => panic!("room inbox answered {other:?}"),
            };
            keys.extend(
                page["page"]["rows"]
                    .as_array()
                    .expect("rows")
                    .iter()
                    .map(|row| row["key"].as_str().expect("input key").to_owned()),
            );
            let continuation = &page["page"]["omission"]["continuation"];
            if continuation.is_null() {
                return keys;
            }
            words = serde_json::from_value::<Vec<String>>(continuation.clone())
                .expect("a continuation is one argument list")
                .split_off(2);
        }
        panic!("paging did not finish: {keys:?}");
    }

    /// The immutable code of the principal `actor` names.
    fn code(&self, actor: &Actor) -> PrincipalCode {
        let roles = self
            .bus
            .read(|read| read.principal_roles())
            .expect("principal roles read");
        PrincipalCode::of(roles.head(actor).expect("the principal is born"))
    }

    /// The pending principal input whose concern cites direction `key`.
    fn input_citing(&self, key: &str) -> Value {
        let Outcome::Reply(reply) = self.principal_inbox(&[]) else {
            panic!("room inbox refused");
        };
        let page = serde_json::to_value(&reply.body).expect("inbox encodes");
        page["page"]["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["concerns"]["key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("no pending input cites {key}: {page}"))
    }
}

/// Each `#KEY` concern is one pending input of the principal it reaches, listed
/// by its own key with its sender, text, and the direction it cites, paged
/// whole and resumably; a cursor naming no input of that principal refuses,
/// and reading writes nothing.
#[test]
fn a_principal_route_is_one_pending_input_its_principal_pages_by_key() {
    let lanes = Lanes::new("principal-inbox");
    let mut keys = Vec::new();
    for (id, ask) in [(20, "first"), (21, "second"), (22, "third")] {
        let direction = lanes.direct(SENDER, id, &format!("direction {ask}"));
        lanes.publish_text(SENDER, &format!("SYN #{direction}: {ask} ask"));
        let input = lanes.input_citing(&direction);
        assert_eq!(
            (
                &input["sender"],
                &input["text"]["text"],
                &input["concerns"]["text"]["text"],
            ),
            (
                &json!(Actor::Lane(test_uuid(SENDER)).to_string()),
                &json!(format!("{ask} ask")),
                &json!(format!("direction {ask}")),
            ),
        );
        keys.push(input["key"].as_str().expect("input key").to_owned());
    }
    let lane_key = lanes.direct(FIRST, 30, "a lane's own input");
    let facts = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .len();

    assert_eq!(lanes.inbox_keys(&[]), keys);
    assert_eq!(lanes.inbox_keys(&["--limit", "2"]), keys, "pages resume");
    assert_eq!(lanes.inbox_keys(&["--after", &keys[1]]), keys[1..]);
    for cursor in ["dVwg6Zqlf", lane_key.as_str()] {
        let Outcome::Refusal(refusal) = lanes.principal_inbox(&["--after", cursor]) else {
            panic!("a cursor naming no principal input of the reader refuses: {cursor}");
        };
        assert!(refusal.reason().ends_with("cursor-invalid"), "{refusal:?}");
    }
    assert_eq!(
        lanes
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("facts read")
            .facts()
            .len(),
        facts,
        "reading writes nothing"
    );
    lanes.finish();
}

/// A packet that reaches the principal and a lane in one room reads back as
/// two routes of one shape: each names its recipient, key, disposition, and
/// line standing, a lane's with its delivery admission, and text names the
/// principal by its immutable code.
#[test]
fn room_history_projects_a_principal_route_as_it_projects_a_lane_route() {
    let lanes = Lanes::new("principal-history");
    let room = lanes.room(ROOM);
    let first = lanes.address(FIRST);
    let direction = lanes.direct(SENDER, 20, "ship when green");
    lanes.publish_text(SENDER, &format!("SYN #{direction} {first}#{room}: shipped"));
    let principal = lanes.input_citing(&direction);
    let [lane] = lanes.pending(FIRST).try_into().expect("one lane input");

    let page = lanes.room_page(&[room.as_str()]);
    let rows = page["page"]["rows"].as_array().expect("rows");
    let [row] = &rows[..] else {
        panic!("one publication: {page}");
    };
    let route = |recipient: String, key: &str, admission: Option<&str>| {
        let mut line = json!({"root": key, "body": "open"});
        if let Some(admission) = admission {
            line["admission"] = json!(admission);
        }
        json!({
            "state": "steering",
            "key": key,
            "recipient": recipient,
            "disposition": "live",
            "line": line,
        })
    };
    let receipts = row["directed"]
        .as_array()
        .expect("directed routes")
        .iter()
        .map(|route| route["receipt"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        receipts,
        [
            route(
                Actor::Lane(test_uuid(FIRST)).to_string(),
                &lane.key,
                Some("active")
            ),
            route(
                lanes.bootstrap().to_string(),
                principal["key"].as_str().expect("input key"),
                None
            ),
        ]
    );

    let Outcome::Reply(reply) = lanes.history(&[room.as_str()]) else {
        panic!("room history refused");
    };
    let text = reply.body.to_string();
    let code = lanes.code(&lanes.bootstrap());
    assert!(
        text.contains(&format!(
            "receipt to={code} key={}",
            principal["key"].as_str().expect("key")
        )),
        "{text}"
    );
    lanes.finish();
}

/// A principal's FIN settles its exact input: the input leaves its inbox, and
/// the room shows the route settled with the body that answered it exactly
/// once.
#[test]
fn a_settled_principal_route_leaves_the_inbox_and_shows_its_body_once() {
    let lanes = Lanes::new("principal-settled");
    let room = lanes.room(ROOM);
    let first = lanes.address(FIRST);
    let direction = lanes.direct(SENDER, 20, "ship when green");
    lanes.publish_text(SENDER, &format!("SYN #{direction} {first}#{room}: shipped"));
    let input = lanes.input_citing(&direction);
    let key = input["key"].as_str().expect("input key").to_owned();
    let principal = lanes.bootstrap().principal().expect("bootstrap principal");
    let request = wire::intent::SteerLineControl {
        input: wire::rooms::PrincipalInputRef {
            directive: input["directive"]
                .as_str()
                .expect("directive")
                .parse()
                .expect("directive parses"),
            principal,
        },
        action: wire::intent::SteerLineAction::Fin,
        body: Some("merged, thanks".to_owned()),
    };
    let roles = lanes
        .bus
        .read(|read| read.principal_roles())
        .expect("principal roles read");
    let command = Command {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(40),
        epoch: lanes.epoch,
        kind: CommandKind::SteerLineControl,
        expected: Vec::new(),
        body: serde_json::to_value(&request).expect("line control encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::Principal(principal),
            epoch: lanes.epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &roles,
    )
    .expect("line control admits");
    let outcome = steering::line_control(
        &lanes.bus,
        lanes.epoch,
        &steer::Config::default(),
        &command,
        &request,
        &mut CommandEffects::default(),
    );
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");

    assert!(lanes.inbox_keys(&[]).is_empty(), "a settled input leaves");
    let page = lanes.room_page(&[room.as_str()]);
    let receipt = page["page"]["rows"][0]["directed"]
        .as_array()
        .expect("directed routes")
        .iter()
        .map(|route| &route["receipt"])
        .find(|receipt| receipt["key"] == key.as_str())
        .expect("the principal route reads")
        .clone();
    assert_ne!(receipt["disposition"], "live", "{receipt}");
    assert_eq!(receipt["answer"]["text"], "merged, thanks", "{receipt}");
    let Outcome::Reply(reply) = lanes.history(&[room.as_str()]) else {
        panic!("room history refused");
    };
    let text = reply.body.to_string();
    assert_eq!(text.matches("| merged, thanks").count(), 1, "{text}");
    lanes.finish();
}

/// A CLI control resolves its key to the exact principal input it names: a
/// key no input carries and a lane's own input each refuse before anything is
/// submitted.
#[test]
fn a_principal_control_names_exactly_one_principal_input() {
    let lanes = Lanes::new("principal-route");
    let direction = lanes.direct(SENDER, 20, "ship when green");
    lanes.publish_text(SENDER, &format!("SYN #{direction}: ship it?"));
    let input = lanes.input_citing(&direction);
    let key = input["key"].as_str().expect("input key");
    let repository = Repository::discover(&lanes.root).expect("fixture repository resolves");

    let resolved = crate::room_command::input::resolve::<()>(&repository, key)
        .expect("a principal input resolves");
    assert_eq!(
        (
            Actor::Principal(resolved.principal),
            resolved.directive.to_string()
        ),
        (
            lanes.bootstrap(),
            input["directive"].as_str().expect("directive").to_owned()
        )
    );
    for (key, reason) in [
        ("dVwg6Zqlf".to_owned(), "room::input-unknown"),
        (direction, "room::input-not-principal"),
    ] {
        let Err(Outcome::Refusal(refusal)) =
            crate::room_command::input::resolve::<()>(&repository, &key)
        else {
            panic!("{key} resolved to a principal input");
        };
        assert_eq!(refusal.reason(), reason, "{key}");
    }
    lanes.finish();
}

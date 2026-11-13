//! A room reads back as its publications in journal order, each projected
//! from the authority that owns its bytes, paged whole and resumable, and
//! the read itself changes nothing.

use serde_json::{Value, json};
use wire::rooms::{
    RoomAuthorityStamp, RoomPublicationContent, RoomPublished, RoomSourceUnavailable,
};

use super::*;
use crate::room_command::RoomHistory;

const ROOM: &str = "dVrD244Jd";

impl Lanes {
    /// Read `spice room history` with `words` after its command path.
    pub(super) fn history(&self, words: &[&str]) -> Outcome<RoomHistory> {
        let matches = crate::cli::command()
            .try_get_matches_from(["spice", "room", "history"].iter().chain(words))
            .expect("room history parses");
        let contract = crate::cli::selected_contract(&matches).expect("room history is registered");
        let journal =
            journal::JournalReader::open(&self.paths.journal).expect("the journal opens to read");
        crate::room_command::answer(
            &journal,
            crate::room_command::SourceOwners {
                tape: &self.paths.tape,
                artifacts: &self.paths.artifacts(),
                git: &self.paths.common,
            },
            crate::cli::selected_matches(&matches),
            contract.path,
        )
    }

    /// One page as its JSON reply body.
    pub(super) fn room_page(&self, words: &[&str]) -> Value {
        match self.history(words) {
            Outcome::Reply(reply) => serde_json::to_value(&reply.body).expect("history encodes"),
            other => panic!("room history answered {other:?}"),
        }
    }

    fn room_refusal(&self, words: &[&str]) -> String {
        match self.history(words) {
            Outcome::Refusal(refusal) => refusal.reason().to_owned(),
            other => panic!("room history did not refuse: {other:?}"),
        }
    }

    /// Every publication's text, following each page's continuation from
    /// `words` until the room is exhausted.
    fn every_text(&self, words: &[String]) -> Vec<String> {
        let mut words = words.to_vec();
        let mut texts = Vec::new();
        for _ in 0..16 {
            let page = self.room_page(&words.iter().map(String::as_str).collect::<Vec<_>>());
            texts.extend(
                page["page"]["rows"]
                    .as_array()
                    .expect("rows")
                    .iter()
                    .map(|row| {
                        row["content"]["text"]
                            .as_str()
                            .expect("an operator post reads as text")
                            .to_owned()
                    }),
            );
            let continuation = &page["page"]["omission"]["continuation"];
            if continuation.is_null() {
                return texts;
            }
            words = serde_json::from_value::<Vec<String>>(continuation.clone())
                .expect("a continuation is one argument list")
                .split_off(2);
        }
        panic!("paging did not finish: {texts:?}");
    }

    /// The operator publishes `text` in `room`.
    pub(super) fn post(
        &self,
        room: &wire::TaskHandle,
        micros: i64,
        text: &str,
    ) -> RoomAuthorityStamp {
        let publication = RoomAuthorityStamp::for_micros(1_760_000_000_000_000 + micros)
            .expect("fixture publication stamp");
        let mut write =
            write(self.bootstrap(), self.epoch, Producer::CommandDoor).expect("post write starts");
        write
            .fact(
                RoomPublished {
                    room: room.born().expect("fixture handle carries its birth"),
                    publication: publication.clone(),
                    content: RoomPublicationContent::Authored {
                        text: text.to_owned(),
                    },
                },
                FactCause::Root,
            )
            .expect("the post is valid");
        commit(&self.bus, write).expect("the post commits");
        publication
    }

    fn fact_count(&self) -> usize {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .len()
    }
}

/// Agent packets read from their author's tape with each directed route's
/// receipt, operator posts from their fact, in journal order; another
/// spelling of the anchor reads the same room under its current handle; the
/// read writes nothing and retires nothing.
#[test]
fn room_history_projects_each_publication_from_its_owning_source() {
    let lanes = Lanes::new("room-history");
    let room = lanes.room(ROOM);
    let first = lanes.address(FIRST);
    lanes.author(
        SENDER,
        &format!("SYN #{room}: for the room\n\nSYN {first}#{room}: for first"),
    );
    lanes.account(SENDER);
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.post(&room, 1, "from the operator");
    let facts = lanes.fact_count();
    let [(_, message)] = lanes.messages().try_into().expect("one directed delivery");

    let page = lanes.room_page(&[room.as_str()]);
    assert_eq!(page["room"], room.as_str());
    let rows = page["page"]["rows"].as_array().expect("rows");
    let texts = rows
        .iter()
        .map(|row| row["content"]["text"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        [
            Some("for the room"),
            Some("for first"),
            Some("from the operator")
        ]
    );
    let agent = Actor::Lane(test_uuid(SENDER)).to_string();
    let author = test_uuid(SENDER + 1).to_string();
    assert_eq!(
        (
            &rows[0]["actor"],
            &rows[0]["author"],
            &rows[0]["directed"],
            &rows[0]["closed"],
        ),
        (&json!(agent), &json!(author), &Value::Null, &json!(true)),
        "a room-only packet owes no receipt and is closed as recorded"
    );
    assert_eq!(
        rows[1]["closed"],
        Value::Null,
        "a packet that also reached a lane is not closed by its room record"
    );
    assert_eq!(
        rows[1]["directed"],
        json!([{
            "directive": message.directive.to_string(),
            "receipt": {
                "state": "steering",
                "key": message.key,
                "recipient": Actor::Lane(test_uuid(FIRST)).to_string(),
                "disposition": "live",
                "line": {"root": message.key, "body": "open", "admission": "active"},
            },
        }])
    );
    assert_eq!(
        (&rows[2]["actor"], &rows[2]["author"]),
        (&json!(lanes.bootstrap().to_string()), &Value::Null),
        "an operator post names the bootstrap principal"
    );
    assert!(page["page"]["omission"].is_null());

    let respelled = format!("OLD-{ROOM}");
    assert_eq!(lanes.room_page(&[&respelled]), page);
    assert_eq!(lanes.fact_count(), facts, "reading writes nothing");
    assert_eq!(lanes.pending(FIRST).len(), 1, "reading retires nothing");
    lanes.finish();
}

/// A packet whose tape no longer holds it still reads, as unavailable with
/// its source and reason.
#[test]
fn a_packet_whose_tape_is_gone_reads_as_unavailable() {
    let lanes = Lanes::new("room-history-missing");
    let room = lanes.room(ROOM);
    lanes.author(SENDER, &format!("SYN #{room}: kept"));
    lanes.account(SENDER);
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    fs::remove_dir_all(lanes.paths.lane_tape(test_uuid(SENDER))).expect("the tape is removed");

    let page = lanes.room_page(&[room.as_str()]);
    let content = &page["page"]["rows"][0]["content"];
    assert_eq!(content["state"], "unavailable");
    assert_eq!(content["source"]["kind"], "transcript");
    assert_eq!(
        content["unavailable"],
        serde_json::to_value(RoomSourceUnavailable::Missing {}).expect("reason encodes")
    );
    lanes.finish();
}

/// Row-bounded and byte-bounded pages each visit every publication once, in
/// order and whole, including one larger than the requested byte bound.
#[test]
fn room_pages_resume_without_duplicating_or_cutting_a_publication() {
    let lanes = Lanes::new("room-history-pages");
    let room = lanes.room(ROOM);
    let texts = (0..5)
        .map(|index| format!("{index}:{}", "x".repeat(400 * index)))
        .collect::<Vec<_>>();
    for (micros, text) in (1..).zip(&texts) {
        lanes.post(&room, micros, text);
    }

    for bound in [["--limit", "2"], ["--max-bytes", "700"]] {
        let words = [room.as_str(), bound[0], bound[1]].map(str::to_owned);
        assert_eq!(lanes.every_text(&words), texts, "paged by {bound:?}");
    }
    lanes.finish();
}

/// A handle that names no task, a task with no record, and a cursor that
/// names no publication of this room each refuse rather than read empty.
#[test]
fn malformed_unknown_and_stale_reads_refuse() {
    let lanes = Lanes::new("room-history-refusals");
    let room = lanes.room(ROOM);
    let other = lanes.room("dVrD244Jf");
    let foreign = lanes.post(&other, 1, "elsewhere");
    lanes.post(&room, 2, "here");
    let unknown = format!(
        "ROOMS-{}",
        wire::TaskStamp::for_micros(1_234_567).expect("fixture stamp")
    );

    assert_eq!(lanes.room_refusal(&["not-a-handle"]), "room::task-invalid");
    assert_eq!(lanes.room_refusal(&[&unknown]), "room::task-unknown");
    for after in ["garbage", foreign.as_str()] {
        assert_eq!(
            lanes.room_refusal(&[room.as_str(), "--after", after]),
            "room::cursor-invalid",
            "--after {after}"
        );
    }
    lanes.finish();
}

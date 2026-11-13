//! Every delivery the driver writes is admitted by a running agent: none
//! depends on catching omp idle.

use std::sync::{Arc, Mutex, PoisonError};

use super::{Driver, FirstTurn};

const FIRST: &str = "# Spice briefing\nthe first turn";
const EARLIER: &str = "1kJK6M0Vx";
const LATER: &str = "1kJK6N3Rx";
const NOTICE: &str = "Task TASK-1 was assigned to this lane";

/// omp's prompt admission as `AgentSession` and the steering extension
/// implement it from 18.8.4 through 18.10.3. A registered extension command
/// runs in either state; an idle agent starts a turn with what it is handed;
/// while a turn runs, a prompt naming a streaming behavior joins that queue,
/// and one naming none is refused as busy.
#[derive(Default)]
struct Omp {
    busy: bool,
    steering: Vec<String>,
    asides: Vec<String>,
    follow_up: Vec<String>,
    handed: Vec<String>,
    refused: Vec<String>,
}

impl Omp {
    fn admit(&mut self, frame: &serde_json::Value) {
        if frame["type"] != "prompt" {
            return;
        }
        let message = frame["message"].as_str().expect("a prompt message");
        if let Some(notice) = message.strip_prefix("/spice-aside ") {
            let notice: serde_json::Value = serde_json::from_str(notice).expect("a notice");
            let text = notice["message"].as_str().expect("a notice message");
            if text.is_empty() {
                return;
            }
            if self.busy {
                self.asides.push(text.to_owned());
            } else {
                self.start(text);
            }
            return;
        }
        match (self.busy, frame["streamingBehavior"].as_str()) {
            (false, _) => self.start(message),
            (true, Some("steer")) => self.steering.push(message.to_owned()),
            (true, Some("followUp")) => self.follow_up.push(message.to_owned()),
            (true, _) => self.refused.push(message.to_owned()),
        }
    }

    fn start(&mut self, message: &str) {
        self.handed.push(message.to_owned());
        self.busy = true;
    }

    /// The running turn passes its step boundaries and ends; each queued
    /// follow-up then runs as its own turn.
    fn settle(&mut self) {
        self.handed.append(&mut self.steering);
        self.handed.append(&mut self.asides);
        self.handed.append(&mut self.follow_up);
        self.busy = false;
    }
}

struct Seat {
    driver: Arc<Driver<Vec<u8>>>,
    inbox: Arc<Mutex<String>>,
    written: usize,
}

impl Seat {
    fn new() -> Self {
        let inbox = Arc::new(Mutex::new(String::new()));
        let reader = Arc::clone(&inbox);
        let driver = Driver::with_readers(
            Vec::new(),
            move || {
                Some(
                    reader
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                        .into_bytes(),
                )
            },
            || None,
            |_| Err("unread".to_owned()),
            steer::Key::for_identity(crate::resident::new_uuid()),
            None,
        );
        Self {
            driver,
            inbox,
            written: 0,
        }
    }

    /// Open the seat and let omp answer the scheme registration, which
    /// releases the first turn.
    fn open(&self) {
        self.hold(FirstTurn::Fresh(FIRST.to_owned()));
        self.register();
    }

    /// Open the seat with its first turn held for the registration answer.
    fn hold(&self, first_turn: FirstTurn) {
        self.driver
            .open(b"", None, Some(first_turn))
            .expect("opening writes");
    }

    /// Answer the scheme registration, releasing the held first turn.
    fn register(&self) {
        let answer = serde_json::to_vec(&serde_json::json!({
            "id": listen::OMP_URI_SCHEMES_REQUEST_ID,
            "type": "response",
            "command": "set_host_uri_schemes",
            "success": false,
            "error": "Unknown command: set_host_uri_schemes",
        }))
        .expect("JSON");
        let locus = listen::Locus::new(0, 0, answer.len() as u64);
        let decoded =
            listen::decode(listen::Dialect::Omp, &answer, locus).expect("locus spans the line");
        let _ask = self.driver.observe(&decoded);
    }

    fn steer(&self, keys: &[&str]) {
        let mut rendered = String::from("<1kJK6Q0bx>\n");
        for key in keys {
            rendered.push_str(&format!(
                "directive=d-{key} key={key} stage=published expires=never\ndirection {key}\n"
            ));
        }
        rendered.push_str("</1kJK6Q0bx>\n");
        *self.inbox.lock().unwrap_or_else(PoisonError::into_inner) = rendered;
        self.driver.deliver_inbox();
    }

    fn notify(&self) {
        self.driver
            .background("task:TASK-1", NOTICE)
            .expect("notice writes");
    }

    /// Hand omp every frame written since the last call.
    fn flush(&mut self, omp: &mut Omp) {
        let input = self.driver.stdin.lock().expect("input locks");
        let frames = input
            .as_ref()
            .expect("input remains open")
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .skip(self.written)
            .map(|line| serde_json::from_slice(line).expect("a frame"))
            .collect::<Vec<serde_json::Value>>();
        self.written += frames.len();
        for frame in &frames {
            omp.admit(frame);
        }
    }
}

fn position(handed: &[String], marker: &str) -> usize {
    let found = handed
        .iter()
        .enumerate()
        .filter(|(_, message)| message.contains(marker))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1, "{marker} handed exactly once: {handed:?}");
    found[0]
}

#[test]
fn a_turn_starting_before_any_delivery_still_admits_each_once_in_order() {
    let deliveries: [fn(&Seat); 4] = [
        Seat::open,
        |seat| seat.steer(&[EARLIER]),
        Seat::notify,
        |seat| seat.steer(&[EARLIER, LATER]),
    ];
    for turn_starts in 0..=deliveries.len() {
        let mut seat = Seat::new();
        let mut omp = Omp::default();
        for (index, deliver) in deliveries.iter().enumerate() {
            if index == turn_starts {
                omp.busy = true;
            }
            deliver(&seat);
            seat.flush(&mut omp);
        }
        omp.settle();

        assert!(
            omp.refused.is_empty(),
            "a turn starting before delivery {turn_starts} refused {:?}",
            omp.refused
        );
        position(&omp.handed, FIRST);
        position(&omp.handed, NOTICE);
        assert!(
            position(&omp.handed, &format!("direction {EARLIER}"))
                < position(&omp.handed, &format!("direction {LATER}")),
            "steering keeps its order: {:?}",
            omp.handed
        );
    }
}

/// A seat woken for steering puts its authority and the current pending inbox
/// in the first prompt. Nothing starts ahead of that prompt, and the pending
/// item is part of it rather than a second turn.
#[test]
fn steering_pending_while_the_first_turn_is_held_joins_that_prompt_once() {
    for first_turn in [FirstTurn::Fresh, FirstTurn::Resumed] {
        let mut seat = Seat::new();
        let mut omp = Omp::default();
        seat.hold(first_turn(FIRST.to_owned()));
        seat.steer(&[EARLIER]);
        seat.flush(&mut omp);
        assert!(
            omp.handed.is_empty(),
            "nothing starts a turn ahead of the first prompt: {:?}",
            omp.handed
        );

        seat.register();
        seat.flush(&mut omp);
        omp.settle();

        assert!(omp.refused.is_empty(), "{:?}", omp.refused);
        assert_eq!(omp.handed.len(), 1, "one first turn: {:?}", omp.handed);
        let prompt = &omp.handed[0];
        let first = prompt.find(FIRST).expect("the first prompt is present");
        let direction = prompt
            .find(&format!("direction {EARLIER}"))
            .expect("pending steering is present");
        assert!(
            first < direction,
            "startup context leads steering: {prompt}"
        );
    }
}

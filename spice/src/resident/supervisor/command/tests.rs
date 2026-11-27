use super::*;

mod retune;
mod task_notices;

use listen::{Dialect, Locus};

/// The driver's command stream: one request per line, with a wake per line.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>, Arc<std::sync::Condvar>);

impl Sink {
    /// Frames the driver has written, once it has written `frames` of them.
    /// A background notice reads as an `aside` frame with its purpose and
    /// message.
    fn answered(&self, frames: usize) -> Vec<serde_json::Value> {
        let written = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (written, wait) = self
            .1
            .wait_timeout_while(written, test_support::DEADLOCK_GUARD, |written| {
                written.iter().filter(|byte| **byte == b'\n').count() < frames
            })
            .unwrap_or_else(PoisonError::into_inner);
        assert!(!wait.timed_out(), "driver answers before the shared guard");
        written
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut frame: serde_json::Value =
                    serde_json::from_slice(line).expect("one JSON request per line");
                if let Some(args) = frame["message"]
                    .as_str()
                    .and_then(|message| message.strip_prefix("/spice-aside "))
                {
                    let notice: serde_json::Value =
                        serde_json::from_str(args).expect("one SDK bridge notice");
                    frame["type"] = serde_json::json!("aside");
                    frame["purpose"] = notice["purpose"].clone();
                    frame["message"] = notice["message"].clone();
                }
                frame
            })
            .collect()
    }
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut written = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        written.extend_from_slice(bytes);
        self.1.notify_all();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A driver with nothing to read and no terminal, writing to `sink`.
fn driver(sink: &Sink) -> Arc<Driver<Sink>> {
    Driver::with_readers(
        sink.clone(),
        || None,
        || None,
        |_| Err("no reads here".to_owned()),
        steer::Key::for_identity(crate::resident::new_uuid()),
        None,
    )
}

fn decoded(bytes: &[u8]) -> Decoded {
    listen::decode(Dialect::Omp, bytes, Locus::new(1, 0, bytes.len() as u64))
        .expect("matching locus")
}

#[test]
fn unread_room_activity_holds_one_fixed_marker_until_it_is_read() {
    let sink = Sink::default();
    let unread = Arc::new(AtomicBool::new(false));
    let reads = Arc::clone(&unread);
    let driver = Driver::with_server(
        sink.clone(),
        Readers {
            unread: Box::new(move || Some(reads.load(Ordering::Relaxed))),
            ..Readers::default()
        },
        Box::new(|_, _| Err("no reads here".to_owned())),
        steer::Key::for_identity(crate::resident::new_uuid()),
        None,
    );
    driver.deliver_room_catch_up(false);
    assert!(
        sink.answered(0).is_empty(),
        "a lane with nothing unread is told nothing"
    );

    unread.store(true, Ordering::Relaxed);
    driver.deliver_room_catch_up(false);
    driver.deliver_room_catch_up(false);
    let frames = sink.answered(1);
    assert_eq!(frames.len(), 1, "later reaching commits add no marker");
    assert_eq!(frames[0]["purpose"], ROOM_CATCH_UP_PURPOSE);
    assert_eq!(frames[0]["message"], ROOM_CATCH_UP_MARKER);

    let heard = format!(
        "{}\n",
        serde_json::json!({
            "type": "message_end",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": ROOM_CATCH_UP_MARKER}],
            },
        })
    );
    assert!(driver.observe(&decoded(heard.as_bytes())).is_none());
    driver.deliver_room_catch_up(false);
    assert_eq!(
        sink.answered(1).len(),
        1,
        "a heard marker is not repeated while the activity stays unread"
    );
    driver.deliver_room_catch_up(true);
    assert_eq!(
        sink.answered(2)[1]["message"],
        ROOM_CATCH_UP_MARKER,
        "a compaction tells the lane again"
    );

    unread.store(false, Ordering::Relaxed);
    driver.deliver_room_catch_up(false);
    assert_eq!(
        sink.answered(3)[2]["message"],
        "",
        "reading the activity withdraws the marker"
    );
}

/// A short directory for test doors: Darwin bounds socket paths to 104 bytes.
fn door_directory() -> PathBuf {
    if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

fn confirmed_background_round_trip<W: Write + Send + 'static>(
    driver: Arc<Driver<W>>,
    occupant: CanonicalUuid,
    notice: crate::resident::design_input::BackgroundNotice,
) -> io::Result<bool> {
    let identity = crate::resident::new_uuid();
    let path = door_directory().join(format!("spice-background-test-{identity}.sock"));
    let door = door::Door::bind(&path, KICK_BYTES)?;
    let responder = std::thread::spawn(move || {
        let kick = door.recv(None).expect("background kick arrives");
        let Kick::BackgroundConfirmed {
            notice,
            request,
            owner,
            reply,
        } = serde_json::from_slice(&kick).expect("background kick decodes")
        else {
            panic!("confirmed delivery sends a confirmed background kick");
        };
        assert_eq!(owner, None, "ordinary notices carry no retained owner");
        let accepted = background_accepted(&driver, occupant, &notice);
        confirmed_background::answer(&reply, request, accepted);
    });
    let result = confirmed_background::send(&path, &door_directory(), notice, None);
    responder.join().expect("background responder joins");
    result
}

#[derive(Clone, Copy)]
struct RefusingSink;

impl Write for RefusingSink {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "fixture refuses the aside",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn confirmed_background_delivery_requires_the_exact_occupant_and_a_successful_write() {
    let occupant = crate::resident::new_uuid();
    let notice = crate::resident::design_input::BackgroundNotice {
        occupant,
        purpose: "design-return:DESIGN-1".to_owned(),
        text: "returned input".to_owned(),
    };
    let sink = Sink::default();
    assert!(
        confirmed_background_round_trip(driver(&sink), occupant, notice.clone())
            .expect("matching delivery answers"),
        "the exact occupant confirms a successful enqueue"
    );
    assert_eq!(sink.answered(1)[0]["message"], notice.text);

    assert!(
        !confirmed_background_round_trip(
            driver(&Sink::default()),
            crate::resident::new_uuid(),
            notice.clone(),
        )
        .expect("mismatched delivery answers"),
        "another occupant refuses the notice"
    );

    let refusing = Driver::with_readers(
        RefusingSink,
        || None,
        || None,
        |_| Err("no reads here".to_owned()),
        steer::Key::for_identity(crate::resident::new_uuid()),
        None,
    );
    assert!(
        !confirmed_background_round_trip(refusing, occupant, notice.clone())
            .expect("failed write answers"),
        "a failed driver write refuses consumption"
    );

    let missing = door_directory().join(format!(
        "spice-background-missing-{}.sock",
        crate::resident::new_uuid()
    ));
    assert!(
        confirmed_background::send(&missing, &door_directory(), notice, None).is_err(),
        "a missing supervisor door is a transport failure"
    );
}

fn missed_refusal(occupant: CanonicalUuid, packet: &str) -> MessageRefusalNotice {
    MessageRefusalNotice {
        occupant,
        packet: packet.to_owned(),
        header: "SYN 1001 1003:".to_owned(),
        line: 1,
        column: 1,
        reached: 1,
        reason: "syn::recipient-unavailable".to_owned(),
        detail: "1003 is vacant".to_owned(),
        closed_line: None,
    }
}

fn purposes(asides: &[serde_json::Value]) -> Vec<(Option<&str>, Option<&str>)> {
    asides
        .iter()
        .map(|aside| (aside["purpose"].as_str(), aside["message"].as_str()))
        .collect()
}

/// Packet refusals never wake an idle occupant, admitted or parked: a missed
/// address waits unwritten until the occupant's next agent start queues it
/// once, and one judged while the occupant runs is queued at once.
#[test]
fn packet_refusals_wait_for_an_idle_occupants_next_agent_start() {
    for admitted in [false, true] {
        let sink = Sink::default();
        let driver = driver(&sink);
        let occupant = crate::resident::new_uuid();
        let missed = missed_refusal(occupant, "4:90:0:7:1");
        let owner = ServiceClaim::for_tests(
            wire::TaskStamp::for_micros(1).expect("fixture stamp is representable"),
            crate::resident::new_uuid(),
            crate::resident::new_uuid(),
        );
        let idle = retention::State::Idle { owner, admitted };
        *driver.retention.lock().unwrap() = idle;

        driver.deliver_message_refusal(&missed);
        assert!(
            sink.answered(0).is_empty(),
            "admitted={admitted}: no refusal is written to an idle occupant"
        );
        assert!(
            matches!(
                *driver.retention.lock().unwrap(),
                retention::State::Idle { .. }
            ),
            "admitted={admitted}: no refusal activates an idle occupant"
        );

        *driver.retention.lock().unwrap() = retention::State::Active;
        let start = decoded(b"{\"type\":\"agent_start\"}\n");
        assert!(driver.observe(&start).is_none());
        assert!(driver.observe(&start).is_none());
        let missed_text = missed.text();
        assert_eq!(
            purposes(&sink.answered(1)),
            [(Some("syn:4:90:0:7:1"), Some(missed_text.as_str()))],
            "admitted={admitted}: the next agent start queues the refusal once"
        );

        let running = missed_refusal(occupant, "4:120:0:9:1");
        driver.deliver_message_refusal(&running);
        let asides = sink.answered(2);
        assert_eq!(
            purposes(&asides[1..]),
            [(Some("syn:4:120:0:9:1"), Some(running.text().as_str()))],
            "admitted={admitted}: a refusal judged while running is queued at once"
        );
    }
}

use std::cell::RefCell;
use std::io::{self, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use super::Driver;
use crate::resource::Access;

const TARGET: &str = "spice://quote/v1/steer/1kQnrzNZx/00";

thread_local! {
    static ANSWERING: RefCell<Option<Ended>> = const { RefCell::new(None) };
}

/// Announces that an answering thread exited, after it answered or stayed
/// silent and let go of the driver.
struct Ended(mpsc::Sender<()>);

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// A driver whose answering threads each announce their exit on the returned
/// receiver.
fn driver_with<W: Write + Send + 'static>(
    stdin: W,
    serve: impl Fn(&str, Access<'_>) -> Result<String, String> + Send + Sync + 'static,
) -> (Arc<Driver<W>>, mpsc::Receiver<()>) {
    let (ended, exits) = mpsc::channel();
    let driver = Driver::with_server(
        stdin,
        crate::resident::supervisor::command::Readers::default(),
        Box::new(move |url: &str, access: Access<'_>| {
            ANSWERING.with(|answering| *answering.borrow_mut() = Some(Ended(ended.clone())));
            serve(url, access)
        }),
        steer::Key::for_identity(crate::resident::new_uuid()),
        None,
    );
    (driver, exits)
}

fn driver(
    serve: impl Fn(&str, Access<'_>) -> Result<String, String> + Send + Sync + 'static,
) -> (Arc<Driver<Vec<u8>>>, mpsc::Receiver<()>) {
    driver_with(Vec::new(), serve)
}

fn decoded_frames(input: &[u8]) -> Vec<serde_json::Value> {
    input
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("a frame"))
        .collect()
}

fn frames(driver: &Driver<Vec<u8>>) -> Vec<serde_json::Value> {
    let input = driver.stdin.lock().expect("input locks");
    decoded_frames(input.as_ref().expect("input remains open"))
}

fn aside(frame: &serde_json::Value) -> Option<serde_json::Value> {
    let notice = frame["message"].as_str()?.strip_prefix("/spice-aside ")?;
    serde_json::from_str(notice).ok()
}

#[derive(Clone, Default)]
struct CountingSink(Arc<Mutex<SinkState>>);

#[derive(Default)]
struct SinkState {
    bytes: Vec<u8>,
    writes: usize,
    flushes: usize,
}

impl Write for CountingSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().expect("sink locks");
        state.writes += 1;
        state.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().expect("sink locks").flushes += 1;
        Ok(())
    }
}

/// Receive the exit of each of `answering` threads.
fn settled(exits: &mpsc::Receiver<()>, answering: usize) {
    for _ in 0..answering {
        exits
            .recv_timeout(test_support::DEADLOCK_GUARD)
            .expect("an answering thread exits");
    }
}

#[test]
fn withdrawing_a_tool_does_not_make_its_running_work_quiescent() {
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let (driver, exits) = driver(move |_, _| {
        entered.send(()).expect("entry remains observed");
        let _ = released.lock().expect("release locks").recv();
        Ok("finished".to_owned())
    });
    driver.serve_uri("pending", "read", TARGET, None);
    entry
        .recv_timeout(test_support::DEADLOCK_GUARD)
        .expect("tool starts");
    driver.withdraw_uri("pending");
    let while_running = driver.quiescent();
    drop(release);
    settled(&exits, 1);
    assert!(
        !while_running,
        "withdrawal did not finish the resource operation"
    );
    assert!(
        driver.quiescent(),
        "only actual completion retires its work"
    );
}

#[test]
fn a_read_answers_with_the_resource_or_its_refusal() {
    let (driver, exits) = driver(|target, _| {
        if target == TARGET {
            Ok("Quoted from operator:\n\nship it".to_owned())
        } else {
            Err("[quote unavailable: reason=digest-mismatch]".to_owned())
        }
    });
    driver.serve_uri("u1", "read", TARGET, None);
    driver.serve_uri("u2", "read", "spice://quote/v1/steer/1kQnrzNZx/11", None);
    settled(&exits, 2);
    let mut answers = frames(&driver);
    answers.sort_by_key(|frame| frame["id"].as_str().map(str::to_owned));
    assert_eq!(
        answers,
        [
            serde_json::json!({
                "type": "host_uri_result",
                "id": "u1",
                "content": "Quoted from operator:\n\nship it",
                "contentType": "text/markdown",
            }),
            serde_json::json!({
                "type": "host_uri_result",
                "id": "u2",
                "isError": true,
                "error": "[quote unavailable: reason=digest-mismatch]",
            }),
        ]
    );
    assert!(
        driver
            .uris
            .pending
            .lock()
            .expect("pending locks")
            .is_empty()
    );
    assert!(
        driver
            .background
            .lock()
            .expect("background locks")
            .is_empty(),
        "reads queue no aside"
    );
}

#[test]
fn a_write_reaches_the_server_with_its_content_and_other_operations_are_refused() {
    let written = Arc::new(Mutex::new(Vec::new()));
    let (driver, exits) = driver({
        let written = Arc::clone(&written);
        move |url, access| {
            let Access::Write(content) = access else {
                panic!("a write never reads");
            };
            written
                .lock()
                .expect("writes lock")
                .push((url.to_owned(), content.to_owned()));
            Ok("edited".to_owned())
        }
    });
    driver.serve_uri("w", "write", "spice://task/X/description", Some("new"));
    driver.serve_uri("x", "write", TARGET, None);
    driver.serve_uri("y", "list", TARGET, None);
    // Only the write reaches a thread; malformed operations refuse inline.
    settled(&exits, 1);
    let frames = frames(&driver);
    let mut answers: Vec<_> = frames
        .iter()
        .filter(|frame| frame["type"] == "host_uri_result")
        .cloned()
        .collect();
    answers.sort_by_key(|frame| frame["id"].as_str().map(str::to_owned));
    let refused = |id: &str| {
        serde_json::json!({
            "type": "host_uri_result",
            "id": id,
            "isError": true,
            "error": vendor::omp::URI_SERVED,
        })
    };
    assert_eq!(
        answers,
        [
            serde_json::json!({
                "type": "host_uri_result",
                "id": "w",
                "content": "edited",
                "contentType": "text/markdown",
            }),
            refused("x"),
            refused("y"),
        ]
    );
    let (aside_index, notice) = frames
        .iter()
        .enumerate()
        .find_map(|(index, frame)| aside(frame).map(|notice| (index, notice)))
        .expect("the write queues an aside");
    assert_eq!(notice["purpose"], "uri-write:w");
    assert!(
        notice["message"]
            .as_str()
            .is_some_and(|message| message.contains("edited"))
    );
    assert_eq!(frames[aside_index + 1]["id"], "w");
    assert_eq!(
        *written.lock().expect("writes lock"),
        [("spice://task/X/description".to_owned(), "new".to_owned())]
    );
    assert!(
        driver
            .uris
            .pending
            .lock()
            .expect("pending locks")
            .is_empty()
    );
}

#[test]
fn a_write_registers_its_aside_and_result_in_one_write_and_flush() {
    for (answer, refused) in [
        (Ok("committed receipt".to_owned()), false),
        (Err("stale digest".to_owned()), true),
    ] {
        let outcome = match &answer {
            Ok(receipt) | Err(receipt) => receipt.clone(),
        };
        let sink = CountingSink::default();
        let (driver, exits) = driver_with(sink.clone(), move |_, access| {
            assert!(matches!(access, Access::Write("new")));
            answer.clone()
        });
        driver.serve_uri("w", "write", "spice://task/X/description", Some("new"));
        settled(&exits, 1);

        let state = sink.0.lock().expect("sink locks");
        assert_eq!((state.writes, state.flushes), (1, 1));
        let frames = decoded_frames(&state.bytes);
        assert_eq!(frames.len(), 2);
        let notice = aside(&frames[0]).expect("aside precedes the result");
        assert_eq!(notice["purpose"], "uri-write:w");
        assert!(
            notice["message"]
                .as_str()
                .is_some_and(|message| message.contains(&outcome))
        );
        assert_eq!(frames[1]["id"], "w");
        assert_eq!(frames[1]["isError"].as_bool().unwrap_or(false), refused);
    }
}

fn withdrawn_write(answer: Result<String, String>) -> Vec<serde_json::Value> {
    let (entered, writing) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (entered, released) = (Mutex::new(entered), Mutex::new(released));
    let (driver, exits) = driver(move |_, access| {
        assert!(matches!(access, Access::Write("new")));
        entered.lock().expect("entry").send(()).expect("test waits");
        released
            .lock()
            .expect("release")
            .recv()
            .expect("test releases");
        answer.clone()
    });
    driver.serve_uri("w", "write", "spice://task/X/description", Some("new"));
    writing.recv().expect("the write began");
    driver.cancel("w");
    release.send(()).expect("the write waits");
    settled(&exits, 1);
    frames(&driver)
}

#[test]
fn a_withdrawn_write_discloses_only_its_native_outcome() {
    for (answer, success, outcome) in [
        (
            Ok("committed receipt".to_owned()),
            true,
            "committed receipt",
        ),
        (Err("stale digest".to_owned()), false, "stale digest"),
    ] {
        let frames = withdrawn_write(answer);
        assert_eq!(frames.len(), 1, "a withdrawn write has no ordinary result");
        let notice = aside(&frames[0]).expect("the outcome remains queued");
        assert_eq!(notice["purpose"], "uri-write:w");
        let message = notice["message"].as_str().expect("notice has text");
        assert!(message.contains(outcome));
        assert_eq!(message.contains("success receipt"), success);
    }
}

#[test]
fn a_withdrawn_quote_read_is_never_answered_and_leaves_nothing_pending() {
    let (entered, reading) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (entered, released) = (Mutex::new(entered), Mutex::new(released));
    let (driver, exits) = driver(move |_, _| {
        entered.lock().expect("entry").send(()).expect("test waits");
        released
            .lock()
            .expect("release")
            .recv()
            .expect("test releases");
        Ok("late".to_owned())
    });
    driver.serve_uri("u1", "read", TARGET, None);
    reading.recv().expect("the read began");
    driver.cancel("u1");
    release.send(()).expect("the read waits");
    settled(&exits, 1);
    assert!(
        frames(&driver).is_empty(),
        "a withdrawn read gets no answer"
    );
    assert!(
        driver
            .uris
            .pending
            .lock()
            .expect("pending locks")
            .is_empty()
    );
}

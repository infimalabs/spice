use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use listen::{Dialect, Locus};
use vendor::omp::{JobCall, JobOp};

use super::super::terminal::job::Ending;
use super::super::terminal::{Control, Job, Jobs, Terminal};
use super::{Driver, FirstTurn, Work};

/// The occupant's input, as the driver writes it.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Sink {
    /// Every background notice written, as its purpose and message.
    fn notices(&self) -> Vec<(String, String)> {
        let bytes = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let frame: serde_json::Value = serde_json::from_slice(line).expect("a frame");
                let notice = frame["message"].as_str()?.strip_prefix("/spice-aside ")?;
                let notice: serde_json::Value = serde_json::from_str(notice).expect("a notice");
                Some((
                    notice["purpose"].as_str()?.to_owned(),
                    notice["message"].as_str()?.to_owned(),
                ))
            })
            .collect()
    }

    /// Every frame written, in order.
    fn frames(&self) -> Vec<serde_json::Value> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).expect("a frame"))
            .collect()
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "spice-job-match-delivery-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
        ));
        std::fs::create_dir_all(&path).expect("scratch");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn terminal(scratch: &Scratch) -> (Jobs, Terminal) {
    let jobs = Jobs::beneath(&scratch.0, crate::resident::new_uuid());
    let terminal = Terminal::new(
        "/bin/sh",
        &scratch.0,
        sill::Environment::empty(),
        jobs.clone(),
    );
    (jobs, terminal)
}

fn seat(sink: &Sink, terminal: &Terminal) -> Arc<Driver<Sink>> {
    Driver::with_readers(
        sink.clone(),
        || None,
        || None,
        |_| Err("no quotes here".to_owned()),
        steer::Key::for_identity(crate::resident::new_uuid()),
        Some(terminal.clone()),
    )
}

fn seat_with_inbox(sink: &Sink, terminal: &Terminal, inbox: String) -> Arc<Driver<Sink>> {
    Driver::with_readers(
        sink.clone(),
        move || Some(inbox.clone().into_bytes()),
        || None,
        |_| Err("no quotes here".to_owned()),
        steer::Key::for_identity(crate::resident::new_uuid()),
        Some(terminal.clone()),
    )
}

const PATTERN: &str = "ready|finished";

/// The notice a job's match is delivered as.
fn told(job: &Job, line: &str) -> (String, String) {
    (
        format!("job:{}:match", job.id()),
        vendor::omp::job_matched(job.id(), PATTERN, line),
    )
}

fn ending_purpose(job: &Job) -> String {
    format!("job:{}", job.id())
}

#[test]
fn a_successor_hears_the_match_its_vacated_predecessor_did_not_then_the_ending() {
    let scratch = Scratch::new("catch-up");
    let (jobs, terminal) = terminal(&scratch);
    let job = jobs.record_for_tests("tail -f build.log", Some(PATTERN), Some(60_000));

    let predecessor = Sink::default();
    let seated = seat(&predecessor, &terminal);
    seated.deliver_match(&job);
    seated.close();
    job.match_for_tests("finished");
    seated.deliver_match(&job);
    job.end_for_tests(Ending::Exited { code: 0 });
    assert!(
        predecessor.notices().is_empty(),
        "no match before it is published, and a closed seat hears nothing"
    );

    let successor = Sink::default();
    let next = seat(&successor, &terminal);
    for owed in terminal.undelivered() {
        next.hear(&terminal, &owed, &Control::new());
    }
    let notices = successor.notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[0], told(&job, "finished"));
    assert_eq!(
        notices[1].0,
        ending_purpose(&job),
        "the ending follows its match"
    );

    next.hear(&terminal, &job, &Control::new());
    assert!(terminal.undelivered().is_empty());
    assert_eq!(successor.notices().len(), 2, "each is told once");
}

#[test]
fn a_job_owed_at_seat_start_waits_unclaimed_behind_the_first_prompt_then_follows_it_once() {
    let scratch = Scratch::new("first-turn");
    let (jobs, terminal) = terminal(&scratch);
    let job = jobs.record_for_tests("cargo build", Some(PATTERN), Some(60_000));
    job.match_for_tests("finished");
    job.end_for_tests(Ending::Exited { code: 0 });

    let sink = Sink::default();
    let seated = seat(&sink, &terminal);
    seated
        .open(b"", None, Some(FirstTurn::Resumed("resumed".to_owned())))
        .expect("opening writes");
    for owed in terminal.undelivered() {
        seated.hear(&terminal, &owed, &Control::new());
    }
    assert!(
        sink.frames().is_empty(),
        "nothing reaches the idle session ahead of the first prompt"
    );
    assert_eq!(
        terminal.undelivered().len(),
        1,
        "a held ending stays owed, so a seat closing now leaves it to its successor"
    );

    let registration = serde_json::to_vec(&serde_json::json!({
        "id": listen::OMP_URI_SCHEMES_REQUEST_ID,
        "type": "response",
        "command": "set_host_uri_schemes",
        "success": true,
    }))
    .expect("JSON");
    let locus = Locus::new(0, 0, registration.len() as u64);
    let _ask = seated.observe(&listen::decode(Dialect::Omp, &registration, locus).expect("locus"));

    let frames = sink.frames();
    assert_eq!(
        frames[0]["id"],
        vendor::omp::PROMPT_REQUEST_ID,
        "{frames:?}"
    );
    let notices = sink.notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[0], told(&job, "finished"));
    assert_eq!(notices[1].0, ending_purpose(&job));
    assert!(terminal.undelivered().is_empty());
}

#[test]
fn a_seated_lane_hears_the_match_once_while_the_job_or_service_runs() {
    for timeout_millis in [Some(60_000), None] {
        let scratch = Scratch::new("seated");
        let (jobs, terminal) = terminal(&scratch);
        let job = jobs.record_for_tests("watch the build", Some(PATTERN), timeout_millis);

        let sink = Sink::default();
        let seated = seat(&sink, &terminal);
        let follower = {
            let (seated, terminal, job) = (Arc::clone(&seated), terminal.clone(), job.clone());
            std::thread::spawn(move || seated.hear(&terminal, &job, &Control::new()))
        };
        job.match_for_tests("ready");
        seated.deliver_match(&job);
        job.end_for_tests(Ending::Exited { code: 0 });
        follower.join().expect("follower ends with the job");

        let notices = sink.notices();
        assert_eq!(notices.len(), 2, "{timeout_millis:?}: {notices:?}");
        assert_eq!(notices[0], told(&job, "ready"), "{timeout_millis:?}");
        assert_eq!(notices[1].0, ending_purpose(&job));
    }
}

#[test]
fn a_blocked_wait_is_restated_after_another_call_settles_until_its_own_answer() {
    let scratch = Scratch::new("restated-wait");
    let (jobs, terminal) = terminal(&scratch);
    let job = jobs.record_for_tests("serve", None, Some(600_000));
    let sink = Sink::default();
    let seated = seat(&sink, &terminal);
    let settled = |id: &str| {
        let record = serde_json::json!({
            "type": "message_end",
            "message": {
                "role": "toolResult",
                "toolCallId": id,
                "toolName": "edit",
                "content": [{"type": "text", "text": "patched"}],
                "isError": false,
            },
        })
        .to_string();
        listen::decode(
            Dialect::Omp,
            record.as_bytes(),
            Locus::new(1, 0, record.len() as u64),
        )
        .expect("matching locus")
    };
    let waiter = {
        let (seated, terminal) = (Arc::clone(&seated), terminal.clone());
        let call = Work::Job(JobCall {
            op: JobOp::Wait,
            id: Some(job.id().to_owned()),
            // Only the job's ending answers this wait, whatever the host's speed.
            timeout: Duration::from_secs(365 * 24 * 60 * 60),
            until: None,
        });
        std::thread::spawn(move || {
            seated.resolve(&terminal, "wait-1", &call, &Control::new(), false);
        })
    };
    while sink.frames().is_empty() {
        std::thread::yield_now();
    }

    seated.observe(&settled("edit-1"));
    job.end_for_tests(Ending::Exited { code: 0 });
    waiter.join().expect("the wait answers when its job ends");
    seated.observe(&settled("edit-2"));

    let frames = sink.frames();
    let calls: Vec<_> = frames
        .iter()
        .map(|frame| (frame["type"].as_str(), frame["id"].as_str()))
        .collect();
    assert_eq!(
        calls,
        [
            (Some("host_tool_update"), Some("wait-1")),
            (Some("host_tool_update"), Some("wait-1")),
            (Some("host_tool_result"), Some("wait-1")),
        ]
    );
    assert_eq!(frames[1]["partialResult"], frames[0]["partialResult"]);
}

#[test]
fn stale_wait_reports_then_replays_inbox_and_delivers_its_ending_once() {
    let scratch = Scratch::new("stale-wait");
    let (jobs, terminal) = terminal(&scratch);
    let job = jobs.record_for_tests("cargo build", None, Some(60_000));
    let sink = Sink::default();
    let inbox = "<1kJK6Q0bx>\n\
                 directive=d-1kJK6M0Vx key=1kJK6M0Vx stage=published expires=never\n\
                 newer direction\n\
                 </1kJK6Q0bx>\n"
        .to_owned();
    let seated = seat_with_inbox(&sink, &terminal, inbox);
    seated.deliver_inbox();

    let call = Work::Job(JobCall {
        op: JobOp::Wait,
        id: Some(job.id().to_owned()),
        timeout: Duration::from_secs(30),
        until: None,
    });
    let control = Control::new();
    assert!(seated.preempt_stale(&call, &control));
    seated.resolve(&terminal, "wait-stale", &call, &control, true);

    let frames = sink.frames();
    assert_eq!(frames.len(), 4, "{frames:?}");
    assert_eq!(frames[1]["type"], "host_tool_update");
    assert_eq!(frames[2]["type"], "host_tool_result");
    assert_eq!(frames[2]["id"], "wait-stale");
    assert!(
        frames[2]["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|result| {
                result.contains(
                    "Newer steering arrived after this model step began; its inbox follows.",
                )
            }),
        "{frames:?}"
    );
    assert_eq!(frames[3]["streamingBehavior"], "steer");
    assert!(
        frames[3]["message"]
            .as_str()
            .is_some_and(|message| message.contains("newer direction")),
        "{frames:?}"
    );
    assert!(
        job.ended().is_none(),
        "the interrupted wait leaves the job alive"
    );

    job.end_for_tests(Ending::Exited { code: 0 });
    seated.hear(&terminal, &job, &Control::new());
    assert_eq!(sink.notices().len(), 1);
    seated.hear(&terminal, &job, &Control::new());
    assert_eq!(sink.notices().len(), 1, "the ending is delivered once");
}

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;

use vendor::omp::OutputPattern;

use super::keeper::match_lane;
use super::reconcile::reconcile_services_beneath;
use super::service::ServiceClaim;
use super::{
    Ending, Job, Jobs, LINE_CAP, LOG, LogLines, REQUEST, REQUEST_BYTES, Request, STOP,
    ServiceIndex, read_json, until,
};

fn scratch() -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "spice-service-index-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
        ))
        .join("lanes")
}

fn cleanup(root: &std::path::Path) {
    fs::remove_dir_all(
        root.parent()
            .expect("the lane root is beneath the fixture state root"),
    )
    .expect("fixture cleanup");
}

fn reconcile(root: &std::path::Path, rebuild: bool) -> String {
    reconcile_with(root, rebuild, |_| false, |_, _| false)
}

fn reconcile_with(
    root: &std::path::Path,
    rebuild: bool,
    task_owned: impl Fn(&ServiceClaim) -> bool,
    turn_owned: impl Fn(wire::CanonicalUuid, wire::CanonicalUuid) -> bool,
) -> String {
    let mut diagnostic = Vec::new();
    reconcile_services_beneath(root, rebuild, task_owned, turn_owned, &mut diagnostic);
    String::from_utf8(diagnostic).expect("diagnostic text")
}

#[test]
fn quiescence_waits_for_ending_delivery_not_just_process_completion() {
    let root = scratch();
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let command = jobs.record_for_tests("build", None, Some(600_000));
    assert!(!jobs.quiescent().expect("running catalog"));
    command.end_for_tests(Ending::Exited { code: 0 });
    assert!(!jobs.quiescent().expect("unheard ending catalog"));
    assert!(command.claim_heard().expect("ending delivery"));
    assert!(jobs.quiescent().expect("delivered ending catalog"));
    cleanup(&root);
}

/// An unowned service is asked to stop on every commit until its ending is
/// observed; only then does it leave the index.
#[test]
fn an_unowned_service_is_stopped_until_its_ending_retires_it() {
    let root = scratch();
    let lane = crate::resident::new_uuid();
    let jobs = Jobs::beneath(&root, lane);
    let service = jobs.record_for_tests("serve", None, None);
    let index = ServiceIndex::beneath(&root);

    reconcile(&root, false);
    let stop = service.directory.join(STOP);
    let stopped = stop.exists();
    let indexed_while_running = index.entries().expect("index reads");
    service.end_for_tests(Ending::Stopped);
    reconcile(&root, false);
    let indexed_after_ending = index.entries().expect("index reads");
    cleanup(&root);

    assert!(stopped, "an unowned service was not asked to stop");
    assert_eq!(indexed_while_running, vec![(lane, service.id().to_owned())]);
    assert!(
        indexed_after_ending.is_empty(),
        "an ended service stayed indexed"
    );
}

/// Commit-time reconciliation reads only the index: retained finite history
/// and records an earlier resident never indexed are untouched until the
/// first reconciliation rebuilds the index, which then stops an unreadable
/// record fail-closed.
#[test]
fn only_a_rebuild_reads_job_history_and_it_fails_closed() {
    let root = scratch();
    let lane = crate::resident::new_uuid();
    let jobs = Jobs::beneath(&root, lane);
    for _ in 0..8 {
        let finite = jobs.record_for_tests("cargo test", None, Some(600_000));
        finite.end_for_tests(Ending::Exited { code: 0 });
        assert!(finite.claim_heard().expect("delivery claims"));
    }
    let running = jobs.record_for_tests("cargo build", None, Some(600_000));
    let unindexed = jobs.record_for_tests("serve", None, None);
    let index = ServiceIndex::beneath(&root);
    index.remove(lane, unindexed.id()).expect("entry removes");
    fs::remove_file(unindexed.directory.join(REQUEST)).expect("request removes");

    reconcile(&root, false);
    let stopped_without_rebuild = unindexed.directory.join(STOP).exists();
    let diagnostic = reconcile(&root, true);
    let stopped_after_rebuild = unindexed.directory.join(STOP).exists();
    let finite_stopped = running.directory.join(STOP).exists();
    let indexed = index.entries().expect("index reads");
    cleanup(&root);

    assert!(
        !stopped_without_rebuild,
        "commit-time reconciliation read unindexed history"
    );
    assert!(
        stopped_after_rebuild,
        "a rebuild did not stop an unreadable record: {diagnostic}"
    );
    assert!(
        !finite_stopped,
        "service reconciliation stopped a finite job"
    );
    assert_eq!(indexed, vec![(lane, unindexed.id().to_owned())]);
}

/// An entry whose record no longer exists names no process and is retired
/// without stopping anything.
#[test]
fn an_entry_without_a_record_is_retired() {
    let root = scratch();
    let lane = crate::resident::new_uuid();
    let index = ServiceIndex::beneath(&root);
    let id = "dW0Cb068M";
    index.insert(lane, id).expect("entry inserts");

    let diagnostic = reconcile(&root, false);
    let indexed = index.entries().expect("index reads");
    cleanup(&root);

    assert!(indexed.is_empty(), "a vanished record stayed indexed");
    assert!(
        diagnostic.is_empty(),
        "a vanished record was reported: {diagnostic}"
    );
}

/// A service never keeps a lane idle: a match nobody hears wakes the lane of
/// a deadline-bounded command, and a service's match wakes nobody.
#[test]
fn only_a_deadline_bounded_commands_unheard_match_wakes_its_lane() {
    let root = scratch();
    let lane = crate::resident::new_uuid();
    let jobs = Jobs::beneath(&root, lane);
    let request = |job: &Job| -> Request {
        read_json(&job.log_path().with_file_name(REQUEST), REQUEST_BYTES).expect("request")
    };
    let watch = jobs.record_for_tests("tail -f build.log", Some("ready"), Some(60_000));
    let service = jobs.record_for_tests("app-demo", Some("ready"), None);

    let woken = (match_lane(&request(&watch)), match_lane(&request(&service)));

    cleanup(&root);
    assert_eq!(woken, (Some(lane), None));
}

/// The watcher publishes exactly the first matching complete line of the
/// log, once, while the command runs; nonmatching and later matching lines
/// publish nothing, an unterminated last line counts once the log is
/// complete, and a log without a match publishes nothing at all.
#[test]
fn the_until_watcher_publishes_only_the_first_matching_log_line_once() {
    let root = scratch();
    let append = |directory: &std::path::Path, text: &str| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join(LOG))
            .and_then(|mut log| log.write_all(text.as_bytes()))
            .expect("log appends");
    };
    let arm = |name: &str, opening: &str| {
        let directory = root.join(name);
        fs::create_dir_all(&directory).expect("job directory");
        append(&directory, opening);
        let (published, told) = mpsc::channel();
        let watcher = until::arm(
            &directory,
            OutputPattern::new("ready \\d").expect("pattern compiles"),
            move || published.send(()).expect("told"),
        )
        .expect("watcher arms");
        (directory, watcher, told)
    };

    let (running, watcher, told) = arm("running", "compiling\nalmost read");
    append(&running, "y\nready 1\nready 2\n");
    told.recv()
        .expect("the first match is told while the command runs");
    append(&running, "ready 3\n");
    watcher.complete();
    let running_match = until::matched(&running).expect("match reads");
    let told_again = told.try_recv().is_ok();

    let (tail, watcher, _told) = arm("tail", "compiling\nready 4");
    watcher.complete();
    let tail_match = until::matched(&tail).expect("match reads");

    let (silent, watcher, told) = arm("silent", "compiling\nready\n");
    watcher.complete();
    let silent_match = until::matched(&silent).expect("match reads");
    let silent_told = told.try_recv().is_ok();

    cleanup(&root);
    assert_eq!(running_match.as_deref(), Some("ready 1"));
    assert!(!told_again, "a later match is not told");
    assert_eq!(tail_match.as_deref(), Some("ready 4"));
    assert_eq!(silent_match, None);
    assert!(!silent_told, "a log without a match tells nothing");
}

/// A line longer than the reader's cap is clipped to it, the rest up to its
/// newline is skipped, and the next line reads whole; an unterminated
/// over-long tail is held only up to the cap.
#[test]
fn log_lines_clip_an_over_long_line_and_read_the_next_whole() {
    let root = scratch();
    fs::create_dir_all(&root).expect("job directory");
    let long = format!("{}ready 9", "x".repeat(LINE_CAP));
    fs::write(
        root.join(LOG),
        format!("{long}\nready 1\n{}", "y".repeat(3 * LINE_CAP)),
    )
    .expect("log writes");
    let mut lines = LogLines::new(root.clone());
    let mut read = Vec::new();
    lines
        .read(&mut |line| read.push(line.to_owned()))
        .expect("log reads");
    let held = lines.partial.len();
    let tail = lines.finish();
    cleanup(&root);

    assert_eq!(read, ["x".repeat(LINE_CAP), "ready 1".to_owned()]);
    assert_eq!(held, LINE_CAP);
    assert_eq!(tail, Some("y".repeat(LINE_CAP)));
}

#[test]
fn job_records_preserve_ambient_task_presence_and_absence() {
    let root = scratch();
    let lane = crate::resident::new_uuid();
    let jobs = Jobs::beneath(&root, lane);
    let task = wire::TaskStamp::for_micros(10).expect("fixture task");
    let associated = jobs.record_bound_for_tests(
        "cargo test",
        None,
        Some(600_000),
        Some(task.clone()),
        None,
        None,
    );
    let unassociated = jobs.record_for_tests("cargo check", None, Some(600_000));

    let associated = associated.listing().expect("associated job lists");
    let unassociated = unassociated.listing().expect("unassociated job lists");
    cleanup(&root);

    assert_eq!(associated.task.as_ref(), Some(&task));
    assert_eq!(unassociated.task, None);
}

#[test]
fn task_service_lifetime_follows_its_exact_claim_owner() {
    let root = scratch();
    let lane = crate::resident::new_uuid();
    let occupant = crate::resident::new_uuid();
    let task = wire::TaskStamp::for_micros(20).expect("fixture task");
    let claim = ServiceClaim::for_tests(task.clone(), lane, occupant);
    let service = Jobs::beneath(&root, lane).record_bound_for_tests(
        "serve",
        None,
        None,
        Some(task.clone()),
        Some(claim),
        None,
    );

    reconcile_with(&root, false, |_| true, |_, _| false);
    let retained = !service.directory.join(STOP).exists();
    reconcile_with(&root, false, |_| false, |_, _| true);
    let stopped = service.directory.join(STOP).exists();
    let recorded_task = service.task().cloned();
    cleanup(&root);

    assert!(retained, "an active task service was stopped");
    assert!(stopped, "a task service survived its claim owner");
    assert_eq!(recorded_task, Some(task));
}

#[test]
fn turn_reconciliation_stops_only_that_turns_unanchored_services() {
    let root = scratch();
    let lane = crate::resident::new_uuid();
    let turn = crate::resident::new_uuid();
    let other_turn = crate::resident::new_uuid();
    let task = wire::TaskStamp::for_micros(30).expect("fixture task");
    let jobs = Jobs::beneath(&root, lane);
    let ending = jobs.record_bound_for_tests("serve one", None, None, None, None, Some(turn));
    let other = jobs.record_bound_for_tests("serve two", None, None, None, None, Some(other_turn));
    let anchored = jobs.record_bound_for_tests(
        "serve task",
        None,
        None,
        Some(task.clone()),
        Some(ServiceClaim::for_tests(task, lane, turn)),
        None,
    );

    reconcile_with(
        &root,
        false,
        |_| true,
        |active_lane, owner| active_lane == lane && owner == other_turn,
    );
    let ending_stopped = ending.directory.join(STOP).exists();
    let other_stopped = other.directory.join(STOP).exists();
    let anchored_stopped = anchored.directory.join(STOP).exists();
    cleanup(&root);

    assert!(ending_stopped, "the ended turn's service kept running");
    assert!(!other_stopped, "another turn's service was stopped");
    assert!(
        !anchored_stopped,
        "a task-owned service was stopped at turn end"
    );
}

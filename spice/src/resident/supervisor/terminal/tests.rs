use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sill::Environment;
use vendor::omp::{JobCall, JobOp, OutputPattern};
use wire::intent::{LaneJobKind, LaneJobStanding};

use super::job::Ending;
use super::{Control, Jobs, Terminal, WaitHints, kept, unheard, wait, wait_until};

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "spice-lane-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple(),
    ))
}

fn append(log: &Path, text: &str) {
    fs::OpenOptions::new()
        .append(true)
        .open(log)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .expect("job output appends");
}

#[test]
fn an_idle_hint_needs_two_same_finite_waits_a_claim_and_has_one_shot() {
    let mut hints = WaitHints::default();

    assert!(!hints.observe("job-a", true), "the first wait hinted");
    assert!(
        hints.observe("job-a", true),
        "the second same-job wait did not qualify"
    );
    assert!(
        !hints.mark_hinted("job-a", false),
        "a lane without a claim consumed the hint"
    );
    assert!(
        hints.mark_hinted("job-a", true),
        "the claimed lane did not receive its qualified hint"
    );
    assert!(
        !hints.observe("job-a", true),
        "one job qualified for a repeated hint"
    );

    assert!(
        !hints.observe("job-b", true),
        "a different job inherited a streak"
    );
    assert!(
        !hints.observe("job-b", false),
        "a service wait advanced the streak"
    );
    assert!(
        !hints.observe("job-b", true),
        "a wait after a service inherited a streak"
    );
    assert!(
        hints.observe("job-b", true),
        "the reset finite streak did not qualify"
    );
}

#[test]
fn a_foreground_promotion_tells_the_agent_to_keep_working() {
    let root = scratch("parallel-promotion");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let job = jobs.record_for_tests("cargo test", None, Some(600_000));

    let report = kept(&job, None, true);

    fs::remove_dir_all(root).expect("fixture cleanup");
    assert!(
        report
            .output
            .contains("Keep working in parallel while it runs; its result arrives on its own.")
    );
}

#[test]
fn a_lane_lists_each_kept_job_by_how_it_lives_and_whether_it_ended() {
    let root = std::env::temp_dir().join(format!(
        "spice-lane-jobs-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple(),
    ));
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let command = jobs.record_for_tests("cargo test", None, Some(600_000));
    let service = jobs.record_for_tests("serve --port 8080", None, None);
    let watch = jobs.record_for_tests("tail -f build.log", Some("ready"), Some(600_000));
    let watched = jobs.record_for_tests("app-demo", Some("ready"), None);
    command.end_for_tests(Ending::Exited { code: 1 });

    let mut listed = unheard(&jobs)
        .into_iter()
        .map(|job| (job.id, job.kind, job.standing))
        .collect::<Vec<_>>();
    listed.sort();
    let mut expected = vec![
        (
            command.id().to_owned(),
            LaneJobKind::Command,
            LaneJobStanding::Unheard,
        ),
        (
            service.id().to_owned(),
            LaneJobKind::Service,
            LaneJobStanding::Running,
        ),
        (
            watch.id().to_owned(),
            LaneJobKind::Watch,
            LaneJobStanding::Running,
        ),
        (
            watched.id().to_owned(),
            LaneJobKind::WatchedService,
            LaneJobStanding::Running,
        ),
    ];
    expected.sort();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(listed, expected);
}

#[test]
fn a_finite_wait_reports_the_job_and_latest_output_while_it_remains_blocked() {
    let root = std::env::temp_dir().join(format!(
        "spice-lane-wait-progress-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple(),
    ));
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let job = jobs.record_for_tests("serve --port 8080", None, Some(600_000));
    fs::write(job.log_path(), "building\nready on 8080\n").expect("job output");
    let terminal = Terminal::new("/bin/sh", &root, Environment::empty(), jobs);
    let call = JobCall {
        op: JobOp::Wait,
        id: Some(job.id().to_owned()),
        timeout: Duration::ZERO,
        until: None,
    };
    let mut progress = Vec::new();

    let report = terminal.job(&call, &Control::new(), &mut |report| {
        progress.push(report.output.clone());
    });

    assert!(
        job.claim_heard().expect("ending claim"),
        "an unfinished wait releases the automatic-delivery claim"
    );
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        progress,
        ["[job ".to_owned() + job.id() + "] serve --port 8080\nready on 8080"]
    );
    assert!(report.output.contains("Command still running"));
}

#[test]
fn a_blocking_wait_claims_only_the_ending_it_reports() {
    let root = scratch("wait-ending-claim");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let job = jobs.record_for_tests("cargo test", None, Some(600_000));
    fs::write(job.log_path(), "running\n").expect("job output");
    let mut ended = false;

    let report = wait(
        &job,
        &Control::new(),
        Duration::from_secs(30),
        Duration::ZERO,
        &mut |_| {
            if !ended {
                ended = true;
                assert!(
                    job.claim_heard().expect("automatic delivery claim"),
                    "a running wait must not durably mark a future ending delivered"
                );
                job.release_heard().expect("release probe claim");
                job.end_for_tests(Ending::Exited { code: 0 });
            }
        },
    )
    .report;

    assert!(ended);
    assert_eq!(report.exit_code, Some(0));
    assert!(
        !job.claim_heard().expect("automatic delivery claim"),
        "the completed wait retains its ending claim"
    );
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_wait_does_not_repeat_an_ending_already_claimed_for_delivery() {
    let root = scratch("wait-claimed-ending");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let job = jobs.record_for_tests("cargo test", None, Some(600_000));
    fs::write(job.log_path(), "finished\n").expect("job output");
    job.end_for_tests(Ending::Exited { code: 0 });
    assert!(job.claim_heard().expect("automatic delivery claim"));

    let report = wait(
        &job,
        &Control::new(),
        Duration::from_secs(30),
        Duration::ZERO,
        &mut |_| {},
    )
    .report;

    assert!(report.output.contains("claimed for automatic delivery"));
    assert_eq!(report.exit_code, None);
    fs::remove_dir_all(root).expect("fixture cleanup");
}
#[test]
fn a_finite_wait_reports_what_its_log_gains_though_its_directory_never_changes() {
    let root = scratch("wait-refresh");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let job = jobs.record_for_tests("serve --port 8080", None, Some(600_000));
    fs::write(job.log_path(), "building\n").expect("job output");
    let control = Control::new();
    let handle = control.handle();
    let mut progress = Vec::new();

    wait(
        &job,
        &control,
        Duration::from_secs(30),
        Duration::from_millis(10),
        &mut |report| {
            progress.push(report.output.clone());
            if progress.len() == 1 {
                append(&job.log_path(), "ready on 8080\n");
            } else {
                handle.cancel();
            }
        },
    );

    let _ = fs::remove_dir_all(&root);
    let named = format!("[job {}] serve --port 8080\n", job.id());
    assert_eq!(
        progress,
        [named.clone() + "building", named + "ready on 8080"]
    );
}

fn pattern(text: &str) -> OutputPattern {
    OutputPattern::new(text).expect("pattern compiles")
}

#[test]
fn a_pattern_wait_on_a_service_answers_from_output_written_before_it_began() {
    let root = scratch("until-before");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let service = jobs.record_for_tests("app-demo", None, None);
    fs::write(
        service.log_path(),
        "staging\n[app-demo] endpoint=http://127.0.0.1:4000\nserving\n",
    )
    .expect("job output");

    let report = wait_until(
        &service,
        &Control::new(),
        Duration::ZERO,
        &pattern(r"endpoint=\S+"),
        Duration::ZERO,
        &mut |_| {},
    )
    .report;

    let running = service.ended().is_none();
    let _ = fs::remove_dir_all(&root);
    assert!(!report.failed);
    assert!(running, "a matched wait leaves the service running");
    assert!(
        report
            .output
            .contains("[app-demo] endpoint=http://127.0.0.1:4000\n\nmatched"),
        "{}",
        report.output
    );
}

#[test]
fn a_pattern_wait_wakes_on_output_written_after_it_began() {
    let root = scratch("until-after");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let service = jobs.record_for_tests("app-demo", None, None);
    fs::write(service.log_path(), "staging\n").expect("job output");
    let mut progress = Vec::new();

    // Only the log grows, and a pattern wait has no timer, so nothing but
    // the log's own growth can answer before the bound.
    let report = wait_until(
        &service,
        &Control::new(),
        Duration::from_secs(30),
        &pattern("^ready on (\\d+)$"),
        Duration::ZERO,
        &mut |report| {
            progress.push(report.output.clone());
            append(&service.log_path(), "still staging\nready on 8080\n");
        },
    )
    .report;

    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        progress,
        [format!("[job {}] app-demo\nstaging", service.id())],
        "the line before the match is progress, and the match ends the wait"
    );
    assert!(
        report.output.contains("ready on 8080\n\nmatched"),
        "{}",
        report.output
    );
}

#[test]
fn a_pattern_wait_that_sees_no_match_answers_at_its_bound_with_the_job_running() {
    let root = scratch("until-elapsed");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let service = jobs.record_for_tests("app-demo", None, None);
    fs::write(service.log_path(), "staging\n").expect("job output");

    let report = wait_until(
        &service,
        &Control::new(),
        Duration::ZERO,
        &pattern("endpoint="),
        Duration::ZERO,
        &mut |_| {},
    )
    .report;

    let _ = fs::remove_dir_all(&root);
    assert!(
        report
            .output
            .contains("no line matched endpoint= within 0 seconds"),
        "{}",
        report.output
    );
    assert!(report.output.contains("Command still running"));
}

#[test]
fn a_pattern_wait_on_an_ended_job_matches_its_unterminated_last_line() {
    let root = scratch("until-ended");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let job = jobs.record_for_tests("build", None, Some(600_000));
    fs::write(job.log_path(), "compiling\nready").expect("job output");
    job.end_for_tests(Ending::Exited { code: 0 });

    let matched = wait_until(
        &job,
        &Control::new(),
        Duration::from_secs(30),
        &pattern("^ready$"),
        Duration::ZERO,
        &mut |_| {},
    )
    .report;
    let unmatched = wait_until(
        &job,
        &Control::new(),
        Duration::from_secs(30),
        &pattern("^failed"),
        Duration::ZERO,
        &mut |_| {},
    )
    .report;

    let _ = fs::remove_dir_all(&root);
    assert!(
        matched.output.ends_with("matched ^ready$: ready"),
        "{}",
        matched.output
    );
    assert!(
        unmatched
            .output
            .ends_with("no line matched ^failed before the job ended"),
        "{}",
        unmatched.output
    );
}

#[test]
fn an_archived_log_that_reached_its_cap_still_reports_its_output_and_what_was_not_kept() {
    let root = scratch("archived");
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let job = jobs.record_for_tests("flood", None, Some(600_000));
    let lines: String = (0..3000).map(|line| format!("line {line}\n")).collect();
    fs::write(job.log_path(), format!("{lines}\n{}\n", super::job::CAPPED)).expect("job output");
    job.end_for_tests(Ending::Exited { code: 0 });
    job.close_log_for_tests(12_345);

    let plaintext = job.log_path().exists();
    let report = super::finished(&job, &job.ended().expect("ended"));
    let until = |expression| {
        wait_until(
            &job,
            &Control::new(),
            Duration::from_secs(30),
            &pattern(expression),
            Duration::ZERO,
            &mut |_| {},
        )
        .report
        .output
    };
    let (first, absent) = (until("^line 0$"), until("^ready$"));

    let _ = fs::remove_dir_all(&root);
    assert!(!plaintext, "the verified archive replaced the plaintext");
    assert!(report.output.contains("line 2999"), "{}", report.output);
    assert!(report.output.contains("log.zst"), "{}", report.output);
    assert!(report.output.contains("12345"), "{}", report.output);
    assert!(first.contains("matched ^line 0$"), "{first}");
    assert!(absent.contains(super::UNSEARCHED), "{absent}");
}

/// A confirmed no-process launch failure removes its record. Cleanup failures
/// and unknown process ownership preserve the record and remain observable
/// rather than deleting a possibly live keeper or masquerading as a clean refusal.
#[test]
fn a_job_whose_keeper_cannot_launch_is_never_silently_kept() {
    let root = std::env::temp_dir().join(format!(
        "spice-lane-jobs-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple(),
    ));
    let jobs = Jobs::beneath(&root, crate::resident::new_uuid());
    let (failure, record_exists) = jobs.start_unlaunchable_for_tests("echo alive");
    let kept = jobs.kept().expect("kept jobs read");
    assert_eq!(failure.kind(), std::io::ErrorKind::NotFound);
    assert!(!record_exists, "a failed launch left its job directory");
    assert!(
        kept.is_empty(),
        "a failed launch left kept jobs: {:?}",
        kept.iter().map(super::Job::id).collect::<Vec<_>>()
    );

    let (failure, record_exists) = jobs.start_uncleanable_for_tests("echo stranded");
    let kept = jobs.kept().expect("uncleaned jobs read");
    assert_eq!(failure.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(record_exists, "the cleanup fixture removed its record");
    assert_eq!(kept.len(), 1, "the surviving record must remain visible");

    let (failure, record_exists) = jobs.start_unknown_for_tests("echo possibly-alive");
    let kept = jobs.kept().expect("unknown-ownership jobs read");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(failure.kind(), std::io::ErrorKind::BrokenPipe);
    assert!(
        record_exists,
        "unknown process ownership deleted its record"
    );
    assert_eq!(kept.len(), 2, "both preserved records must remain visible");
}

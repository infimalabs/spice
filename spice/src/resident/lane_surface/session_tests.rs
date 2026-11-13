use super::*;

static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const WORKTREE: &str = "/worktrees/resumed";
const IDENTITY: &str = "018f7777-7777-7444-8888-999999999999";

#[derive(Default)]
struct ExitSink(std::sync::Mutex<Vec<CaptureExit>>);

impl CaptureReporter for ExitSink {
    fn record(&self, _record: CaptureRecord) -> Result<(), CaptureReportError> {
        Ok(())
    }

    fn failed(&self, _failure: CaptureFailure) {}

    fn exited(&self, exit: CaptureExit) {
        self.0.lock().expect("exit sink lock").push(exit);
    }
}

/// One isolated durable root. Resuming a lane reads a session head and a tape
/// and nothing else, so these tests need neither a bus nor a repository.
struct Fixture {
    root: std::path::PathBuf,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID")
}

fn fixture() -> Fixture {
    let ordinal = NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "spice-lane-resume-{}-{ordinal}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("fixture directory");
    Fixture {
        root,
        lane: uuid(1),
        occupant: uuid(2),
    }
}

fn finish(fixture: Fixture) {
    std::fs::remove_dir_all(&fixture.root).expect("fixture cleanup");
}

fn paths(fixture: &Fixture) -> RuntimePaths {
    RuntimePaths {
        worktree: fixture.root.clone(),
        common: fixture.root.clone(),
        runtime: fixture.root.join("runtime"),
        live: fixture.root.join("runtime").join("runtime"),
        data: fixture.root.join("data"),
        journal: fixture.root.join("authority.sqlite3"),
        resident: fixture.root.join("resident"),
        serve: fixture.root.join("serve"),
        lane_serve: fixture.root.join("lane-serve"),
        degraded: fixture.root.join("degraded"),
        tape: fixture.root.join("lanes"),
        supervisors: fixture.root.join("lanes"),
        steering_media: fixture.root.join("lanes"),
        releases: fixture.root.join("authority/releases"),
        stage: fixture.root.join("stage"),
    }
}

/// A durable row whose session head holds in every field, before one test
/// bends exactly one of them away.
fn row(fixture: &Fixture) -> wire::LaneResidency {
    wire::LaneResidency {
        lane: fixture.lane,
        team: fixture.lane,
        name: None,
        worktree: WORKTREE.into(),
        life: "drive".into(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(fixture.occupant),
        lineage: vec![fixture.occupant],
        capabilities: Vec::new(),
        session: Some(wire::LaneSession {
            lane: fixture.lane,
            occupant: fixture.occupant,
            worktree: WORKTREE.into(),
            vendor: "omp".parse().expect("fixture vendor"),
            launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
            epoch: uuid(3),
            stream: "stdout".into(),
            identity: IDENTITY.into(),
            locus: wire::TapeLocus {
                member: 0,
                offset: 0,
                length: 0,
            },
        }),
        disposition: OCCUPIED.into(),
    }
}

fn never_started(row: &mut wire::LaneResidency) {
    row.occupant = None;
    row.lineage.clear();
    row.session = None;
    row.disposition = VACANT.into();
}

fn head(row: &mut wire::LaneResidency) -> &mut wire::LaneSession {
    row.session
        .as_mut()
        .expect("fixture row declares session authority")
}

#[test]
fn declaration_reporter_forwards_the_kernel_exit_to_its_resident_sink() {
    let lane = uuid(1);
    let occupant = uuid(2);
    let inner = Arc::new(ExitSink::default());
    let (reply, _session) = mpsc::sync_channel(1);
    let reporter = DeclarationReporter {
        inner: inner.clone(),
        lane,
        occupant,
        worktree: WORKTREE.to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: "sonnet".to_owned(),
        effort: "high".to_owned(),
        fast_mode: false,
        git: "skipped:test".to_owned(),
        rtk: wire::body::RtkHealth::new(
            wire::body::RtkHealthState::Active,
            Some("0.45.0".to_owned()),
            "fixture active",
        ),
        epoch: uuid(3),
        stream: vendor::RecordStream::Stdout,
        expected: None,
        state: Arc::new(Mutex::new(DeclarationState {
            reply,
            seen: None,
            witness: DeclarationWitness::default(),
        })),
    };
    let exit = CaptureExit { lane, occupant };

    reporter.exited(exit);

    assert_eq!(*inner.0.lock().expect("exit sink lock"), vec![exit]);
}

/// One omp state reply, in the words the dialect actually reads.
fn declaration(identity: &str) -> Vec<u8> {
    format!(
        r#"{{"id":"spice-state","type":"response","command":"get_state","success":true,"data":{{"sessionId":"{identity}"}}}}"#
    )
    .into_bytes()
}

/// Commit one record to this lane's durable tape and bind the head to it.
fn declare(fixture: &Fixture, row: &mut wire::LaneResidency, record: &[u8]) {
    let root = paths(fixture).lane_tape(row.lane).join(row.vendor.as_str());
    let tape = tape::Tape::open(root).expect("fixture tape opens");
    let locus = tape.tee(record).expect("fixture record commits");
    head(row).locus = wire::TapeLocus {
        member: locus.member,
        offset: locus.offset,
        length: locus.length,
    };
}

fn refusal(fixture: &Fixture, row: &wire::LaneResidency) -> Refusal {
    match resume_session(&paths(fixture), row) {
        Err(Outcome::Refusal(refusal)) => refusal,
        Err(Outcome::Fault(fault)) => {
            panic!("resume faulted rather than refusing: {}", fault.reason)
        }
        Err(Outcome::Reply(_)) | Ok(_) => panic!("resume admitted a session head it must refuse"),
    }
}

fn reason(fixture: &Fixture, row: &wire::LaneResidency) -> String {
    refusal(fixture, row).reason().to_owned()
}

#[test]
fn resume_returns_the_identity_the_durable_tape_still_declares() {
    let fixture = fixture();
    let mut row = row(&fixture);
    declare(&fixture, &mut row, &declaration(IDENTITY));
    match resume_session(&paths(&fixture), &row).expect("a confirmed head resumes") {
        vendor::Resume::Session(identity) => assert_eq!(identity.as_str(), IDENTITY),
        vendor::Resume::Fresh => panic!("a confirmed session head resumed as a fresh occupant"),
    }
    finish(fixture);
}

#[test]
fn launch_resume_starts_fresh_for_a_never_started_vacant_lane() {
    let fixture = fixture();
    let mut row = row(&fixture);
    never_started(&mut row);
    match launch_resume(&paths(&fixture), &row).expect("never-started lane starts fresh") {
        vendor::Resume::Fresh => {}
        vendor::Resume::Session(identity) => {
            panic!("never-started lane unexpectedly resumed {identity:?}")
        }
    }
    finish(fixture);
}

/// Every refusal names the head itself, so an operator reading one knows which
/// durable record to repair without reading this chain.
#[test]
fn a_refused_resume_witnesses_the_durable_session_head() {
    let fixture = fixture();
    let mut row = row(&fixture);
    row.session = None;
    assert_eq!(
        refusal(&fixture, &row)
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::witness),
        Some("lane-session-head")
    );
    finish(fixture);
}

#[test]
fn resume_refuses_a_lane_seated_before_session_authority_existed() {
    let fixture = fixture();
    let mut row = row(&fixture);
    row.session = None;
    assert_eq!(reason(&fixture, &row), "lane::session-authority-missing");
    finish(fixture);
}

#[test]
fn resume_refuses_a_session_head_declared_by_another_lane() {
    let fixture = fixture();
    let mut row = row(&fixture);
    head(&mut row).lane = uuid(9);
    assert_eq!(reason(&fixture, &row), "lane::session-cross-lane");
    finish(fixture);
}

#[test]
fn resume_refuses_a_session_head_declared_in_another_worktree() {
    let fixture = fixture();
    let mut row = row(&fixture);
    head(&mut row).worktree = "/worktrees/elsewhere".into();
    assert_eq!(reason(&fixture, &row), "lane::session-wrong-worktree");
    finish(fixture);
}

#[test]
fn resume_refuses_a_session_head_declared_by_another_vendor() {
    let fixture = fixture();
    let mut row = row(&fixture);
    head(&mut row).vendor = "gemini".parse().expect("fixture vendor");
    assert_eq!(reason(&fixture, &row), "lane::session-cross-vendor");
    finish(fixture);
}

/// Reached only from the seat and recall surfaces: predecessor import faults on
/// an empty lineage of its own before it ever asks for a resume.
#[test]
fn resume_refuses_a_durable_row_that_lost_its_occupant_team() {
    let fixture = fixture();
    let mut row = row(&fixture);
    row.lineage.clear();
    assert_eq!(reason(&fixture, &row), "lane::session-lineage-missing");
    finish(fixture);
}

#[test]
fn resume_refuses_a_session_head_declared_by_an_earlier_occupant() {
    let fixture = fixture();
    let mut row = row(&fixture);
    row.lineage.push(uuid(9));
    assert_eq!(reason(&fixture, &row), "lane::session-cross-occupant");
    finish(fixture);
}

#[test]
fn resume_refuses_a_session_head_declared_off_the_selected_stream() {
    let fixture = fixture();
    let mut row = row(&fixture);
    head(&mut row).stream = "stderr".into();
    assert_eq!(reason(&fixture, &row), "lane::session-wrong-stream");
    finish(fixture);
}

#[test]
fn resume_refuses_a_vendor_with_no_declaration_dialect() {
    let fixture = fixture();
    let mut row = row(&fixture);
    row.vendor = "gemini".parse().expect("fixture vendor");
    head(&mut row).vendor = row.vendor.clone();
    assert_eq!(reason(&fixture, &row), "lane::session-vendor-unsupported");
    finish(fixture);
}

#[test]
fn resume_refuses_a_head_whose_lane_tape_is_no_longer_present() {
    let fixture = fixture();
    let row = row(&fixture);
    assert_eq!(reason(&fixture, &row), "lane::session-tape-moved");
    finish(fixture);
}

#[test]
fn resume_refuses_a_locus_reaching_past_the_tape_that_holds_it() {
    let fixture = fixture();
    let mut row = row(&fixture);
    declare(&fixture, &mut row, &declaration(IDENTITY));
    head(&mut row).locus.length += 1;
    assert_eq!(reason(&fixture, &row), "lane::session-tape-moved");
    finish(fixture);
}

/// A locus that slid onto a neighbouring record still reads and still decodes.
/// Only the declaration it was supposed to name is gone.
#[test]
fn resume_refuses_a_locus_naming_a_record_that_declares_no_session() {
    let fixture = fixture();
    let mut row = row(&fixture);
    declare(&fixture, &mut row, br#"{"type":"agent_start"}"#);
    let refusal = refusal(&fixture, &row);
    assert_eq!(refusal.reason(), "lane::session-tape-moved");
    assert_eq!(
        refusal
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::observed),
        None
    );
    finish(fixture);
}

#[test]
fn resume_refuses_a_locus_declaring_an_identity_the_head_never_named() {
    let fixture = fixture();
    let mut row = row(&fixture);
    declare(
        &fixture,
        &mut row,
        &declaration("018f0000-0000-7000-8000-000000000000"),
    );
    let refusal = refusal(&fixture, &row);
    assert_eq!(refusal.reason(), "lane::session-tape-moved");
    assert_eq!(
        refusal
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::observed),
        Some("018f0000-0000-7000-8000-000000000000")
    );
    finish(fixture);
}

/// The head and the tape can agree on an identity no vendor can be resumed
/// under, which the durable record admits and the vendor identity refuses.
#[test]
fn resume_refuses_an_agreed_identity_no_vendor_can_resume_under() {
    let fixture = fixture();
    let mut row = row(&fixture);
    head(&mut row).identity = String::new();
    declare(&fixture, &mut row, &declaration(""));
    assert_eq!(reason(&fixture, &row), "lane::session-identity-invalid");
    finish(fixture);
}

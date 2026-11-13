use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use listen::{
    Dialect, EventKind, Mode, Observation, RetroSelector, Span, StreamOutcome, SurfaceError,
    SurfaceEvent, SurfaceReasoning, SurfaceResidue, SurfaceToolMoment, SurfaceToolResult,
    TailBudget, Watch, WatchDeltaRow, retro,
};
use tape::Tape;

const PROSE: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"working"}],"stopReason":"stop"}}"#;
const REASONING: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"inspect"}],"stopReason":"stop"}}"#;
const READY: &[u8] = br#"{"type":"ready"}"#;
const DIALECTS: &[(&str, Dialect)] = &[("omp", Dialect::Omp), ("fixture", Dialect::Omp)];
fn watch_budget() -> TailBudget {
    TailBudget::new(
        1024 * 1024,
        16 * 1024 * 1024,
        1_000,
        256,
        Duration::from_secs(1),
    )
    .expect("surface fixture tail budget is valid")
}

fn poll_watch(watch: &mut Watch) -> Result<StreamOutcome, SurfaceError> {
    let delta = watch.poll_bounded(watch_budget())?;
    let mut root = PathBuf::new();
    let mut observations = Vec::new();
    for row in delta.rows {
        root = row.outcome.root;
        observations.extend(row.outcome.observations);
    }
    watch.resume_at(delta.cursor);
    Ok(StreamOutcome {
        mode: Mode::Watch,
        root,
        observations,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct Entry {
    directory: bool,
    modified: SystemTime,
    bytes: Vec<u8>,
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "spice-listen-surface-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("scratch root is exclusively created");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn remove(self) {
        fs::remove_dir_all(&self.0).expect("scratch root is explicitly removed");
        assert!(!self.0.exists(), "scratch root outlived removal");
        std::mem::forget(self);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(bytes: &[u8]) -> Vec<u8> {
    let mut record = bytes.to_vec();
    record.push(b'\n');
    record
}

fn session(root: &Path, lane: &str, dialect: &str) -> (PathBuf, Tape) {
    let path = root.join(lane).join(dialect);
    let tape = Tape::open(&path).expect("fixture tape opens");
    (path, tape)
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn walk(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Entry>) {
        let mut children = fs::read_dir(path)
            .expect("snapshot directory reads")
            .collect::<Result<Vec<_>, _>>()
            .expect("snapshot entries read");
        children.sort_by_key(std::fs::DirEntry::path);
        for child in children {
            let path = child.path();
            let metadata = child.metadata().expect("snapshot metadata reads");
            let directory = metadata.is_dir();
            let bytes = if directory {
                Vec::new()
            } else {
                fs::read(&path).expect("snapshot file reads")
            };
            entries.insert(
                path.strip_prefix(root)
                    .expect("snapshot path is beneath root")
                    .to_path_buf(),
                Entry {
                    directory,
                    modified: metadata.modified().expect("fixture has modification time"),
                    bytes,
                },
            );
            if directory {
                walk(root, &path, entries);
            }
        }
    }

    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries);
    entries
}

fn row_event_kinds<'a>(rows: impl IntoIterator<Item = &'a WatchDeltaRow>) -> Vec<EventKind> {
    rows.into_iter()
        .flat_map(|row| &row.outcome.observations)
        .filter_map(|observation| match observation {
            Observation::Event { event, .. } => Some(event.kind()),
            _ => None,
        })
        .collect()
}

fn event_count(observations: &[Observation]) -> usize {
    observations
        .iter()
        .filter(|observation| matches!(observation, Observation::Event { .. }))
        .count()
}

#[test]
fn a_third_declared_name_changes_discovery_only_through_the_supplied_value() {
    let scratch = Scratch::new("third-declaration");
    let (path, tape) = session(scratch.path(), "lane-a", "fixture");
    let mut bytes = PROSE.to_vec();
    bytes.push(b'\n');
    tape.tee(&bytes).expect("fixture record is durable");

    let replay = retro(scratch.path(), RetroSelector::Path(path), DIALECTS)
        .expect("third declaration replays");
    assert_eq!(event_count(&replay.observations), 1);
}

#[test]
fn only_root_lane_vendor_directories_are_sessions() {
    let scratch = Scratch::new("fixed-depth");
    let (_, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(&record(PROSE)).expect("fixture record is durable");
    // Source trees under the root may name a module after a vendor; such a
    // directory has no tape identity and must not be adopted as a session.
    let nested = scratch.path().join("target/copy/vendor/src/omp");
    fs::create_dir_all(&nested).expect("nested vendor-named directory is created");
    fs::write(nested.join("lib.rs"), b"// not a tape\n").expect("nested source file is written");

    let replay = retro(
        scratch.path(),
        RetroSelector::Path(scratch.path().to_path_buf()),
        DIALECTS,
    )
    .expect("a vendor-named directory below the session depth is not scanned");
    assert_eq!(event_count(&replay.observations), 1);
    assert!(
        replay
            .observations
            .iter()
            .all(|observation| match observation {
                Observation::Event { lane, .. } => lane == "lane-a",
                _ => true,
            })
    );
}

#[test]
fn watch_heals_missed_wakes_by_comparing_durable_cursors_across_growth_and_sealing() {
    let scratch = Scratch::new("watch");
    let (_, tape) = session(scratch.path(), "lane-a", "omp");
    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());

    let empty = poll_watch(&mut watch).expect("empty session is observed");
    assert!(matches!(
        empty.observations.as_slice(),
        [Observation::Absent { .. }]
    ));

    tape.tee(&record(PROSE))
        .expect("first complete record is durable");
    let first = poll_watch(&mut watch).expect("first cursor delta reads");
    assert_eq!(event_count(&first.observations), 1);
    assert!(first.observations.iter().any(|observation| matches!(
        observation,
        Observation::Canary {
            expected: EventKind::Reasoning,
            ..
        }
    )));
    assert!(
        poll_watch(&mut watch)
            .expect("an unchanged cursor reconciles")
            .is_empty()
    );

    let complete = record(REASONING);
    let split = complete.len() / 2;
    tape.tee(&complete[..split])
        .expect("partial record prefix is durable");
    let partial = poll_watch(&mut watch).expect("partial record is visible");
    assert!(matches!(
        partial.observations.as_slice(),
        [Observation::Pending { .. }]
    ));
    tape.tee(&complete[split..])
        .expect("record suffix is durable without a separate watch wake");
    let completed = poll_watch(&mut watch).expect("cursor comparison heals the missed wake");
    assert_eq!(event_count(&completed.observations), 1);

    tape.tee(&record(PROSE))
        .expect("first unobserved record is durable");
    tape.tee(&record(REASONING))
        .expect("second unobserved record is durable");
    assert_eq!(
        event_count(
            &poll_watch(&mut watch)
                .expect("one reconciliation reads both missed records")
                .observations
        ),
        2
    );

    tape.seal().expect("first member seals");
    tape.tee(&record(PROSE))
        .expect("next member begins after rotation");
    assert_eq!(
        event_count(
            &poll_watch(&mut watch)
                .expect("cursor crosses the sealed member boundary")
                .observations
        ),
        1
    );

    scratch.remove();
}

#[test]
fn watch_reports_unchanged_status_once_per_run_and_again_after_it_changes() {
    let scratch = Scratch::new("watch-status");
    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());
    let absent = |outcome: &StreamOutcome, path: &Path| {
        matches!(
            outcome.observations.as_slice(),
            [Observation::Absent { selector }] if Path::new(selector) == path
        )
    };

    let root = poll_watch(&mut watch).expect("empty root reconciles");
    assert!(absent(&root, scratch.path()));
    assert!(
        poll_watch(&mut watch)
            .expect("empty root repeats")
            .is_empty()
    );

    let (tape_path, tape) = session(scratch.path(), "lane-a", "omp");
    let empty = poll_watch(&mut watch).expect("empty tape reconciles");
    assert!(absent(&empty, &tape_path));
    assert!(
        poll_watch(&mut watch)
            .expect("empty tape repeats")
            .is_empty()
    );

    let complete = record(REASONING);
    let split = complete.len() / 2;
    tape.tee(&complete[..split])
        .expect("partial record prefix is durable");
    let pending = poll_watch(&mut watch).expect("partial record is visible");
    let [Observation::Pending { spans: first, .. }] = pending.observations.as_slice() else {
        panic!("one pending status row: {pending:?}");
    };
    assert!(poll_watch(&mut watch).expect("pending repeats").is_empty());

    let encoded = wire::encode(watch.cursor()).expect("cursor encodes");
    let mut resumed = Watch::resume(
        scratch.path(),
        DIALECTS.iter().copied(),
        wire::decode(&encoded).expect("cursor decodes"),
    );
    assert_eq!(
        poll_watch(&mut resumed).expect("resumed run reconciles"),
        pending
    );
    assert!(
        poll_watch(&mut resumed)
            .expect("resumed repeats")
            .is_empty()
    );

    tape.tee(&complete[split..=split])
        .expect("pending prefix grows");
    let grown = poll_watch(&mut watch).expect("grown prefix is visible");
    assert!(matches!(
        grown.observations.as_slice(),
        [Observation::Pending { spans, .. }] if spans != first
    ));

    drop(tape);
    fs::remove_dir_all(scratch.path().join("lane-a")).expect("session is removed");
    let vanished = poll_watch(&mut watch).expect("vanished session reconciles");
    assert!(matches!(
        vanished.observations.as_slice(),
        [Observation::Missing { .. }, Observation::Absent { selector }]
            if Path::new(selector) == scratch.path()
    ));
    let (_, _tape) = session(scratch.path(), "lane-a", "omp");
    let recreated = poll_watch(&mut watch).expect("recreated session reconciles");
    assert!(absent(&recreated, &tape_path));

    scratch.remove();
}

#[test]
fn a_record_that_surfaces_nothing_advances_the_cursor_without_a_row() {
    let scratch = Scratch::new("watch-silent");
    let (_, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(&record(PROSE)).expect("first record is durable");
    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());
    assert_eq!(
        event_count(&poll_watch(&mut watch).expect("first record").observations),
        1
    );

    tape.tee(&record(READY)).expect("silent record is durable");
    let silent = watch
        .poll_bounded(watch_budget())
        .expect("silent record reconciles");
    assert!(silent.rows.is_empty());
    assert_ne!(&silent.cursor, watch.cursor());
    watch.resume_at(silent.cursor);

    for bytes in [REASONING, READY, PROSE] {
        tape.tee(&record(bytes)).expect("record is durable");
    }
    let delta = watch
        .poll_bounded(watch_budget())
        .expect("mixed records reconcile");
    assert_eq!(delta.rows.len(), 2);
    assert_eq!(
        row_event_kinds(&delta.rows),
        vec![EventKind::Reasoning, EventKind::Prose]
    );
    for (index, row) in delta.rows.iter().enumerate() {
        let replay = Watch::resume(scratch.path(), DIALECTS.iter().copied(), row.cursor.clone())
            .poll_bounded(watch_budget())
            .expect("resume from an emitted cursor reconciles");
        assert_eq!(
            row_event_kinds(&replay.rows),
            row_event_kinds(&delta.rows[index + 1..])
        );
    }

    scratch.remove();
}

#[test]
fn watch_and_retro_mark_reasoning_without_its_text() {
    let scratch = Scratch::new("reasoning-withheld");
    let (path, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(&record(REASONING))
        .expect("reasoning record is durable");
    let watched = poll_watch(&mut Watch::new(scratch.path(), DIALECTS.iter().copied()))
        .expect("watch reads the reasoning record");
    let replay = retro(scratch.path(), RetroSelector::Path(path), DIALECTS)
        .expect("retro reads the reasoning record");

    for outcome in [watched, replay] {
        let reasoning: Vec<_> = outcome
            .observations
            .iter()
            .filter_map(|observation| match observation {
                Observation::Event {
                    event: SurfaceEvent::Reasoning { content },
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, vec![SurfaceReasoning::Withheld { bytes: 7 }]);
        let bytes = wire::encode(&outcome).expect("stream outcome encodes");
        assert!(!String::from_utf8_lossy(&bytes).contains("inspect"));
    }

    scratch.remove();
}

#[test]
fn a_resumed_watch_emits_only_records_after_its_durable_cursor() {
    let scratch = Scratch::new("watch-resume");
    let (_, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(&record(PROSE)).expect("first record is durable");

    let first = Watch::new(scratch.path(), DIALECTS.iter().copied())
        .poll_bounded(watch_budget())
        .expect("first bounded snapshot succeeds");
    assert_eq!(
        first
            .rows
            .iter()
            .flat_map(|row| &row.outcome.observations)
            .filter(|observation| matches!(observation, Observation::Event { .. }))
            .count(),
        1
    );
    let cursor = first
        .rows
        .last()
        .expect("first snapshot has a resumable row")
        .cursor
        .clone();

    let resumed = Watch::resume(scratch.path(), DIALECTS.iter().copied(), cursor);
    assert!(
        resumed
            .poll_bounded(watch_budget())
            .expect("unchanged resumed source reconciles")
            .rows
            .is_empty()
    );
    tape.tee(&record(REASONING))
        .expect("second record is durable");
    let next = resumed
        .poll_bounded(watch_budget())
        .expect("resumed source reads new records");
    let events = next
        .rows
        .iter()
        .flat_map(|row| &row.outcome.observations)
        .filter_map(|observation| match observation {
            Observation::Event { event, .. } => Some(event.kind()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(events, vec![EventKind::Reasoning]);

    scratch.remove();
}

#[test]
fn watch_reports_replacement_and_resumes_from_the_new_generation() {
    let scratch = Scratch::new("watch-replacement");
    let (path, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(&record(PROSE))
        .expect("first-generation record is durable");
    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());
    assert_eq!(
        event_count(
            &poll_watch(&mut watch)
                .expect("first generation is observed")
                .observations
        ),
        1
    );

    drop(tape);
    fs::remove_dir_all(&path).expect("first generation is removed");
    let replacement = Tape::open(&path).expect("replacement generation opens");
    replacement
        .tee(&record(REASONING))
        .expect("replacement record is durable");
    let observed = poll_watch(&mut watch).expect("replacement generation reconciles");
    assert!(
        observed
            .observations
            .iter()
            .any(|observation| matches!(observation, Observation::Missing { .. }))
    );
    assert!(observed.observations.iter().any(|observation| matches!(
        observation,
        Observation::Event { event, .. } if event.kind() == EventKind::Reasoning
    )));

    scratch.remove();
}

#[test]
fn a_bounded_snapshot_reports_history_outside_its_tail_window() {
    let scratch = Scratch::new("watch-skipped");
    let (_, tape) = session(scratch.path(), "lane-a", "omp");
    for _ in 0..20 {
        tape.tee(&record(PROSE)).expect("history record is durable");
    }
    let budget = TailBudget::new(64, 16 * 1024 * 1024, 8, 32, Duration::from_secs(1))
        .expect("small fixture budget is valid");
    let delta = Watch::new(scratch.path(), DIALECTS.iter().copied())
        .poll_bounded(budget)
        .expect("bounded snapshot succeeds");
    assert!(delta.rows.iter().any(|row| {
        row.outcome
            .observations
            .iter()
            .any(|observation| matches!(observation, Observation::Missing { .. }))
    }));

    scratch.remove();
}

#[test]
fn retro_accounts_for_partial_malformed_missing_and_ambiguous_spans() {
    let scratch = Scratch::new("retro");
    let (omp_tape, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(b"not-json\n")
        .expect("malformed complete record is durable");
    tape.tee(b"{\"type\":\"message_end\"")
        .expect("partial trailing record is durable");

    let replay = retro(
        scratch.path(),
        RetroSelector::Path(omp_tape.clone()),
        DIALECTS,
    )
    .expect("path replay reads preserved tape");
    assert_eq!(replay.mode, Mode::Retro);
    assert!(replay.observations.iter().any(|observation| matches!(
        observation,
        Observation::Residue {
            residue: SurfaceResidue::Malformed { .. },
            ..
        }
    )));
    assert!(replay.observations.iter().any(|observation| matches!(
        observation,
        Observation::Residue {
            residue: SurfaceResidue::Truncated {},
            ..
        }
    )));

    let missing_lane = retro(
        scratch.path(),
        RetroSelector::Lane("lane-missing".to_owned()),
        DIALECTS,
    )
    .expect("missing lane is accounted");
    assert!(matches!(
        missing_lane.observations.as_slice(),
        [Observation::Missing { .. }]
    ));

    let missing_locus = retro(
        scratch.path(),
        RetroSelector::Locus(Span {
            member: 9,
            offset: 0,
            length: 1,
        }),
        DIALECTS,
    )
    .expect("missing locus is accounted");
    assert!(matches!(
        missing_locus.observations.as_slice(),
        [Observation::Missing { .. }]
    ));

    let (_, fixture) = session(scratch.path(), "lane-b", "fixture");
    fixture
        .tee(b"{\"type\":\"agent_start\"}\n")
        .expect("second session record is durable");
    let ambiguous = retro(
        scratch.path(),
        RetroSelector::Locus(Span {
            member: 1,
            offset: 0,
            length: 1,
        }),
        DIALECTS,
    )
    .expect("ambiguous locus is accounted");
    assert!(matches!(
        ambiguous.observations.as_slice(),
        [Observation::Ambiguous { candidates, .. }]
            if candidates == &vec![omp_tape, scratch.path().join("lane-b/fixture")]
    ));

    scratch.remove();
}

#[test]
fn watch_and_retro_leave_authority_and_discovery_bytes_unchanged() {
    let scratch = Scratch::new("readonly");
    let (omp_tape, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(&record(PROSE)).expect("fixture record is durable");
    for name in ["journal", "claims", "heads", "discovery"] {
        fs::write(scratch.path().join(name), format!("sentinel:{name}\n"))
            .expect("authority sentinel writes");
    }
    let before = snapshot(scratch.path());

    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());
    assert_eq!(
        event_count(&poll_watch(&mut watch).expect("watch reads").observations),
        1
    );
    let replay =
        retro(scratch.path(), RetroSelector::Path(omp_tape), DIALECTS).expect("retro reads");
    assert_eq!(event_count(&replay.observations), 1);

    assert_eq!(snapshot(scratch.path()), before);

    let missing = scratch.path().join("never-created/omp");
    let mut missing_watch = Watch::new(&missing, DIALECTS.iter().copied());
    assert!(matches!(
        poll_watch(&mut missing_watch)
            .expect("missing watch root is accounted")
            .observations
            .as_slice(),
        [Observation::Absent { .. }]
    ));
    assert!(matches!(
        retro(
            scratch.path(),
            RetroSelector::Path(missing.clone()),
            DIALECTS
        )
        .expect("missing retro path is accounted")
        .observations
        .as_slice(),
        [Observation::Missing { .. }]
    ));
    assert!(
        !missing.exists(),
        "observation never creates a selected path"
    );

    scratch.remove();
}

#[test]
fn retro_selector_grammar_never_falls_through_to_another_meaning() {
    assert_eq!(
        RetroSelector::parse("2:17:31").expect("compact locus parses"),
        RetroSelector::Locus(Span {
            member: 2,
            offset: 17,
            length: 31
        })
    );
    assert_eq!(
        RetroSelector::parse("member 2 offset 17 length 31").expect("long locus parses"),
        RetroSelector::Locus(Span {
            member: 2,
            offset: 17,
            length: 31
        })
    );
    assert_eq!(
        RetroSelector::parse("missing/path").expect("path syntax parses despite absence"),
        RetroSelector::Path(PathBuf::from("missing/path"))
    );
    assert_eq!(
        RetroSelector::parse("lane-a").expect("lane parses"),
        RetroSelector::Lane("lane-a".to_owned())
    );
    assert!(RetroSelector::parse("one:two:three").is_err());
    assert!(RetroSelector::parse("1:2").is_err());
    assert!(RetroSelector::parse("1:2:3:4").is_err());
    assert!(RetroSelector::parse("member 2 offset nope length 31").is_err());
}

#[test]
fn records_keep_one_typed_identity_across_sealed_member_boundaries() {
    let scratch = Scratch::new("cross-member");
    let (omp_tape, tape) = session(scratch.path(), "lane-a", "omp");
    let complete = record(PROSE);
    let first = complete.len() / 3;
    let second = first * 2;

    tape.tee(&complete[..first])
        .expect("first record fragment is durable");
    tape.seal().expect("first fragment member seals");
    tape.tee(&complete[first..second])
        .expect("second record fragment is durable");
    tape.seal().expect("second fragment member seals");
    tape.tee(&complete[second..])
        .expect("record delimiter is durable in the active member");

    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());
    let watched = poll_watch(&mut watch).expect("watch joins all three fragments");
    let events = watched
        .observations
        .iter()
        .filter_map(|observation| match observation {
            Observation::Event { spans, event, .. } => Some((spans, event)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1.kind(), EventKind::Prose);
    assert_eq!(
        events[0].0,
        &vec![
            Span {
                member: 1,
                offset: 0,
                length: first as u64,
            },
            Span {
                member: 2,
                offset: 0,
                length: (second - first) as u64,
            },
            Span {
                member: 3,
                offset: 0,
                length: (complete.len() - second) as u64,
            },
        ]
    );
    assert!(watched.observations.iter().all(|observation| !matches!(
        observation,
        Observation::Residue { .. } | Observation::Pending { .. }
    )));
    assert!(
        poll_watch(&mut watch)
            .expect("cursor advances once")
            .is_empty()
    );

    let replay = retro(scratch.path(), RetroSelector::Path(omp_tape), DIALECTS)
        .expect("retro joins the same fragments");
    assert_eq!(event_count(&replay.observations), 1);
    assert!(replay.observations.iter().all(|observation| !matches!(
        observation,
        Observation::Residue { .. } | Observation::Pending { .. }
    )));

    scratch.remove();
}

#[test]
fn a_sealed_prefix_remains_pending_until_the_active_member_completes_it() {
    let scratch = Scratch::new("sealed-prefix");
    let (_, tape) = session(scratch.path(), "lane-a", "omp");
    let complete = record(REASONING);
    let split = complete.len() / 2;
    tape.tee(&complete[..split])
        .expect("record prefix is durable");
    tape.seal().expect("record prefix seals");

    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());
    let pending = poll_watch(&mut watch).expect("sealed prefix remains pending");
    assert!(matches!(
        pending.observations.as_slice(),
        [Observation::Pending { spans, .. }] if spans == &vec![Span {
            member: 1,
            offset: 0,
            length: split as u64,
        }]
    ));

    tape.tee(&complete[split..])
        .expect("active member completes the record");
    let completed = poll_watch(&mut watch).expect("completion joins the sealed prefix");
    assert_eq!(event_count(&completed.observations), 1);
    assert!(completed.observations.iter().any(|observation| matches!(
        observation,
        Observation::Event { spans, event, .. }
            if event.kind() == EventKind::Reasoning && spans.len() == 2
    )));
    assert!(completed.observations.iter().all(|observation| !matches!(
        observation,
        Observation::Residue { .. } | Observation::Pending { .. }
    )));

    scratch.remove();
}

#[test]
fn a_newline_before_rotation_keeps_the_next_split_record_distinct() {
    let scratch = Scratch::new("newline-rotation");
    let (_, tape) = session(scratch.path(), "lane-a", "omp");
    let first = record(PROSE);
    let second = record(REASONING);
    let split = second.len() / 2;
    let mut member = first.clone();
    member.extend_from_slice(&second[..split]);
    tape.tee(&member)
        .expect("whole first record and second prefix are durable");
    tape.seal().expect("mixed member seals");
    tape.tee(&second[split..])
        .expect("second record completes after rotation");

    let mut watch = Watch::new(scratch.path(), DIALECTS.iter().copied());
    let observed = poll_watch(&mut watch).expect("both records decode once");
    let events = observed
        .observations
        .iter()
        .filter_map(|observation| match observation {
            Observation::Event { spans, event, .. } => Some((spans, event.kind())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].1, EventKind::Prose);
    assert_eq!(events[0].0.len(), 1);
    assert_eq!(events[1].1, EventKind::Reasoning);
    assert_eq!(events[1].0.len(), 2);
    assert!(observed.observations.iter().all(|observation| !matches!(
        observation,
        Observation::Residue { .. } | Observation::Pending { .. }
    )));

    scratch.remove();
}

#[test]
fn finite_retro_accounts_one_terminal_partial_over_every_member() {
    let scratch = Scratch::new("terminal-partial");
    let (omp_tape, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(b"{\"type\":")
        .expect("first incomplete fragment is durable");
    tape.seal().expect("first incomplete fragment seals");
    tape.tee(b"\"message_end\"")
        .expect("second incomplete fragment is durable");

    let replay = retro(scratch.path(), RetroSelector::Path(omp_tape), DIALECTS)
        .expect("finite retro accounts the terminal fragment");
    assert!(matches!(
        replay.observations.as_slice(),
        [Observation::Residue {
            spans,
            residue: SurfaceResidue::Truncated {},
            ..
        }] if spans == &vec![
            Span { member: 1, offset: 0, length: 8 },
            Span { member: 2, offset: 0, length: 13 },
        ]
    ));

    scratch.remove();
}

#[test]
fn retro_backfills_tool_media_from_durable_tape_even_when_the_call_is_missing() {
    let scratch = Scratch::new("tool-media-backfill");
    let (omp_tape, tape) = session(scratch.path(), "lane-a", "omp");
    let result = serde_json::to_vec(&serde_json::json!({
        "type": "message_end",
        "message": {
            "role": "toolResult",
            "toolCallId": "call-image",
            "toolName": "read",
            "content": [{
                "type": "image",
                "data": "iVBORw0KGgo=",
                "mimeType": "image/png",
            }],
            "isError": false,
        },
    }))
    .expect("tool image result encodes");
    tape.tee(&record(&result))
        .expect("tool image result is durable");

    let replay = retro(scratch.path(), RetroSelector::Path(omp_tape), DIALECTS)
        .expect("finite replay reads the durable result");
    assert!(matches!(
        replay.observations.as_slice(),
        [Observation::Event {
            event: SurfaceEvent::Tool {
                call_id,
                moment: SurfaceToolMoment::Result {
                    standing: SurfaceToolResult::Succeeded,
                    media,
                    ..
                },
            },
            ..
        }] if call_id == "call-image"
            && media.len() == 1
            && media[0].identity().is_ok()
    ));

    scratch.remove();
}

#[test]
fn retro_backfills_delivered_directive_media_beside_its_authored_message() {
    let scratch = Scratch::new("directive-media-backfill");
    let (omp_tape, tape) = session(scratch.path(), "lane-a", "omp");
    let name = r"operator](capture)\雪.png";
    let media = wire::Media::from_base64(name, "image/png", "iVBORw0KGgo=")
        .expect("fixture attachment admits");
    let text = format!(
        "<1kJnbzxxx>\ndirective=018f82d4-6f59-7d06-92e3-bce9131163af \
         key=1kBsk001x stage=published expires=2026-08-01T06:30:00.000Z\n\
         inspect this capture\n\n{}\n</1kJnbzxxx>\n",
        media.markdown().expect("fixture Markdown renders"),
    );
    let message = serde_json::to_vec(&serde_json::json!({
        "type": "message_end",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}],
        },
    }))
    .expect("delivered directive transcript record encodes");
    tape.tee(&record(&message))
        .expect("delivered directive is durable");

    let replay = retro(scratch.path(), RetroSelector::Path(omp_tape), DIALECTS)
        .expect("finite replay reads the authored directive");
    assert!(matches!(
        replay.observations.as_slice(),
        [Observation::Event {
            spans,
            event: SurfaceEvent::Prose {
                text: observed,
                media,
                ..
            },
            ..
        }] if observed == &text
            && spans == &vec![Span { member: 1, offset: 0, length: u64::try_from(message.len() + 1).expect("fixture record length fits") }]
            && media.len() == 1
            && media[0].name == name
            && media[0].identity().is_ok()
    ));

    scratch.remove();
}

#[test]
fn every_surface_event_has_one_closed_canonical_wire_shape() {
    let events = vec![
        SurfaceEvent::Session {
            identity: "0192f806-a22e-708b-bef1-0573a7d27786".to_owned(),
        },
        SurfaceEvent::Prose {
            text: "hello".to_owned(),
            phase: Some("final".to_owned()),
            media: Vec::new(),
        },
        SurfaceEvent::Reasoning {
            content: SurfaceReasoning::Withheld { bytes: 7 },
        },
        SurfaceEvent::Tool {
            call_id: "call-1".to_owned(),
            moment: SurfaceToolMoment::Call {
                name: "shell".to_owned(),
                arguments: "{}".to_owned(),
            },
        },
        SurfaceEvent::Tool {
            call_id: "call-1".to_owned(),
            moment: SurfaceToolMoment::Update {
                name: "shell".to_owned(),
                arguments: "{}".to_owned(),
            },
        },
        SurfaceEvent::Tool {
            call_id: "call-1".to_owned(),
            moment: SurfaceToolMoment::Result {
                standing: SurfaceToolResult::Succeeded,
                output: None,
                exit_code: None,
                media: Vec::new(),
            },
        },
        SurfaceEvent::Tool {
            call_id: "call-2".to_owned(),
            moment: SurfaceToolMoment::Result {
                standing: SurfaceToolResult::Failed,
                output: None,
                exit_code: None,
                media: Vec::new(),
            },
        },
        SurfaceEvent::Tool {
            call_id: "call-3".to_owned(),
            moment: SurfaceToolMoment::Result {
                standing: SurfaceToolResult::Unreported,
                output: None,
                exit_code: None,
                media: Vec::new(),
            },
        },
        SurfaceEvent::Usage {
            turns: 1,
            input_tokens: 1,
            cached_input_tokens: 2,
            cache_write_input_tokens: 3,
            output_tokens: 4,
            reasoning_output_tokens: Some(5),
        },
        SurfaceEvent::Compaction {},
        SurfaceEvent::Failure {
            message: "failed".to_owned(),
        },
    ];
    assert_eq!(
        events.iter().map(SurfaceEvent::kind).collect::<Vec<_>>(),
        vec![
            EventKind::Session,
            EventKind::Prose,
            EventKind::Reasoning,
            EventKind::Tool,
            EventKind::Tool,
            EventKind::Tool,
            EventKind::Tool,
            EventKind::Tool,
            EventKind::Usage,
            EventKind::Compaction,
            EventKind::Failure,
        ]
    );
    for event in events {
        let bytes = wire::encode(&event).expect("surface event encodes");
        let decoded: SurfaceEvent = wire::decode(&bytes).expect("surface event decodes");
        assert_eq!(decoded, event);
        assert_eq!(wire::encode(&decoded).expect("event re-encodes"), bytes);
    }
}

#[test]
fn stream_identity_refuses_missing_extra_and_unknown_nested_shapes() {
    let outcome = StreamOutcome {
        mode: Mode::Watch,
        root: PathBuf::from("root"),
        observations: vec![Observation::Event {
            lane: "lane-a".to_owned(),
            dialect: Dialect::Omp,
            tape: PathBuf::from("root/lane-a/omp"),
            spans: vec![Span {
                member: 1,
                offset: 2,
                length: 3,
            }],
            event: SurfaceEvent::Prose {
                text: "hello".to_owned(),
                phase: None,
                media: Vec::new(),
            },
        }],
    };
    let bytes = wire::encode(&outcome).expect("stream outcome encodes");
    let canonical: serde_json::Value =
        serde_json::from_slice(&bytes).expect("canonical outcome is JSON");
    assert!(canonical["observations"][0]["event"].get("phase").is_none());
    let decoded: StreamOutcome = wire::decode(&bytes).expect("stream outcome decodes");
    assert_eq!(decoded, outcome);
    assert_eq!(wire::encode(&decoded).expect("outcome re-encodes"), bytes);

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    value["extra"] = true.into();
    assert!(wire::decode::<StreamOutcome>(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    value
        .as_object_mut()
        .expect("outcome is an object")
        .remove("mode");
    assert!(wire::decode::<StreamOutcome>(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    value["observations"][0]["spans"][0]["extra"] = true.into();
    assert!(wire::decode::<StreamOutcome>(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    value["observations"][0]["event"]["extra"] = true.into();
    assert!(wire::decode::<StreamOutcome>(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    value["observations"][0]["event"]
        .as_object_mut()
        .expect("event is an object")
        .remove("text");
    assert!(wire::decode::<StreamOutcome>(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    value["observations"][0]["event"]["kind"] = "future".into();
    assert!(wire::decode::<StreamOutcome>(&serde_json::to_vec(&value).unwrap()).is_err());

    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
    value["observations"][0]["event"]["phase"] = serde_json::Value::Null;
    assert!(wire::decode::<StreamOutcome>(&serde_json::to_vec(&value).unwrap()).is_err());

    let usage = SurfaceEvent::Usage {
        turns: 1,
        input_tokens: 1,
        cached_input_tokens: 2,
        cache_write_input_tokens: 3,
        output_tokens: 4,
        reasoning_output_tokens: None,
    };
    let usage_bytes = wire::encode(&usage).expect("usage without reasoning tokens encodes");
    let mut usage_value: serde_json::Value =
        serde_json::from_slice(&usage_bytes).expect("canonical usage is JSON");
    assert!(usage_value.get("reasoning_output_tokens").is_none());
    usage_value["reasoning_output_tokens"] = serde_json::Value::Null;
    assert!(wire::decode::<SurfaceEvent>(&serde_json::to_vec(&usage_value).unwrap()).is_err());
}

#[test]
fn every_closed_surface_vocabulary_refuses_unknown_variants() {
    assert!(wire::decode::<Dialect>(br#""future""#).is_err());
    assert!(wire::decode::<EventKind>(br#""future""#).is_err());
    assert!(wire::decode::<Mode>(br#""future""#).is_err());
    assert!(wire::decode::<Observation>(br#"{"type":"future"}"#).is_err());
    assert!(wire::decode::<SurfaceEvent>(br#"{"kind":"future"}"#).is_err());
    assert!(wire::decode::<SurfaceReasoning>(br#"{"form":"future"}"#).is_err());
    assert!(wire::decode::<SurfaceToolMoment>(br#"{"moment":"future"}"#).is_err());
    assert!(wire::decode::<SurfaceToolResult>(br#""future""#).is_err());
    assert!(wire::decode::<SurfaceResidue>(br#"{"reason":"future"}"#).is_err());
}

#[cfg(unix)]
#[test]
fn an_explicit_session_symlink_is_not_discovered_as_preserved_tape() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new("symlink");
    let (omp_tape, tape) = session(scratch.path(), "lane-a", "omp");
    tape.tee(&record(PROSE))
        .expect("real fixture record is durable");
    let alias = scratch.path().join("lane-b/omp");
    fs::create_dir(alias.parent().expect("alias has a parent")).expect("alias parent exists");
    symlink(&omp_tape, &alias).expect("fixture symlink exists");

    let replay = retro(scratch.path(), RetroSelector::Path(alias), DIALECTS)
        .expect("a symlink is accounted without being followed");
    assert!(matches!(
        replay.observations.as_slice(),
        [Observation::Missing { .. }]
    ));

    scratch.remove();
}

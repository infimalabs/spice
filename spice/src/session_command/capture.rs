//! Immutable live-history bindings over repository-owned selected-record tapes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use wire::{CanonicalUuid, Refusal};

use super::Environment;

const TOKEN_PREFIX: &str = "capture1.";
const WINDOW_BYTES: u64 = 16 * 1024 * 1024;
const SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const RECORDS: usize = 64 * 1024;
const PASSES: usize = 2 * 1024;
const DURATION: Duration = Duration::from_secs(15);

/// Journal authority that selects one lane session and tape generation.
#[derive(Clone, Debug)]
pub(crate) struct Authority {
    session: wire::LaneSession,
    head: wire::TranscriptSourceHead,
}

impl Authority {
    pub(crate) fn new(
        session: wire::LaneSession,
        head: wire::TranscriptSourceHead,
    ) -> Result<Self, String> {
        if session.lane != head.lane
            || session.vendor != head.vendor
            || head.stream != wire::body::TASK_CREATION_CONTEXT_STREAM
        {
            return Err(
                "lane session and transcript head name different capture streams".to_owned(),
            );
        }
        Ok(Self { session, head })
    }
}

/// Exact immutable coordinates carried by generated live-history continuations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    worktree: String,
    vendor: wire::Name,
    stream: String,
    source: CanonicalUuid,
    session: String,
    declaration_member: u64,
    declaration_offset: u64,
    declaration_length: u64,
    through_member: u64,
    through_offset: u64,
}

impl Binding {
    pub(crate) fn encode(&self) -> String {
        let bytes = serde_json::to_vec(self)
            .expect("capture binding has one infallible JSON representation");
        format!(
            "{TOKEN_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        )
    }

    pub(crate) fn decode(token: &str) -> Result<Self, Refusal> {
        let encoded = token.strip_prefix(TOKEN_PREFIX).ok_or_else(|| {
            refusal(
                "session::capture-binding-invalid",
                "binding prefix is not capture1".to_owned(),
                "copy the complete --capture value from a live briefing",
            )
        })?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|error| {
                refusal(
                    "session::capture-binding-invalid",
                    error.to_string(),
                    "copy the complete --capture value from a live briefing",
                )
            })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            refusal(
                "session::capture-binding-invalid",
                error.to_string(),
                "copy the complete --capture value from a live briefing",
            )
        })
    }

    pub(crate) fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    pub(crate) fn session(&self) -> &str {
        &self.session
    }

    pub(crate) fn command_arguments(&self) -> [String; 2] {
        ["--capture".to_owned(), self.encode()]
    }
}

/// One bounded canonical selected-record window and the exact binding that read it.
pub(crate) struct Snapshot {
    pub(crate) binding: Binding,
    pub(crate) records: Vec<listen::Record>,
    pub(crate) withheld_source_bytes: u64,
    pub(crate) source_name: String,
    pub(crate) vendor: super::model::Vendor,
}

/// Read the current generation selected by journal authority and freeze its observed end.
pub(crate) fn current(
    authority: &Authority,
    environment: &Environment,
) -> Result<Snapshot, Refusal> {
    let root = root(
        environment,
        &authority.session.worktree,
        authority.session.lane,
        &authority.session.vendor,
    )?;
    let dialect = dialect(&authority.session.vendor)?;
    let expected = authority.head.source.to_string();
    validate_source(&root, &expected)?;
    validate_declaration(
        &root,
        dialect,
        &authority.session.identity,
        authority.session.locus,
    )?;
    let tail = listen::scan_tail(&root, dialect, budget()).map_err(|error| {
        refusal(
            "session::capture-unreadable",
            error.to_string(),
            "retain the repository tape and retry the bounded live-history read",
        )
    })?;
    if tail.scan.checkpoint.source.as_str() != expected {
        return Err(refusal(
            "session::capture-generation-changed",
            format!("journal={} tape={}", expected, tail.scan.checkpoint.source),
            "refresh the briefing so journal authority can select the replacement generation",
        ));
    }
    let records = records_for_session(tail.scan.records, &authority.session.identity)?;
    Ok(Snapshot {
        binding: Binding {
            lane: authority.session.lane,
            occupant: authority.session.occupant,
            worktree: authority.session.worktree.clone(),
            vendor: authority.session.vendor.clone(),
            stream: authority.head.stream.clone(),
            source: authority.head.source,
            session: authority.session.identity.clone(),
            declaration_member: authority.session.locus.member,
            declaration_offset: authority.session.locus.offset,
            declaration_length: authority.session.locus.length,
            through_member: tail.scan.checkpoint.next.member,
            through_offset: tail.scan.checkpoint.next.offset,
        },
        records,
        withheld_source_bytes: tail.withheld_source_bytes,
        source_name: source_name(authority.session.lane, &authority.session.vendor, &expected),
        vendor: super::model::Vendor::inferred(dialect),
    })
}

/// Reopen one immutable generated continuation without consulting provider state.
pub(crate) fn reopen(binding: Binding, environment: &Environment) -> Result<Snapshot, Refusal> {
    let root = root(
        environment,
        &binding.worktree,
        binding.lane,
        &binding.vendor,
    )?;
    let dialect = dialect(&binding.vendor)?;
    let expected = binding.source.to_string();
    validate_source(&root, &expected)?;
    validate_declaration(
        &root,
        dialect,
        &binding.session,
        wire::TapeLocus {
            member: binding.declaration_member,
            offset: binding.declaration_offset,
            length: binding.declaration_length,
        },
    )?;
    let before = listen::Checkpoint {
        source: expected
            .parse()
            .expect("canonical journal source is a tape source"),
        next: listen::Position {
            member: binding.through_member,
            offset: binding.through_offset,
        },
    };
    let tail = listen::scan_before(&root, dialect, &before, budget()).map_err(|error| {
        refusal(
            "session::capture-continuation-unreadable",
            error.to_string(),
            "refresh the live briefing for a current binding or retain the named tape generation",
        )
    })?;
    let records = records_for_session(tail.scan.records, &binding.session)?;
    Ok(Snapshot {
        source_name: source_name(binding.lane, &binding.vendor, &expected),
        vendor: super::model::Vendor::inferred(dialect),
        binding,
        records,
        withheld_source_bytes: tail.withheld_source_bytes,
    })
}

fn budget() -> listen::TailBudget {
    listen::TailBudget::new(WINDOW_BYTES, SOURCE_BYTES, RECORDS, PASSES, DURATION)
        .expect("live history allowances are positive")
}

fn root(
    environment: &Environment,
    worktree: &str,
    lane: CanonicalUuid,
    vendor: &wire::Name,
) -> Result<PathBuf, Refusal> {
    let cwd = environment.cwd.as_ref().ok_or_else(|| {
        refusal(
            "session::capture-repository-unavailable",
            "current directory is unavailable".to_owned(),
            "run the live-history command from its repository worktree",
        )
    })?;
    let repository = spice::repository::Repository::discover(cwd).map_err(|error| {
        refusal(
            "session::capture-repository-unavailable",
            error.to_string(),
            "run the live-history command from its repository worktree",
        )
    })?;
    if repository.worktree_root() != Path::new(worktree) {
        return Err(refusal(
            "session::capture-worktree-mismatch",
            format!(
                "binding={} repository={}",
                worktree,
                repository.worktree_root().display()
            ),
            "run the generated continuation from the worktree that issued it",
        ));
    }
    let state = repository.admit_state().map_err(|error| {
        refusal(
            "session::capture-state-unavailable",
            error.to_string(),
            "restore the repository-owned Spice state and retry",
        )
    })?;
    Ok(state.lane_tape(lane).join(vendor.as_str()))
}

fn dialect(vendor: &wire::Name) -> Result<listen::Dialect, Refusal> {
    spice::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == vendor.as_str())
        .map(|entry| entry.vendor.session_gate().dialect())
        .ok_or_else(|| {
            refusal(
                "session::capture-vendor-unregistered",
                vendor.to_string(),
                "install the vendor extension that owns this captured stream",
            )
        })
}

fn validate_source(root: &Path, expected: &str) -> Result<(), Refusal> {
    let tape = tape::Tape::open_existing(root).map_err(|error| {
        refusal(
            "session::capture-missing",
            format!("{}: {error}", root.display()),
            "restore the named repository tape; provider session files are not a fallback",
        )
    })?;
    let observed = tape.source_id().map_err(|error| {
        refusal(
            "session::capture-source-unreadable",
            error.to_string(),
            "restore the tape source identity and retry",
        )
    })?;
    if observed.as_str() == expected {
        Ok(())
    } else {
        Err(refusal(
            "session::capture-generation-changed",
            format!("expected={expected} observed={observed}"),
            "refresh the live briefing for the replacement tape generation",
        ))
    }
}

fn validate_declaration(
    root: &Path,
    dialect: listen::Dialect,
    session: &str,
    locus: wire::TapeLocus,
) -> Result<(), Refusal> {
    let tape = tape::Tape::open_existing(root).map_err(|error| {
        refusal(
            "session::capture-missing",
            error.to_string(),
            "restore the named repository tape",
        )
    })?;
    let locus = tape::Locus::new(locus.member, locus.offset, locus.length);
    let bytes = tape.read(locus).map_err(|error| {
        refusal(
            "session::capture-declaration-unreadable",
            error.to_string(),
            "restore the exact session declaration bytes and retry",
        )
    })?;
    let decoded = listen::decode_record(Some(dialect), &bytes, locus).map_err(|error| {
        refusal(
            "session::capture-declaration-invalid",
            error.to_string(),
            "restore the exact selected session declaration record",
        )
    })?;
    let found = match decoded.decoded {
        listen::Decoded::Events { events, .. } => events.into_iter().any(|event| {
            matches!(event, listen::Event::Session(declaration) if declaration.identity == session)
        }),
        listen::Decoded::Residue(_) => false,
    };
    if found {
        Ok(())
    } else {
        Err(refusal(
            "session::capture-session-mismatch",
            format!("locus={locus} session={session}"),
            "refresh from journal authority; do not infer a session from neighboring bytes",
        ))
    }
}

fn records_for_session(
    records: Vec<listen::Record>,
    session: &str,
) -> Result<Vec<listen::Record>, Refusal> {
    let mut latest_fresh = None;
    for (index, record) in records.iter().enumerate() {
        let listen::RecordPayload::Events(events) = &record.payload else {
            continue;
        };
        for event in events {
            if let listen::Event::Session(declaration) = &event.event
                && declaration.fresh
            {
                latest_fresh = Some((index, declaration.identity.as_str()));
            }
        }
    }
    let start = match latest_fresh {
        Some((index, identity)) if identity == session => index,
        Some((_, identity)) => {
            return Err(refusal(
                "session::capture-session-superseded",
                format!("expected={session} newest={identity}"),
                "use the immutable continuation issued before succession or refresh current history",
            ));
        }
        None => 0,
    };
    Ok(records.into_iter().skip(start).collect())
}

fn source_name(lane: CanonicalUuid, vendor: &wire::Name, source: &str) -> String {
    format!("capture:{lane}/{vendor}/{source}")
}

fn refusal(reason: &str, observed: String, recovery: &str) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("session-capture".to_owned()),
            Some("one exact repository-owned selected-record generation".to_owned()),
            Some(observed),
        )
        .with_detail(recovery),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const SESSION: &str = "018f82d4-6f59-7d06-92e3-bce9131163aa";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "spice-session-capture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("capture fixture root");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        worktree: PathBuf,
        tape_root: PathBuf,
        tape: Option<tape::Tape>,
        authority: Authority,
        _scratch: Scratch,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_windows(0)
        }

        fn with_windows(windows: usize) -> Self {
            let scratch = Scratch::new();
            fs::create_dir(scratch.path().join(".git")).expect("fixture repository");
            fs::write(scratch.path().join(wire::TRACKED_POLICY_FILE), "")
                .expect("fixture tracked policy");
            let repository = spice::repository::Repository::discover(scratch.path())
                .expect("fixture repository resolves");
            let worktree = repository.worktree_root().to_path_buf();
            let lane = uuid(1);
            let occupant = uuid(2);
            let vendor: wire::Name = "omp".parse().expect("fixture vendor");
            let tape_root = repository
                .admit_state()
                .expect("fixture state admits")
                .lane_tape(lane)
                .join(vendor.as_str());
            fs::create_dir_all(&tape_root).expect("fixture tape root");
            let tape = tape::Tape::open(&tape_root).expect("fixture tape opens");
            let declaration = format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{SESSION}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"cwd\":\"{}\"}}\n",
                worktree.display()
            );
            let locus = tape
                .tee(declaration.as_bytes())
                .expect("session declaration persists");
            if windows == 0 {
                tape.tee(
                    br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"captured final"}],"stopReason":"stop"}}
"#,
                )
                .expect("captured final persists");
            } else {
                for index in 0..windows {
                    Self::write_window(&tape, index);
                }
            }
            let scan =
                listen::scan(&tape_root, listen::Dialect::Omp, None).expect("fixture tape scans");
            let source = scan
                .checkpoint
                .source
                .as_str()
                .parse::<CanonicalUuid>()
                .expect("fixture source is canonical");
            let session = wire::LaneSession {
                lane,
                occupant,
                worktree: worktree.display().to_string(),
                vendor: vendor.clone(),
                launch: wire::body::LaneLaunchIdentity::new("fixture", "high"),
                epoch: uuid(3),
                stream: "stdout".to_owned(),
                identity: SESSION.to_owned(),
                locus: wire::TapeLocus {
                    member: locus.member,
                    offset: locus.offset,
                    length: locus.length,
                },
            };
            let head = wire::TranscriptSourceHead {
                lane,
                vendor,
                stream: "session".to_owned(),
                source,
                next: wire::body::TranscriptPosition {
                    member: scan.checkpoint.next.member,
                    offset: scan.checkpoint.next.offset,
                },
            };
            let authority = Authority::new(session, head).expect("fixture authority matches");
            Self {
                _scratch: scratch,
                worktree,
                tape_root,
                tape: Some(tape),
                authority,
            }
        }

        fn write_window(tape: &tape::Tape, index: usize) {
            let instant = format!("2026-01-01T00:00:{:02}.000Z", index * 2 + 1);
            let compacted = format!("2026-01-01T00:00:{:02}.000Z", index * 2 + 2);
            let records = [
                format!(
                    r#"{{"type":"message_end","timestamp":"{instant}","message":{{"role":"user","content":[{{"type":"text","text":"prompt {index}"}}],"attribution":"user"}}}}"#
                ),
                format!(
                    r#"{{"type":"message_end","timestamp":"{instant}","message":{{"role":"assistant","content":[{{"type":"text","text":"final {index}"}}],"stopReason":"stop"}}}}"#
                ),
                format!(
                    r#"{{"type":"prompt_result","timestamp":"{instant}","id":"prompt-{index}","agentInvoked":true,"status":"completed","sessionSettled":true}}"#
                ),
                format!(
                    r#"{{"type":"auto_compaction_end","timestamp":"{compacted}","action":"manual","result":{{"summary":"window {index}"}},"aborted":false,"willRetry":false}}"#
                ),
            ];
            for record in records {
                tape.tee(format!("{record}\n").as_bytes())
                    .expect("window record persists");
            }
        }

        fn append_window(&self, index: usize) {
            Self::write_window(
                self.tape.as_ref().expect("fixture tape remains open"),
                index,
            );
        }

        fn environment(&self) -> Environment {
            Environment::new(Vec::new(), None, None, Some(self.worktree.clone())).unglanced()
        }
    }

    fn uuid(tail: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{tail:02x}")
            .parse()
            .expect("fixture UUID")
    }

    #[test]
    fn captured_sweep_continuations_page_frozen_windows_once() {
        const WINDOWS: usize = 8;
        let fixture = Fixture::with_windows(WINDOWS);
        let snapshot =
            current(&fixture.authority, &fixture.environment()).expect("current capture");
        let binding = snapshot.binding.clone();
        let mut arguments = vec![
            "session".to_owned(),
            "sweep".to_owned(),
            "--capture".to_owned(),
            binding.encode(),
            "--limit".to_owned(),
            "3".to_owned(),
        ];
        let mut starts = Vec::new();
        let mut page_sizes = Vec::new();
        for page_index in 0..WINDOWS {
            let matches = crate::cli::command()
                .try_get_matches_from(
                    std::iter::once("spice").chain(arguments.iter().map(String::as_str)),
                )
                .expect("sweep continuation syntax parses");
            let page = crate::read_page::Page::from_matches(
                crate::cli::selected_matches(&matches),
                &["session", "sweep"],
            )
            .expect("sweep page admits");
            let snapshot =
                reopen(binding.clone(), &fixture.environment()).expect("frozen capture reopens");
            let super::super::model::Report::Sweep { page, .. } =
                super::super::project::sweep_capture(snapshot, page).expect("sweep page reads")
            else {
                panic!("captured sweep report");
            };
            if page_index == 0 {
                fixture.append_window(WINDOWS);
            }
            let before = WINDOWS - starts.len();
            page_sizes.push(page.rows.len());
            for row in &page.rows {
                assert!(
                    row.detail
                        .arguments()
                        .windows(2)
                        .any(|words| words == ["--start", row.start.as_str()]),
                    "window detail starts at its compaction"
                );
                starts.push(row.start.clone());
            }
            let Some(omission) = page.omission else {
                assert_eq!(starts.len(), WINDOWS);
                break;
            };
            assert_eq!(omission.population, before);
            assert_eq!(omission.withheld, WINDOWS - starts.len());
            arguments = omission.continuation.arguments().to_vec();
        }
        assert_eq!(page_sizes, [3, 3, 2]);
        let unique = starts.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(starts.len(), WINDOWS);
        assert_eq!(unique.len(), WINDOWS, "{starts:?}");
    }

    #[test]
    fn replacing_the_tape_generation_refuses_an_old_binding() {
        let mut fixture = Fixture::new();
        let binding = current(&fixture.authority, &fixture.environment())
            .expect("current capture")
            .binding;
        drop(fixture.tape.take());
        fs::remove_dir_all(&fixture.tape_root).expect("old generation removed");
        fs::create_dir_all(&fixture.tape_root).expect("replacement root");
        let replacement = tape::Tape::open(&fixture.tape_root).expect("replacement tape opens");
        replacement.tee(b"{}\n").expect("replacement has content");

        let refusal = match reopen(binding, &fixture.environment()) {
            Ok(_) => panic!("old generation must refuse"),
            Err(refusal) => refusal,
        };
        assert_eq!(
            refusal
                .diagnostic()
                .expect("generation refusal is diagnosed")
                .reason(),
            "session::capture-generation-changed"
        );
    }

    #[test]
    fn authority_and_selected_records_refuse_cross_stream_or_superseded_sessions() {
        let fixture = Fixture::new();
        let mut foreign = fixture.authority.head.clone();
        foreign.lane = uuid(9);
        assert!(Authority::new(fixture.authority.session.clone(), foreign).is_err());
        let mut foreign = fixture.authority.head.clone();
        foreign.stream = "stdout".to_owned();
        assert!(Authority::new(fixture.authority.session.clone(), foreign).is_err());

        let snapshot =
            current(&fixture.authority, &fixture.environment()).expect("current capture");
        assert!(records_for_session(snapshot.records.clone(), SESSION).is_ok());
        let different = uuid(8).to_string();
        let refusal = records_for_session(snapshot.records, &different)
            .expect_err("a different session refuses");
        assert_eq!(
            refusal
                .diagnostic()
                .expect("session refusal is diagnosed")
                .reason(),
            "session::capture-session-superseded"
        );
    }
}

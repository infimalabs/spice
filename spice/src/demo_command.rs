//! Zero-setup native app demonstration environment.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant as Monotonic};

use clap::ArgMatches;
use journal::{FactCause, JournalWrite};
use notify::{RecursiveMode, Watcher as _};
use serde::{Deserialize, Serialize};
use spice::ambient::Ambient;
use spice::shell_quote;
use wire::{
    Actor, CanonicalUuid, DemoAnswer, Outcome, Producer, Refusal, Reply, ReplyPosition,
    ResidencyHead, ResidentState, TaskHead, WorkflowHead, body,
};

use crate::control_signal::ControlRelay;
use crate::{environment_fault, protocol_fault};

mod inbox_seed;
mod line_seed;
mod pass_seed;
mod recovery;

use recovery::{preserve_environment_failure, preserve_failure, require_utf8};

const MANIFEST_NAME: &str = ".spice-demo.json";
const TRANSCRIPT_NAME: &str = "spice-demo-transcript.jsonl";
const CONFIG_BYTES: &[u8] = b"# Demo repository policy: every key keeps its built-in value.\n";
// Demo task births are current-codec stamps of fixed authored microseconds,
// 2026-08-06T04:17:41.781000Z onward, one millisecond apart.
const DEMO_TASK: &str = "DEMO-dPCb71yWr";
const DEMO_STAMP: &str = "dPCb71yWr";
const DEMO_TITLE: &str = "Add machine-readable status output";
const DEMO_TOPIC: &str = "/rim/spice/demo";

fn demo_topic() -> wire::Topic {
    DEMO_TOPIC
        .parse()
        .expect("the demo topic is a public topic")
}
const DEMO_PRINCIPAL: &str = "018f82d4-6f59-7d06-92e3-bce913116410";
const DEMO_LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116411";
const DEMO_TEAM: &str = "73e7a0fc-8aab-4e51-9c19-eedc66913c5d";
const DEMO_OOPS_BATCH_STAMP: &str = "dPCb71yv4";
const DEMO_OOPS_REPORT_STAMP: &str = "dPCb71zGJ";
const DEMO_OOPS_REPORT_TITLE: &str = "Observed command friction";
const DEMO_FINISHED_TASK_STAMP: &str = "dPCb71zdX";
const DEMO_FINISHED_TASK_TITLE: &str = "Document the status output contract";
const DEMO_CONCLUDED_DESIGN_STAMP: &str = "dPCb7200m";
const DEMO_CONCLUDED_DESIGN_TITLE: &str = "Choose machine-readable operator views";
const DEMO_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce913116412";
const DEMO_EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce913116413";
const DEMO_OCCUPANT: &str = "018f82d4-6f59-7d06-92e3-bce913116414";
const DEMO_SECOND_LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116415";
const DEMO_SECOND_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce913116416";
const DEMO_SECOND_OCCUPANT: &str = "018f82d4-6f59-7d06-92e3-bce913116417";
const DEMO_MESSAGE_LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116418";
const DEMO_MESSAGE_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce913116419";
const DEMO_MESSAGE_OCCUPANT: &str = "018f82d4-6f59-7d06-92e3-bce91311641a";
const DEMO_INNER_DIRECTIVE: &str = "018f82d4-6f59-7d06-92e3-bce91311641b";
const DEMO_OUTER_DIRECTIVE: &str = "018f82d4-6f59-7d06-92e3-bce91311641c";
const DEMO_FIRST_CONCERN: &str = "018f82d4-6f59-7d06-92e3-bce91311641d";
const DEMO_SECOND_CONCERN: &str = "018f82d4-6f59-7d06-92e3-bce91311641e";
const DEMO_INNER_KEY: &str = "dDmm00001";
const DEMO_OUTER_KEY: &str = "dDmm00002";
const DEMO_FIRST_CONCERN_KEY: &str = "dDmm00003";
const DEMO_SECOND_CONCERN_KEY: &str = "dDmm00004";
const DEMO_FIRST_PRINCIPAL_KEY: &str = "dDmm00005";
const DEMO_SECOND_PRINCIPAL_KEY: &str = "dDmm00006";
const DEMO_INNER_TEXT: &str =
    "Keep the machine-readable rows tied to the same status report as the table.";
const DEMO_FIRST_CONCERN_TEXT: &str = "Should the JSON rows remain newline-delimited?";
/// A 48×32 PNG the first concern's operator direction attaches, so the inbox
/// shows a direction's image.
const DEMO_FIRST_CONCERN_IMAGE: &str = "iVBORw0KGgoAAAANSUhEUgAAADAAAAAgCAIAAADbtmxLAAAARklEQVR42mO46W+LFUWtyMaKaK2eYdRBow4addCIc9BAWYxL/aiDRh006qCR56DRknrUQaMOGnXQaHtotOoYddCog0a4gwCRnXZ5/euVFQAAAABJRU5ErkJggg==";
const DEMO_SECOND_CONCERN_TEXT: &str = "Should failed lanes retain their diagnostic field?";
const DEMO_RESIDENT_AT: &str = "2026-07-11T04:59:00.000000Z";
const DEMO_CLAIM_EXPIRY: &str = "2099-07-11T05:00:00.000000Z";
const DEMO_CLAIM_MAXIMUM: &str = "2100-07-11T05:00:00.000000Z";
const SEED_BRANCH: &str = "refs/heads/main";
const SEED_NAME: &str = "spice demo";
const SEED_EMAIL: &str = "demo@spice.invalid";
const SEED_MESSAGE: &str = "spice demo seed\n";
const SEED_COMMIT: &str = "a53ad58b1356a5f7abea7437975321850e04b217";
const WATCH_SCAN_INTERVAL: Duration = Duration::from_millis(100);
const READY_DEADLINE: Duration = Duration::from_secs(10);
const STOP_DEADLINE: Duration = Duration::from_secs(10);

type DemoResult<T> = Result<T, Box<Outcome<DemoAnswer>>>;

#[derive(Clone, Copy)]
struct DemoLane {
    lane: &'static str,
    capability: &'static str,
    occupant: &'static str,
    name: &'static str,
}

#[derive(Clone, Copy)]
struct DemoMessageLane {
    lane: &'static str,
    capability: &'static str,
    occupant: &'static str,
    name: &'static str,
}

const DEMO_MESSAGE_SEAT: DemoMessageLane = DemoMessageLane {
    lane: DEMO_MESSAGE_LANE,
    capability: DEMO_MESSAGE_CAPABILITY,
    occupant: DEMO_MESSAGE_OCCUPANT,
    name: "Nora",
};

/// The transcript-backed lanes whose conversation lines the seed admits.
const DEMO_IRIS_SEAT: DemoMessageLane = DemoMessageLane {
    lane: "018f82d4-6f59-7d06-92e3-bce913116430",
    capability: "018f82d4-6f59-7d06-92e3-bce913116431",
    occupant: "018f82d4-6f59-7d06-92e3-bce913116432",
    name: "Iris",
};

const DEMO_THEO_SEAT: DemoMessageLane = DemoMessageLane {
    lane: "018f82d4-6f59-7d06-92e3-bce913116433",
    capability: "018f82d4-6f59-7d06-92e3-bce913116434",
    occupant: "018f82d4-6f59-7d06-92e3-bce913116435",
    name: "Theo",
};

/// A lane removed from the residency before the lines begin, so a packet
/// still naming its occupant's address reaches nobody there.
const DEMO_JUNO_SEAT: DemoMessageLane = DemoMessageLane {
    lane: "018f82d4-6f59-7d06-92e3-bce913116436",
    capability: "018f82d4-6f59-7d06-92e3-bce913116437",
    occupant: "018f82d4-6f59-7d06-92e3-bce913116438",
    name: "Juno",
};

/// Every seat whose occupant writes an omp transcript, in seating order.
const DEMO_TRANSCRIPT_SEATS: [DemoMessageLane; 4] = [
    DEMO_MESSAGE_SEAT,
    DEMO_IRIS_SEAT,
    DEMO_THEO_SEAT,
    DEMO_JUNO_SEAT,
];

const DEMO_LANES: [DemoLane; 2] = [
    DemoLane {
        lane: DEMO_LANE,
        capability: DEMO_CAPABILITY,
        occupant: DEMO_OCCUPANT,
        name: "Ava",
    },
    DemoLane {
        lane: DEMO_SECOND_LANE,
        capability: DEMO_SECOND_CAPABILITY,
        occupant: DEMO_SECOND_OCCUPANT,
        name: "Milo",
    },
];

const CANNED_TRANSCRIPT: [(&str, &str); 4] = [
    (
        "2026-07-11T05:00:00.000000Z",
        concat!(
            "Picking up the task to add a `--porcelain` flag to `spice status`. ",
            "Reading the status command to find where the human-readable table is ",
            "assembled, so a machine-readable path can branch from the same data."
        ),
    ),
    (
        "2026-07-11T05:01:00.000000Z",
        concat!(
            "Found it: `render_status` builds the table from a single `StatusReport`. ",
            "I'll serialize that same report to newline-delimited JSON behind ",
            "`--porcelain`, so the table and the JSON always read from one source and ",
            "cannot drift."
        ),
    ),
    (
        "2026-07-11T05:02:00.000000Z",
        concat!(
            "Implemented and covered: `spice status --porcelain` now prints one JSON ",
            "object per lane, and `test_status_porcelain_matches_table` asserts the ",
            "JSON rows carry the same lane ids the table renders. Full suite is green ",
            "and the worktree is clean."
        ),
    ),
    (
        "2026-07-11T05:03:00.000000Z",
        concat!(
            "Done -- `spice status --porcelain` ships behind one flag, with a single ",
            "renderer feeding both views and a test pinning them together. Keeping ",
            "the task with Ava while it is reviewed."
        ),
    ),
];

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DemoManifest {
    schema: u32,
    task: String,
    lane: CanonicalUuid,
    transcript: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DemoTurn<'a> {
    timestamp: &'a str,
    role: &'a str,
    text: &'a str,
}

#[derive(Clone, Debug)]
struct Environment {
    root: PathBuf,
    transcript: PathBuf,
    state: spice::repository::StatePaths,
    launch_command: String,
    remove_command: String,
    automatic: bool,
}

enum Event {
    Changed(Result<(), String>),
    Resident(Outcome<()>),
    Signal,
}

/// Seed or run one isolated native app environment.
pub(crate) fn run(matches: &ArgMatches, ambient: &Ambient) -> Outcome<DemoAnswer> {
    let root = matches.get_one::<String>("root").map(PathBuf::from);
    let reuse = matches.get_one::<String>("reuse").map(PathBuf::from);
    let seed_only = matches.get_flag("seed-only");
    let preserve = matches.get_flag("preserve");
    let open = matches.get_flag("open");
    if root.is_some() && reuse.is_some() {
        return refusal(
            "demo::root-selection-refused",
            Some("choose either --root for a new seed or --reuse for an existing seed"),
            Some("both --root and --reuse were supplied"),
            "select one demo root mode",
        );
    }
    if seed_only && reuse.is_some() {
        return refusal(
            "demo::mode-refused",
            Some("--seed-only creates one new deterministic seed"),
            Some("--seed-only was combined with --reuse"),
            "remove --reuse or run the existing seed without --seed-only",
        );
    }

    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return environment_fault("demo::executable-unavailable", error.to_string()),
    };
    let environment = match reuse {
        Some(root) => reopen_environment(&root, &executable),
        None => seed_environment(root.as_deref(), &executable),
    };
    let environment = match environment {
        Ok(environment) => environment,
        Err(failure) => return *failure,
    };
    if seed_only {
        return reply(&environment, "seeded", "preserved", None);
    }
    let cleanup = environment.automatic && !preserve;
    app_environment(environment, cleanup, ambient, open)
}

fn seed_environment(root: Option<&Path>, executable: &Path) -> DemoResult<Environment> {
    let working = std::env::current_dir().map_err(|error| {
        environment_fault(
            "demo::source-repository-unavailable",
            format!("current directory: {error}"),
        )
    })?;
    seed_environment_from(root, executable, &working)
}

fn seed_environment_from(
    root: Option<&Path>,
    executable: &Path,
    source_start: &Path,
) -> DemoResult<Environment> {
    require_utf8(executable, "current spice executable")?;
    let (root, automatic) = prepare_root(root)?;
    let result = seed_prepared_root(&root, executable, automatic, source_start);
    result.map_err(|failure| preserve_failure(&root, executable, *failure).into())
}

fn seed_prepared_root(
    root: &Path,
    executable: &Path,
    automatic: bool,
    source_start: &Path,
) -> DemoResult<Environment> {
    born_repository(root)?;
    let source_root = application_source_root(source_start)?;
    let source = serve::AppSource::resolve(&source_root)
        .map_err(|error| environment_fault("demo::app-unavailable", error.to_string()))?;
    finish_seed_prepared_root(root, executable, automatic, &source)
}

fn application_source_root(start: &Path) -> DemoResult<PathBuf> {
    spice::repository::Repository::discover(start)
        .map(|repository| repository.worktree_root().to_path_buf())
        .map_err(|error| {
            environment_fault("demo::source-repository-unavailable", error.to_string()).into()
        })
}

fn finish_seed_prepared_root(
    root: &Path,
    executable: &Path,
    automatic: bool,
    source: &serve::AppSource,
) -> DemoResult<Environment> {
    seed_lane_app(root, source)?;
    let repository = spice::repository::Repository::discover(root).map_err(|error| {
        environment_fault(spice::repository::ResolveError::REASON, error.to_string())
    })?;
    let state = repository
        .admit_state()
        .map_err(|error| crate::state_failure(&error).outcome())?;
    fs::write(root.join(wire::TRACKED_POLICY_FILE), CONFIG_BYTES)
        .map_err(|error| environment_fault("demo::config-unwritable", error.to_string()))?;
    fs::create_dir_all(state.data()).map_err(|error| {
        environment_fault(
            "demo::state-unavailable",
            format!("{}: {error}", state.data().display()),
        )
    })?;
    let transcript = root.join(TRANSCRIPT_NAME);
    fs::write(&transcript, transcript_bytes()?).map_err(|error| {
        environment_fault(
            "demo::transcript-unwritable",
            format!("{}: {error}", transcript.display()),
        )
    })?;
    seed_journal(&state, root)?;
    inbox_seed::seed_message_tape(&state)?;
    inbox_seed::seed_inbox_conversations(&state)?;
    line_seed::seed(root, &state)?;
    let manifest = DemoManifest {
        schema: 1,
        task: DEMO_TASK.to_owned(),
        lane: uuid(DEMO_LANE),
        transcript: TRANSCRIPT_NAME.to_owned(),
    };
    let mut manifest_bytes = wire::encode(&manifest)
        .map_err(|error| protocol_fault("demo::manifest-invalid", error.to_string()))?;
    manifest_bytes.push(b'\n');
    let manifest_path = root.join(MANIFEST_NAME);
    fs::write(&manifest_path, manifest_bytes).map_err(|error| {
        environment_fault(
            "demo::manifest-unwritable",
            format!("{}: {error}", manifest_path.display()),
        )
    })?;
    environment(root.to_path_buf(), transcript, state, executable, automatic)
}

/// Copy the invoking worktree's current immutable app generation into the demo lane.
fn seed_lane_app(root: &Path, source: &serve::AppSource) -> DemoResult<()> {
    let generation = source
        .current_generation()
        .map_err(|error| environment_fault("demo::app-unavailable", error.to_string()))?;
    let stage = root.join(serve::APP_STAGE);
    let target_generation = stage
        .join(serve::APP_STAGE_GENERATIONS)
        .join(generation.identity().as_str());
    fs::create_dir_all(&target_generation).map_err(|error| {
        environment_fault(
            "demo::app-stage-unwritable",
            format!("{}: {error}", target_generation.display()),
        )
    })?;
    for (file, served) in generation.files() {
        let target = target_generation.join(file.as_ref());
        fs::write(&target, served.bytes()).map_err(|error| {
            environment_fault(
                "demo::app-stage-unwritable",
                format!("{}: {error}", target.display()),
            )
        })?;
    }
    let pointer = stage.join(serve::APP_STAGE_CURRENT);
    fs::write(&pointer, format!("{}\n", generation.identity())).map_err(|error| {
        environment_fault(
            "demo::app-stage-unwritable",
            format!("{}: {error}", pointer.display()),
        )
    })?;
    Ok(())
}

fn reopen_environment(root: &Path, executable: &Path) -> DemoResult<Environment> {
    require_utf8(executable, "current spice executable")?;
    let root = canonical_demo_root(root)?;
    let manifest_path = root.join(MANIFEST_NAME);
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        refusal(
            "demo::seed-unrecognized",
            Some("one complete spice demo manifest"),
            Some(&format!("{}: {error}", manifest_path.display())),
            "seed a new root with `spice demo --seed-only` or name its exact output root",
        )
    })?;
    let manifest: DemoManifest = wire::decode(&manifest_bytes).map_err(|error| {
        refusal(
            "demo::seed-unrecognized",
            Some("the current spice demo manifest schema"),
            Some(&error.to_string()),
            "remove this disposable root or seed a new one",
        )
    })?;
    if manifest != expected_manifest() {
        return Err(refusal(
            "demo::seed-unrecognized",
            Some("the exact deterministic spice demo identity"),
            Some(&format!("{manifest:?}")),
            "remove this disposable root or seed a new one",
        )
        .into());
    }
    let transcript = root.join(&manifest.transcript);
    let observed = fs::read(&transcript).map_err(|error| {
        environment_fault(
            "demo::transcript-unreadable",
            format!("{}: {error}", transcript.display()),
        )
    })?;
    let expected = transcript_bytes()?;
    if observed != expected {
        return Err(refusal(
            "demo::transcript-moved",
            Some("the deterministic canned transcript"),
            Some(&transcript.display().to_string()),
            "restore the seeded transcript or remove this disposable root",
        )
        .into());
    }
    // Lanes legitimately advance a demo root's branch, policy, residency, and
    // journal. Reopening proves only that this root was born from the seed:
    // its history still contains the seed commit and its journal still holds
    // the seeded facts, which append-only writes never remove.
    if !descends_from_seed(&root)? {
        return Err(refusal(
            "demo::commit-moved",
            Some(&format!("a HEAD commit descending from {SEED_COMMIT}")),
            Some(&seeded_head(&root)?),
            "check out a commit descending from the seeded commit or seed a new one",
        )
        .into());
    }
    let repository = spice::repository::Repository::discover(&root).map_err(|error| {
        environment_fault(spice::repository::ResolveError::REASON, error.to_string())
    })?;
    let state = repository
        .admit_state()
        .map_err(|error| crate::state_failure(&error).outcome())?;
    validate_seeded_journal(&state)?;
    environment(root, transcript, state, executable, false)
}

fn prepare_root(requested: Option<&Path>) -> DemoResult<(PathBuf, bool)> {
    match requested {
        Some(requested) => {
            match fs::symlink_metadata(requested) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(refusal(
                        "demo::root-refused",
                        Some("a new path or one real empty directory"),
                        Some(&requested.display().to_string()),
                        "choose a new or empty disposable directory",
                    )
                    .into());
                }
                Ok(_) => {
                    let mut entries = fs::read_dir(requested).map_err(|error| {
                        environment_fault(
                            "demo::root-unreadable",
                            format!("{}: {error}", requested.display()),
                        )
                    })?;
                    if entries
                        .next()
                        .transpose()
                        .map_err(|error| {
                            environment_fault("demo::root-unreadable", error.to_string())
                        })?
                        .is_some()
                    {
                        return Err(refusal(
                            "demo::root-not-empty",
                            Some("a new path or one empty disposable directory"),
                            Some(&requested.display().to_string()),
                            concat!(
                                "choose a new or empty disposable directory; existing ",
                                "contents were left unchanged"
                            ),
                        )
                        .into());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let parent = requested.parent().unwrap_or_else(|| Path::new("."));
                    fs::canonicalize(parent).map_err(|error| {
                        environment_fault(
                            "demo::root-parent-unavailable",
                            format!("{}: {error}", parent.display()),
                        )
                    })?;
                    fs::create_dir(requested).map_err(|error| {
                        environment_fault(
                            "demo::root-unavailable",
                            format!("{}: {error}", requested.display()),
                        )
                    })?;
                }
                Err(error) => {
                    return Err(environment_fault(
                        "demo::root-unavailable",
                        format!("{}: {error}", requested.display()),
                    )
                    .into());
                }
            }
            let root = canonical_demo_root(requested)?;
            require_utf8(&root, "demo root")?;
            Ok((root, false))
        }
        None => {
            for _ in 0..16 {
                let root = std::env::temp_dir().join(format!(
                    "spice-demo-{}-{}",
                    std::process::id(),
                    uuid::Uuid::new_v4().simple()
                ));
                match fs::create_dir(&root) {
                    Ok(()) => {
                        let root = canonical_demo_root(&root)?;
                        require_utf8(&root, "demo root")?;
                        return Ok((root, true));
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(environment_fault(
                            "demo::root-unavailable",
                            format!("{}: {error}", root.display()),
                        )
                        .into());
                    }
                }
            }
            Err(environment_fault(
                "demo::root-unavailable",
                "sixteen exclusive temporary root attempts collided",
            )
            .into())
        }
    }
}

fn canonical_demo_root(root: &Path) -> DemoResult<PathBuf> {
    Ok(fs::canonicalize(root).map_err(|error| {
        environment_fault(
            "demo::root-unavailable",
            format!("{}: {error}", root.display()),
        )
    })?)
}

/// Create one demo repository already born on its seeded `main` commit.
///
/// Tree, identity, message, and time are all compiled in, so every disposable
/// root reaches the same commit id and reopening can hold a root to it. The
/// branch has to be compiled in the same way: a plain init resolves the initial
/// `HEAD` through the operator's own `init.defaultBranch`, which would park a
/// freshly seeded root on an unborn branch beside the seeded commit and make
/// reopening refuse a root nobody moved. Isolated options read no configuration
/// outside this root, so the seed describes itself and not the machine.
fn born_repository(root: &Path) -> DemoResult<()> {
    let repository = gix::ThreadSafeRepository::init_opts(
        root,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        gix::open::Options::isolated(),
    )
    .map_err(|error| {
        environment_fault(
            "demo::git-init-failed",
            format!("{}: {error}", root.display()),
        )
    })?
    .to_thread_local();
    let empty_tree = repository
        .write_object(gix::objs::Tree::default())
        .map_err(|error| environment_fault("demo::seed-commit-failed", error.to_string()))?;
    let stamp = format!(
        "{} +0000",
        instant(CANNED_TRANSCRIPT[0].0).get().as_second()
    );
    let identity = gix::actor::SignatureRef {
        name: SEED_NAME.into(),
        email: SEED_EMAIL.into(),
        time: &stamp,
    };
    repository
        .commit_as(
            identity,
            identity,
            SEED_BRANCH,
            SEED_MESSAGE,
            empty_tree,
            gix::commit::NO_PARENT_IDS,
        )
        .map_err(|error| environment_fault("demo::seed-commit-failed", error.to_string()))?;
    Ok(())
}

/// Name the branch and commit a demo root is parked on, however it got there.
///
/// A detached, unborn, or moved head all describe themselves in this one
/// spelling, which both the seed contract and reuse refusals report.
fn seeded_head(root: &Path) -> DemoResult<String> {
    let repository = gix::open(root).map_err(|error| {
        environment_fault(
            "demo::repository-unavailable",
            format!("{}: {error}", root.display()),
        )
    })?;
    let head = repository
        .head()
        .map_err(|error| environment_fault("demo::repository-unavailable", error.to_string()))?;
    let branch = head.referent_name().map_or_else(
        || "detached HEAD".to_owned(),
        |name| name.as_bstr().to_string(),
    );
    let commit = head
        .id()
        .map_or_else(|| "no commit".to_owned(), |id| id.detach().to_string());
    Ok(format!("{branch} at {commit}"))
}

/// Whether the demo root's HEAD commit is the seed commit or descends from it.
///
/// An unborn head, or one whose history cannot be walked to the seed, did not
/// grow from this seed and is not reusable as a demo root.
fn descends_from_seed(root: &Path) -> DemoResult<bool> {
    let repository = gix::open(root).map_err(|error| {
        environment_fault(
            "demo::repository-unavailable",
            format!("{}: {error}", root.display()),
        )
    })?;
    let head = repository
        .head()
        .map_err(|error| environment_fault("demo::repository-unavailable", error.to_string()))?;
    let Some(tip) = head.id() else {
        return Ok(false);
    };
    let seed = gix::ObjectId::from_hex(SEED_COMMIT.as_bytes())
        .expect("the demo seed commit constant is a full object id");
    let Ok(walk) = repository.rev_walk([tip.detach()]).all() else {
        return Ok(false);
    };
    Ok(walk.map_while(Result::ok).any(|commit| commit.id == seed))
}

fn seed_journal(state: &spice::repository::StatePaths, root: &Path) -> DemoResult<()> {
    let mut journal = journal::Journal::open(state.journal())
        .map_err(|error| environment_fault("demo::journal-unavailable", error.to_string()))?;
    crate::resident::admit_steering_index(&mut journal)
        .map_err(|outcome| environment_fault("demo::journal-invalid", format!("{outcome:?}")))?;
    seed_principal(&mut journal)?;
    seed_lanes(&mut journal, root)?;
    inbox_seed::seed_demo_directions(&mut journal)?;
    inbox_seed::vacate_seat(&mut journal, DEMO_MESSAGE_SEAT)?;
    line_seed::retire_absent_seat(&mut journal)?;
    let task: wire::TaskHandle = DEMO_TASK.parse().expect("demo task handle is valid");
    let previous = seed_task(&mut journal, &task)?;
    let born = task.born().expect("demo task carries a canonical stamp");
    seed_claim(
        &mut journal,
        &born,
        &["todo"],
        DEMO_LANES[0],
        CANNED_TRANSCRIPT[0].0,
    )?;
    seed_turns(&mut journal, &task, previous)?;
    seed_oops_design(&mut journal)?;
    seed_finished_work(&mut journal)?;
    pass_seed::seed(&mut journal)?;
    journal
        .verify()
        .map_err(|error| environment_fault("demo::journal-invalid", error.to_string()))?;
    Ok(())
}

/// Admit the fixed bootstrap principal before any fixture writes as a
/// principal. The resident then maps its owner capability to this first-born
/// identity instead of minting a different principal at launch.
fn seed_principal(journal: &mut journal::Journal) -> DemoResult<()> {
    let principal = uuid(DEMO_PRINCIPAL);
    let profile = wire::principal::PrincipalProfile {
        name: Some(SEED_NAME.to_owned()),
        email: Some(SEED_EMAIL.to_owned()),
    };
    let roles = std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator]);
    let admitted = wire::principal::PrincipalAdmitted {
        principal,
        profile: profile.clone(),
        roles: roles.clone(),
        capability: None,
    };
    let origin = format!("resident:{}", uuid(DEMO_EPOCH))
        .parse()
        .expect("demo resident origin is valid");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin,
        instant(DEMO_RESIDENT_AT),
        Producer::CommandDoor,
    );
    let born = write
        .fact(admitted, FactCause::Root)
        .map_err(build_failure)?;
    write.principal_head(born, move |born| wire::principal::PrincipalHead {
        principal,
        born,
        profile,
        roles,
        capability: None,
    });
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

fn seed_task(journal: &mut journal::Journal, task: &wire::TaskHandle) -> DemoResult<wire::Seq> {
    let born = task.born().expect("demo task carries a canonical stamp");
    let phase: wire::Phase = "todo".parse().expect("demo phase is valid");
    let origin: wire::Origin = format!("task:{DEMO_STAMP}")
        .parse()
        .expect("demo task origin is valid");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin,
        instant(CANNED_TRANSCRIPT[0].0),
        Producer::CommandDoor,
    );
    let added = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: None,
                task: born.clone(),
                title: DEMO_TITLE.to_owned(),
                pitch: wire::pitch::derive(DEMO_TITLE, None),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: demo_topic().into(),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, phase],
                phase,
                revision: 1,
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::SpiceActor,
                ),
            },
            FactCause::Root,
        )
        .map_err(build_failure)?;
    // The demo task answers the seeded outer direction, whose words quote the
    // inner one, so its room opens on that request with the quote expanded.
    let statement_origin = wire::TaskProvenance::Steering(DEMO_OUTER_KEY.to_owned());
    let statement_task = born.clone();
    let workflow_task = born.clone();
    let workflow_phase = phase;
    write.task_head(added, move |revision| TaskHead {
        preparation: None,
        intent: None,
        born: statement_task,
        topic_origin: revision,
        title: DEMO_TITLE.to_owned(),
        pitch: wire::pitch::derive(DEMO_TITLE, None),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: demo_topic().into(),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: statement_origin,
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision,
    });
    write.workflow_head(workflow_task, added, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, workflow_phase],
        phase: workflow_phase,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    Ok(journal.apply(write).map_err(commit_failure)?.last())
}

/// Give a seeded lane the same canonical claim authority a real allocation
/// writes, so every consumer derives team ownership from production evidence.
fn seed_claim(
    journal: &mut journal::Journal,
    born: &wire::TaskStamp,
    flow: &[&str],
    lane: DemoLane,
    at: &str,
) -> DemoResult<()> {
    let flow = std::iter::once(wire::Phase::Draft)
        .chain(
            flow.iter()
                .map(|phase| phase.parse().expect("demo phase is valid")),
        )
        .collect::<Vec<wire::Phase>>();
    let workflow = WorkflowHead {
        phase: flow[1],
        flow,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    };
    let actor = Actor::Lane(uuid(lane.lane));
    let at = instant(at);
    let mut write = JournalWrite::new(
        actor.clone(),
        format!("task:{born}")
            .parse()
            .expect("demo task origin is valid"),
        at,
        Producer::CommandDoor,
    );
    board::allocate::mint(
        &mut write,
        board::allocate::Mint {
            task: born,
            actor: &actor,
            workflow: &workflow,
            topic: Some(&demo_topic()),
            lane: uuid(lane.lane),
            occupant: uuid(lane.occupant),
            epoch: uuid(DEMO_EPOCH),
            at,
            expiry: instant(DEMO_CLAIM_EXPIRY),
            maximum: instant(DEMO_CLAIM_MAXIMUM),
            git: None,
            recovery: None,
            source: body::SourceBoundary::Proven { heads: Vec::new() },
            cause: None,
        },
    )
    .map_err(|error| build_failure(error.into_build()))?;
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

fn seed_oops_design(journal: &mut journal::Journal) -> DemoResult<()> {
    let batch: wire::TaskStamp = DEMO_OOPS_BATCH_STAMP
        .parse()
        .expect("demo OOPS batch stamp is valid");
    let report: wire::TaskStamp = DEMO_OOPS_REPORT_STAMP
        .parse()
        .expect("demo OOPS report stamp is valid");
    seed_board_task(
        journal,
        DemoBoardTask {
            born: batch.clone(),
            title: wire::OOPS_DESIGN_TITLE,
            topic: wire::OOPS_DESIGN_TOPIC,
            term: &[],
            preparation: None,
            intent: Some(wire::TaskIntent::OpportunisticDesign),
            dependencies: Vec::new(),
            flow: &["design", "review"],
            at: "2026-07-11T05:04:00.000000Z",
        },
    )?;
    seed_board_task(
        journal,
        DemoBoardTask {
            born: report.clone(),
            title: DEMO_OOPS_REPORT_TITLE,
            topic: DEMO_TOPIC,
            term: &[],
            preparation: Some(wire::TaskPreparation {
                parent: batch.clone(),
                controller: batch.clone(),
                construction: report,
            }),
            intent: Some(wire::TaskIntent::Plan),
            dependencies: Vec::new(),
            flow: &["plan", "review"],
            at: "2026-07-11T05:05:00.000000Z",
        },
    )?;
    // A second lane works the Design.
    seed_claim(
        journal,
        &batch,
        &["design", "review"],
        DEMO_LANES[1],
        "2026-07-11T05:07:00.000000Z",
    )?;
    // The other lane stepped out of the Design's room, so its standing shows
    // a departed lane beside the working one.
    let leaver = uuid(DEMO_LANES[0].lane);
    let at = instant("2026-07-11T05:08:00.000000Z");
    let mut write = JournalWrite::new(
        Actor::Lane(leaver),
        format!("task:{DEMO_OOPS_BATCH_STAMP}")
            .parse()
            .expect("demo Design origin is valid"),
        at,
        Producer::CommandDoor,
    );
    write
        .fact(
            wire::rooms::RoomLeft {
                room: batch,
                publication: wire::rooms::RoomAuthorityStamp::for_micros(at.get().as_microsecond())
                    .expect("the leave instant names a publication"),
                lane: leaver,
                clause: "Matched on vocabulary only; no source-backed evidence on these reports."
                    .to_owned(),
            },
            FactCause::Root,
        )
        .map_err(build_failure)?;
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}
fn seed_finished_work(journal: &mut journal::Journal) -> DemoResult<()> {
    let design: wire::TaskStamp = DEMO_CONCLUDED_DESIGN_STAMP
        .parse()
        .expect("demo concluded Design stamp is valid");
    let statement = seed_board_task(
        journal,
        DemoBoardTask {
            born: design.clone(),
            title: DEMO_CONCLUDED_DESIGN_TITLE,
            topic: DEMO_TOPIC,
            term: &[],
            preparation: None,
            intent: Some(wire::TaskIntent::Design),
            dependencies: Vec::new(),
            flow: &["design"],
            at: "2026-07-11T05:09:00.000000Z",
        },
    )?;

    let task: wire::TaskStamp = DEMO_FINISHED_TASK_STAMP
        .parse()
        .expect("demo finished task stamp is valid");
    let output_revision = seed_board_task(
        journal,
        DemoBoardTask {
            born: task.clone(),
            title: DEMO_FINISHED_TASK_TITLE,
            topic: DEMO_TOPIC,
            term: &["The status output contract is documented."],
            preparation: Some(wire::TaskPreparation {
                parent: design.clone(),
                controller: design.clone(),
                construction: task.clone(),
            }),
            intent: None,
            dependencies: Vec::new(),
            flow: &["todo"],
            at: "2026-07-11T05:10:00.000000Z",
        },
    )?;
    seed_settled_design_pass(journal, &design, statement, &task, output_revision)?;
    finish_board_task(journal, task, "todo", "2026-07-11T05:11:00.000000Z", false)?;
    finish_board_task(
        journal,
        design,
        "design",
        "2026-07-11T05:12:00.000000Z",
        true,
    )
}

fn seed_settled_design_pass(
    journal: &mut journal::Journal,
    design: &wire::TaskStamp,
    statement: wire::Seq,
    output: &wire::TaskStamp,
    output_revision: wire::Seq,
) -> DemoResult<()> {
    let pass = pass_seed::complete_pass(journal, design, statement, "2026-07-11T05:10:30.000000Z")?;
    let decision_ref = wire::rooms::DecisionRef {
        controller: design.clone(),
        decision: wire::SequenceCode::new(1),
    };
    let revisions = vec![wire::rooms::OutputRevision {
        task: output.clone(),
        statement: output_revision,
        workflow: output_revision,
        dependencies: Vec::new(),
    }];
    let decision = wire::rooms::PreparationDecision {
        decision: decision_ref.clone(),
        source: wire::rooms::RoomSource::Journal {
            fact: wire::SequenceCode::new(pass.get()),
        },
        basis: wire::rooms::DecisionBasis {
            statement,
            room: None,
            outputs: revisions.clone(),
        },
        outcome: wire::rooms::DecisionOutcome::Resolved {
            outputs: vec![wire::rooms::OutputDecision {
                output: output.clone(),
                verdict: wire::rooms::OutputVerdict::Accepted,
                comment: Some("Keep the output small and machine-readable.".to_owned()),
            }],
            comment: Some("Accepted for the deterministic history proof.".to_owned()),
        },
    };
    let mut decided = JournalWrite::new(
        Actor::Principal(uuid(DEMO_PRINCIPAL)),
        format!("task:{design}")
            .parse()
            .expect("demo Design origin is valid"),
        instant("2026-07-11T05:10:40.000000Z"),
        Producer::CommandDoor,
    );
    let decision_fact = decided
        .fact(
            wire::rooms::PreparationDecided { decision },
            FactCause::Root,
        )
        .map_err(build_failure)?;
    let authorization = wire::rooms::ExecutionAuthorization {
        decision: decision_ref,
        selected_outputs: vec![output.clone()],
        output_revisions: revisions,
    };
    pass_seed::activate_demo_output(&mut decided, authorization, output, decision_fact)?;
    journal.apply(decided).map_err(commit_failure)?;
    Ok(())
}

fn finish_board_task(
    journal: &mut journal::Journal,
    task: wire::TaskStamp,
    phase: &str,
    at: &str,
    design: bool,
) -> DemoResult<()> {
    let phase = phase.parse::<wire::Phase>().expect("demo phase is valid");
    let at = instant(at);
    let mut write = JournalWrite::new(
        Actor::Principal(uuid(DEMO_PRINCIPAL)),
        format!("task:{task}")
            .parse()
            .expect("demo task origin is valid"),
        at,
        Producer::CommandDoor,
    );
    let terminal = if design {
        write
            .fact(
                body::TaskConcluded {
                    task: task.clone(),
                    conclusion: body::DesignConclusion::Done {},
                },
                FactCause::Root,
            )
            .map_err(build_failure)?
    } else {
        write
            .fact(
                body::TaskDone {
                    task: task.clone(),
                    validation: "Completed in the deterministic app demonstration.".to_owned(),
                    terminal: None,
                    git: Some(body::GitBoundary {
                        head: SEED_COMMIT.to_owned(),
                        integrated: SEED_COMMIT.to_owned(),
                        upstream: None,
                        upstream_head: None,
                        local_commits: 1,
                    }),
                    source: body::SourceBoundary::Proven { heads: Vec::new() },
                    disposition: None,
                },
                FactCause::Root,
            )
            .map_err(build_failure)?
    };
    write.workflow_head(task, terminal, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, phase],
        phase,
        status: wire::WorkflowStatus::completed(at),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

/// One deterministic task in its authored flow. The opportunistic Design keeps
/// its fixed OOPS statement; every other fixture carries a derived pitch.
struct DemoBoardTask<'a> {
    born: wire::TaskStamp,
    title: &'a str,
    topic: &'a str,
    term: &'a [&'a str],
    preparation: Option<wire::TaskPreparation>,
    intent: Option<wire::TaskIntent>,
    dependencies: Vec<wire::TaskStamp>,
    flow: &'a [&'a str],
    at: &'a str,
}

fn seed_board_task(
    journal: &mut journal::Journal,
    task: DemoBoardTask<'_>,
) -> DemoResult<wire::Seq> {
    let DemoBoardTask {
        born,
        title,
        topic,
        term,
        preparation,
        intent,
        dependencies,
        flow,
        at,
    } = task;
    let statement_topic: wire::TaskTopic = topic.parse().expect("demo topics are public topics");
    let (pitch, description, term) =
        if intent.is_some_and(|intent| intent.is_opportunistic_design()) {
            (
                wire::OOPS_DESIGN_PITCH.to_owned(),
                Some(wire::OOPS_DESIGN_DESCRIPTION.to_owned()),
                wire::OOPS_DESIGN_TERM.map(str::to_owned).to_vec(),
            )
        } else {
            (
                wire::pitch::derive(title, None),
                None,
                term.iter().map(|item| (*item).to_owned()).collect(),
            )
        };
    let phases = std::iter::once(wire::Phase::Draft)
        .chain(
            flow.iter()
                .map(|phase| phase.parse().expect("demo OOPS phase is valid")),
        )
        .collect::<Vec<wire::Phase>>();
    let phase = if preparation.is_some() {
        wire::Phase::Draft
    } else {
        phases[1]
    };
    let origin: wire::Origin = format!("task:{born}")
        .parse()
        .expect("demo OOPS task origin is valid");
    let mut write = JournalWrite::new(Actor::Spice, origin, instant(at), Producer::CommandDoor);
    let added = write
        .fact(
            body::TaskAdded {
                preparation: preparation.clone(),
                intent,
                task: born.clone(),
                title: title.to_owned(),
                pitch: pitch.clone(),
                description: description.clone(),
                term: term.clone(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: statement_topic.clone(),
                submitted: false,
                priority: None,
                taste: None,
                depends: dependencies.clone(),
                flow: phases.clone(),
                phase,
                revision: 1,
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::SpiceActor,
                ),
            },
            FactCause::Root,
        )
        .map_err(build_failure)?;
    let statement_task = born.clone();
    let statement_provenance = wire::TaskProvenance::Task(born.clone());
    let workflow_task = born;
    let statement_title = title.to_owned();
    write.task_head(added, move |revision| TaskHead {
        preparation,
        intent,
        born: statement_task,
        topic_origin: revision,
        title: statement_title,
        pitch,
        description,
        term,
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: statement_topic,
        priority: None,
        taste: None,
        dependencies,
        provenance: statement_provenance,
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision,
    });
    write.workflow_head(workflow_task, added, move |_| WorkflowHead {
        flow: phases,
        phase,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    Ok(journal.apply(write).map_err(commit_failure)?.last())
}

fn seed_turns(
    journal: &mut journal::Journal,
    task: &wire::TaskHandle,
    mut previous: wire::Seq,
) -> DemoResult<()> {
    let room = task.born().expect("demo task carries a canonical stamp");
    for (timestamp, text) in CANNED_TRANSCRIPT {
        let at = instant(timestamp);
        let mut write = JournalWrite::new(
            Actor::Spice,
            format!("task:{DEMO_STAMP}")
                .parse()
                .expect("demo task origin is valid"),
            at,
            Producer::CommandDoor,
        );
        write
            .fact(
                wire::rooms::RoomPublished {
                    room: room.clone(),
                    publication: wire::rooms::RoomAuthorityStamp::for_micros(
                        at.get().as_microsecond(),
                    )
                    .expect("each canned turn instant names a publication"),
                    content: wire::rooms::RoomPublicationContent::Authored {
                        text: text.to_owned(),
                    },
                },
                FactCause::Existing(previous),
            )
            .map_err(build_failure)?;
        previous = journal.apply(write).map_err(commit_failure)?.last();
    }
    Ok(())
}

fn seed_lanes(journal: &mut journal::Journal, root: &Path) -> DemoResult<()> {
    let epoch = uuid(DEMO_EPOCH);
    let team = uuid(DEMO_TEAM);
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{epoch}")
            .parse()
            .expect("demo resident origin is valid"),
        instant(DEMO_RESIDENT_AT),
        Producer::ResidentRecognition,
    );
    let mut cause = FactCause::Root;
    for seed in DEMO_LANES {
        let seated = write
            .fact(
                body::LaneSeated {
                    lane: uuid(seed.lane),
                    team,
                    capability: uuid(seed.capability),
                    name: Some(seed.name.to_owned()),
                    worktree: root.display().to_string(),
                    life: "drive".to_owned(),
                    vendor: crate::resident::SIMULATED_VENDOR
                        .parse()
                        .expect("simulated demo vendor is valid"),
                    model: None,
                    effort: None,
                },
                cause,
            )
            .map_err(build_failure)?;
        let started = write
            .fact(
                body::LaneStarted {
                    lane: uuid(seed.lane),
                    occupant: uuid(seed.occupant),
                    epoch,
                    group: 0,
                    fresh_session: true,
                },
                FactCause::Pending(seated),
            )
            .map_err(build_failure)?;
        cause = FactCause::Pending(started);
    }
    for seat in DEMO_TRANSCRIPT_SEATS {
        let seated = write
            .fact(
                body::LaneSeated {
                    lane: uuid(seat.lane),
                    team,
                    capability: uuid(seat.capability),
                    name: Some(seat.name.to_owned()),
                    worktree: root.display().to_string(),
                    life: "drive".to_owned(),
                    vendor: "omp".parse().expect("OMP demo vendor is valid"),
                    model: None,
                    effort: None,
                },
                cause,
            )
            .map_err(build_failure)?;
        let started = write
            .fact(
                body::LaneStarted {
                    lane: uuid(seat.lane),
                    occupant: uuid(seat.occupant),
                    epoch,
                    group: 0,
                    fresh_session: false,
                },
                FactCause::Pending(seated),
            )
            .map_err(build_failure)?;
        cause = FactCause::Pending(started);
    }
    let FactCause::Pending(started) = cause else {
        unreachable!("the demo seats at least one lane");
    };
    let lanes = demo_residency_lanes(root, team);
    write.residency_head(started, move |_| ResidencyHead {
        epoch,
        state: ResidentState::Dark,
        topology_revision: 0,
        lanes,
        subscriptions: vec![wire::TeamSubscription {
            team,
            formed_at: Some(instant(DEMO_RESIDENT_AT)),
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    });
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

fn demo_residency_lanes(root: &Path, team: CanonicalUuid) -> Vec<wire::LaneResidency> {
    let worktree = root.display().to_string();
    let mut lanes = DEMO_LANES
        .into_iter()
        .map(|seed| wire::LaneResidency {
            lane: uuid(seed.lane),
            team,
            name: Some(seed.name.to_owned()),
            worktree: worktree.clone(),
            life: "drive".to_owned(),
            vendor: crate::resident::SIMULATED_VENDOR
                .parse()
                .expect("simulated demo vendor is valid"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(uuid(seed.occupant)),
            lineage: vec![uuid(seed.occupant)],
            capabilities: vec![uuid(seed.capability)],
            session: None,
            disposition: crate::resident::SIMULATED_DISPOSITION.to_owned(),
        })
        .collect::<Vec<_>>();
    lanes.extend(
        DEMO_TRANSCRIPT_SEATS
            .into_iter()
            .map(|seat| wire::LaneResidency {
                lane: uuid(seat.lane),
                team,
                name: Some(seat.name.to_owned()),
                worktree: worktree.clone(),
                life: "drive".to_owned(),
                vendor: "omp".parse().expect("OMP demo vendor is valid"),
                model: None,
                effort: None,
                fast_mode: false,
                occupant: Some(uuid(seat.occupant)),
                lineage: vec![uuid(seat.occupant)],
                capabilities: vec![uuid(seat.capability)],
                session: None,
                disposition: "running".to_owned(),
            }),
    );
    lanes
}

fn validate_seeded_journal(state: &spice::repository::StatePaths) -> DemoResult<()> {
    let reader = journal::JournalReader::open(state.journal())
        .map_err(|error| environment_fault("demo::journal-unavailable", error.to_string()))?;
    let (_, snapshot) = reader
        .read_with(|read| read.verified_fact_snapshot())
        .map_err(|error| environment_fault("demo::journal-invalid", error.to_string()))?;
    let task = snapshot.facts().iter().any(|envelope| {
        envelope.fact.kind() == wire::FactKind::TaskAdded
            && envelope
                .fact
                .body()
                .get("task")
                .and_then(serde_json::Value::as_str)
                == Some(DEMO_STAMP)
    });
    let lanes = DEMO_LANES.iter().all(|seed| {
        snapshot.facts().iter().any(|envelope| {
            envelope.fact.kind() == wire::FactKind::LaneSeated
                && envelope
                    .fact
                    .body()
                    .get("lane")
                    .and_then(serde_json::Value::as_str)
                    == Some(seed.lane)
        })
    });
    let starts = DEMO_LANES.iter().all(|seed| {
        snapshot.facts().iter().any(|envelope| {
            envelope
                .fact
                .read::<body::LaneStarted>()
                .is_some_and(|started| {
                    started.lane == uuid(seed.lane)
                        && started.occupant == uuid(seed.occupant)
                        && started.epoch == uuid(DEMO_EPOCH)
                })
        })
    });
    let claimed = snapshot.facts().iter().any(|envelope| {
        envelope.fact.read::<body::ClaimWon>().is_some_and(|claim| {
            claim.task == DEMO_STAMP.parse().expect("demo task stamp is valid")
                && claim.actor == Actor::Lane(uuid(DEMO_LANE))
                && claim.lane == uuid(DEMO_LANE)
                && claim.occupant == uuid(DEMO_OCCUPANT)
                && claim.epoch == uuid(DEMO_EPOCH)
        })
    });
    let notes = snapshot
        .facts()
        .iter()
        .filter(|envelope| envelope.fact.kind() == wire::FactKind::RoomPublished)
        .count();
    if !task || !lanes || !starts || !claimed || notes < CANNED_TRANSCRIPT.len() {
        return Err(refusal(
            "demo::seed-moved",
            Some("the seeded task, Ava's claim, two seated simulated lanes, and four canned turns"),
            Some(&format!(
                "task={task}, claimed={claimed}, lanes={lanes}, starts={starts}, turns={notes}"
            )),
            "restore the seeded journal or remove this disposable root",
        )
        .into());
    }
    Ok(())
}

mod runtime;
use runtime::*;

#[cfg(test)]
#[path = "demo_command/tests.rs"]
mod tests;

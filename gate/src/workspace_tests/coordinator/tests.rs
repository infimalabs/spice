use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Condvar, Mutex};

use super::*;

#[derive(Default)]
struct Hold {
    released: Mutex<bool>,
    wake: Condvar,
}

impl Hold {
    fn wait(&self) {
        let mut released = self.released.lock().expect("hold state");
        while !*released {
            released = self.wake.wait(released).expect("hold wake");
        }
    }

    fn release(&self) {
        *self.released.lock().expect("hold state") = true;
        self.wake.notify_all();
    }
}

#[derive(Clone)]
enum Behavior {
    Pass,
    Exit(i32),
    TimeOut,
    Refuse,
    UntilCancelled,
    /// Block at an event boundary controlled by the test.
    Hold(Arc<Hold>),
}

struct Launched {
    argv: Vec<String>,
    temporary: PathBuf,
    temporary_present: bool,
}

/// Which shard a behavior answers for.
#[derive(Clone, Copy)]
enum Selector {
    /// The direct executable belonging to this package.
    Package(&'static str),
    /// The prebuilt partition running the exclusive-lane case, which is
    /// the one partition that does not skip it.
    Exclusive(&'static str),
}

impl Selector {
    fn selects(self, argv: &[String]) -> bool {
        match self {
            Self::Package(id) => argv
                .first()
                .is_some_and(|program| Path::new(program).file_name() == Some(OsStr::new(id))),
            Self::Exclusive(case) => !argv.iter().any(|argument| argument == case),
        }
    }
}

/// Shards answered in process, each by the behavior its selector names.
struct Shards {
    behaviors: Vec<(Selector, Behavior)>,
    active: AtomicUsize,
    peak: AtomicUsize,
    cancelled: AtomicBool,
    launched: Mutex<Vec<Launched>>,
}

impl Shards {
    fn new(behaviors: &[(Selector, Behavior)]) -> Arc<Self> {
        Arc::new(Self {
            behaviors: behaviors.to_vec(),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            launched: Mutex::new(Vec::new()),
        })
    }

    fn behavior(&self, argv: &[String]) -> Behavior {
        self.behaviors
            .iter()
            .find(|(selector, _)| selector.selects(argv))
            .map_or(Behavior::Pass, |(_, behavior)| behavior.clone())
    }

    fn launched(&self) -> std::sync::MutexGuard<'_, Vec<Launched>> {
        self.launched.lock().expect("launch record")
    }
}

impl ShardLauncher for Shards {
    fn run(
        &self,
        spec: Spec,
        _observer: Arc<LastTest>,
        cancellation: &CancellationToken,
        _policy: RunPolicy,
    ) -> Result<WaitOutcome, WorkspaceTestError> {
        let active = self.active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
        self.peak.fetch_max(active, AtomicOrdering::SeqCst);
        let argv = std::iter::once(spec.argv.program())
            .chain(spec.argv.arguments().iter().map(OsString::as_os_str))
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let variable = |name: &str| {
            spec.environment
                .entries()
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
        };
        let temporary = PathBuf::from(variable("TMPDIR").expect("shard temporary root"));
        let behavior = self.behavior(&argv);
        self.launched().push(Launched {
            temporary_present: temporary.is_dir(),
            temporary,
            argv,
        });
        let completion = match behavior {
            Behavior::Pass => Completion::Exited(exit(0)),
            Behavior::Exit(code) => Completion::Exited(exit(code)),
            Behavior::TimeOut => Completion::TimedOut(exit(0)),
            Behavior::Refuse => {
                self.active.fetch_sub(1, AtomicOrdering::SeqCst);
                return Err(WorkspaceTestError::new("fixture launch refused"));
            }
            Behavior::UntilCancelled => {
                let parked = thread::current();
                let _registration = cancellation.register_waker(Arc::new(move || parked.unpark()));
                while !cancellation.is_cancelled() {
                    thread::park();
                }
                self.cancelled.store(true, AtomicOrdering::SeqCst);
                Completion::Cancelled(exit(0))
            }
            Behavior::Hold(hold) => {
                hold.wait();
                Completion::Exited(exit(0))
            }
        };
        self.active.fetch_sub(1, AtomicOrdering::SeqCst);
        Ok(WaitOutcome::externally_reaped_completion(completion))
    }
}

fn exit(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

fn target(package: &str, kind: &str, name: &str) -> TargetIdentity {
    TargetIdentity {
        package: package.to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
        required_features: Vec::new(),
        source: PathBuf::from(format!("/workspace/{package}/src/lib.rs")),
        declared_path_sources: Vec::new(),
        source_bytes: 1,
    }
}

fn prebuilt_test(name: &str) -> PrebuiltTest {
    PrebuiltTest {
        package: name.to_owned(),
        package_root: PathBuf::from(format!("/workspace/{name}")),
        target: target(name, "lib", name),
        executable: PathBuf::from(format!("/workspace/target/debug/deps/{name}-id")),
        partition: None,
    }
}

fn policy(concurrency: usize) -> RunPolicy {
    RunPolicy {
        concurrency,
        test_threads: Some(1),
        shard_deadline: Duration::from_secs(10),
        progress_interval: Duration::from_secs(10),
        output_drain: Duration::from_secs(1),
        reap: Duration::from_secs(1),
        grace: Duration::from_secs(1),
    }
}

fn coordinator<'a>(
    shards: Vec<PrebuiltTest>,
    concurrency: usize,
    launcher: &Arc<Shards>,
    output: &'a mut Vec<u8>,
) -> Coordinator<'a, Vec<u8>> {
    let environment =
        Environment::new([("CARGO_TARGET_DIR", "/target-root")]).expect("fixture environment");
    Coordinator::from_execution_shards(
        shards,
        environment,
        policy(concurrency),
        Arc::clone(launcher) as Arc<dyn ShardLauncher>,
        output,
    )
}

fn run(
    shards: Vec<PrebuiltTest>,
    concurrency: usize,
    launcher: &Arc<Shards>,
) -> Result<(), WorkspaceTestError> {
    coordinator(shards, concurrency, launcher, &mut Vec::new()).run()
}

fn prebuilt_shards(names: &[&str]) -> Vec<PrebuiltTest> {
    names.iter().map(|name| prebuilt_test(name)).collect()
}

#[test]
fn every_shard_runs_once_within_the_bound_in_its_own_lane_and_temporary_root() {
    let overlap = Arc::new(Hold::default());
    let launcher = Shards::new(&[
        (
            Selector::Package("a-id"),
            Behavior::Hold(Arc::clone(&overlap)),
        ),
        (
            Selector::Package("b-id"),
            Behavior::Hold(Arc::clone(&overlap)),
        ),
    ]);
    let mut output = Vec::new();
    let mut coordinator = coordinator(
        prebuilt_shards(&["a", "b", "c", "d", "e"]),
        2,
        &launcher,
        &mut output,
    );
    coordinator
        .start_available()
        .expect("the first bounded wave starts");
    let active = coordinator.active.len();
    let pending = coordinator.pending.len();
    overlap.release();
    coordinator.run().expect("passing shards complete the run");
    assert_eq!(active, 2);
    assert_eq!(pending, 3);

    let launched = launcher.launched();
    let mut packages = launched
        .iter()
        .map(|launch| {
            Path::new(&launch.argv[0])
                .file_name()
                .expect("a direct shard names its executable")
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    packages.sort();
    assert_eq!(packages, ["a-id", "b-id", "c-id", "d-id", "e-id"]);
    assert!(launcher.peak.load(AtomicOrdering::SeqCst) <= 2);
    for launch in launched.iter() {
        assert!(
            launch.temporary_present,
            "the shard ran without its temporary root"
        );
        assert!(
            !launch.temporary.exists(),
            "the shard temporary root outlived its shard"
        );
    }
    let temporaries = launched
        .iter()
        .map(|launch| &launch.temporary)
        .collect::<BTreeSet<_>>();
    assert_eq!(temporaries.len(), launched.len());
}

#[test]
fn a_failed_shard_cancels_its_running_peers_and_starts_no_more() {
    let launcher = Shards::new(&[
        (Selector::Package("a-id"), Behavior::Exit(1)),
        (Selector::Package("b-id"), Behavior::UntilCancelled),
    ]);
    let error = run(prebuilt_shards(&["a", "b", "c"]), 2, &launcher)
        .expect_err("a failing shard fails the run")
        .to_string();

    assert!(
        error.contains("\"a\"") && error.contains("exit code=1"),
        "the failure names neither the failed package nor its exit: {error}"
    );
    assert!(
        launcher.cancelled.load(AtomicOrdering::SeqCst),
        "the running peer never observed cancellation"
    );
    let launched = launcher.launched();
    assert_eq!(
        launched.len(),
        2,
        "a pending shard started after the failure"
    );
    assert!(
        launched
            .iter()
            .all(|launch| Path::new(&launch.argv[0]).file_name() != Some(OsStr::new("c-id")))
    );
}

#[test]
fn each_unsuccessful_completion_fails_the_run_by_its_disposition() {
    for (behavior, disposition) in [
        (Behavior::Exit(3), "exit code=3"),
        (Behavior::TimeOut, "timeout"),
        (Behavior::Refuse, "supervision-error"),
    ] {
        let launcher = Shards::new(&[(Selector::Package("a-id"), behavior)]);
        let error = run(prebuilt_shards(&["a"]), 1, &launcher)
            .expect_err("an unsuccessful shard fails the run")
            .to_string();
        assert!(
            error.contains(disposition),
            "expected {disposition} in {error}"
        );
    }
}

#[test]
fn an_exclusive_partition_holds_the_whole_coordinator() {
    let test = PrebuiltTest {
        package: "x".to_owned(),
        package_root: PathBuf::from("/workspace/x"),
        target: target("x", "test", "suite"),
        executable: PathBuf::from("/workspace/target/debug/deps/suite"),
        partition: None,
    };
    let cases = ["exclusive_lane_alone", "first", "second", "third"].map(str::to_owned);
    let partitions = test.into_partitions(&cases, 3);
    assert_eq!(partitions.len(), 3);
    let release = Arc::new(Hold::default());
    let launcher = Shards::new(&[(
        Selector::Exclusive("exclusive_lane_alone"),
        Behavior::Hold(Arc::clone(&release)),
    )]);
    let mut output = Vec::new();
    let mut coordinator = coordinator(partitions, 3, &launcher, &mut output);
    coordinator
        .start_available()
        .expect("the exclusive partition starts");
    let active = coordinator.active.len();
    let pending = coordinator.pending.len();
    release.release();
    coordinator.run().expect("partitions complete the run");
    assert_eq!(active, 1);
    assert_eq!(pending, 2);
    assert_eq!(launcher.launched().len(), 3);
}

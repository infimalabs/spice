use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lane::capture::{CaptureFailure, CaptureRecord, CaptureReportError, CaptureReporter};

use super::super::super::read_materializations::CurrentReads;
use super::*;

pub(in crate::resident) const SESSION: &str = "018f7777-7777-7444-8888-999999999999";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

pub(in crate::resident) struct LaunchFixture {
    pub(in crate::resident) root: PathBuf,
    pub(in crate::resident) paths: RuntimePaths,
    pub(in crate::resident) epoch: CanonicalUuid,
    pub(in crate::resident) lane: CanonicalUuid,
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) capability: CanonicalUuid,
    pub(in crate::resident) vendor: wire::Name,
    reads: OnceCell<CurrentReads>,
}

impl LaunchFixture {
    pub(in crate::resident) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-resident-launch-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("fixture source directory creates");
        let root = fs::canonicalize(root).expect("fixture root canonicalizes");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"resident-launch\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .expect("fixture manifest writes");
        fs::write(
            root.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"resident-launch\"\nversion = \"0.0.0\"\n",
        )
        .expect("fixture lockfile writes");
        fs::write(
            root.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"fixture-1.0\"\ntargets = []\n",
        )
        .expect("fixture toolchain declaration writes");
        fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").expect("fixture source writes");
        fs::create_dir(root.join(".git")).expect("fixture Git directory creates");
        fs::write(root.join("spice.toml"), "[lane]\nlease = \"20s\"\n")
            .expect("fixture launch configuration writes");
        let repository = Repository::discover(&root).expect("fixture repository discovers");
        let paths = RuntimePaths::from_repository(&repository).expect("runtime paths resolve");
        Self {
            root,
            paths,
            epoch: uuid(1),
            lane: uuid(2),
            occupant: uuid(3),
            capability: uuid(4),
            vendor: "omp".parse().expect("fixture vendor name"),
            reads: OnceCell::new(),
        }
    }

    pub(in crate::resident) fn request(&self, resume: vendor::Resume) -> OccupantLaunchRequest<'_> {
        OccupantLaunchRequest {
            worktree: self.root.to_str().expect("fixture worktree is UTF-8"),
            vendor: &self.vendor,
            epoch: self.epoch,
            lane: self.lane,
            occupant: self.occupant,
            resume,
            fast_mode: false,
            model: None,
            effort: None,
        }
    }

    pub(in crate::resident) fn reporter(&self) -> Arc<dyn CaptureReporter> {
        Arc::new(SilentReporter)
    }

    /// The lane runtime over `bus`, reading consent under the queue posture.
    ///
    /// The reads start once and live as long as the fixture.
    pub(in crate::resident) fn lane_runtime<'a>(
        &'a self,
        bus: &Bus,
        capabilities: &'a serve::axum::Capabilities,
        discovery: &'a serve::Discovery,
    ) -> super::super::Runtime<'a> {
        let reads = self
            .reads
            .get_or_init(|| CurrentReads::start_queueing(bus, &self.paths));
        super::super::Runtime::new(capabilities, discovery, self.reporter(), reads)
    }
}

impl Drop for LaunchFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("launch fixture removes");
    }
}

/// Launch probes that answer without Git, RTK, or the host toolchain: the
/// worktree is already on its baseline, the Git shadow adds nothing, RTK
/// launched, Cargo isolation names a private home inside the worktree without
/// preparing it, and the briefing leaves the working state unread.
///
/// `HOME` is the fixture's own and omp's state-root override is absent, so a
/// launch pins and discovers omp sessions inside the fixture rather than the
/// developer's session store. Probes with no home answer no platform home
/// spelling, so a launch that pins one refuses.
#[derive(Default)]
pub(in crate::resident) struct InertProbes {
    home: Option<PathBuf>,
}

impl InertProbes {
    /// Probes whose `HOME` lies inside the runtime directory of `paths`.
    pub(in crate::resident) fn within(paths: &RuntimePaths) -> Self {
        Self {
            home: Some(paths.runtime.join("home")),
        }
    }
}

impl LaunchProbes for InertProbes {
    fn advance(&self, _repository: &Repository) -> String {
        "current".to_owned()
    }

    fn shadow(&self, _repository: &Repository, base: Environment) -> CommandResult<Environment> {
        Ok(base)
    }

    fn rtk(
        &self,
        _executable: &std::ffi::OsStr,
        _working: &Path,
        _environment: &Environment,
    ) -> wire::body::RtkHealth {
        wire::body::RtkHealth::new(
            wire::body::RtkHealthState::Active,
            None,
            "in-process launch probe",
        )
    }

    fn cargo(
        &self,
        repository: &Repository,
        _lane: CanonicalUuid,
    ) -> CommandResult<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
        Ok(vec![(
            spice::cargo_isolation::CARGO_HOME_VARIABLE.into(),
            repository
                .worktree_root()
                .join("cargo-home")
                .into_os_string(),
        )])
    }

    fn session(&self, worktree: PathBuf) -> crate::session_command::Environment {
        crate::main_tail::session_environment_from(|name| self.variable(name), Some(worktree))
            .unglanced()
    }

    fn variable(&self, name: &str) -> Option<std::ffi::OsString> {
        match name {
            "HOME" => self.home.clone().map(PathBuf::into_os_string),
            "USERPROFILE" | "HOMEDRIVE" | "HOMEPATH" => None,
            _ if name == vendor::OMP.transcript_layout().home_environment => None,
            _ => spice::ambient::capture()
                .get(name)
                .map(std::ffi::OsStr::to_os_string),
        }
    }
}

/// The stdout an omp occupant launched under `spec` emits: readiness, then
/// the session its argv resumes, or [`SESSION`] when it resumes none.
pub(in crate::resident) fn stand_in_stream(spec: &Spec) -> Vec<u8> {
    let session = spec
        .argv
        .arguments()
        .windows(2)
        .filter(|pair| pair[0] == "--resume")
        .filter_map(|pair| pair[1].to_str())
        .next_back()
        .unwrap_or(SESSION);
    format!(
        concat!(
            "{{\"type\":\"ready\",\"protocolVersion\":1}}\n",
            "{{\"id\":\"spice-state\",\"type\":\"response\",\"command\":\"get_state\",",
            "\"success\":true,\"data\":{{\"sessionId\":\"{}\"}}}}\n",
        ),
        session,
    )
    .into_bytes()
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID")
}

struct SilentReporter;

impl CaptureReporter for SilentReporter {
    fn record(&self, _record: CaptureRecord) -> Result<(), CaptureReportError> {
        Ok(())
    }

    fn failed(&self, _failure: CaptureFailure) {}
}

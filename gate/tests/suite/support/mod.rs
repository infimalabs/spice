use gate::metrics::BaselineCapture;
use gate::{
    ActorCapture, CORE_GATE_IDS, ClaimCapture, CoreArmInput, CoreArmReport, CoreEvidence,
    CoreInputs, CoreObservation, CoreReport, CoreSource, GateId, JudgedLatches, LatchEntry,
    ParsedContract, RimCapture, Selector, StateLedger, acquire_core, evaluate_core,
};

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

mod repository;

/// Names tried before a scratch root is declared unobtainable.
const CREATE_ATTEMPTS: usize = 128;

static NEXT_TREE: AtomicU32 = AtomicU32::new(0);

/// A source tree the fixture creates, owns, and can prove it removed.
pub(crate) struct Tree {
    pub(crate) root: PathBuf,
    removed: bool,
}

impl Tree {
    pub(crate) fn new(label: &str) -> Self {
        let directory = std::env::temp_dir();
        let process = std::process::id();
        let root = create_scratch_root(std::iter::repeat_with(|| {
            let unique = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
            directory.join(format!("gate-{label}-{process}-{unique}"))
        }));
        Self {
            root,
            removed: false,
        }
    }

    pub(crate) fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        let parent = path.parent().expect("a fixture file sits in a directory");
        fs::create_dir_all(parent).expect("create a fixture directory");
        fs::write(&path, contents).expect("write a fixture file");
    }

    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Remove the owned tree and prove it is gone.
    ///
    /// Enforcement relies on `Drop`; the ownership tests call this observable
    /// path.
    pub(crate) fn remove(mut self) {
        fs::remove_dir_all(&self.root)
            .unwrap_or_else(|error| panic!("remove fixture at {}: {error}", self.root.display()));
        assert!(
            !self.root.exists(),
            "fixture outlived removal at {}",
            self.root.display()
        );
        self.removed = true;
    }
}

impl Drop for Tree {
    /// Best effort during unwinding; explicit teardown remains observable.
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One initialized fixture repository with a committed identity.
pub(crate) fn git_repository(label: &str) -> Tree {
    let repository = Tree::new(label);
    repository.initialize();
    repository
}

/// Stage everything present and commit it under one message.
pub(crate) fn git_commit(repository: &Tree, message: &str) {
    repository.stage_all();
    repository.commit(message);
}

/// One tracked workspace document, read from the real workspace.
pub(crate) fn workspace_document(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {} for the fixture: {error}", path.display()))
}

/// One tracked policy document a fixture repository can be gated against.
///
/// Every document carries the file-class selections parsing requires, so a
/// fixture states only what it is actually about and still loads at all.
pub(crate) fn policy_document(body: &str) -> String {
    format!("{}{body}", gate::NARROWEST_FILE_CLASSES)
}

/// One committed fixture repository carrying the tracked policy.
pub(crate) fn contract_repository(label: &str) -> Tree {
    let repository = git_repository(label);
    repository.write("spice.toml", &workspace_document("spice.toml"));
    repository.write("src/app.rs", "fn main() {}\n");
    git_commit(&repository, "fixture contract");
    repository
}

/// Parse a fixture's already selected policy without executing its tools.
pub(crate) fn selected_contract(root: &Path) -> ParsedContract {
    let policy_path = root.join(wire::TRACKED_POLICY_FILE);
    let policy = fs::read_to_string(&policy_path).expect("read selected policy");
    ParsedContract::from_tracked(&policy_path, &policy).expect("parse the selected contract")
}

/// The first candidate this call creates, never one it merely finds.
pub(crate) fn create_scratch_root(candidates: impl Iterator<Item = PathBuf>) -> PathBuf {
    for root in candidates.take(CREATE_ATTEMPTS) {
        match fs::create_dir(&root) {
            Ok(()) => return root,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("create fixture root at {}: {error}", root.display()),
        }
    }
    panic!("no fixture root was free within {CREATE_ATTEMPTS} attempts");
}

/// One fixture repository acquired and judged as a whole core battery.
///
/// Every acquisition is checked for the two invariants no arm may break: the
/// battery covers each compiled declaration in canonical order, and every arm
/// is labelled with the digest of the tree it was actually taken from.
pub(crate) fn acquired(repository: &Tree, claim: ClaimCapture, actor: ActorCapture) -> CoreReport {
    evaluate_core(&acquired_inputs(repository, claim, actor, &[]))
}

/// One fixture repository acquired against a known durable latch inventory.
///
/// The inputs are returned unjudged because the elastic bounds an acquisition
/// decided live on them: judging consumes the inputs to produce arm standings,
/// and a caller reading the decisions from a report would be reading a
/// fold rather than what the arms were held to.
pub(crate) fn acquired_inputs(
    repository: &Tree,
    claim: ClaimCapture,
    actor: ActorCapture,
    latches: &[LatchEntry],
) -> CoreInputs {
    let tree = Selector::staged()
        .materialize(&repository.root)
        .expect("materialize the captured index");
    let contract = selected_contract(tree.root());
    let baselines = captured_baselines(repository, &contract);
    let rim = rim(repository);
    let source = CoreSource::capture(&tree, &contract, claim, actor, &judged(latches), &rim)
        .expect("capture one immutable core source");
    let inputs =
        acquire_core(&source, &contract, &baselines).expect("acquire one complete core battery");

    assert_eq!(
        inputs
            .arms()
            .iter()
            .map(CoreArmInput::id)
            .collect::<Vec<_>>(),
        CORE_GATE_IDS,
        "acquisition covers every compiled declaration in canonical order"
    );
    for arm in inputs.arms() {
        assert_eq!(
            arm.tree(),
            source.digest(),
            "{} was acquired from a different tree than the captured source",
            arm.id()
        );
    }
    inputs
}

/// The run inventory a ledger holding exactly `latches` judges against, none of
/// them with a recorded origin.
pub(crate) fn judged(latches: &[LatchEntry]) -> JudgedLatches {
    let ledger: StateLedger = serde_json::from_value(serde_json::json!({
        "version": gate::state::STATE_LEDGER_VERSION,
        "latches": latches,
        "claims": [],
    }))
    .expect("a ledger of canonical latches decodes");
    JudgedLatches::new(&ledger, 0.0)
}

pub(crate) fn rim(repository: &Tree) -> RimCapture {
    RimCapture::new(repository.root.clone(), None, std::iter::empty::<String>())
        .expect("fixture repository is one absolute rim")
}

/// Committed texts captured beside a direct core fixture source.
pub(crate) fn captured_baselines(repository: &Tree, contract: &ParsedContract) -> BaselineCapture {
    let classes = contract.rule_policy().document().file_classes();
    let mut suffixes = classes.file_shape().to_vec();
    suffixes.extend(classes.complexity().iter().cloned());
    suffixes.extend(classes.magic().iter().cloned());
    suffixes.sort();
    suffixes.dedup();
    BaselineCapture::capture(&repository.root, &suffixes)
}

/// The one arm of an acquired report a test is speaking about.
pub(crate) fn acquired_arm(report: &CoreReport, id: GateId) -> &CoreArmReport {
    report
        .arms()
        .iter()
        .find(|arm| arm.id() == id)
        .unwrap_or_else(|| panic!("{id} is missing from an acquired report"))
}

/// The measured observation one piece of evidence stands for.
pub(crate) fn measured(evidence: CoreEvidence) -> CoreObservation {
    CoreObservation::Measured { evidence }
}

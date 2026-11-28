//! One staged payload under a scratch layout, shared by the proof suites.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use wire::manifest::{
    BuildPins, InventoryEntry, ProofEnvironment, ProofReceipt, ReleaseTarget, TargetPayload,
};
use wire::{Digest, Domain};

use super::{Proved, Staged};
use crate::layout::Layout;
use crate::replay::{Battery, CheckKind, CheckStanding, ReplayVerdict};

/// A staged payload under `selected`, with room beside it for decoys.
pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) selected: PathBuf,
    pub(crate) worktree: PathBuf,
    common: PathBuf,
    pub(crate) target: ReleaseTarget,
    payload: TargetPayload,
    pub(crate) payload_bytes: Vec<u8>,
}

impl Fixture {
    pub(crate) fn new(target: ReleaseTarget) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spice-proof-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let selected = root.join("selected");
        let worktree = selected.join("worktree");
        let common = selected.join("common");
        fs::create_dir_all(&worktree).expect("fixture worktree");
        fs::create_dir_all(&common).expect("fixture common directory");
        let payload = payload(target);
        let payload_bytes = wire::encode(&payload).expect("fixture payload encodes");
        let fixture = Self {
            root,
            selected,
            worktree,
            common,
            target,
            payload,
            payload_bytes,
        };
        let layout = fixture.layout();
        fs::create_dir_all(layout.payload_path().parent().expect("payload parent"))
            .expect("create payload parent");
        fs::write(layout.payload_path(), &fixture.payload_bytes).expect("write payload");
        fixture
    }

    pub(crate) fn layout(&self) -> Layout {
        Layout::new(
            &self.worktree,
            self.common.join(".spice/proof"),
            self.target,
        )
    }

    pub(crate) fn staged(&self) -> Staged {
        Staged::admit(&self.layout()).expect("fixture payload stages")
    }

    /// The staged payload proved by `replay` in a target-matched environment.
    pub(crate) fn proved_by(&self, replay: ReplayVerdict) -> Proved {
        self.staged()
            .bind(self.target, || Ok(replay))
            .expect("fixture replay proves")
    }

    pub(crate) fn environment(&self) -> ProofEnvironment {
        match self.target {
            ReleaseTarget::X86_64UnknownLinuxMusl => {
                ProofEnvironment::oci(&self.payload.pins.proof_container)
                    .expect("fixture image is pinned")
            }
            ReleaseTarget::Aarch64AppleDarwin => ProofEnvironment::native(Digest::compute(
                Domain::ProofNativeRunner,
                b"fixture native runner",
            )),
            ReleaseTarget::X86_64PcWindowsMsvc => {
                unreachable!("this fixture does not construct hosted Windows payloads")
            }
        }
    }

    pub(crate) fn clean_replay(&self) -> ReplayVerdict {
        ReplayVerdict::fixture_clean(
            self.target,
            self.environment(),
            Battery::current().digest().clone(),
        )
    }

    pub(crate) fn refused_replay(&self, first: CheckStanding) -> ReplayVerdict {
        let mut standings = [CheckStanding::Clean; CheckKind::ALL.len()];
        standings[0] = first;
        ReplayVerdict::fixture_with(
            self.target,
            self.environment(),
            Battery::current().digest().clone(),
            standings,
        )
    }

    /// The receipt `replay` derives for this fixture's payload.
    pub(crate) fn receipt(&self, replay: &ReplayVerdict) -> ProofReceipt {
        replay
            .receipt(
                &self.payload,
                Digest::compute(Domain::DistTargetPayload, &self.payload_bytes),
            )
            .expect("fixture replay matches payload")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fixture");
    }
}

fn payload(target: ReleaseTarget) -> TargetPayload {
    let pins = BuildPins {
        rust: "rustc 1.97.1".to_owned(),
        wasm_target: "wasm32-unknown-unknown".to_owned(),
        trunk: "trunk 0.22.1".to_owned(),
        proof_container: format!("registry.example/proof@sha256:{}", "4".repeat(64)),
    };
    let app = Digest::compute(Domain::DistArtifact, b"app generation");
    let native = Digest::compute(Domain::DistArtifact, b"native");
    let mut inventory = vec![InventoryEntry {
        path: target.executable_path().to_owned(),
        mode: 0o755,
        bytes: 6,
        digest: native.clone(),
    }];
    crate::test_support::symbol_recipe(&mut inventory);
    let symbols = crate::test_support::symbols(target, &native, &mut inventory);
    inventory.sort_by(|left, right| left.path.cmp(&right.path));
    TargetPayload {
        version: wire::manifest::TARGET_PAYLOAD_VERSION,
        target,
        source: Digest::compute(Domain::DistArtifact, b"source"),
        archive: Digest::compute(Domain::DistSourceArchive, b"source archive"),
        app,
        canvas: Digest::compute(Domain::DistArtifact, b"canvas archive"),
        native,
        symbols,
        pins_digest: Digest::compute(Domain::DistPins, &wire::encode(&pins).expect("pins encode")),
        inventory_digest: Digest::compute(
            Domain::DistInventory,
            &wire::encode(&inventory).expect("inventory encodes"),
        ),
        pins,
        inventory,
    }
}

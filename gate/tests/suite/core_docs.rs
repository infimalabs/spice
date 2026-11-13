//! Repository documents: compiled elastic caps and peer-held bound claims.
//!
//! Both properties are asserted through the actual acquisition path rather
//! than by calling the measurement engine directly.

use gate::{
    ActorCapture, ActorInputs, ClaimCapture, ClaimStatus, CoreEvidence, CoreObservation,
    CoreStanding, CurrentState, GateId, LatchEntry, LatchKind, LatchTransition, PeerClaim,
    RenameMap, RepoDocLimit, StateKey, evaluate_core,
};

use super::support::{
    Tree, acquired, acquired_arm, acquired_inputs, contract_repository, git_commit,
    workspace_document,
};

/// The packaged base cap for Markdown past the bounded depths.
const DEEPEST_MARKDOWN_CHARS: u64 = 120_000;

/// One document, two actors, one compiled cap.
///
/// Actor identity still crosses acquisition for shared claim arbitration, but
/// both actors must judge the same selected bytes against the policy-compiled
/// integer ceiling.
#[test]
fn repository_documents_are_selected_measured_and_judged_only_by_the_engine() {
    let repository = contract_repository("core-acquire-repo-docs");
    let doctrine = "x".repeat(20_000);
    repository.write("DOCTRINE.md", &doctrine);
    repository.write("docs/reference/generated/REFERENCE.md", "deep doctrine");
    git_commit(&repository, "repository document fixture");

    assert_repository_documents(&repository, "worktree-author");
    assert_repository_documents(&repository, "worktree-peer");

    repository.remove();
}

/// Acquire one fixture as `actor` and assert the whole document inventory.
fn assert_repository_documents(repository: &Tree, actor: &str) {
    let report = acquired(
        repository,
        ClaimCapture::Idle,
        ActorCapture::Acting {
            actor: actor.to_owned(),
            bound_claims: Vec::new(),
        },
    );
    let arm = acquired_arm(&report, GateId::RepoDocs);
    let CoreObservation::Measured {
        evidence: CoreEvidence::RepoDocs(evidence),
    } = arm.observation()
    else {
        panic!("repository-document provider emitted no measurement: {arm:?}");
    };
    let [bounded, deepest] = evidence.measurements() else {
        panic!("unexpected repository-document inventory: {evidence:?}");
    };
    assert_eq!(bounded.path(), "DOCTRINE.md");
    assert_eq!(bounded.characters(), 20_000);
    assert_eq!(
        bounded.bound(),
        RepoDocLimit::AtMost { characters: 15_000 },
        "{actor} did not receive the one policy-compiled cap"
    );
    assert!(!bounded.peer_claim());
    assert_eq!(deepest.path(), "docs/reference/generated/REFERENCE.md");
    assert_eq!(deepest.characters(), 13);
    assert!(
        matches!(
            deepest.bound(),
            RepoDocLimit::AtMost { characters } if characters >= DEEPEST_MARKDOWN_CHARS
        ),
        "the deepest Markdown tier is capped at its packaged base: {:?}",
        deepest.bound()
    );
    assert!(!deepest.peer_claim());
    assert_eq!(arm.standing(), CoreStanding::Failed);
}

/// A document a peer already claimed is judged as someone else's seam.
///
/// The two documents are identical across all three acquisitions below; only
/// the ledger the acting actor carries differs. An acquisition that reports a
/// constant `false` for every document produces the character-cap sentence
/// every time, so the first two cannot both hold unless the claim is really
/// resolved. Both of those claim both documents, so the untouched one also
/// shows that a document inside its bound claims nothing regardless of who
/// holds its ledger entry. The third claims only that untouched document: a
/// resolution that answered from whichever claim the ledger happened to hold
/// rather than the one over this document would call the oversized document
/// peer-held over an entry naming a different file.
#[test]
fn a_peer_held_bound_claim_redirects_the_document_diagnostic() {
    let repository = contract_repository("core-acquire-repo-docs-claimed");
    repository.write("DOCTRINE.md", &"x".repeat(20_000));
    repository.write("GUIDE.md", "a document well inside its bound");
    git_commit(&repository, "repository document fixture");

    assert_claimed_document(
        &repository,
        "worktree-author",
        minted_claims(&repository, "worktree-peer", &["DOCTRINE.md", "GUIDE.md"]),
        true,
    );
    assert_claimed_document(
        &repository,
        "  worktree-author  ",
        minted_claims(&repository, "worktree-author", &["DOCTRINE.md", "GUIDE.md"]),
        false,
    );
    assert_claimed_document(
        &repository,
        "worktree-author",
        minted_claims(&repository, "worktree-peer", &["GUIDE.md"]),
        false,
    );

    repository.remove();
}

/// Acquire as `acting` carrying `claims` and assert what the over-limit
/// document was told while its within-bounds neighbour claims nothing.
///
/// The caller hands over whatever spelling it holds, so the padded call below
/// is judged against the same cap and the same owner as the bare one: a claim
/// belongs to an identity, not to a spelling with surrounding space.
fn assert_claimed_document(
    repository: &Tree,
    acting: &str,
    claims: Vec<PeerClaim>,
    peer_claim: bool,
) {
    let report = acquired(
        repository,
        ClaimCapture::Idle,
        ActorCapture::Acting {
            actor: acting.to_owned(),
            bound_claims: claims,
        },
    );
    let arm = acquired_arm(&report, GateId::RepoDocs);
    let CoreObservation::Measured {
        evidence: CoreEvidence::RepoDocs(evidence),
    } = arm.observation()
    else {
        panic!("repository-document provider emitted no measurement: {arm:?}");
    };
    let [bounded, within] = evidence.measurements() else {
        panic!("unexpected repository-document inventory: {evidence:?}");
    };
    assert_eq!(bounded.path(), "DOCTRINE.md");
    assert_eq!(
        bounded.bound(),
        RepoDocLimit::AtMost { characters: 15_000 },
        "the compiled cap is unchanged by who holds the claim"
    );
    assert_eq!(
        arm.standing(),
        if peer_claim {
            CoreStanding::Passed
        } else {
            CoreStanding::Failed
        }
    );
    assert_eq!(
        bounded.peer_claim(),
        peer_claim,
        "DOCTRINE.md names the wrong owner for the ledger {acting:?} handed over"
    );
    assert_eq!(within.path(), "GUIDE.md");
    assert!(
        matches!(within.bound(), RepoDocLimit::AtMost { .. }),
        "the neighbour must carry a cap it stayed inside, not no cap at all"
    );
    assert!(
        !within.peer_claim(),
        "a document inside its bound breached nothing to be claimed over"
    );
    assert!(!arm.diagnostic().contains("GUIDE"));
}

/// One breach the actor owns keeps the whole arm failed.
///
/// Both documents are over their caps and only one of them is claimed, so this
/// fixture proves that peer ownership redirects the arm only when every breach
/// belongs to another repair seam. One actor-owned breach remains actionable
/// even when a neighbouring breach is already assigned.
#[test]
fn an_actor_owned_breach_keeps_a_mixed_document_arm_failed() {
    let repository = contract_repository("core-acquire-repo-docs-mixed");
    repository.write("CHARTER.md", &"x".repeat(20_000));
    repository.write("DOCTRINE.md", &"x".repeat(20_000));
    git_commit(&repository, "two oversized documents, one of them claimed");

    let report = acquired(
        &repository,
        ClaimCapture::Idle,
        ActorCapture::Acting {
            actor: "worktree-author".to_owned(),
            bound_claims: minted_claims(&repository, "worktree-peer", &["CHARTER.md"]),
        },
    );
    let arm = acquired_arm(&report, GateId::RepoDocs);
    let CoreObservation::Measured {
        evidence: CoreEvidence::RepoDocs(evidence),
    } = arm.observation()
    else {
        panic!("repository-document provider emitted no measurement: {arm:?}");
    };
    let [charter, doctrine] = evidence.measurements() else {
        panic!("unexpected repository-document inventory: {evidence:?}");
    };
    assert_eq!(charter.path(), "CHARTER.md");
    assert!(
        charter.peer_claim(),
        "the breach a peer claimed is the peer's seam"
    );
    assert_eq!(doctrine.path(), "DOCTRINE.md");
    assert!(
        !doctrine.peer_claim(),
        "the breach nobody claimed is the acting actor's own"
    );
    assert_eq!(arm.standing(), CoreStanding::Failed);

    repository.remove();
}

/// The tracked declaration decides which documents the arm measures.
///
/// Neither `NOTICE.txt` nor `CHARTER.txt` carries a markdown suffix, so no
/// packaged rule can reach either one and the declaration is the only thing
/// that can put one in front of the arm. `CHARTER.txt` is present and
/// undeclared in both halves, so an arm that measured every tracked file would
/// fail here; the fixture's one markdown guide is measured in both, so an arm
/// that measured nothing at all would fail too.
#[test]
fn a_tracked_document_declaration_decides_what_the_arm_measures() {
    let silent = document_repository("core-acquire-repo-docs-undeclared", None);
    assert_eq!(
        measured_documents(&silent),
        ["GUIDE.md"],
        "a repository declaring nothing measured a document nothing selects"
    );
    silent.remove();

    let declared = document_repository(
        "core-acquire-repo-docs-declared",
        Some("docs = [\"NOTICE.txt\"]"),
    );
    assert_eq!(
        measured_documents(&declared),
        ["GUIDE.md", "NOTICE.txt"],
        "the declared document never reached the arm that measures it"
    );
    declared.remove();
}

/// One fixture carrying two undeclarable documents and an optional declaration.
fn document_repository(label: &str, declaration: Option<&str>) -> Tree {
    let repository = contract_repository(label);
    repository.write("GUIDE.md", "a document well inside its bound");
    repository.write("NOTICE.txt", "doctrine a repository can declare");
    repository.write("CHARTER.txt", "doctrine no repository declared");
    if let Some(docs) = declaration {
        repository.write(
            "spice.toml",
            &format!(
                "{}\n[policy.repo_truth]\n{docs}\n",
                workspace_document("spice.toml")
            ),
        );
    }
    git_commit(&repository, "repository document declaration fixture");
    repository
}

/// Every document path this repository's arm measured, in acquired order.
fn measured_documents(repository: &Tree) -> Vec<String> {
    let report = acquired(
        repository,
        ClaimCapture::Idle,
        ActorCapture::Acting {
            actor: "worktree-author".to_owned(),
            bound_claims: Vec::new(),
        },
    );
    let arm = acquired_arm(&report, GateId::RepoDocs);
    let CoreObservation::Measured {
        evidence: CoreEvidence::RepoDocs(evidence),
    } = arm.observation()
    else {
        panic!("repository-document provider emitted no measurement: {arm:?}");
    };
    evidence
        .measurements()
        .iter()
        .map(|measurement| measurement.path().to_owned())
        .collect()
}

/// Genuine claims over `paths`, created by `actor` through the ledger itself.
///
/// A claim assembled by the test could disagree with the ownership rule
/// acquisition resolves against; one minted by a real transaction cannot.
fn minted_claims(repository: &Tree, actor: &str, paths: &[&str]) -> Vec<PeerClaim> {
    let mut state = CurrentState::load(
        repository.path("current-state.json"),
        repository.path("current-state.lock"),
    )
    .expect("load one empty current ledger");
    let renames = RenameMap::new(Vec::<(String, String)>::new()).expect("no staged renames");
    state
        .begin_run(&renames, paths, 1_700_000_000.0, None)
        .expect("begin one run holding the claimed paths live");
    let decisions = state
        .claim(
            paths,
            &ActorInputs::new(paths[0])
                .expect("actor provenance for the claimed paths")
                .with_active_actor(actor),
        )
        .expect("claim the paths for one actor");
    assert!(
        decisions
            .iter()
            .all(|decision| decision.status() == ClaimStatus::Claimed),
        "an empty ledger leaves every requested path free: {decisions:?}"
    );
    state
        .reconcile(std::iter::empty())
        .expect("reconcile an empty observation inventory");
    state
        .finish()
        .expect("finish the claiming transaction")
        .after()
        .claims()
        .to_vec()
}

/// The acquisition reports the elastic bound every document was judged against.
///
/// The arm's own evidence carries a cap and nothing else, so a run that
/// published only that could not say which rule chose the number, which actor
/// varied it, or how much headroom the ratchet lent. The decision retained here
/// is the one the arm was judged against, resolved through the same
/// `MetricStateBinding` a durable latch reconciles through, so the report and
/// the ledger cannot describe one document two ways.
///
/// The two documents are deliberately unlike. They sit at different depths, so
/// the packaged per-depth Markdown rules select a different rule for each,
/// and only one of them breaches anything: the root document is capped and over
/// its cap, while the nested one is held to the deepest tier and inside it. A
/// decision vector built from one occurrence, or from the first resolution
/// reused for the rest, cannot carry both.
#[test]
fn every_measured_document_carries_the_bound_decision_it_was_judged_against() {
    let repository = contract_repository("core-acquire-repo-docs-decided");
    repository.write("DOCTRINE.md", &"x".repeat(20_000));
    repository.write("docs/reference/generated/REFERENCE.md", "deep doctrine");
    git_commit(
        &repository,
        "one over-cap document beside one deep document",
    );

    let inputs = acquired_inputs(
        &repository,
        ClaimCapture::Idle,
        ActorCapture::Acting {
            actor: "worktree-author".to_owned(),
            bound_claims: Vec::new(),
        },
        &[],
    );
    let [bounded, deepest] = inputs.bounds() else {
        panic!(
            "two measured documents decided neither bound, or only one: {:?}",
            inputs.bounds()
        );
    };

    assert_eq!(bounded.metric(), "repository-document.characters");
    assert_eq!(bounded.measured(), 20_000);
    assert_eq!(bounded.base_limit(), 10_000);
    assert_eq!(
        bounded.elastic_limit(),
        15_000,
        "the policy-compiled ceiling is the same integer every actor receives"
    );
    assert_eq!(bounded.active_limit(), 15_000);
    assert!(!bounded.unlimited());
    assert!(bounded.over_limit() && bounded.over_base() && bounded.elastic_breach());
    assert_eq!(bounded.latch_transition(), LatchTransition::Latched);

    assert_eq!(
        deepest.base_limit(),
        DEEPEST_MARKDOWN_CHARS,
        "the deepest tier's rule never reached this document's decision"
    );
    assert!(!deepest.unlimited());
    assert!(!deepest.over_limit() && !deepest.elastic_breach());
    assert_eq!(deepest.latch_transition(), LatchTransition::Clear);

    repository.remove();
}

/// A document the ledger already latched is held to its base limit.
///
/// The elastic limit is headroom the ratchet lends until it is spent, and a
/// prior breach spent it. The document measured here is the one the test above
/// measures unlatched, under the same tree, actor, and policy; only the durable
/// inventory the caller read differs. A build that never carries the ledger into
/// the decision hands out the lent cap here too, and cannot hold a latched
/// document to anything.
#[test]
fn a_latched_document_is_decided_and_judged_against_its_base_limit() {
    let repository = contract_repository("core-acquire-repo-docs-latched");
    repository.write("DOCTRINE.md", &"x".repeat(20_000));
    git_commit(&repository, "one document a prior run already latched");

    let latched = LatchEntry::new(
        LatchKind::RepoDocChars,
        StateKey::path("DOCTRINE.md").expect("a canonical document key"),
    )
    .expect("a path-keyed document latch");
    let inputs = acquired_inputs(
        &repository,
        ClaimCapture::Idle,
        ActorCapture::Acting {
            actor: "worktree-author".to_owned(),
            bound_claims: Vec::new(),
        },
        std::slice::from_ref(&latched),
    );
    let [decision] = inputs.bounds() else {
        panic!(
            "one measured document decided one bound: {:?}",
            inputs.bounds()
        );
    };
    assert_eq!(
        decision.active_limit(),
        10_000,
        "a latched document kept the elastic headroom a prior breach already spent"
    );
    assert_eq!(decision.elastic_limit(), 15_000);
    assert_eq!(decision.latch_transition(), LatchTransition::Held);

    let report = evaluate_core(&inputs);
    let arm = acquired_arm(&report, GateId::RepoDocs);
    assert_eq!(arm.standing(), CoreStanding::Failed);
    assert!(
        arm.diagnostic()
            .contains("DOCTRINE.md: 20000 characters (cap 10000)"),
        "the arm was judged against a cap the decision did not hold it to: {}",
        arm.diagnostic()
    );
    assert!(
        arm.diagnostic().contains("inherited debt") && arm.diagnostic().contains("unknown"),
        "a refusal held by a latch of unrecorded origin must read as inherited debt of unknown \
         origin, not as a verdict on this change: {}",
        arm.diagnostic()
    );

    repository.remove();
}

/// Editing inherited document debt spends flex until the document heals.
#[test]
fn an_authored_document_above_its_committed_base_is_held_to_the_true_limit() {
    const PATH: &str = "DOCTRINE.md";
    const BASE: u64 = 10_000;
    const DEBT: u64 = 12_000;

    let repository = contract_repository("core-acquire-repo-docs-committed-debt");
    repository.write(PATH, &"x".repeat(DEBT as usize));
    git_commit(&repository, "commit document debt inside flex");

    let acquire = || {
        acquired_inputs(
            &repository,
            ClaimCapture::Idle,
            ActorCapture::Acting {
                actor: "worktree-author".to_owned(),
                bound_claims: Vec::new(),
            },
            &[],
        )
    };
    let unchanged = acquire();
    let [elastic] = unchanged.bounds() else {
        panic!(
            "one document must decide one bound: {:?}",
            unchanged.bounds()
        );
    };
    assert_eq!(
        (
            elastic.base_limit(),
            elastic.elastic_limit(),
            elastic.active_limit(),
        ),
        (BASE, 15_000, 15_000)
    );
    assert!(elastic.over_base() && !elastic.over_limit());
    assert_eq!(elastic.latch_transition(), LatchTransition::Clear);
    assert_eq!(
        acquired_arm(&evaluate_core(&unchanged), GateId::RepoDocs).standing(),
        CoreStanding::Passed,
        "unchanged committed debt keeps its 1.5x flex"
    );

    repository.write(PATH, &"y".repeat(DEBT as usize));
    repository.stage(&[PATH]);
    let changed = acquire();
    let [held] = changed.bounds() else {
        panic!(
            "one changed document must decide one bound: {:?}",
            changed.bounds()
        );
    };
    assert_eq!(held.active_limit(), BASE);
    assert_eq!(held.measured(), DEBT);
    assert!(held.measured() > BASE && held.measured() < held.elastic_limit());
    assert!(held.over_limit() && !held.elastic_breach());
    assert_eq!(held.latch_transition(), LatchTransition::Held);
    assert_eq!(
        acquired_arm(&evaluate_core(&changed), GateId::RepoDocs).standing(),
        CoreStanding::Failed,
        "a same-length authored edit remains actionable until it reaches base"
    );

    repository.write(PATH, &"x".repeat(BASE as usize));
    repository.stage(&[PATH]);
    let healed = acquire();
    let [decision] = healed.bounds() else {
        panic!(
            "one healed document must decide one bound: {:?}",
            healed.bounds()
        );
    };
    assert_eq!(decision.active_limit(), BASE);
    assert!(!decision.over_limit() && !decision.over_base());
    assert_eq!(decision.latch_transition(), LatchTransition::Healed);
    assert_eq!(
        acquired_arm(&evaluate_core(&healed), GateId::RepoDocs).standing(),
        CoreStanding::Passed
    );

    repository.remove();
}

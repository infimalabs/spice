use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::super::{GIT_PRE_COMMIT_PATH, RECORD_PATH};
use super::*;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repository(label: &str) -> (Scratch, Repository) {
    let root = std::env::temp_dir().join(format!(
        "spice-install-progress-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&root).expect("scratch root creates");
    crate::test_repository::initialize(&root);
    let repository = Repository::discover(&root).expect("scratch repository discovers");
    (Scratch(root), repository)
}

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    fs::create_dir_all(path.parent().expect("fixture file has a parent"))
        .expect("fixture parent creates");
    fs::write(path, bytes).expect("fixture bytes write");
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("fixture mode writes");
    #[cfg(windows)]
    sill::authority::set_owner_mode(path, mode).expect("fixture mode writes");
}

fn mode_of(path: &Path) -> u32 {
    let file = fs::File::open(path).expect("fixture reopens");
    sill::authority::file_mode(&file).expect("fixture mode reads") & 0o777
}

fn parent_reversals(plan: &InstallPlan) -> Vec<(Location, String)> {
    plan.operations
        .iter()
        .flat_map(|operation| {
            operation
                .introduced_parents
                .iter()
                .cloned()
                .map(|parent| (operation.location, parent))
        })
        .chain(plan.retirements.iter().flat_map(|retirement| {
            retirement
                .introduced_parents
                .iter()
                .cloned()
                .map(|parent| (Location::CommonGit, parent))
        }))
        .collect()
}

fn install_without_binary(repository: &Repository) -> Vec<(Location, String)> {
    let prepared = prepare(repository, None, None).expect("install prepares");
    let reversals = parent_reversals(prepared.plan());
    apply_prepared_record(repository, None, prepared).expect("install applies");
    reversals
}

fn refresh_without_binary(repository: &Repository) -> Vec<(Location, String)> {
    let LoadedRecord { record, generation } = read_record(repository)
        .expect("receipt reads")
        .expect("receipt exists");
    let plan =
        refresh_plan(repository, None, &record, None).expect("refresh plan carries ownership");
    let reversals = parent_reversals(&plan);
    let prepared = fresh(plan, Some(generation)).expect("refresh prepares");
    apply_prepared_record(repository, None, prepared).expect("refresh applies");
    reversals
}

#[test]
fn fresh_preview_leaves_the_state_root_absent() {
    let (_scratch, repository) = repository("read-only-preview");
    let state_root = repository.state_root();
    assert!(!state_root.exists());

    super::super::preview(&repository, None).expect("install previews");

    assert!(!state_root.exists(), "preview created runtime state");
}

#[test]
fn unapply_retires_disposable_state_and_generated_git_shadows() {
    let (_scratch, repository) = repository("unapply-disposable-state");
    install_without_binary(&repository);
    let shadow = repository
        .git_dir()
        .join(crate::git_shadow::AGENT_GITCONFIG_NAME);
    let linked = repository
        .common_dir()
        .join("worktrees/lane")
        .join(crate::git_shadow::AGENT_GITCONFIG_NAME);
    fs::create_dir_all(linked.parent().expect("linked shadow has a parent"))
        .expect("linked Git administration creates");
    let content = crate::git_shadow::shadow_content("lane", "/etc/gitconfig");
    fs::write(&shadow, &content).expect("Git shadow writes");
    fs::write(&linked, content).expect("linked Git shadow writes");

    let receipt = super::super::unapply(&repository).expect("install unapplies");

    assert_eq!(receipt.status(), InstallStatus::Uninstalled);
    assert!(!shadow.exists(), "generated Git shadow survived unapply");
    assert!(
        !linked.exists(),
        "generated linked Git shadow survived unapply"
    );
    assert!(
        !repository.state_root().exists(),
        "empty runtime state survived unapply"
    );
}

#[test]
fn unapply_preserves_durable_runtime_state() {
    let (_scratch, repository) = repository("unapply-durable-state");
    install_without_binary(&repository);
    let configuration = repository.state_root().join("config.toml");
    fs::write(&configuration, b"[app]\n").expect("runtime configuration writes");

    super::super::unapply(&repository).expect("install unapplies");

    assert_eq!(
        fs::read(&configuration).expect("runtime configuration remains"),
        b"[app]\n"
    );
}

#[test]
fn foreign_git_shadow_refuses_unapply_before_install_mutation() {
    let (_scratch, repository) = repository("unapply-foreign-shadow");
    install_without_binary(&repository);
    let shadow = repository
        .git_dir()
        .join(crate::git_shadow::AGENT_GITCONFIG_NAME);
    fs::write(&shadow, b"operator-owned\n").expect("foreign Git shadow writes");

    assert!(matches!(
        super::super::unapply(&repository),
        Err(InstallError::Drift { .. })
    ));
    assert_eq!(
        fs::read(&shadow).expect("foreign Git shadow remains"),
        b"operator-owned\n"
    );
    assert!(
        repository.common_dir().join(".spice-install").exists(),
        "unapply mutated the appliance before refusing the foreign shadow"
    );
}

#[test]
fn refresh_keeps_each_initial_parent_reversal_until_unapply() {
    let (_scratch, repository) = repository("refresh-parent-ownership");
    let initial = install_without_binary(&repository);
    assert!(!initial.is_empty());
    assert_eq!(
        initial.len(),
        initial.iter().collect::<BTreeSet<_>>().len(),
        "initial install assigns every introduced directory once"
    );

    assert_eq!(refresh_without_binary(&repository), initial);
    assert_eq!(refresh_without_binary(&repository), initial);
    unapply_record(&repository).expect("refreshed install unapplies");

    for (location, relative) in initial {
        assert!(
            !location.root(&repository).join(&relative).exists(),
            "empty introduced directory survived unapply: {relative}"
        );
    }
}

/// Record `path` in the complete receipt as published by a release whose
/// asset label this build does not know, with `bytes` in place at `mode`. A
/// `prior` image becomes the operation's reversal image, stored as its
/// install would have stored it.
fn record_foreign_operation(
    repository: &Repository,
    tag: &str,
    path: &str,
    (bytes, mode): (&[u8], u32),
    prior: Option<(&[u8], u32)>,
) -> Operation {
    let root = repository.common_dir();
    let file = root.join(path);
    let previous = prior.map(|(bytes, mode)| {
        write_mode(&file, bytes, mode);
        GeneratedImage::new(mode, bytes)
    });
    let bound = bind_destination(root, path).expect("foreign path binds");
    if let Some(previous) = &previous {
        reversal::ensure_payload(
            repository,
            previous,
            Some((Location::CommonGit, &bound.destination, &bound.parent)),
        )
        .expect("prior image stores");
    }
    write_mode(&file, bytes, mode);
    let (observed, parent) =
        observe_bound(root, &bound.destination, &bound.parent).expect("foreign file observes");
    let observed = observed.expect("foreign file exists");
    let operation = Operation {
        asset: AssetTag::Unknown(tag.to_owned()),
        location: Location::CommonGit,
        path: path.to_owned(),
        destination: bound.destination.clone(),
        parent: bound.parent,
        previous,
        generated: observed.generated(),
        superseded: None,
        publication_parent: None,
        introduced_parents: bound.introduced_parents,
    };
    let LoadedRecord {
        mut record,
        generation,
    } = read_record(repository)
        .expect("receipt reads")
        .expect("receipt exists");
    record.plan.operations.push(operation.clone());
    record
        .destinations
        .push((Location::CommonGit, bound.destination));
    record.installed.push(Some(observed.generation));
    record.installed_parents.push(parent);
    record.restored.push(None);
    record.completed += 1;
    record.plan_digest = record.plan.digest().expect("plan digests");
    write_record(repository, &record, generation).expect("foreign receipt publishes");
    operation
}

fn preview_text(prepared: PreparedInstall) -> String {
    struct Text(InstallPreview);
    impl std::fmt::Display for Text {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.write_text(formatter, &"APPLY")
        }
    }
    Text(InstallPreview::from_prepared(prepared).expect("plan previews")).to_string()
}

const FOREIGN_SKILL: &str = ".spice-install/agents/skills/spice/SKILL.md";

/// A recorded file this build no longer publishes is discarded by the
/// evidence the receipt holds, whatever label the release that wrote it used:
/// the discard inherits the recorded binding, withdraws the directories that
/// install introduced once empty, and is never restored.
#[test]
fn a_refresh_discards_a_recorded_file_this_build_no_longer_publishes() {
    let (_scratch, repository) = repository("derived-discard");
    install_without_binary(&repository);
    let recorded = record_foreign_operation(
        &repository,
        "skill",
        FOREIGN_SKILL,
        (b"retired skill\n", DATA_MODE),
        None,
    );
    assert!(!recorded.introduced_parents.is_empty());

    let prepared = prepare(&repository, None, None).expect("foreign receipt refreshes");
    let plan = prepared.plan();
    let retirement = plan
        .retirements()
        .iter()
        .find(|retirement| retirement.path() == FOREIGN_SKILL)
        .expect("the unpublished file is retired");
    assert!(retirement.discards_payload());
    assert_eq!(retirement.previous(), None);
    assert_eq!(retirement.parent, recorded.parent);
    assert_eq!(retirement.introduced_parents(), recorded.introduced_parents);
    assert!(plan.orphans().is_empty());
    assert!(
        !plan
            .operations()
            .iter()
            .any(|operation| operation.path() == FOREIGN_SKILL)
    );
    assert!(
        preview_text(prepare(&repository, None, None).expect("plan previews"))
            .contains(&format!("  remove {FOREIGN_SKILL}\n"))
    );

    apply_prepared_record(&repository, None, prepared).expect("refresh applies");
    let file = repository.common_dir().join(FOREIGN_SKILL);
    assert!(!file.exists(), "the discard removed the recorded file");
    assert!(
        !repository
            .common_dir()
            .join(".spice-install/agents")
            .exists(),
        "the emptied introduced directories were withdrawn"
    );
    let receipt = unapply_record(&repository).expect("refreshed install unapplies");
    assert_eq!(receipt.status(), InstallStatus::Uninstalled);
    assert!(!file.exists(), "unapply never restores a discard");
}

/// A recorded file whose content drifted from the recorded image is evidence
/// of someone else's work: the refresh leaves it in place, stops owning it,
/// and names both images instead of refusing.
#[test]
fn a_drifted_recorded_file_is_orphaned_and_reported_instead_of_removed() {
    let (_scratch, repository) = repository("orphaned-drift");
    install_without_binary(&repository);
    let recorded = record_foreign_operation(
        &repository,
        "skill",
        FOREIGN_SKILL,
        (b"retired skill\n", DATA_MODE),
        None,
    );
    let file = repository.common_dir().join(FOREIGN_SKILL);
    write_mode(&file, b"edited by hand\n", DATA_MODE);
    let observed = GeneratedImage::new(DATA_MODE, b"edited by hand\n");

    let prepared = prepare(&repository, None, None).expect("drift does not refuse");
    let plan = prepared.plan();
    assert!(
        !plan
            .retirements()
            .iter()
            .any(|retirement| retirement.path() == FOREIGN_SKILL)
    );
    assert_eq!(
        plan.orphans(),
        [Orphan {
            path: FOREIGN_SKILL.to_owned(),
            recorded: recorded.generated.clone(),
            observed: observed.clone(),
        }]
    );
    let preview = preview_text(prepare(&repository, None, None).expect("plan previews"));
    assert!(
        preview.contains(&format!(
            "  leave {FOREIGN_SKILL}: content differs from the recorded image {} (observed {})",
            recorded.generated.digest(),
            observed.digest()
        )),
        "{preview}"
    );

    let receipt = apply_prepared_record(&repository, None, prepared).expect("refresh applies");
    assert_eq!(receipt.status(), InstallStatus::Complete);
    assert!(
        receipt
            .to_string()
            .contains(&format!("leave {FOREIGN_SKILL}")),
        "{receipt}"
    );
    assert_eq!(fs::read(&file).expect("orphan stays"), b"edited by hand\n");
    assert!(
        !receipt
            .plan()
            .operations()
            .iter()
            .any(|operation| operation.path() == FOREIGN_SKILL),
        "the orphan left receipt ownership"
    );
    unapply_record(&repository).expect("refreshed install unapplies");
    assert_eq!(fs::read(&file).expect("orphan stays"), b"edited by hand\n");
}

/// A recorded file that replaced a prior image is retired with that image,
/// and every later refresh carries the retirement forward with the reversal
/// image its first install captured, so unapply restores what was there.
#[test]
fn a_restorable_retirement_is_carried_forward_and_restored_on_unapply() {
    let (_scratch, repository) = repository("carried-retirement");
    install_without_binary(&repository);
    let xtask = ".spice-install/bin/xtask";
    let prior = b"#!/bin/sh\nexit 1\n";
    let recorded = record_foreign_operation(
        &repository,
        "xtask",
        xtask,
        (b"retired xtask\n", EXECUTABLE_MODE),
        Some((prior, EXECUTABLE_MODE)),
    );
    let previous = recorded.previous.clone().expect("prior image recorded");

    let prepared = prepare(&repository, None, None).expect("foreign receipt refreshes");
    let retirement = prepared
        .plan()
        .retirements()
        .iter()
        .find(|retirement| retirement.path() == xtask)
        .expect("the unpublished file is retired");
    assert!(!retirement.discards_payload());
    assert_eq!(retirement.previous(), Some(&previous));
    apply_prepared_record(&repository, None, prepared).expect("refresh applies");
    let file = repository.common_dir().join(xtask);
    assert!(!file.exists(), "the retirement removed the recorded file");

    refresh_without_binary(&repository);
    let carried = read_record(&repository)
        .expect("receipt reads")
        .expect("receipt exists")
        .record
        .plan;
    assert_eq!(carried.version(), RECORD_VERSION);
    let retirement = carried
        .retirements()
        .iter()
        .find(|retirement| retirement.path() == xtask)
        .expect("the retirement is carried forward");
    assert_eq!(retirement.previous(), Some(&previous));

    unapply_record(&repository).expect("refreshed install unapplies");
    assert_eq!(fs::read(&file).expect("prior bytes restored"), prior);
    assert_eq!(mode_of(&file), EXECUTABLE_MODE);
}

/// A settled receipt is validated by what it records, not by this build's
/// inventory: an unknown label passes, while a path, location, or mode that
/// would reach outside the install roots refuses before any plan, as does a
/// later schema or a field this build does not decode. A plan this build
/// derives is still held to its canonical inventory.
#[test]
fn a_historical_receipt_is_validated_intrinsically_and_a_current_plan_canonically() {
    let (_scratch, repository) = repository("historical-validation");
    let plan = prepare(&repository, None, None)
        .expect("install prepares")
        .into_plan();
    let mut foreign = plan.clone();
    let operation = &mut foreign.operations[0];
    assert_eq!(
        operation.parent.ancestor, "",
        "a fresh scratch repository has no install directory"
    );
    operation.asset = AssetTag::Unknown("later-asset".to_owned());
    operation.path = ".spice-install/later/asset".to_owned();
    operation.destination = ".spice-install/later/asset".to_owned();
    operation.parent.missing = ".spice-install/later".to_owned();
    operation.introduced_parents = vec![
        ".spice-install".to_owned(),
        ".spice-install/later".to_owned(),
    ];
    foreign
        .validate_historical_receipt()
        .expect("an unknown label is evidence a historical receipt may carry");
    assert!(matches!(
        foreign.validate(),
        Err(InstallError::InvalidRecord(_))
    ));

    let refused = |mutate: &dyn Fn(&mut Operation)| {
        let mut plan = foreign.clone();
        mutate(&mut plan.operations[0]);
        matches!(
            plan.validate_historical_receipt(),
            Err(InstallError::InvalidRecord(_))
        )
    };
    assert!(refused(&|operation| {
        operation.path = "../escape".to_owned();
        operation.destination = "../escape".to_owned();
    }));
    assert!(refused(&|operation| {
        operation.path = "/tmp/escape".to_owned();
        operation.destination = "/tmp/escape".to_owned();
    }));
    assert!(refused(&|operation| operation.generated.mode = 0o640));

    let mut later = plan.clone();
    later.version = PLAN_VERSION + 1;
    assert!(matches!(
        later.validate_historical_receipt(),
        Err(InstallError::InvalidRecord(_))
    ));
    let mut encoded: serde_json::Value =
        serde_json::from_slice(&plan.canonical_bytes().expect("plan encodes"))
            .expect("plan is JSON");
    encoded["later_field"] = serde_json::Value::Bool(true);
    assert!(wire::decode::<InstallPlan>(&serde_json::to_vec(&encoded).expect("encodes")).is_err());
}

fn write_foreign_version(repository: &Repository, version: u16) -> Vec<u8> {
    install_without_binary(repository);
    let LoadedRecord { mut record, .. } = read_record(repository)
        .expect("receipt reads")
        .expect("receipt exists");
    record.version = version;
    record.plan.version = version;
    record.plan_digest = record.plan.digest().expect("plan digests");
    let bytes = wire::encode(&record).expect("record encodes");
    fs::write(
        record_path(repository).expect("record path resolves"),
        &bytes,
    )
    .expect("foreign receipt publishes");
    bytes
}

fn assert_foreign<T>(result: Result<T, InstallError>, version: u16, label: &str) {
    match result {
        Err(InstallError::ForeignRecord { version: named, .. }) => {
            assert_eq!(named, version, "{label}")
        }
        Err(error) => panic!("{label} returned the wrong error: {error}"),
        Ok(_) => panic!("{label} was admitted"),
    }
}

/// A receipt of any other schema, older or newer, belongs to the appliance
/// that wrote it: every direction refuses it by version and leaves it intact.
#[test]
fn a_receipt_of_another_schema_refuses_every_install_direction() {
    for version in [RECORD_VERSION - 1, RECORD_VERSION + 1] {
        let label = format!("foreign-{version}");
        let (_scratch, repository) = repository(&label);
        let bytes = write_foreign_version(&repository, version);

        assert_foreign(prepare(&repository, None, None), version, &label);
        let expected = Digest::compute(Domain::InstallPlan, b"foreign receipt");
        assert_foreign(
            apply_expected(&repository, None, &expected, None),
            version,
            &label,
        );
        assert_foreign(
            super::super::audit_appliance(&repository, &repository.common_dir().join("spice")),
            version,
            &label,
        );
        assert_foreign(unapply_record(&repository), version, &label);
        assert_eq!(
            fs::read(record_path(&repository).expect("record path resolves"))
                .expect("receipt reads"),
            bytes,
            "{label}"
        );
    }
}

#[test]
fn unapply_retains_preexisting_directories_and_operator_content_after_refresh() {
    let (_scratch, repository) = repository("refresh-parent-boundaries");
    let preexisting = repository.common_dir().join(".spice-install");
    fs::create_dir_all(&preexisting).expect("preexisting install directory creates");
    install_without_binary(&repository);
    refresh_without_binary(&repository);

    let operator_file = repository.common_dir().join(".spice-install/operator.txt");
    fs::write(&operator_file, b"operator content").expect("operator content writes");
    unapply_record(&repository).expect("refreshed install unapplies");

    assert!(preexisting.is_dir());
    assert_eq!(
        fs::read(&operator_file).expect("operator content remains"),
        b"operator content"
    );
    assert!(
        !repository
            .common_dir()
            .join(".spice-install/githooks")
            .exists()
    );
}

#[test]
fn preserved_payloads_stay_out_of_checkpoints_and_survive_both_resume_frontiers() {
    let (_scratch, repository) = repository("reversal-payload");
    let target = repository.common_dir().join(GIT_PRE_COMMIT_PATH);
    let prior = vec![0xA5; 2 * 1024 * 1024];
    // Windows restores only owner-only modes, so that is all it admits.
    let prior_mode = if cfg!(unix) { 0o640 } else { 0o600 };
    write_mode(&target, &prior, prior_mode);

    // Start a real apply and stop after the preserved target moved but
    // before its completion reached the receipt: the one admitted
    // ahead-of-record crash frontier.
    let prepared = prepare(&repository, None, None).expect("install prepares");
    let PreparedInstall {
        mut record,
        receipt,
        fresh,
    } = prepared;
    assert!(fresh);
    let generated = materialize_operations(&record, None, None).expect("operations materialize");
    record
        .plan
        .validate_pristine(&repository)
        .expect("pristine plan validates");
    require_receipt(&repository, &record.plan, receipt.as_ref()).expect("receipt remains absent");
    let mut generation =
        write_initial_record(&repository, &record).expect("initial receipt publishes");
    reversal::ensure_payloads(&repository, &record.plan).expect("reversal payloads publish");
    let frontier = record
        .plan
        .operations
        .iter()
        .position(|operation| operation.asset == Asset::GitPreCommit)
        .expect("pre-commit hook operation exists");
    let mut durable_completed = record.completed;
    while record.completed < frontier {
        let index = record.completed;
        let (next, changed) = apply_operation(
            &repository,
            &mut record,
            &generated[index],
            index,
            generation,
            durable_completed,
        )
        .expect("operation applies");
        generation = next;
        record.completed += 1;
        if changed {
            generation =
                write_record(&repository, &record, generation).expect("progress publishes");
            durable_completed = record.completed;
        }
    }
    let (_, changed) = apply_operation(
        &repository,
        &mut record,
        &generated[frontier],
        frontier,
        generation,
        durable_completed,
    )
    .expect("frontier operation applies");
    assert!(changed);
    drop(record);

    let resumed = prepare(&repository, None, None).expect("interrupted apply reloads");
    apply_prepared_record(&repository, None, resumed).expect("interrupted apply completes");
    let record_length = fs::metadata(repository.state_root().join(RECORD_PATH))
        .expect("receipt metadata reads")
        .len();
    assert!(
        record_length < u64::try_from(prior.len()).unwrap(),
        "the receipt embedded the preserved payload"
    );

    // Enter unapply and stop after one real file-reversal checkpoint. The
    // next invocation must accept that durable prefix and finish from the
    // external payload store.
    let LoadedRecord {
        mut record,
        generation,
    } = read_record(&repository)
        .expect("receipt reads")
        .expect("receipt exists");
    verify_progress(&repository, &record).expect("completed install verifies");
    reversal::ensure_payloads(&repository, &record.plan).expect("payload store verifies");
    record.status = InstallStatus::Unapplying;
    record.completed = 0;
    record.restored.fill(None);
    record.restored_retirements.fill(None);
    let mut generation = write_record(&repository, &record, generation).expect("unapply starts");
    while let Some(next) = reverse_retirement(&repository, &mut record, generation.clone())
        .expect("retirement reverses")
    {
        generation = next;
    }
    reverse_file_operation(&repository, &mut record, generation)
        .expect("one file reversal publishes");
    drop(record);

    let receipt = unapply_record(&repository).expect("interrupted unapply completes");
    assert_eq!(receipt.status(), InstallStatus::Uninstalled);
    assert_eq!(fs::read(&target).expect("prior bytes read"), prior);
    assert_eq!(mode_of(&target), prior_mode);
}

/// The changes a person-facing preview lists, each indented under its header.
fn previewed_changes(repository: &Repository, binary: &Path) -> Vec<String> {
    struct Text(InstallPreview);
    impl std::fmt::Display for Text {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.write_text(formatter, &"APPLY")
        }
    }
    let prepared = prepare(repository, Some(binary), None).expect("install prepares");
    let text = Text(InstallPreview::from_prepared(prepared).expect("plan previews")).to_string();
    assert!(text.ends_with("APPLY"), "{text}");
    text.lines()
        .filter_map(|line| line.strip_prefix("  "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_completed_plan_missing_one_current_fixed_asset_is_not_current() {
    let (_scratch, repository) = repository("missing-current-fixed-asset");
    let prepared = prepare(&repository, None, None).expect("install prepares");
    let mut prior = prepared.plan().clone();
    prior
        .operations
        .retain(|operation| operation.asset != Asset::GitHookConfig);

    assert!(
        super::super::current_build_differs(&repository, &prior, None)
            .expect("build comparison succeeds"),
        "a release adding a fixed asset must refresh an older complete receipt"
    );
}

#[test]
fn a_preview_over_a_complete_install_of_the_same_build_changes_nothing() {
    let (_scratch, repository) = repository("converged-preview");
    let builds = Scratch(std::env::temp_dir().join(format!(
        "spice-install-builds-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    )));
    let build = |name: &str| {
        let path = builds.0.join(name);
        let generation = wire::Digest::compute(wire::Domain::DistArtifact, name.as_bytes());
        write_mode(&path, &super::super::canvas::linked(&generation), 0o755);
        path
    };
    let (first, second) = (build("first-spice"), build("second-spice"));
    let prepared = prepare(&repository, Some(&first), None).expect("install prepares");
    apply_prepared_record(&repository, Some(&first), prepared).expect("install applies");

    assert_eq!(
        previewed_changes(&repository, &first),
        Vec::<String>::new(),
        "a complete install of the same build has nothing left to publish"
    );
    assert_eq!(
        previewed_changes(&repository, &second),
        ["update .spice-install/bin/spice"],
        "another build updates exactly the installed binary, its hooks path already set"
    );
}

#[test]
fn lifecycle_policy_is_part_of_the_digest_bound_plan() {
    let (_scratch, repository) = repository("lifecycle-policy");
    let default = prepare(&repository, None, Some(LifecycleOwnerStanding::Default))
        .expect("default lifecycle plan prepares");
    let configured_standing = LifecycleOwnerStanding::Configured(Digest::compute(
        Domain::InstallPlan,
        b"configured keeper policy",
    ));
    let configured = prepare(&repository, None, Some(configured_standing.clone()))
        .expect("configured lifecycle plan prepares");

    assert_ne!(default.digest(), configured.digest());
    assert_eq!(
        configured.plan().lifecycle_owner(),
        Some(&configured_standing)
    );
    struct PreviewText(InstallPreview);
    impl std::fmt::Display for PreviewText {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.write_text(formatter, &"APPLY")
        }
    }
    let text = PreviewText(
        InstallPreview::from_prepared(configured).expect("configured lifecycle plan previews"),
    )
    .to_string();
    assert!(text.contains("ensure Windows lifecycle owner"), "{text}");
}

fn installed_generations(repository: &Repository) -> Vec<Option<Generation>> {
    read_record(repository)
        .expect("receipt reads")
        .expect("receipt exists")
        .record
        .installed
}

#[test]
fn a_lifecycle_plan_publishes_to_the_establishing_boundary_and_settles_once_owned() {
    let (_scratch, repository) = repository("lifecycle-boundary");
    let standing = Some(LifecycleOwnerStanding::Default);
    let prepared = prepare(&repository, None, standing.clone()).expect("lifecycle plan prepares");
    let file_stages = prepared.plan().operation_count();
    let receipt = apply_prepared_record(&repository, None, prepared).expect("publication applies");
    assert_eq!(receipt.status(), InstallStatus::Establishing);
    assert_eq!(receipt.completed(), file_stages);
    assert_eq!(receipt.plan().operation_count(), file_stages + 1);
    let published = installed_generations(&repository);
    assert!(published.iter().all(Option::is_some));

    // Owner establishment failed after publication: the next apply resumes
    // at the boundary rather than converging or republishing.
    let resumed =
        prepare(&repository, None, standing.clone()).expect("establishing record reloads");
    assert!(!resumed.converged());
    assert_eq!(
        apply_prepared_record(&repository, None, resumed)
            .expect("resume stops at the boundary")
            .status(),
        InstallStatus::Establishing
    );
    assert_eq!(installed_generations(&repository), published);

    let settled = complete_lifecycle(&repository).expect("owned install settles");
    assert_eq!(settled.status(), InstallStatus::Complete);
    assert_eq!(settled.completed(), file_stages + 1);
    assert_eq!(
        complete_lifecycle(&repository)
            .expect("settling twice is harmless")
            .status(),
        InstallStatus::Complete
    );
    assert!(
        prepare(&repository, None, standing)
            .expect("settled install reloads")
            .converged()
    );
}

#[test]
fn a_plan_without_a_lifecycle_owner_settles_complete_directly() {
    let (_scratch, repository) = repository("no-lifecycle");
    assert!(matches!(
        complete_lifecycle(&repository),
        Err(InstallError::RecordMissing)
    ));
    let prepared = prepare(&repository, None, None).expect("install prepares");
    let receipt = apply_prepared_record(&repository, None, prepared).expect("install applies");
    assert_eq!(receipt.status(), InstallStatus::Complete);
    assert_eq!(receipt.completed(), receipt.plan().operation_count());
}

#[test]
fn an_establishing_install_reverses_like_a_complete_one() {
    let (_scratch, repository) = repository("lifecycle-reversal");
    let prepared = prepare(&repository, None, Some(LifecycleOwnerStanding::Default))
        .expect("lifecycle plan prepares");
    let launcher = repository.common_dir().join(GIT_PRE_COMMIT_PATH);
    assert_eq!(
        apply_prepared_record(&repository, None, prepared)
            .expect("publication applies")
            .status(),
        InstallStatus::Establishing
    );
    assert!(launcher.exists());
    let receipt = unapply_record(&repository).expect("establishing install reverses");
    assert_eq!(receipt.status(), InstallStatus::Uninstalled);
    assert!(!launcher.exists());
}

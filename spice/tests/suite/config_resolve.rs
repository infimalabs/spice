use std::path::Path;

use lane::life::Life;
use spice::config::{
    Builtin, Document, FlagError, FlagOverride, FlagValue, Inputs, IssueKind, PathSegment,
    Resolved, Root, descriptor, resolve,
};
use wire::{ConfigSource, Name};

#[cfg(not(windows))]
fn absolute(name: &str) -> String {
    format!("/opt/spice/{name}")
}

#[cfg(windows)]
fn absolute(name: &str) -> String {
    format!("C:/spice/{name}")
}

fn documents<'a>(tracked: Option<&'a str>, local: Option<&'a str>) -> Inputs<'a> {
    Inputs::new(
        tracked.map(|text| Document::new("spice.toml", text)),
        local.map(|text| Document::new(".spice/local.toml", text)),
        &[],
    )
}

fn worktree_documents<'a>(
    tracked: Option<&'a str>,
    worktree: Option<&'a str>,
    local: Option<&'a str>,
) -> Inputs<'a> {
    Inputs::with_worktree(
        tracked.map(|text| Document::new("spice.toml", text)),
        worktree.map(|text| Document::new(".git/worktrees/lane/.spice/worktree.toml", text)),
        local.map(|text| Document::new(".git/.spice/config.toml", text)),
        &[],
    )
}

#[test]
fn worktree_vendor_model_and_effort_override_local_without_admitting_authority() {
    let resolved = resolve(worktree_documents(
        None,
        Some("[vendor.omp]\nmodel = 'lane-model'\neffort = 'xhigh'\n"),
        Some("[vendor.omp]\nmodel = 'shared-model'\neffort = 'low'\n"),
    ))
    .expect("worktree launch preferences resolve");

    assert_eq!(resolved.root().vendor().omp().model(), "lane-model");
    assert_eq!(
        resolved.root().vendor().omp().effort(),
        vendor::Effort::XHigh
    );
    for path in ["vendor.omp.model", "vendor.omp.effort"] {
        assert_eq!(path_source(&resolved, path), ConfigSource::Worktree);
    }

    let error = resolve(worktree_documents(
        None,
        Some(concat!(
            "[commands]\nverify = ['unsafe-tool']\n",
            "[vendor.omp]\nbinary = '/tmp/omp'\nfast_mode = true\n",
        )),
        None,
    ))
    .expect_err("worktree intent cannot introduce commands or vendor authority");
    for path in [
        "commands.verify",
        "vendor.omp.binary",
        "vendor.omp.fast_mode",
    ] {
        assert_policy_issue(&error, ConfigSource::Worktree, path);
    }
}

#[test]
fn worktree_voices_override_the_shared_local_voices() {
    let resolved = resolve(worktree_documents(
        None,
        Some("[say]\nvoices = ['Samantha', 'English (United States)']\n"),
        Some("[say]\nvoices = ['Zoe (Premium)']\n"),
    ))
    .expect("worktree voices resolve");

    assert_eq!(
        resolved.root().say().voices(),
        ["Samantha", "English (United States)"]
    );
    assert_eq!(path_source(&resolved, "say.voices"), ConfigSource::Worktree);

    let shared = resolve(worktree_documents(
        None,
        None,
        Some("[say]\nvoices = ['Zoe (Premium)']\n"),
    ))
    .expect("shared local voices resolve");
    assert_eq!(shared.root().say().voices(), ["Zoe (Premium)"]);
    assert_eq!(path_source(&shared, "say.voices"), ConfigSource::Local);
}

#[test]
fn tracked_policy_cannot_pin_worktree_preferences() {
    let error = resolve(worktree_documents(
        Some(concat!(
            "[vendor.omp]\nmodel = 'tracked-model'\neffort = 'low'\n",
            "[say]\nvoices = ['Zoe (Premium)']\n",
        )),
        None,
        None,
    ))
    .expect_err("tracked policy never pins a worktree preference");
    for path in ["vendor.omp.model", "vendor.omp.effort", "say.voices"] {
        assert_policy_issue(&error, ConfigSource::Tracked, path);
    }
}

fn assert_policy_issue(error: &spice::config::ResolveError, source: ConfigSource, path: &str) {
    assert!(
        error.issues().iter().any(|issue| {
            issue.kind() == IssueKind::Policy
                && issue.source() == source
                && issue
                    .path()
                    .is_some_and(|actual| actual.to_string() == path)
        }),
        "expected a {source} policy issue at {path}"
    );
}

fn path_source(resolved: &Resolved, expected: &str) -> ConfigSource {
    resolved
        .provenance()
        .iter()
        .find(|row| row.path().to_string() == expected)
        .unwrap_or_else(|| panic!("missing provenance for {expected}"))
        .source()
}

fn path_builtin(resolved: &Resolved, expected: &str) -> Option<Builtin> {
    resolved
        .provenance()
        .iter()
        .find(|row| row.path().to_string() == expected)
        .unwrap_or_else(|| panic!("missing provenance for {expected}"))
        .builtin()
}

fn flag(path: &[PathSegment<'_>], value: FlagValue) -> FlagOverride {
    let mapping = descriptor(path)
        .and_then(|entry| entry.flag())
        .expect("fixture path has a command mapping");
    FlagOverride::new(mapping, value).expect("fixture value matches its mapping")
}

#[test]
fn missing_and_empty_documents_are_empty_intent() {
    let missing = resolve(Inputs::empty()).expect("compiled defaults resolve");
    let empty = resolve(documents(Some(""), Some(""))).expect("empty documents resolve");

    assert_eq!(missing, empty);
}

#[test]
fn local_rtk_executable_overrides_its_builtin_with_provenance() {
    let local_path = absolute("local-rtk");
    let local = format!("[rtk]\nexecutable = {local_path:?}\n");
    let resolved = resolve(documents(None, Some(&local))).expect("local RTK executable resolves");

    assert_eq!(resolved.root().rtk().executable(), local_path);
    assert_eq!(
        path_source(&resolved, "rtk.executable"),
        ConfigSource::Local
    );
    assert_eq!(
        path_builtin(&resolved, "rtk.executable"),
        Some(Builtin::String("rtk"))
    );
}

#[test]
fn disabling_builtins_without_a_command_refuses_at_the_authored_field() {
    let error = resolve(documents(
        Some("[policy]\npre_commit_builtins = false\n"),
        None,
    ))
    .expect_err("an empty effective pre-commit battery must refuse");
    let [issue] = error.issues() else {
        panic!("one authored field should explain the empty battery");
    };
    assert_eq!(issue.source(), ConfigSource::Tracked);
    assert_eq!(issue.kind(), IssueKind::Malformed);
    assert_eq!(
        issue.path().map(ToString::to_string).as_deref(),
        Some("policy.pre_commit_builtins")
    );
}

#[test]
fn speech_resolves_from_local_settings_with_device_default_voice() {
    let disabled = resolve(documents(None, Some("[say]\nbackend = 'disabled'\n")))
        .expect("local disabled speech resolves");
    assert_eq!(
        disabled.root().say().backend(),
        serve::SpeechBackend::Disabled
    );
    assert_eq!(path_source(&disabled, "say.backend"), ConfigSource::Local);

    let unset = resolve(documents(None, None)).expect("unset speech resolves");
    assert_eq!(unset.root().say().backend(), serve::SpeechBackend::System);
    assert!(unset.root().say().voices().is_empty());
    assert_eq!(unset.root().say().words_per_minute(), 200);

    let system = resolve(documents(None, Some("[say]\nwords_per_minute = 205\n")))
        .expect("local system speech resolves");
    assert_eq!(system.root().say().words_per_minute(), 205);
}

#[test]
fn speech_configuration_refuses_malformed_settings() {
    for (document, path) in [
        ("[say]\nbackend = 'mystery'\n", "say.backend"),
        ("[say]\nvoices = ['Zoe', '']\n", "say.voices"),
        ("[say]\nwords_per_minute = 0\n", "say.words_per_minute"),
    ] {
        let error = resolve(documents(None, Some(document)))
            .expect_err("malformed speech configuration must be refused");
        assert!(
            error.issues().iter().any(|issue| {
                issue
                    .path()
                    .is_some_and(|candidate| candidate.to_string() == path)
                    && issue.kind() == IssueKind::Malformed
            }),
            "missing malformed issue for {path}: {:?}",
            error.issues()
        );
    }
}

#[test]
fn current_metadata_resolves_from_its_owned_layers() {
    let tracked = concat!(
        "[policy]\nenv_names = ['FIXTURE_INBOX']\n",
        "[taste.polling]\nwords = ['polling']\nmessage = 'use events'\n",
    );
    let local = "[lock.worker]\ndirectory = 'locks/worker'\nshards = 2\n";

    let current = resolve(documents(Some(tracked), Some(local)))
        .expect("current resolution admits typed shared metadata");
    assert_eq!(current.root().policy().env_names(), ["FIXTURE_INBOX"]);
    assert_eq!(current.root().taste().bag().tastes().len(), 1);
    assert_eq!(current.root().lock().named().len(), 1);
    assert_eq!(
        path_source(&current, "taste.polling.words"),
        ConfigSource::Tracked
    );
    assert_eq!(
        path_source(&current, "lock.worker.shards"),
        ConfigSource::Local
    );
}

#[test]
fn every_two_layer_precedence_pair_is_deterministic() {
    let tracked = resolve(documents(Some("[lane]\nvendor = \"fixture\"\n"), None))
        .expect("tracked shadows builtin");
    assert_eq!(tracked.root().lane().vendor().as_str(), "fixture");
    assert_eq!(path_source(&tracked, "lane.vendor"), ConfigSource::Tracked);
    assert_eq!(
        path_builtin(&tracked, "lane.vendor"),
        Some(Builtin::String("omp"))
    );

    let local = resolve(documents(None, Some("[listen]\nwindow = \"2m\"\n")))
        .expect("local shadows builtin");
    assert_eq!(
        local.root().listen().window(),
        std::time::Duration::from_secs(120)
    );
    assert_eq!(path_source(&local, "listen.window"), ConfigSource::Local);

    let vendor_flag = flag(
        &[PathSegment::Key("lane"), PathSegment::Key("vendor")],
        FlagValue::Name("gemini".parse::<Name>().expect("valid vendor")),
    );
    let only_flag = resolve(Inputs::new(None, None, std::slice::from_ref(&vendor_flag)))
        .expect("flag shadows builtin");
    assert_eq!(only_flag.root().lane().vendor().as_str(), "gemini");
    assert_eq!(path_source(&only_flag, "lane.vendor"), ConfigSource::Flag);
    assert_eq!(
        path_builtin(&only_flag, "lane.vendor"),
        Some(Builtin::String("omp"))
    );

    let tracked_flag = resolve(Inputs::new(
        Some(Document::new(
            "spice.toml",
            "[lane]\nvendor = \"fixture\"\n",
        )),
        None,
        std::slice::from_ref(&vendor_flag),
    ))
    .expect("flag shadows tracked");
    assert_eq!(tracked_flag.root().lane().vendor().as_str(), "gemini");
    assert_eq!(
        path_source(&tracked_flag, "lane.vendor"),
        ConfigSource::Flag
    );

    let local_and_flag = resolve(Inputs::new(
        None,
        Some(Document::new(
            ".spice/local.toml",
            "[listen]\nwindow = \"4m\"\n",
        )),
        &[vendor_flag],
    ))
    .expect("local and flag own disjoint admitted leaves");
    assert_eq!(
        path_source(&local_and_flag, "listen.window"),
        ConfigSource::Local
    );
    assert_eq!(
        path_source(&local_and_flag, "lane.vendor"),
        ConfigSource::Flag
    );
}

#[test]
fn shared_metadata_is_tracked_only_and_enforces_names() {
    let resolved = resolve(documents(
        Some(
            r#"
[policy]
env_names = ["FIXTURE_INBOX", "FIXTURE_PROOF"]
"#,
        ),
        None,
    ))
    .expect("shared tracked metadata resolves");

    assert_eq!(
        resolved.root().policy().env_names(),
        ["FIXTURE_INBOX", "FIXTURE_PROOF"]
    );
    assert_eq!(
        path_source(&resolved, "policy.env_names"),
        ConfigSource::Tracked
    );

    let error = resolve(documents(
        Some(
            r#"
[policy]
env_names = ["FIXTURE_INBOX", "FIXTURE_INBOX"]
"#,
        ),
        Some(
            r#"
[policy]
env_names = ["LOCAL_NAME"]
"#,
        ),
    ))
    .expect_err("invalid and local shared metadata refuses");

    for (source, kind, path) in [
        (
            ConfigSource::Tracked,
            IssueKind::Malformed,
            "policy.env_names",
        ),
        (ConfigSource::Local, IssueKind::Policy, "policy.env_names"),
    ] {
        assert!(
            error.issues().iter().any(|issue| {
                issue.source() == source
                    && issue.kind() == kind
                    && issue
                        .path()
                        .is_some_and(|actual| actual.to_string() == path)
            }),
            "missing {source:?} {kind:?} at {path}: {:#?}",
            error.issues()
        );
    }
}

#[test]
fn proof_executable_paths_are_optional_local_values_with_exact_rendering() {
    let container = absolute("bin/../container");
    let runner = absolute("runner");

    for (include_container, include_runner) in [(true, false), (false, true), (true, true)] {
        let local = if include_container {
            format!("[proof]\ncontainer = \"{container}\"\n")
        } else {
            Default::default()
        };
        let worktree = if include_runner {
            format!("[proof]\nrunner = \"{runner}\"\n")
        } else {
            Default::default()
        };
        let resolved = resolve(worktree_documents(None, Some(&worktree), Some(&local)))
            .expect("absolute local and worktree paths resolve");

        assert_eq!(
            resolved.root().proof().container().map(AsRef::as_ref),
            include_container.then_some(Path::new(&container))
        );
        assert_eq!(
            resolved.root().proof().runner().map(AsRef::as_ref),
            include_runner.then_some(Path::new(&runner))
        );
        for (path, present, source) in [
            ("proof.container", include_container, ConfigSource::Local),
            ("proof.runner", include_runner, ConfigSource::Worktree),
        ] {
            assert_eq!(
                path_source(&resolved, path),
                if present {
                    source
                } else {
                    ConfigSource::Builtin
                }
            );
            assert_eq!(
                path_builtin(&resolved, path),
                present.then_some(Builtin::Absent)
            );
        }

        let rendered = toml::to_string_pretty(resolved.root()).expect("effective TOML renders");
        let toml_round_trip: Root = toml::from_str(&rendered).expect("effective TOML decodes");
        assert_eq!(&toml_round_trip, resolved.root());
        let json = serde_json::to_value(resolved.root()).expect("effective JSON renders");
        assert_eq!(
            json["proof"]
                .get("container")
                .and_then(|value| value.as_str()),
            include_container.then_some(container.as_str())
        );
        assert_eq!(
            json["proof"].get("runner").and_then(|value| value.as_str()),
            include_runner.then_some(runner.as_str())
        );
        let json_round_trip: Root =
            serde_json::from_value(json).expect("effective JSON decodes without null paths");
        assert_eq!(&json_round_trip, resolved.root());
    }
}

#[test]
fn proof_runner_binds_only_in_its_worktree_while_the_container_stays_git_common() {
    let sealed = absolute("sealed-runner");
    let container = absolute("container");
    let worktree = format!("[proof]\nrunner = \"{sealed}\"\n");
    let local = format!("[proof]\ncontainer = \"{container}\"\n");
    let resolved = resolve(worktree_documents(None, Some(&worktree), Some(&local)))
        .expect("a worktree runner resolves beside the Git-common container");
    assert_eq!(
        resolved.root().proof().runner().map(AsRef::as_ref),
        Some(Path::new(&sealed))
    );
    assert_eq!(
        path_source(&resolved, "proof.runner"),
        ConfigSource::Worktree
    );
    assert_eq!(
        path_source(&resolved, "proof.container"),
        ConfigSource::Local
    );

    // A runner is sealed for one worktree's HEAD and vendor tree, so a
    // Git-common default would hand every other worktree a stale seal: it is
    // reported, never applied.
    let shared = absolute("shared-runner");
    let stale = format!("[proof]\ncontainer = \"{container}\"\nrunner = \"{shared}\"\n");
    let error = resolve(worktree_documents(None, Some(&worktree), Some(&stale)))
        .expect_err("a Git-common runner never binds a worktree");
    assert_policy_issue(&error, ConfigSource::Local, "proof.runner");
    assert!(
        error
            .issues()
            .iter()
            .any(|issue| issue.message().contains("worktree.toml")
                && issue.message().contains("never a Git-common default"))
    );

    let worktree_container = format!("[proof]\ncontainer = \"{container}\"\n");
    let error = resolve(worktree_documents(None, Some(&worktree_container), None))
        .expect_err("a worktree never selects the Git-common container");
    assert_policy_issue(&error, ConfigSource::Worktree, "proof.container");

    let runner = descriptor(&[PathSegment::Key("proof"), PathSegment::Key("runner")])
        .expect("proof runner descriptor");
    assert_eq!(runner.layers(), [ConfigSource::Worktree]);
    let container = descriptor(&[PathSegment::Key("proof"), PathSegment::Key("container")])
        .expect("proof container descriptor");
    assert_eq!(container.layers(), [ConfigSource::Local]);
    assert!(runner.flag().is_none() && container.flag().is_none());
}

#[test]
fn proof_executable_paths_refuse_tracked_relative_and_empty_routes() {
    let container = absolute("container");
    let runner = absolute("runner");
    let tracked = format!("[proof]\ncontainer = \"{container}\"\nrunner = \"{runner}\"\n");
    let local = "[proof]\ncontainer = \"container\"\nrunner = \"\"\nextra = \"ignored\"\n";
    let error = resolve(documents(Some(&tracked), Some(local)))
        .expect_err("tracked, relative, empty, and unknown proof paths refuse together");

    assert_eq!(
        error
            .issues()
            .iter()
            .map(|issue| (
                issue.path().map(ToString::to_string),
                issue.source(),
                issue.kind()
            ))
            .collect::<Vec<_>>(),
        [
            (
                Some("proof.container".to_owned()),
                ConfigSource::Tracked,
                IssueKind::Policy,
            ),
            (
                Some("proof.container".to_owned()),
                ConfigSource::Local,
                IssueKind::Malformed,
            ),
            (
                Some("proof.extra".to_owned()),
                ConfigSource::Local,
                IssueKind::Unknown,
            ),
            (
                Some("proof.runner".to_owned()),
                ConfigSource::Tracked,
                IssueKind::Policy,
            ),
            (
                Some("proof.runner".to_owned()),
                ConfigSource::Local,
                IssueKind::Policy,
            ),
        ]
    );
    assert!(
        error
            .issues()
            .iter()
            .filter(|issue| issue.kind() == IssueKind::Policy)
            .all(|issue| issue.message().contains(".spice/"))
    );
}

const TRACKED_ALL_DESCRIPTOR_DOCUMENT: &str = r#"
[commands]
"report.inspect" = ["tool", "report", "inspect"]
release = ["tool", "release"]

[wrappers]
spice = "appliance"

[taste.alpha]
words = ["done"]
message = "Done."
enabled = false
scopes = { paths = ["src/**"], extensions = [".rs"], vendors = ["omp"], models = ["gpt-current"], phases = ["pre-commit-success"] }

[judge]
enabled = true

[policy]
env_names = ["FIXTURE_INBOX", "FIXTURE_PROOF"]
pre_commit_builtins = true
pre_commit = [
    { label = "Rust gate", run = ["gate/pre-commit"], scopes = { paths = ["src/**"] } },
]
test_engines = ["cargo-workspace"]
package_roots = ["src/fixture"]
generated_paths = ["build/fixture"]
name_cluster_threshold = 4
test_paths = ["tests/fixture"]
admission = ["fixture-load", "--offline"]
[policy.repo_truth]
docs = ["DOCTRINE.md"]

[policy.file_shape]
source_suffixes = [".rs", ".py"]

[policy.languages]
complexity = [".rs", ".py"]
magic = [".rs"]

[policy.package]
boundary_underscore_pattern = "^[a-z][a-z0-9_]*$"

[policy.commit_message]
allowed_trailers = ["Spice-Origin"]
blocked_trailers = ["Co-Authored-By"]


[consent]
posture = "deny"

[[consent.rule]]
name = "reads"
tool = "read*"
answer = "allow"
budget = 2

[gate.walk]
skip = ["target/**"]

[board.next]
horizon = "4h"

[lane]
life = "Steer"
lease = "20m"
runtime = "8h"
vendor = "omp"
deaths = 4
young = "40s"
hold = "40m"
narration = 40

[steer]
expiry = "12h"
resend = ["2m", "4m", "6m"]

[vendor.omp]
binary = "/opt/omp"
fast_mode = true
lsp = true
auto_compact_window_tokens = 250000

"#;

/// Every machine-local leaf, which only Git-common local config admits.
const LOCAL_MACHINE_DOCUMENT: &str = r#"
[rtk]
executable = "local-rtk"

[listen]
window = "2m"

[lock]
busy = 70
taken = 71
full = 72

[lock.alpha]
directory = "locks/alpha"
shards = 2

[lock.single]
path = "locks/single.lock"

[serve.pack]
rows = 50

[say]
backend = "disabled"
words_per_minute = 190
voices = ["Samantha"]
"#;

#[test]
fn authored_layers_retain_their_provenance() {
    let local = format!(
        concat!(
            "{}",
            "[proof]\ncontainer = \"{}\"\n",
            "[vendor.omp]\nmodel = \"omp-current\"\neffort = \"max\"\n",
            "[release.darwin]\nidentity = \"Developer ID Application: Holder (TEAM123456)\"\n",
            "notary_profile = \"spice\"\n",
        ),
        LOCAL_MACHINE_DOCUMENT,
        absolute("container"),
    );
    let worktree = format!("[proof]\nrunner = \"{}\"\n", absolute("runner"));
    let resolved = resolve(worktree_documents(
        Some(TRACKED_ALL_DESCRIPTOR_DOCUMENT),
        Some(&worktree),
        Some(&local),
    ))
    .expect("tracked, local, and worktree descriptors resolve together");

    for row in resolved.provenance() {
        let segments = row.path().segments().collect::<Vec<_>>();
        let layers = spice::config::descriptors()
            .iter()
            .find(|entry| entry.template().matches(&segments))
            .map(|entry| entry.layers());
        let expected = match layers {
            Some(layers) if layers.contains(&ConfigSource::Tracked) => ConfigSource::Tracked,
            Some([ConfigSource::Worktree]) => ConfigSource::Worktree,
            _ => ConfigSource::Local,
        };
        assert_eq!(row.source(), expected, "{}", row.path());
    }
}

#[test]
fn team_tables_preserve_empty_named_declarations_and_policy_boundaries() {
    let resolved = resolve(documents(Some(
        "[team.default]\ndrive = []\n[team.open]\n[team.limited]\ndrive = [{ topic = ['/spice/*'], phase = ['todo'], team = ['self'] }]\n"
    ), None)).expect("whole team tables resolve");
    assert!(resolved.root().team().named("open").is_some());
    assert!(resolved.root().team().named("missing").is_none());
    assert_eq!(path_source(&resolved, "team.open"), ConfigSource::Tracked);
    assert_eq!(
        path_source(&resolved, "team.limited"),
        ConfigSource::Tracked
    );
    for text in [
        "[team.default]\ndrain = [{ exclusive = true }]",
        "[team.named]\ndrive = false",
        "[team.named]\ndrain = [{ topic = ['['] }]",
        "[team.named]\ndrive = [{ unknown = true }]",
    ] {
        assert!(resolve(documents(Some(text), None)).is_err(), "{text}");
    }
    assert!(resolve(documents(None, Some("[team.open]\ndrive = true"))).is_err());
}

#[test]
fn all_four_layers_use_one_leaf_order() {
    let life = flag(
        &[PathSegment::Key("lane"), PathSegment::Key("life")],
        FlagValue::Life(Life::Drain),
    );
    let vendor = flag(
        &[PathSegment::Key("lane"), PathSegment::Key("vendor")],
        FlagValue::Name("gemini".parse().expect("valid vendor")),
    );
    let flags = [life, vendor];
    let resolved = resolve(Inputs::new(
        Some(Document::new(
            "spice.toml",
            "[lane]\nlife = \"Steer\"\nvendor = \"omp\"\n",
        )),
        Some(Document::new(
            ".spice/local.toml",
            "[listen]\nwindow = \"3m\"\n[lock]\nbusy = 71\n",
        )),
        &flags,
    ))
    .expect("all four layers resolve");

    assert_eq!(resolved.root().lane().life(), Life::Drain);
    assert_eq!(resolved.root().lane().vendor().as_str(), "gemini");
    assert_eq!(
        resolved.root().listen().window(),
        std::time::Duration::from_secs(180)
    );
    assert_eq!(resolved.root().lock().exits().busy().get(), 71);
    assert_eq!(path_source(&resolved, "lane.life"), ConfigSource::Flag);
    assert_eq!(path_source(&resolved, "lane.vendor"), ConfigSource::Flag);
    assert_eq!(path_source(&resolved, "listen.window"), ConfigSource::Local);
    assert_eq!(path_source(&resolved, "lock.busy"), ConfigSource::Local);
}

#[test]
fn tables_merge_recursively_and_dynamic_lock_rows_keep_typed_paths() {
    let resolved = resolve(documents(
        Some("[vendor.omp]\nlsp = false\n"),
        Some(concat!(
            "[vendor.omp]\nmodel = \"omp-current\"\n",
            "[lock]\nbusy = 70\ntaken = 71\nfull = 72\n",
            "[lock.alpha]\ndirectory = 'locks/alpha'\nshards = 4\n",
            "[lock.\"a.b\"]\ndirectory = 'locks/a.b'\nshards = 3\n",
            "[lock.zeta]\ndirectory = 'locks/zeta'\nshards = 5\n",
        )),
    ))
    .expect("tables merge across layers");

    assert!(!resolved.root().vendor().omp().lsp());
    assert_eq!(resolved.root().vendor().omp().model(), "omp-current");
    assert_eq!(
        path_source(&resolved, "vendor.omp.lsp"),
        ConfigSource::Tracked
    );
    assert_eq!(
        path_source(&resolved, "vendor.omp.model"),
        ConfigSource::Local
    );
    assert_eq!(resolved.root().lock().exits().busy().get(), 70);
    assert_eq!(
        resolved
            .root()
            .lock()
            .named()
            .map(|(name, target)| (name.as_str(), target.shards().get()))
            .collect::<Vec<_>>(),
        [("a.b", 3), ("alpha", 4), ("zeta", 5)]
    );
    assert_eq!(
        path_source(&resolved, "lock.\"a.b\".shards"),
        ConfigSource::Local
    );
    assert_eq!(path_builtin(&resolved, "lock.alpha.shards"), None);
}

#[test]
fn lists_replace_as_units_and_taste_rows_retain_declared_order() {
    let resolved = resolve(documents(
        Some(
            r#"
[steer]
resend = ["2m", "4m", "6m"]

[gate.walk]
skip = ["target/**"]

[taste.zeta]
words = ["wait"]
message = "Wait."

[taste.alpha]
words = ["done"]
message = "Done."
"#,
        ),
        None,
    ))
    .expect("ordered lists and tastes resolve");

    assert_eq!(
        resolved
            .root()
            .steer()
            .resend()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["2m", "4m", "6m"]
    );
    assert_eq!(
        resolved
            .root()
            .gate()
            .walk()
            .skip()
            .rules()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["target/**"]
    );
    assert_eq!(
        resolved
            .root()
            .taste()
            .bag()
            .tastes()
            .iter()
            .map(|taste| taste.name().as_str())
            .collect::<Vec<_>>(),
        ["zeta", "alpha"]
    );
    assert_eq!(
        path_source(&resolved, "steer.resend"),
        ConfigSource::Tracked
    );
}

mod tail;

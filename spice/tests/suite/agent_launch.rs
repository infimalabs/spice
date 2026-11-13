use std::ffi::OsString;
use std::path::PathBuf;

use sill::Environment;
use spice::agent_launch::{ComposeError, Inputs, SPICE_LANE, compose};
use spice::config::Root;
use vendor::{Effort, Resume, SessionId};
use wire::{CanonicalUuid, Name};

const LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163c7";
const SYSTEM_PROMPT_BYTES_CEILING: usize = 2_048;

fn root(document: &str) -> Root {
    let deserializer = toml::Deserializer::parse(document).expect("launch fixture TOML parses");
    serde_path_to_error::deserialize(deserializer).expect("launch fixture root admits")
}

fn lane() -> CanonicalUuid {
    LANE.parse::<CanonicalUuid>().expect("fixture lane")
}

fn inputs(vendor: &str, environment: Environment, resume: Resume) -> Inputs {
    Inputs {
        worktree: PathBuf::from("/work/tree"),
        vendor: vendor.parse::<Name>().expect("fixture vendor name"),
        prompt: "[$spice](<spice://SKILL.md>)".to_owned(),
        bootstrap: spice::install::skill_bootstrap().to_owned(),
        environment,
        lane: lane(),
        resume,
        config_overlay: Some(PathBuf::from("/supervisors/omp.yml")),
        steering_extension: Some(PathBuf::from("/supervisors/omp-steering.mjs")),
        fast_mode: true,
        model: None,
        effort: None,
    }
}

fn environment_value<'a>(environment: &'a Environment, key: &str) -> Option<&'a str> {
    environment
        .entries()
        .iter()
        .find(|(candidate, _)| candidate == key)
        .and_then(|(_, value)| value.to_str())
}

fn arguments(record: &vendor::Record) -> Vec<String> {
    record
        .arguments()
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn omp_plan_materializes_configured_values_and_the_lane_derived_key() {
    let root = root(
        r#"
[vendor.omp]
binary = "/opt/omp"
model = "opus"
effort = "max"
"#,
    );
    let plan = compose(
        &root,
        inputs(
            "omp",
            Environment::new([("HOME", "/home/operator")]).expect("fixture environment"),
            Resume::Fresh,
        ),
    )
    .expect("omp plan composes");
    let launch = plan.launch();

    assert_eq!(plan.binary(), "/opt/omp");
    assert_eq!(launch.model, "opus");
    assert_eq!(launch.effort, Effort::Max);
    let announced = &launch
        .steering_key
        .as_ref()
        .expect("steering key is materialized")
        .token;
    assert_eq!(
        announced,
        steer::Key::for_identity(lane()).as_str(),
        "the announced steering key is the seal derived from the lane identity"
    );
    assert_ne!(
        announced, LANE,
        "the raw lane uuid is not the announced key"
    );
    assert_eq!(
        environment_value(plan.environment(), SPICE_LANE),
        Some(LANE)
    );
    assert_eq!(launch.bootstrap, spice::install::skill_bootstrap());
    let system_prompt = vendor::omp::system_prompt(launch);
    let turn_prompt = vendor::omp::turn_prompt(launch);
    assert!(
        system_prompt.len() < SYSTEM_PROMPT_BYTES_CEILING,
        "the command-line system card stays compact"
    );
    assert!(turn_prompt.contains(spice::install::SKILL_RESOURCE));
}

#[test]
fn omp_plan_carries_the_global_fast_mode_into_its_opening() {
    let plan = compose(
        &Root::default(),
        inputs("omp", Environment::empty(), Resume::Fresh),
    )
    .expect("omp plan composes");
    assert_eq!(plan.launch().fast_mode, Some(true));
    let record = plan.record().expect("omp plan renders");
    let selection = record
        .model_selection()
        .expect("omp opens with a selection");
    assert_eq!(
        selection,
        &vendor::omp::ModelSelection::new(selection.spelling(), plan.launch().effort, true),
        "the opening states the global fast mode for the selected model"
    );
    let rendered = arguments(&record);
    assert!(!rendered.iter().any(|argument| argument == "--service-tier"));
    assert!(
        rendered
            .windows(2)
            .any(|pair| pair == ["--config", "/supervisors/omp.yml"])
    );
}

#[test]
fn the_omp_roster_names_the_language_server_tool_unless_policy_declines_it() {
    let admitted = compose(
        &Root::default(),
        inputs("omp", Environment::empty(), Resume::Fresh),
    )
    .expect("omp plan composes");
    assert_eq!(admitted.launch().lsp, Some(true));
    assert!(
        arguments(&admitted.record().expect("omp plan renders"))
            .windows(2)
            .any(|pair| pair == ["--tools", vendor::omp::LSP_TOOLS])
    );

    let root = root("[vendor.omp]\nlsp = false\n");
    let declined = compose(&root, inputs("omp", Environment::empty(), Resume::Fresh))
        .expect("omp plan composes");
    assert_eq!(declined.launch().lsp, Some(false));
    assert!(
        arguments(&declined.record().expect("omp plan renders"))
            .windows(2)
            .any(|pair| pair == ["--tools", vendor::omp::TOOLS])
    );
}

#[test]
fn omp_containment_overrides_an_inherited_external_mux_opt_in() {
    let inherited = Environment::new([("PI_DISABLE_LSPMUX", "0")]).expect("fixture environment");
    let plan = compose(
        &root("[vendor.omp]\nlsp = true\n"),
        inputs("omp", inherited, Resume::Fresh),
    )
    .expect("omp plan composes without duplicate environment keys");

    assert_eq!(
        environment_value(plan.environment(), "PI_DISABLE_LSPMUX"),
        Some("1"),
        "the child must not connect to an external mux outside its owner"
    );
}

#[test]
fn fresh_resume_and_recomposition_keep_the_same_admitted_policy() {
    let root = root(
        r#"
[vendor.omp]
model = "gpt-stable"
effort = "high"
fast_mode = false
"#,
    );
    let environment = Environment::new([("PATH", "/usr/bin")]).expect("fixture environment admits");
    let fresh = compose(&root, inputs("omp", environment.clone(), Resume::Fresh))
        .expect("fresh plan composes");
    let restarted = compose(&root, inputs("omp", environment.clone(), Resume::Fresh))
        .expect("restart plan composes");
    assert_eq!(restarted, fresh);

    let resumed = compose(
        &root,
        inputs(
            "omp",
            environment,
            Resume::Session(
                SessionId::new("01a0a807-666f-7524-be0d-2ee476b6c15c").expect("session id"),
            ),
        ),
    )
    .expect("resume plan composes");
    assert_eq!(resumed.binary(), fresh.binary());
    assert_eq!(resumed.environment(), fresh.environment());
    assert_eq!(resumed.launch().model, fresh.launch().model);
    assert_eq!(resumed.launch().effort, fresh.launch().effort);
    assert_eq!(resumed.launch().fast_mode, fresh.launch().fast_mode);
    assert_eq!(
        resumed.launch().fast_mode,
        Some(true),
        "fresh and resumed launches use the authoritative global value"
    );
    assert_eq!(resumed.launch().steering_key, fresh.launch().steering_key);
}

#[test]
fn composition_refuses_unsupported_vendor_and_reserved_lane_environment() {
    for unsupported in ["gemini", "other"] {
        assert_eq!(
            compose(
                &Root::default(),
                inputs(unsupported, Environment::empty(), Resume::Fresh),
            )
            .expect_err("a vendor without a declared value refuses"),
            ComposeError::UnsupportedVendor(unsupported.to_owned())
        );
    }

    let reserved = Environment::new([(OsString::from(SPICE_LANE), OsString::from(LANE))])
        .expect("reserved fixture environment itself admits");
    let error = compose(&Root::default(), inputs("omp", reserved, Resume::Fresh))
        .expect_err("caller-supplied lane identity refuses");
    assert_eq!(error, ComposeError::ReservedEnvironment(SPICE_LANE));
}

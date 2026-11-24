use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use std::{
    io::{self, Write},
    process::ExitStatus,
    sync::OnceLock,
    time::Instant,
};

use listen::SurfaceError;

use spice::ambient::Ambient;
use spice::cli;
#[cfg(not(target_arch = "wasm32"))]
use spice::placeholder::PlaceholderReason;
use wire::{Fault, FaultClass, Outcome, Refusal};

/// Carry a refusal or fault across reply-body types; a reply never stands for
/// a failure.
fn typed_outcome<A, B>(outcome: Outcome<A>) -> Outcome<B> {
    match outcome {
        Outcome::Reply(_) => unreachable!("a failure outcome cannot carry a reply"),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn command_fault<B>(
    class: FaultClass,
    reason: impl Into<String>,
    detail: impl Into<String>,
) -> Outcome<B> {
    Outcome::Fault(Fault {
        class,
        reason: reason.into(),
        detail: detail.into(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn environment_fault<B>(reason: impl Into<String>, detail: impl Into<String>) -> Outcome<B> {
    command_fault(FaultClass::Environment, reason, detail)
}

#[cfg(not(target_arch = "wasm32"))]
fn protocol_fault<B>(reason: impl Into<String>, detail: impl Into<String>) -> Outcome<B> {
    command_fault(FaultClass::Protocol, reason, detail)
}

#[cfg(not(target_arch = "wasm32"))]
fn store_fault<B>(reason: impl Into<String>, detail: impl Into<String>) -> Outcome<B> {
    command_fault(FaultClass::Store, reason, detail)
}

/// A bare refusal naming what was observed, for any reply-body type.
#[cfg(not(target_arch = "wasm32"))]
fn command_refusal<B>(
    reason: impl Into<String>,
    observed: Option<String>,
    detail: impl Into<String>,
) -> Outcome<B> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(reason, None, None, observed).with_detail(detail),
    ))
}

/// An optional non-negative integer flag, refused by name when malformed.
#[cfg(not(target_arch = "wasm32"))]
fn numeric_option<B>(
    matches: &clap::ArgMatches,
    name: &str,
    reason: &str,
) -> Result<Option<usize>, Outcome<B>> {
    matches
        .get_one::<String>(name)
        .map(|written| {
            written.parse::<usize>().map_err(|_| {
                command_refusal(
                    reason,
                    Some(written.clone()),
                    format!("name a non-negative integer after --{name}"),
                )
            })
        })
        .transpose()
}

#[cfg(not(target_arch = "wasm32"))]
mod app_command;
#[cfg(not(target_arch = "wasm32"))]
mod board_command;
#[cfg(not(target_arch = "wasm32"))]
mod cargo_command;
#[cfg(not(target_arch = "wasm32"))]
mod command_client;
#[cfg(not(target_arch = "wasm32"))]
mod config_command;
#[cfg(not(target_arch = "wasm32"))]
mod consent_command;
#[cfg(not(target_arch = "wasm32"))]
mod control_signal;
#[cfg(not(target_arch = "wasm32"))]
mod demo_command;
#[cfg(not(target_arch = "wasm32"))]
mod design_command;
#[cfg(not(target_arch = "wasm32"))]
mod doctor;
#[cfg(not(target_arch = "wasm32"))]
mod gate_command;
#[cfg(not(target_arch = "wasm32"))]
mod install_command;
#[cfg(not(target_arch = "wasm32"))]
mod install_package;
#[cfg(not(target_arch = "wasm32"))]
mod install_symbols_command;
#[cfg(not(target_arch = "wasm32"))]
mod journal_command;
#[cfg(not(target_arch = "wasm32"))]
mod journal_lock;
#[cfg(not(target_arch = "wasm32"))]
mod judge;
#[cfg(not(target_arch = "wasm32"))]
mod lane_command;
#[cfg(not(target_arch = "wasm32"))]
mod lane_selector;
#[cfg(not(target_arch = "wasm32"))]
mod lock_command;
#[cfg(not(target_arch = "wasm32"))]
mod message_shape;
#[cfg(not(target_arch = "wasm32"))]
mod peer_line;
mod proof_command;
#[cfg(not(target_arch = "wasm32"))]
mod quote_command;
#[cfg(not(target_arch = "wasm32"))]
mod read_page;
#[cfg(not(target_arch = "wasm32"))]
mod release_command;
#[cfg(not(target_arch = "wasm32"))]
mod release_seal;
mod render;
#[cfg(not(target_arch = "wasm32"))]
mod resident;
#[cfg(not(target_arch = "wasm32"))]
mod resource;
#[cfg(not(target_arch = "wasm32"))]
mod restore_command;
#[cfg(not(target_arch = "wasm32"))]
mod retro_command;
#[cfg(not(target_arch = "wasm32"))]
mod room_command;
#[cfg(not(target_arch = "wasm32"))]
mod rtk_command;
#[cfg(not(target_arch = "wasm32"))]
mod session_command;
#[cfg(not(target_arch = "wasm32"))]
mod session_media;
#[cfg(not(target_arch = "wasm32"))]
mod settings_command;
#[cfg(not(target_arch = "wasm32"))]
mod steering_command;
#[cfg(not(target_arch = "wasm32"))]
mod steering_read;
#[cfg(not(target_arch = "wasm32"))]
mod tape_command;
#[cfg(not(target_arch = "wasm32"))]
mod task_artifact_command;
#[cfg(not(target_arch = "wasm32"))]
mod task_next_delivery;
#[cfg(not(target_arch = "wasm32"))]
mod taste_command;
#[cfg(not(target_arch = "wasm32"))]
mod team_command;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the binary uses only the repository fixture operations needed by resident tests"
)]
mod test_repository;
#[cfg(not(target_arch = "wasm32"))]
mod topic_command;
#[cfg(not(target_arch = "wasm32"))]
mod trust_command;
mod watch_command;

use render::Form;

const REPLY_EXIT: i32 = 0;
const FAULT_EXIT: i32 = 1;
const REFUSAL_EXIT: i32 = 2;

enum StateFailure {
    Refusal(Refusal),
    Fault(Fault),
}

impl StateFailure {
    #[cfg(not(target_arch = "wasm32"))]
    fn outcome<B>(self) -> Outcome<B> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }
}

fn state_failure(error: &spice::repository::StateError) -> StateFailure {
    match error {
        spice::repository::StateError::Filesystem { .. } => StateFailure::Fault(Fault {
            class: FaultClass::Environment,
            reason: "repository::state-path-unavailable".to_owned(),
            detail: error.to_string(),
        }),
        spice::repository::StateError::Linked { .. }
        | spice::repository::StateError::NotDirectory { .. }
        | spice::repository::StateError::NotRegularFile { .. } => {
            StateFailure::Refusal(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "repository::state-path-refused".to_owned(),
                    Some("repository-state-root".to_owned()),
                    Some(spice::repository::state_shape_expectation()),
                    Some(error.to_string()),
                )
                .with_detail(format!(
                    "{}; repair the malformed state component at {}",
                    error,
                    error.path().display()
                )),
            ))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bootstrap_invocation() -> (Ambient, cli::MountRegistry, clap::ArgMatches, bool) {
    let _ = PROCESS_STARTED.set(Instant::now());
    let ambient = spice::ambient::capture();
    if resident::keeper::package_default_selected(std::env::args_os()) {
        deliver(&resident::keeper::install_default(&ambient), Form::Json);
    }
    if resident::keeper::worker_selected(std::env::args_os()) {
        deliver(&resident::keeper::keep(&ambient), Form::Json);
    }
    if let Some(code) = resident::supervisor::entry(std::env::args_os(), &ambient) {
        terminate_code(code);
    }
    if let Some(code) = app_command::window_entry(std::env::args_os()) {
        terminate_code(code);
    }
    if let Some(code) = gate_command::pre_commit_worker_entry(std::env::args_os(), &ambient) {
        terminate_code(code);
    }
    let bootstrap_form = if cli::bootstrap_json_selected(std::env::args_os()) {
        Form::Json
    } else {
        Form::Text
    };
    let mut arguments = std::env::args_os().collect::<Vec<_>>();
    let test_app_stage = match extract_test_app_stage(&mut arguments) {
        Ok(selected) => selected,
        Err(fault) => deliver(&Outcome::<()>::Fault(fault), bootstrap_form),
    };
    let bootstrap_configuration_owner =
        cli::bootstrap_configuration_owner_selected(arguments.iter().cloned());
    let bootstrap_lock = cli::bootstrap_lock_selected(arguments.iter().cloned());
    let (mounts, matches) = match cli::command().try_get_matches_from(arguments.clone()) {
        Ok(builtin_matches) => match load_mounted_registry(bootstrap_form) {
            Ok(mounts) => {
                let matches = mounts.command().get_matches_from(arguments);
                (mounts, matches)
            }
            Err(error)
                if mount_error_is_ignorable(
                    &error,
                    bootstrap_configuration_owner,
                    bootstrap_lock,
                ) =>
            {
                (cli::MountRegistry::default(), builtin_matches)
            }
            Err(error) => deliver_mount_refusal(error, bootstrap_form),
        },
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayVersion => version_flag(),
        Err(_) => {
            let mounts = mounted_registry(
                bootstrap_form,
                bootstrap_configuration_owner,
                bootstrap_lock,
            );
            let matches = mounts.command().get_matches_from(arguments);
            (mounts, matches)
        }
    };
    (ambient, mounts, matches, test_app_stage)
}

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    let (ambient, mounts, matches, test_app_stage) = bootstrap_invocation();
    #[cfg(target_arch = "wasm32")]
    let matches = cli::command().get_matches();
    #[cfg(not(target_arch = "wasm32"))]
    let configuration = mounts.configuration();
    #[cfg(target_arch = "wasm32")]
    let configuration = None::<&spice::config::RepositoryCapture>;
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(selection) = mounts.selected(&matches) {
            run_mounted(selection);
        }
    }

    let contract = cli::selected_contract(&matches)
        .expect("the generated parser returns only registered command leaves");
    let form = if cli::json_selected(&matches) {
        Form::Json
    } else {
        Form::Text
    };
    let leaf = cli::selected_matches(&matches);
    match contract.path {
        ["watch"] => run_watch(leaf, form),
        #[cfg(not(target_arch = "wasm32"))]
        ["retro"] => retro_command::run(leaf, form),
        ["proof"] => deliver(&proof_command::seal(leaf, configuration), form),
        ["proof", "replay"] => deliver(&proof_command::replay(leaf, configuration), form),
        _ => {}
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let invocation = NativeInvocation(leaf, form, configuration, &ambient);
        invocation.dispatch_gate(contract);
        invocation.dispatch_policy(contract);
        invocation.dispatch_controls(contract);
    }
    #[cfg(target_arch = "wasm32")]
    {
        deliver_target_refusal(contract.path, form);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if contract.path == ["up"] {
        resident_up(leaf, form, test_app_stage, &ambient);
    } else if test_app_stage {
        deliver(
            &environment_fault::<()>(
                "serve::test-app-selection-invalid",
                "the debug-only app stage argument is valid only for `spice up`",
            ),
            form,
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    if contract.path == ["down"] {
        resident_down(leaf, contract, form, &ambient);
    }
    #[cfg(not(target_arch = "wasm32"))]
    deliver_display(&not_landed::<wire::AppClosed>(contract), form);
}

#[cfg(all(not(target_arch = "wasm32"), debug_assertions))]
fn extract_test_app_stage(arguments: &mut Vec<OsString>) -> Result<bool, Fault> {
    let mut occurrences = 0;
    arguments.retain(|argument| {
        if argument == spice::install::TEST_APP_STAGE_ARGUMENT {
            occurrences += 1;
            false
        } else {
            true
        }
    });
    if occurrences > 1 {
        return Err(Fault {
            class: FaultClass::Environment,
            reason: "serve::test-app-selection-invalid".to_owned(),
            detail: "the debug-only app stage argument may appear only once".to_owned(),
        });
    }
    Ok(occurrences == 1)
}

#[cfg(all(not(target_arch = "wasm32"), not(debug_assertions)))]
const fn extract_test_app_stage(_arguments: &mut Vec<OsString>) -> Result<bool, Fault> {
    Ok(false)
}

#[cfg(not(target_arch = "wasm32"))]
static PROCESS_STARTED: OnceLock<Instant> = OnceLock::new();

#[cfg(not(target_arch = "wasm32"))]
fn process_started() -> Instant {
    *PROCESS_STARTED.get_or_init(Instant::now)
}

#[cfg(not(target_arch = "wasm32"))]
mod main_native_runtime;
#[cfg(not(target_arch = "wasm32"))]
use main_native_runtime::{
    deliver_mount_refusal, load_mounted_registry, mount_error_is_ignorable, mounted_registry,
    resident_down, resident_up, run_mounted,
};

#[cfg(not(target_arch = "wasm32"))]
struct NativeInvocation<'a>(
    &'a clap::ArgMatches,
    Form,
    Option<&'a spice::config::RepositoryCapture>,
    &'a Ambient,
);

#[cfg(not(target_arch = "wasm32"))]
impl NativeInvocation<'_> {
    fn dispatch_gate(&self, contract: &cli::Contract) {
        let &Self(leaf, form, configuration, ambient) = self;
        // Under `--json` standard error carries one document; gate progress
        // speaks only to text invocations.
        let live = form == Form::Text;
        match contract.path {
            ["doctor"] => deliver_display(&doctor_outcome(configuration, ambient), form),
            ["demo"] => deliver(&demo_command::run(leaf, ambient), form),
            ["app"] if leaf.get_flag("foreground") => {
                deliver_display(&app_command::run(leaf, ambient), form)
            }
            ["app"] => deliver_display(&app_command::open(leaf, ambient), form),
            ["gate"] => deliver_with_reply_text(
                &gate_command::run(leaf, live, ambient),
                form,
                gate_command::BatteryRun::text,
            ),
            ["gate", "prove"] => deliver_with_reply_text(
                &gate_command::prove(leaf, live, ambient),
                form,
                gate_command::ProveReport::text,
            ),
            ["gate", "camp"] => deliver_with_reply_text(
                &gate_command::camp(leaf, live),
                form,
                gate_command::CampAnswer::text,
            ),
            ["gate", "latches"] => deliver_with_reply_text(
                &gate_command::latches(),
                form,
                gate_command::LatchesAnswer::text,
            ),
            ["gate", "commit-msg"] => {
                deliver_commit_message(gate_command::commit_message(leaf, configuration), form)
            }
            ["gate", "pre-commit"] => deliver_commit_gate(
                &gate_command::pre_commit(leaf, configuration, live, ambient),
                form,
            ),
            ["gate", "selection"] => {
                let outcome = gate_command::selection(live, ambient);
                if gate_command::CurrentSourceSelectionReport::speaks(&outcome) {
                    deliver_with_reply_text(
                        &outcome,
                        form,
                        gate_command::CurrentSourceSelectionReport::text,
                    )
                } else {
                    deliver_commit_gate(&outcome, form)
                }
            }
            ["install"] => match install_command::run(leaf, ambient) {
                install_command::Answer::Plan(outcome) => deliver_display(&outcome, form),
                install_command::Answer::Receipt(outcome) => deliver_display(&outcome, form),
                install_command::Answer::Child(status) => terminate_child(status),
            },
            ["install", "symbols"] => deliver(&install_symbols_command::run(leaf, ambient), form),
            ["config"] => deliver_config(&config_command::run(leaf), form),
            _ => {}
        }
    }

    fn dispatch_retained_read(&self, contract: &cli::Contract) {
        let &Self(leaf, form, _, ambient) = self;
        match contract.path {
            ["room", "history"] => deliver_display(&room_command::run(leaf, contract), form),
            ["room", "inbox"] => deliver_display(
                &room_command::inbox::run(leaf, contract, command_client::seated_lane(ambient)),
                form,
            ),
            ["room", "awareness"] => {
                deliver_display(&room_command::awareness::run(leaf, contract), form)
            }
            ["task", "authority"] => deliver_display(&design_command::run(leaf, contract), form),
            _ => unreachable!("retained read dispatch requires a retained read contract"),
        }
    }

    fn dispatch_policy(&self, contract: &cli::Contract) {
        let &Self(leaf, form, configuration, ambient) = self;
        match contract.path {
            ["dev", "cargo"] => match cargo_command::run(leaf, ambient) {
                cargo_command::RunResult::Child(status) => terminate_child(status),
                cargo_command::RunResult::Rejected { outcome, code } => {
                    deliver_with_code(&outcome, form, code)
                }
            },
            ["dev", "xtask"] => match cargo_command::xtask(leaf, ambient) {
                cargo_command::RunResult::Child(status) => terminate_child(status),
                cargo_command::RunResult::Rejected { outcome, code } => {
                    deliver_with_code(&outcome, form, code)
                }
            },
            ["dev", "cargo-store"] => deliver(&cargo_command::store(leaf, ambient), form),
            ["dev", "cargo-footprint"] => deliver_display(&cargo_command::footprint(), form),
            ["rtk", "gain"] => deliver(&rtk_command::gain(leaf, configuration), form),
            ["consent"] => deliver_display(&consent_command::read(leaf, contract, ambient), form),
            ["consent", "allow"] => deliver(
                &consent_command::decide(leaf, contract, wire::Answer::Granted, ambient),
                form,
            ),
            ["consent", "deny"] => deliver(
                &consent_command::decide(leaf, contract, wire::Answer::Denied, ambient),
                form,
            ),
            ["consent", "policy"] => deliver(&consent_command::policy(contract, ambient), form),
            ["lock"] => deliver(&lock_command::report(configuration), form),
            ["lock", "run"] => match lock_command::run(leaf, configuration, ambient) {
                lock_command::RunResult::Child(status) => terminate_child(status),
                lock_command::RunResult::Rejected { outcome, code } => {
                    deliver_with_code(&outcome, form, code)
                }
            },
            ["task", "artifact", "add"]
            | ["task", "artifact", "list"]
            | ["task", "artifact", "show"]
            | ["task", "artifact", "prune"]
            | ["task", "artifact", "remove"] => {
                deliver(&task_artifact_command::run(leaf, contract), form)
            }
            ["room", "history"]
            | ["room", "inbox"]
            | ["room", "awareness"]
            | ["task", "authority"] => {
                self.dispatch_retained_read(contract);
            }
            ["backup"] => deliver(&journal_command::backup(leaf), form),
            ["restore"] => deliver(&restore_command::restore(leaf), form),
            ["journal"] => deliver_display(&journal_command::report(leaf, contract, ambient), form),
            ["journal", "reindex"] => deliver(&journal_command::reindex(), form),
            ["journal", "verify"] => deliver(&journal_command::verify(contract, ambient), form),
            ["thread"] => deliver_display(&journal_command::thread(leaf, contract, ambient), form),
            ["version"] => deliver(&version_outcome(), form),
            ["release", "notes"] => deliver_with_reply_text(
                &release_command::notes(leaf),
                form,
                release_command::NotesView::text,
            ),
            _ => {}
        }
    }

    fn dispatch_session_control(&self, contract: &cli::Contract) -> bool {
        let &Self(leaf, form, _, ambient) = self;
        match contract.path {
            ["session", "briefing"]
            | ["session", "context"]
            | ["session", "sweep"]
            | ["session", "turns"] => deliver_with_reply_text(
                &session_command::run(leaf, contract.path, &session_environment(ambient)),
                form,
                session_command::Report::text,
            ),
            ["resource", "read"] => {
                deliver_with_reply_text(&resource::read_command(leaf, ambient), form, |answer| {
                    Some(&answer.text)
                })
            }
            _ => false,
        }
    }

    fn dispatch_board_control(&self, contract: &cli::Contract) -> bool {
        let &Self(leaf, form, _, ambient) = self;
        match contract.path {
            ["board"] => deliver_with_reply_text(
                &board_command::board_view(leaf, contract, ambient),
                form,
                board_command::BoardView::text,
            ),
            ["board", "graph"] | ["board", "handout"] => deliver_with_reply_text(
                &board_command::graph_view(leaf, contract, ambient),
                form,
                board_command::GraphView::text,
            ),
            ["task", "export"] => deliver_with_reply_text(
                &board_command::export_task_set(leaf, ambient),
                form,
                board_command::TaskSetExport::text,
            ),
            ["task", "import"] => deliver_with_reply_text(
                &board_command::import_task_set(leaf, ambient),
                form,
                board_command::TaskSetImport::text,
            ),
            ["room", "post"]
            | ["room", "score"]
            | ["room", "leave"]
            | ["room", "contribute"]
            | ["room", "ack"]
            | ["room", "nak"]
            | ["room", "reply"]
            | ["room", "recover"]
            | ["task", "list"]
            | ["task", "status"]
            | ["task", "show"]
            | ["task", "add"]
            | ["task", "capture"]
            | ["task", "join"]
            | ["task", "done"]
            | ["task", "review"]
            | ["task", "note"]
            | ["task", "defer"]
            | ["task", "wake"]
            | ["task", "target"]
            | ["task", "file"]
            | ["task", "place"]
            | ["task", "free"]
            | ["task", "oops"]
            | ["task", "depends"]
            | ["task", "modify"]
            | ["task", "require"]
            | ["task", "close"]
            | ["task", "delete"]
            | ["task", "conclude"]
            | ["group", "list"]
            | ["group", "move"]
            | ["group", "arrange"]
            | ["group", "disband"]
            | ["principal", "list"]
            | ["principal", "admit"]
            | ["principal", "profile"]
            | ["principal", "role", "grant" | "revoke"]
            | ["principal", "deauthorize"]
            | ["task", "decide"]
            | ["task", "adopt"]
            | ["doc", "ledger"]
            | ["sweep"] => {
                deliver_command_answer(&board_command::run(leaf, contract, ambient), form);
            }
            ["task", "next"] => task_next_delivery::deliver(
                &board_command::run_task_next(contract, ambient),
                form,
                ambient.get(steer::hook::LANE).is_some(),
            ),
            _ => false,
        }
    }

    fn dispatch_controls(&self, contract: &cli::Contract) {
        let &Self(leaf, form, configuration, ambient) = self;
        if self.dispatch_session_control(contract) || self.dispatch_board_control(contract) {
            return;
        }
        match contract.path {
            ["restore", "verify"] => deliver(&restore_command::verify(leaf), form),
            ["lane", "list"] => deliver_display(&lane_command::list(leaf, contract, ambient), form),
            ["settings"] | ["settings", "fast"] => {
                deliver_display(&settings_command::run(leaf, contract, ambient), form);
            }
            ["team", "show" | "drive" | "drain" | "fast" | "name"] => {
                deliver_display(&team_command::run(leaf, contract, ambient), form);
            }
            ["lane"]
            | ["lane", "add"]
            | ["lane", "drain"]
            | ["lane", "wake"]
            | ["lane", "model"]
            | ["lane", "effort"]
            | ["lane", "fast"]
            | ["lane", "renew"]
            | ["lane", "remove"]
            | ["lane", "import"]
            | ["recall"] => deliver_command_answer(
                &lane_command::run(leaf, contract, configuration, ambient),
                form,
            ),
            ["lane", "purge"] => deliver(&lane_command::purge::run(ambient), form),
            ["release", "seal"] => deliver(&release_seal::run(contract, ambient), form),
            ["steer"] | ["steer", "cancel"] | ["steer", "resend"] | ["steer", "recover"] => {
                deliver(&steering_command::run(leaf, contract, ambient), form)
            }
            ["inbox"] => {
                deliver_display(&steering_command::inbox::run(leaf, contract, ambient), form)
            }
            ["quote"] => deliver_display(&quote_command::run(leaf, contract, ambient), form),
            ["topic", _] => deliver_display(&topic_command::run(leaf, contract, ambient), form),
            ["trust"] => deliver_display(&trust_command::report(leaf, contract, ambient), form),
            ["trust", "show"] => {
                deliver_display(&trust_command::show_report(leaf, contract, ambient), form)
            }
            ["trust", "pend"] => {
                deliver(&trust_command::pend(contract, configuration, ambient), form)
            }
            ["trust", "grant"] => {
                deliver(&trust_command::rule(leaf, contract, true, ambient), form)
            }
            ["trust", "revoke"] => {
                deliver(&trust_command::rule(leaf, contract, false, ambient), form)
            }
            ["taste"] => deliver_display(
                &taste_command::report(leaf, contract, configuration, ambient),
                form,
            ),
            ["taste", "add"] => deliver(
                &taste_command::add(leaf, contract, configuration, ambient),
                form,
            ),
            ["taste", "drop"] => deliver(
                &taste_command::drop(leaf, contract, configuration, ambient),
                form,
            ),
            ["tape", "archive"] => deliver_display(&tape_command::run(leaf), form),
            ["tape", "check"] => {
                deliver_display(&journal_command::tape::run(leaf, contract, ambient), form)
            }
            _ => {}
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn deliver_target_refusal(path: &[&str], form: Form) {
    let Some(fault) = target_unsupported(path) else {
        return;
    };
    deliver_fault(fault, form);
}

#[cfg(any(test, target_arch = "wasm32"))]
fn target_unsupported(path: &[&str]) -> Option<Fault> {
    let (reason, detail) = match path {
        ["demo"] => (
            "demo::target-unsupported",
            "filesystem seeding and platform browser launch are unavailable on wasm32",
        ),
        ["gate", ..] => (
            "gate::target-unsupported",
            "immutable Git tree selection is unavailable on wasm32",
        ),
        ["release", "notes"] => (
            "release::target-unsupported",
            "immutable Git history inspection for release notes is unavailable on wasm32",
        ),
        ["install"] => (
            "install::target-unsupported",
            "filesystem installation is unavailable on wasm32",
        ),
        ["install", "symbols"] => (
            "install::target-unsupported",
            "installed symbol verification is unavailable on wasm32",
        ),
        ["lock"] | ["lock", "run"] => (
            "lock::target-unsupported",
            "filesystem-backed process locks are unavailable on wasm32",
        ),
        ["restore", "verify"] => (
            "restore::target-unsupported",
            "reading and verifying a restore archive is unavailable on wasm32",
        ),
        ["retro"] => (
            "retro::target-unsupported",
            "transcript tape reads and file export are unavailable on wasm32",
        ),
        ["session", ..] => (
            "session::target-unsupported",
            "filesystem transcript forensics are unavailable on wasm32",
        ),
        ["rtk", "gain"] => (
            "rtk::target-unsupported",
            "repository history discovery and native RTK execution are unavailable on wasm32",
        ),
        _ => return None,
    };
    Some(Fault {
        class: FaultClass::Environment,
        reason: reason.to_owned(),
        detail: detail.to_owned(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn not_landed<B>(contract: &cli::Contract) -> Outcome<B> {
    not_landed_from(contract.path, cli::landing(contract.path))
}

#[cfg(not(target_arch = "wasm32"))]
fn not_landed_from<B>(path: &[&str], landing: cli::Landing) -> Outcome<B> {
    match landing {
        cli::Landing::Landed => protocol_fault(
            PlaceholderReason::DispatchGap.to_reason(),
            format!(
                concat!(
                    "registered landed command reached the terminal ",
                    "release branch: {}"
                ),
                path.join(" ")
            ),
        ),
        cli::Landing::GapOwned(owner) => {
            Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
                PlaceholderReason::NotLanded.to_reason(),
                Some("landing-owner".to_owned()),
                Some(owner.to_owned()),
                Some(path.join(" ")),
            )))
        }
        cli::Landing::Unaccounted => protocol_fault(
            PlaceholderReason::LandingUnaccounted.to_reason(),
            format!(
                "registered command has no landing disposition: {}",
                path.join(" ")
            ),
        ),
    }
}

/// Acquire Doctor's ambient inputs at the binary rim, retaining presence but
/// never the values of Spice-owned variables.
#[cfg(not(target_arch = "wasm32"))]
fn doctor_outcome(
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Outcome<wire::DoctorReport<spice::config::Root>> {
    let working = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            return environment_fault("doctor::working-directory-unavailable", error.to_string());
        }
    };
    let environment: Vec<_> = wire::SpiceVariable::ALL
        .iter()
        .map(|variable| (*variable, ambient.get(variable.as_str()).is_some()))
        .collect();

    doctor::inspect(&working, &environment, ambient, configuration)
}

#[cfg(not(target_arch = "wasm32"))]
fn version_flag() -> ! {
    let error = cli::command()
        .version(spice::version::report())
        .try_get_matches_from(["spice", "--version"])
        .expect_err("the version action exits before command dispatch");
    error.exit()
}

/// Builds the daemonless identity report from the executable and repository
/// this process is actually running against.
#[cfg(not(target_arch = "wasm32"))]
fn version_outcome() -> Outcome<wire::Version> {
    let start = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            return environment_fault("version::working-directory-unavailable", error.to_string());
        }
    };
    let running = match spice::version::running() {
        Ok(running) => running,
        Err(fault) => return Outcome::Fault(fault),
    };
    let repository = match spice::repository::Repository::discover(&start) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    let state = match repository.admit_state() {
        Ok(state) => state,
        Err(error) => return state_failure(&error).outcome(),
    };
    let app = match spice::version::app_generation(
        repository.worktree_root(),
        spice::version::canvas(),
    ) {
        Ok(generation) => generation,
        Err(detail) => {
            return environment_fault("version::app-unavailable", detail);
        }
    };
    let reader = match journal::JournalReader::open(state.journal()) {
        Ok(reader) => reader,
        Err(error) => match state.journal().try_exists() {
            Ok(false) => {
                return command_refusal(
                    "version::journal-absent",
                    Some(state.journal().display().to_string()),
                    "the repository journal is absent; run `spice up` once to initialize it",
                );
            }
            Ok(true) | Err(_) => return journal_version_failure(error),
        },
    };
    let journal = match reader.schema_version() {
        Ok(version) => version,
        Err(error) => return journal_version_failure(error),
    };

    Outcome::Reply(wire::Reply {
        position: wire::ReplyPosition::Direct {},
        body: version_identity(running, journal, app),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn version_identity(
    running: &spice::version::Running,
    journal: u32,
    app: wire::Digest,
) -> wire::Version {
    wire::Version {
        product: running.product.clone(),
        binary: running.binary.clone(),
        wire: wire::WireVersion::CURRENT,
        journal,
        app_protocol: wire::AppProtocolVersion::CURRENT,
        app,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn journal_version_failure(error: journal::OpenError) -> Outcome<wire::Version> {
    match error {
        journal::OpenError::Migration(migration) => Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "journal::schema-refused".to_owned(),
                None,
                Some(format!(
                    "finalized journal schema {}",
                    journal::SCHEMA_VERSION
                )),
                Some(migration.to_string()),
            )
            .with_detail(concat!(
                "finish or replace the unfinalized or incompatible authority ",
                "before asking for version"
            )),
        )),
        error => command_fault(
            FaultClass::Store,
            "version::journal-unreadable",
            error.to_string(),
        ),
    }
}

fn run_watch(matches: &clap::ArgMatches, form: Form) -> ! {
    deliver_display(&watch_command::run(matches, extension_dialects()), form)
}

fn extension_dialects() -> Vec<(&'static str, listen::Dialect)> {
    spice::extension::VENDORS
        .iter()
        .map(|entry| (entry.vendor.name(), entry.vendor.dialect()))
        .collect()
}

fn surface_fault(error: SurfaceError) -> Fault {
    Fault {
        class: FaultClass::Environment,
        reason: "listen::surface".to_owned(),
        detail: error.to_string(),
    }
}

fn render_fault(error: String) -> Fault {
    Fault {
        class: FaultClass::Protocol,
        reason: "render::unencodable".to_owned(),
        detail: error,
    }
}

fn deliver_fault(fault: Fault, form: Form) -> ! {
    deliver(&Outcome::<()>::Fault(fault), form)
}

#[cfg(not(target_arch = "wasm32"))]
fn terminate_child(status: ExitStatus) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            terminate_code(128 + signal);
        }
    }

    terminate_code(status.code().unwrap_or(FAULT_EXIT));
}

/// Renders one outcome in the selected form and exits on it.
///
/// An outcome that cannot be encoded is a defect in this binary's own DTOs
/// rather than anything the caller did, so it becomes a fault with its own
/// exit code. That fault is rendered as text, which is total, so this reaches
/// an exit in one step and never asks the encoder a second time.
fn deliver<B: serde::Serialize>(outcome: &Outcome<B>, form: Form) -> ! {
    deliver_rendering(outcome, render::render(outcome, form))
}

#[cfg(not(target_arch = "wasm32"))]
fn deliver_with_reply_text<B: serde::Serialize>(
    outcome: &Outcome<B>,
    form: Form,
    reply_text: fn(&B) -> Option<&str>,
) -> ! {
    deliver_rendering(
        outcome,
        render::render_with_reply_text(outcome, form, reply_text),
    )
}

fn deliver_display<B: serde::Serialize + std::fmt::Display>(outcome: &Outcome<B>, form: Form) -> ! {
    deliver_rendering(outcome, render::render_display(outcome, form))
}

#[cfg(not(target_arch = "wasm32"))]
fn deliver_command_answer(outcome: &Outcome<wire::intent::CommandAnswer>, form: Form) -> ! {
    deliver_with_reply_text(outcome, form, command_answer_text)
}

#[cfg(not(target_arch = "wasm32"))]
fn command_answer_text(body: &wire::intent::CommandAnswer) -> Option<&str> {
    match body {
        wire::intent::CommandAnswer::Text(answer) => Some(&answer.text),
        wire::intent::CommandAnswer::RoomScore(answer) => Some(&answer.text),
        _ => None,
    }
}

fn deliver_rendering<B>(outcome: &Outcome<B>, rendering: Result<render::Rendering, String>) -> ! {
    match rendering {
        Ok(rendering) => {
            rendering.speak();
            terminate(outcome);
        }
        Err(error) => {
            let fault: Outcome<()> = Outcome::Fault(render_fault(error));
            render::render(&fault, Form::Text)
                .expect("the text rendering of a fault encodes nothing")
                .speak();
            terminate(&fault);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn deliver_commit_gate<B: serde::Serialize>(outcome: &Outcome<B>, form: Form) -> ! {
    match render::render_commit_gate(outcome, form) {
        Ok(Some(rendering)) => {
            rendering.speak();
            terminate(outcome);
        }
        Ok(None) => terminate(outcome),
        Err(error) => {
            let fault: Outcome<()> = Outcome::Fault(render_fault(error));
            render::render(&fault, Form::Text)
                .expect("the text rendering of a fault encodes nothing")
                .speak();
            terminate(&fault);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn deliver_commit_message(run: gate_command::CommitMessageRun, form: Form) -> ! {
    if run.folded() && form == Form::Text {
        eprintln!(
            concat!(
                "spice commit-msg: auto-folded commit message body lines; ",
                "keep body text wrapped at {} chars"
            ),
            spice::commit_message::WRAP_LIMIT
        );
    }
    deliver_commit_gate(run.outcome(), form)
}

#[cfg(not(target_arch = "wasm32"))]
fn deliver_with_code<B: serde::Serialize>(outcome: &Outcome<B>, form: Form, code: i32) -> ! {
    match render::render(outcome, form) {
        Ok(rendering) => {
            rendering.speak();
            terminate_code(code);
        }
        Err(error) => {
            let fault: Outcome<()> = Outcome::Fault(render_fault(error));
            render::render(&fault, Form::Text)
                .expect("the text rendering of a fault encodes nothing")
                .speak();
            terminate(&fault);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn deliver_config(outcome: &Outcome<config_command::ConfigReport>, form: Form) -> ! {
    if form == Form::Text
        && let Outcome::Reply(reply) = outcome
    {
        match reply.body.text() {
            Ok(document) => {
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(&document);
                let _ = stdout.flush();
                terminate_code(REPLY_EXIT);
            }
            Err(error) => deliver_fault(render_fault(error.to_string()), Form::Text),
        }
    }
    deliver(outcome, form)
}

mod main_tail;

pub use main_tail::terminate;
pub(crate) use main_tail::terminate_code;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use main_tail::{session_environment, working_directory};

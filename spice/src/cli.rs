//! The generated command tree and its closed contract registry.

use std::collections::BTreeSet;

#[cfg(not(target_arch = "wasm32"))]
use std::ffi::{OsStr, OsString};
use std::time::Duration;

use clap::{Arg, ArgAction, ArgMatches, Command};
use wire::{CommandKind, FactKind};

mod mounts;
mod options;
mod syntax;
mod tools;

pub use mounts::{Mount, MountLoadError, MountRegistry, MountRegistryError, MountSelection};

use syntax::register_syntax;
#[cfg(test)]
use syntax::{is_optional, syntax_tokens};
pub use tools::ExternalTool;
/// The one compiled optional judge executable shared by resident and task adjudication.
pub const JUDGE_EXECUTABLE: &str = tools::JUDGE_EXECUTABLE;
use tools::{
    CALLER_COMMAND, DOCTOR_TOOLS, GATE_POLICY_TOOLS, INSTALL_SYMBOLS_TOOLS, NATIVE_CARGO_TOOL,
    NO_TOOLS, OCCUPANT_LAUNCH_TOOLS, PROOF_TOOLS, REFERENCE_TRANSACTION_TOOLS, RTK_QUERY_TOOLS,
    TASK_DONE_TOOLS, TASK_GIT_TOOLS, UP_TOOLS,
};
#[cfg(test)]
use tools::{ToolBounds, ToolCancellation, ToolNetwork, posture_violations};

/// The renderer flag, spelled once so its registration and its reader cannot
/// drift into naming two different arguments that merely agree today.
pub const JSON: &str = "json";
const MOUNT_ARGUMENT: &str = "MOUNT_ARGUMENTS";

/// Principal admitted to a command leaf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Principal {
    Local,
    Principal,
    Lane,
    PrincipalOrLane,
}

impl Principal {
    /// How a refusal names the principal a leaf admits.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Principal => "principal",
            Self::Lane => "lane",
            Self::PrincipalOrLane => "principal or lane",
        }
    }
}

/// Relationship a leaf has with the authoritative journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalAccess {
    None,
    Inspect,
    Read,
    Command,
    Recovery,
    Resident,
}

/// Journal relationship a successful reply must report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyPositionLaw {
    /// The command is daemonless and has no journal position.
    Direct,
    /// The command answers from a read snapshot.
    Snapshot,
    /// The command commits authoritative state before answering.
    Committed,
    /// The command either re-delivers an unchanged snapshot or commits movement.
    SnapshotOrCommitted,
    /// The command either hands off durable work or commits its terminal movement.
    DirectOrCommitted,
}

impl JournalAccess {
    const fn reply_position(self) -> ReplyPositionLaw {
        match self {
            Self::None | Self::Inspect => ReplyPositionLaw::Direct,
            Self::Read => ReplyPositionLaw::Snapshot,
            Self::Command | Self::Recovery | Self::Resident => ReplyPositionLaw::Committed,
        }
    }
}

/// Additional posture or admission rule carried by a leaf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Guard {
    Daemonless,
    Offline,
    Toolful,
    Resident,
    OperatorCapability,
    LaneCapability,
    DigestOnMutation,
    JournalLock,
}

/// Shape returned when a leaf succeeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultClass {
    Reply,
    Plan,
    Stream,
}

/// Public process outcomes shared by every command renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitOutcome {
    Reply,
    Refusal,
    Fault,
}

impl ExitOutcome {
    pub const ALL: &[Self] = &[Self::Reply, Self::Refusal, Self::Fault];

    #[cfg(test)]
    const fn code(self) -> i32 {
        match self {
            Self::Reply => 0,
            Self::Refusal => 2,
            Self::Fault => 1,
        }
    }
}

/// Temporary common bound for every app opening and command answer.
pub const APP_ANSWER_BOUND: Duration = Duration::from_secs(20);

/// Bound for an answer the resident composes from state it already holds.
const HELD_STATE_ANSWER: Duration = APP_ANSWER_BOUND;

/// Bound for an answer that first launches a process or publishes durable
/// objects, or waits for resident quiescence, all of which contend with every
/// other lane on the host.
const CONTENDED_ANSWER: Duration = APP_ANSWER_BOUND;

/// Bound for handing durable task-completion work to its resolution worker.
const TASK_COMPLETION_ANSWER: Duration = APP_ANSWER_BOUND;

/// How long the native client blocks for one command's answer.
///
/// This is the registry. The match is exhaustive, so a command added without a
/// stated bound fails to compile: the bound cannot be inherited or guessed,
/// only stated. A command reaches the contended bound when its answer waits on
/// work the host schedules against every other lane, and an answer that has not
/// arrived by then is reported as an elapsed deadline rather than as a peer
/// that failed.
#[must_use]
pub const fn answer_bound(kind: CommandKind) -> Duration {
    match kind {
        CommandKind::OutboxSubmit | CommandKind::OutboxRead => HELD_STATE_ANSWER,
        CommandKind::TaskDone => TASK_COMPLETION_ANSWER,
        CommandKind::LaneSeat | CommandKind::LaneState | CommandKind::UpChange => CONTENDED_ANSWER,
        CommandKind::BoardRead
        | CommandKind::TaskDetail
        | CommandKind::TaskRelations
        | CommandKind::TaskTimeline
        | CommandKind::BoardGraph
        | CommandKind::BoardHandout
        | CommandKind::DocLedger
        | CommandKind::PreparationDecide
        | CommandKind::OutputAdopt
        | CommandKind::RackRead
        | CommandKind::GlobalSettingsRead
        | CommandKind::GlobalSettingsSet
        | CommandKind::VendorCatalog
        | CommandKind::RoomPost
        | CommandKind::RoomScore
        | CommandKind::RoomConversationText
        | CommandKind::RoomLeave
        | CommandKind::RoomContribute
        | CommandKind::RoomContributionRead
        | CommandKind::TaskAdd
        | CommandKind::TaskOopsAdd
        | CommandKind::TaskSweep
        | CommandKind::TaskEdit
        | CommandKind::TaskDelete
        | CommandKind::TaskClose
        | CommandKind::TaskConclude
        | CommandKind::TaskNote
        | CommandKind::TaskDefer
        | CommandKind::TaskWake
        | CommandKind::TaskFile
        | CommandKind::TaskPlace
        | CommandKind::TaskMedia
        | CommandKind::TaskDepends
        | CommandKind::TaskTarget
        | CommandKind::TaskJoin
        | CommandKind::TaskInvite
        | CommandKind::TaskReview
        | CommandKind::ClaimWin
        | CommandKind::ClaimRefresh
        | CommandKind::ClaimFree
        | CommandKind::ClaimLapse
        | CommandKind::SteerSend
        | CommandKind::SteerInbox
        | CommandKind::SteerLineControl
        | CommandKind::QuoteRead
        | CommandKind::TopicRead
        | CommandKind::TopicEdit
        | CommandKind::SteerResend
        | CommandKind::SteerRecover
        | CommandKind::SteerCancel
        | CommandKind::ConsentRead
        | CommandKind::ConsentPolicy
        | CommandKind::ConsentDecide
        | CommandKind::TrustPend
        | CommandKind::TrustGrant
        | CommandKind::TrustRevoke
        | CommandKind::TrustReport
        | CommandKind::TrustShow
        | CommandKind::PrincipalDirectoryRead
        | CommandKind::PrincipalRead
        | CommandKind::PrincipalAdmit
        | CommandKind::PrincipalEdit
        | CommandKind::PrincipalGrant
        | CommandKind::PrincipalRevoke
        | CommandKind::PrincipalDeauthorize
        | CommandKind::LaneRead
        | CommandKind::LaneJobsRead
        | CommandKind::LaneJobRead
        | CommandKind::LaneJobStop
        | CommandKind::LaneRenewal
        | CommandKind::LaneRetune
        | CommandKind::LaneEffortRetune
        | CommandKind::LaneFastRetune
        | CommandKind::TeamConfigure
        | CommandKind::TeamArrange
        | CommandKind::LaneRecall
        | CommandKind::ProofRecognize => HELD_STATE_ANSWER,
    }
}

/// One generated CLI contract row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contract {
    pub path: &'static [&'static str],
    pub syntax: &'static str,
    pub principal: Principal,
    pub journal: JournalAccess,
    pub reply_position: ReplyPositionLaw,
    pub facts: &'static [FactKind],
    pub guards: &'static [Guard],
    pub tools: &'static [ExternalTool],
    pub result: ResultClass,
    pub outcomes: &'static [ExitOutcome],
    pub battery: &'static str,
}

/// Whether one registered leaf has executable Rust dispatch today.
///
/// A gap names the public task that owns it. `Unaccounted` exists so a newly
/// registered leaf fails the landing check instead of quietly joining the
/// terminal release refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Landing {
    Landed,
    /// A current public task owns this intentionally visible terminal refusal.
    GapOwned(&'static str),
    Unaccounted,
}

mod contracts;
use contracts::*;
mod summaries;

/// The complete registry in the order used by help and Doctor rendering.
///
/// Returning the static rows performs no discovery or inspection. The Doctor
/// surface adds observations later without gaining a second command list.
pub const fn contracts() -> &'static [Contract] {
    CONTRACTS
}

/// Classify executable landing independently from parser registration.
///
/// This second closed table is intentional: adding a command contract does
/// not make its implementation real. The binary terminal branch uses this
/// classification to distinguish tracked work from a dispatch defect.
pub fn landing(path: &[&str]) -> Landing {
    if observation_landed(path) || repository_landed(path) || coordination_landed(path) {
        Landing::Landed
    } else {
        Landing::Unaccounted
    }
}

fn observation_landed(path: &[&str]) -> bool {
    matches!(
        path,
        ["app"]
            | ["demo"]
            | ["watch"]
            | ["retro"]
            | ["session", "briefing"]
            | ["session", "context"]
            | ["session", "sweep"]
            | ["session", "turns"]
            | ["doctor"]
            | ["version"]
            | ["rtk", "gain"]
            | ["gate"]
            | ["gate", "prove"]
            | ["gate", "camp"]
            | ["gate", "latches"]
            | ["gate", "commit-msg"]
            | ["gate", "pre-commit"]
            | ["gate", "selection"]
            | ["gate", "reference-transaction"]
    )
}

fn repository_landed(path: &[&str]) -> bool {
    matches!(
        path,
        ["install"]
            | ["install", "symbols"]
            | ["dev", "cargo"]
            | ["dev", "xtask"]
            | ["dev", "cargo-store"]
            | ["dev", "cargo-footprint"]
            | ["lock"]
            | ["lock", "run"]
            | ["proof"]
            | ["proof", "replay"]
            | ["release", "notes"]
            | ["release", "seal"]
            | ["up"]
            | ["down"]
            | ["config"]
            | ["trust"]
            | ["trust", "show"]
            | ["trust", "pend"]
            | ["trust", "grant"]
            | ["trust", "revoke"]
            | ["backup"]
            | ["restore"]
            | ["journal"]
            | ["journal", "reindex"]
            | ["journal", "verify"]
            | ["thread"]
            | ["restore", "verify"]
            | ["tape", "archive"]
    )
}

fn coordination_landed(path: &[&str]) -> bool {
    matches!(
        path,
        ["settings"]
            | ["settings", "fast"]
            | ["team", "show"]
            | ["team", "drive"]
            | ["team", "drain"]
            | ["team", "fast"]
            | ["team", "name"]
            | ["lane"]
            | ["lane", "list"]
            | ["lane", "add"]
            | ["lane", "drain"]
            | ["lane", "wake"]
            | ["lane", "model"]
            | ["lane", "effort"]
            | ["lane", "fast"]
            | ["lane", "renew"]
            | ["lane", "remove"]
            | ["lane", "import"]
            | ["lane", "purge"]
            | ["recall"]
            | ["steer"]
            | ["steer", "cancel"]
            | ["steer", "resend"]
            | ["steer", "recover"]
            | ["inbox"]
            | ["quote"]
            | ["topic", "show"]
            | ["topic", "preview"]
            | ["topic", "move"]
            | ["topic", "describe"]
            | ["consent"]
            | ["consent", "allow"]
            | ["consent", "deny"]
            | ["consent", "policy"]
            | ["board"]
            | ["board", "graph"]
            | ["board", "handout"]
            | ["room", "post"]
            | ["room", "score"]
            | ["room", "history"]
            | ["room", "leave"]
            | ["room", "contribute"]
            | ["room", "inbox"]
            | ["room", "ack"]
            | ["room", "nak"]
            | ["room", "fin"]
            | ["room", "awareness"]
            | ["principal", "list"]
            | ["principal", "admit"]
            | ["principal", "profile"]
            | ["principal", "role", "grant"]
            | ["principal", "role", "revoke"]
            | ["principal", "deauthorize"]
            | ["task", "authority"]
            | ["task", "decide"]
            | ["task", "adopt"]
            | ["task", "list"]
            | ["task", "status"]
            | ["task", "show"]
            | ["task", "next"]
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
            | ["task", "artifact", "add"]
            | ["task", "artifact", "list"]
            | ["task", "artifact", "show"]
            | ["task", "artifact", "prune"]
            | ["task", "artifact", "remove"]
            | ["doc", "ledger"]
            | ["sweep"]
            | ["taste"]
            | ["taste", "add"]
            | ["taste", "drop"]
    )
}

/// Build the one parser and help tree from the registry [`contracts()`] returns.
pub fn command() -> Command {
    command_from(contracts(), &[])
}

/// The bounded-output policy every read surface follows; see `wire::output`.
const OUTPUT_POLICY: &str = "\
Output: reads show current, relevant state by default. History and detail are
explicit flags, and every read stays bounded: --limit, --max-bytes, and
--max-text raise detail only up to the surface maximum. Withheld rows or text
are always counted, with the exact command that continues after them. Complete
documents and evidence are exported to a named file with their digest, never
streamed. Mutation replies are complete and concise.";

/// Finite watch defaults and explicit follow bounds.
const WATCH_HELP: &str = "\
Bare watch emits one bounded current transcript snapshot and exits. --follow
requires --deadline-seconds SECONDS (positive, at most 300), coalesces filesystem
wakes to at most ten per second, and stops at the deadline, 3000 wakes, or the
aggregate row/byte limit. Every successful result ends with a watch.v1 resume
cursor; --after CURSOR resumes without repeating an admitted record. Source
replacement and history skipped outside the bounded tail are explicit observations.";

/// How `task artifact list` pages its citations.
const ARTIFACT_LIST_HELP: &str = "\
Lists the task's artifacts newest first: 20 by default, at most 500 (--limit),
within 8 KiB of encoded rows by default and 128 KiB at most (--max-bytes). A
partial page states how many it withheld and the exact command that continues
at the first withheld artifact (--after ARTIFACT, inclusive).";

/// How `task artifact show` reads a payload without flooding a terminal.
const ARTIFACT_SHOW_HELP: &str = "\
Shows the artifact's metadata and a bounded window of its payload, after
proving the payload against its retained digest. Text shows its first 4096
bytes; a non-text payload shows only its store path. --offset and --bytes
(1 to 16384) read a window anywhere: text on character boundaries, other
payloads as hex. A window that stops short names the command for the next.
No form writes raw payload bytes to the terminal.

--output PATH writes the exact whole payload to PATH atomically, verifies the
written file against the retained digest, and reports its path, size, and
digest without echoing contents. It is never truncated; '-' is refused.";

/// Selection, claim, and dry-board semantics for the allocator entry point.
const TASK_NEXT_HELP: &str = "\
Returns this lane's current assignment or pending private Design request first.
Otherwise claims the allocator's next eligible task. When no task is eligible,
reports a bounded board summary and leaves state unchanged.";

/// Existing-task capture and new-work adoption are distinct forms.
const TASK_CAPTURE_HELP: &str = "\
Existing task: `spice task capture HANDLE [--steal]` claims the named task,
adopting loose commits or dirty work when present. The task keeps its origin.

New task: omit HANDLE and use `spice task capture --title TITLE --topic TOPIC`
with work to adopt. --title, --pitch, --topic, --origin, --description and
--term are creation-only fields and cannot accompany an existing HANDLE.

An interrupted adoption resumes against its sealed target. Use
`spice task capture --restore` without HANDLE, task fields or --steal to restore it.";

/// Direct and invited participation in one current task phase.
const TASK_JOIN_HELP: &str = "\
HANDLE consumes capacity already opened by the current phase target and never
changes that target. An invitation KEY accepts one live invitation: it joins
the task's phase as an ordinary claim, raising the phase target atomically
when the phase has no spare capacity.

--invite asks one partner lane into a task you hold; omit HANDLE to name your
sole active claim. Without =ADDR the resident recruits only from your own team
and follows allocation there: it binds the best launchable vacant teammate,
trying the next when one cannot bind, and asks exactly one occupied teammate
only when none did; it never reaches another team. =ADDR may name a lane on any
team. A vacant lane joins at once: the result is `joined`, it receives the
claim, and the phase target grows if needed, before its launch. An occupied
lane is `invited`: the result names its steering KEY, cancellation directive
UUID and ten-minute expiry. ACK or `task join KEY` joins; NAK declines; either
answer is relayed to you. Each asking claim may have only one live offer,
addressed or not: another request refuses naming it. `spice steer cancel UUID`
cancels your own offer immediately, permitting a replacement; canceled keys
cannot join. Acceptance, decline, expiry or lost asking authority also permits
a fresh request. Vacant joins fill immediately and permit successive requests.
Expiry follows ten minutes, claim/seat loss, or phase/generation change, not
recipient reseating. `task show HANDLE` projects invited/joined/declined/expired.";

/// Explicit target edits belong to the operator, not the holder.
const TASK_TARGET_HELP: &str = "\
Only the operator may set explicit targets, including future phases or lowering.
Lanes recruit through invitations: any needed growth commits with the partner
claim that fills it. Targets count distinct lanes, not replacement occupants,
and are not capped by the number of currently registered lanes.

If an edit satisfies the current round from its accepted arrivals and no
active claim remains, it closes that round in the same transaction. No new
arrival is needed; ordinary phase and verdict rules still apply.";

/// Initial framing for typed planning work created through `task add`.
const TASK_ADD_HELP: &str = "\
Omit --intent for ordinary work. --intent plan declares a Plan whose default
route is plan,review. --intent design declares a Design with one open design
phase; plan,design and review,design add an ordinary intake phase.

A Design is itself a question: phrase TITLE as the question under Design.
Each --term value states one proposition with an unambiguous positive side;
participants vote on it, and it passes to answer rather than becoming an
implementation completion criterion. Put an open-ended research prompt in the
room with `spice room contribute HANDLE --ask TEXT` instead.";
/// Portable task definitions deliberately exclude repository authority.
const TASK_INTERCHANGE_HELP: &str = "\
Export writes one versioned, bounded document containing recreatable public
task statements and export-local dependency/preparation references. Its receipt
counts private, OOPS, priority, and transitively dependent tasks excluded with
claims, workflow progress, origins, durable identities, taste state, and every
other repository-specific authority.

Import validates the complete document before creating anything, allocates
fresh task identities, remaps local references, and submits each definition
through ordinary task admission. --origin supplies one local causal origin
for every imported task; lanes must name one unless they already hold work.";

/// Usage notes for a leaf whose bounds its syntax line cannot carry.
fn leaf_help(path: &[&str]) -> Option<&'static str> {
    match path {
        ["watch"] => Some(WATCH_HELP),
        ["task", "next"] => Some(TASK_NEXT_HELP),
        ["task", "capture"] => Some(TASK_CAPTURE_HELP),
        ["task", "join"] => Some(TASK_JOIN_HELP),
        ["task", "target"] => Some(TASK_TARGET_HELP),
        ["steer", "cancel"] => Some(
            "Cancel one live steering directive by its UUID. The operator may cancel any directive; a lane may cancel only its own typed task invitation. Cancellation invalidates the invitation key and permits a replacement request immediately.",
        ),
        ["task", "add"] => Some(TASK_ADD_HELP),
        ["task", "export" | "import"] => Some(TASK_INTERCHANGE_HELP),
        ["task", "artifact", "list"] => Some(ARTIFACT_LIST_HELP),
        ["task", "artifact", "show"] => Some(ARTIFACT_SHOW_HELP),
        _ => None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn product_version() -> String {
    crate::version::product().to_string()
}

#[cfg(target_arch = "wasm32")]
fn product_version() -> String {
    dist::current_tracked_version()
        .expect("the tracked product version is validated by the repository gate")
        .product(
            dist::current_counter_sum().expect("the compiled counters are validated"),
            0,
            dist::Build::Dev,
        )
        .to_string()
}

fn command_from(contracts: &[Contract], mounts: &[Mount]) -> Command {
    let mut root = Command::new("spice")
        .about("Repository-local coordination appliance")
        .after_help(OUTPUT_POLICY)
        .version(product_version())
        .subcommand_required(true)
        .arg_required_else_help(true)
        .disable_help_subcommand(true)
        .arg(
            Arg::new(JSON)
                .long(JSON)
                .global(true)
                .action(ArgAction::SetTrue),
        );
    root = options::describe(root, &[])
        .unwrap_or_else(|error| panic!("invalid root option help: {error}"));

    for child in child_names(contracts, mounts, &[]) {
        root = root.subcommand(summarized(contracts, mounts, &[child]));
    }
    root
}

fn branch(contracts: &[Contract], mounts: &[Mount], name: &str, path: &[String]) -> Command {
    let borrowed = path.iter().map(String::as_str).collect::<Vec<_>>();
    let contract = contract_at(contracts, &borrowed);
    let mount = mount_at(mounts, &borrowed);
    let children = child_names(contracts, mounts, &borrowed);
    let mut command = Command::new(name.to_owned()).disable_help_subcommand(true);
    if !children.is_empty() {
        command = command.subcommand_negates_reqs(true);
    }

    if let Some(contract) = contract {
        command = command.override_usage(format!("spice {}", contract.syntax));
        if let Some(help) = leaf_help(&borrowed) {
            command = command.after_help(help);
        }
        command = register_syntax(command, contract).unwrap_or_else(|error| {
            panic!("invalid CLI contract syntax {:?}: {error}", contract.path)
        });
        command = options::describe(command, contract.path)
            .unwrap_or_else(|error| panic!("invalid CLI option help {:?}: {error}", contract.path));
        if !children.is_empty() {
            command = command.subcommand_negates_reqs(true);
        }
    } else if mount.is_some() {
        command = command.disable_help_flag(true).trailing_var_arg(true).arg(
            Arg::new(MOUNT_ARGUMENT)
                .num_args(0..)
                .allow_hyphen_values(true)
                .value_parser(clap::builder::OsStringValueParser::new()),
        );
    } else {
        command = command
            .subcommand_required(true)
            .arg_required_else_help(true);
    }

    for child in children {
        let mut child_path = path.to_vec();
        child_path.push(child);
        command = command.subcommand(summarized(contracts, mounts, &child_path));
    }
    command.subcommand_precedence_over_arg(true)
}

/// One branch under its help row; a registered path without one refuses.
fn summarized(contracts: &[Contract], mounts: &[Mount], path: &[String]) -> Command {
    let registered = contracts.iter().any(|contract| {
        contract.path.len() >= path.len()
            && contract
                .path
                .iter()
                .zip(path)
                .all(|(word, name)| *word == name.as_str())
    });
    let summary = match summaries::summary(path) {
        Some(summary) => summary,
        None if registered => panic!("registered command {path:?} has no help summary"),
        None => summaries::MOUNTED_SUMMARY,
    };
    let name = path.last().expect("a branch path names its command");
    branch(contracts, mounts, name, path).about(summary)
}

fn child_names(contracts: &[Contract], mounts: &[Mount], path: &[&str]) -> BTreeSet<String> {
    contracts
        .iter()
        .filter(|contract| contract.path.starts_with(path) && contract.path.len() > path.len())
        .map(|contract| contract.path[path.len()].to_owned())
        .chain(
            mounts
                .iter()
                .filter(|mount| {
                    mount.path().len() > path.len()
                        && mount
                            .path()
                            .iter()
                            .map(String::as_str)
                            .zip(path.iter().copied())
                            .all(|(left, right)| left == right)
                })
                .map(|mount| mount.path()[path.len()].clone()),
        )
        .collect()
}

fn mount_at<'a>(mounts: &'a [Mount], path: &[&str]) -> Option<&'a Mount> {
    mounts.iter().find(|mount| {
        mount.path().len() == path.len()
            && mount
                .path()
                .iter()
                .map(String::as_str)
                .eq(path.iter().copied())
    })
}

fn contract_at<'a>(contracts: &'a [Contract], path: &[&str]) -> Option<&'a Contract> {
    contracts.iter().find(|contract| contract.path == path)
}

/// Recover one registered contract for an internal typed client preflight.
#[cfg(not(target_arch = "wasm32"))]
pub fn contract(path: &[&str]) -> Option<&'static Contract> {
    contract_at(contracts(), path)
}

/// Recover the contract selected by the generated parser.
pub fn selected_contract(matches: &ArgMatches) -> Option<&'static Contract> {
    let path = selected_path(matches);
    contract_at(contracts(), &path)
}

fn selected_path(matches: &ArgMatches) -> Vec<&str> {
    let mut path = Vec::new();
    let mut cursor = matches;
    while let Some((name, child)) = cursor.subcommand() {
        path.push(name);
        cursor = child;
    }
    path
}

/// Recover the leaf matches selected by the generated parser.
pub fn selected_matches(mut matches: &ArgMatches) -> &ArgMatches {
    while let Some((_, child)) = matches.subcommand() {
        matches = child;
    }
    matches
}

/// Whether the caller asked for the JSON rendering.
///
/// The flag is global, so clap admits it anywhere a renderer flag can be
/// meant and reports it from the root regardless of where it was written. The
/// one place it is not a renderer flag is after a trailing `--`, where the
/// operand is the caller's own command and its flags are that command's, not
/// spice's.
pub fn json_selected(matches: &ArgMatches) -> bool {
    matches.get_flag(JSON)
}

/// Whether a raw native invocation requests JSON before repository command
/// mounts have been loaded and the complete parser can exist.
///
/// This bootstrap read is used only to render a mount-loading failure. The
/// first `--` ends Spice's arguments, exactly as it does for the generated
/// parser, so a child command's own `--json` remains its operand.
#[cfg(not(target_arch = "wasm32"))]
pub fn bootstrap_json_selected(arguments: impl IntoIterator<Item = OsString>) -> bool {
    let requested = format!("--{JSON}");
    for argument in arguments.into_iter().skip(1) {
        if argument == OsStr::new("--") {
            return false;
        }
        if argument == OsStr::new(&requested) {
            return true;
        }
    }
    false
}

/// Whether the raw native invocation selects a top-level configuration owner.
///
/// Config and Doctor own typed configuration refusal and diagnosis, so their
/// bootstrap paths must be recognizable before repository command mounts can
/// be loaded. Only the first non-global argument is considered; a nested or
/// child-command token has no authority to change mount loading.
/// This is only a routing hint; the generated parser remains the syntax authority.
#[cfg(not(target_arch = "wasm32"))]
pub fn bootstrap_configuration_owner_selected(
    arguments: impl IntoIterator<Item = OsString>,
) -> bool {
    bootstrap_leaf_selected(arguments, &["config", "doctor"])
}

/// Whether the raw native invocation selects the top-level Lock command.
#[cfg(not(target_arch = "wasm32"))]
pub fn bootstrap_lock_selected(arguments: impl IntoIterator<Item = OsString>) -> bool {
    bootstrap_leaf_selected(arguments, &["lock"])
}

#[cfg(not(target_arch = "wasm32"))]
fn bootstrap_leaf_selected(arguments: impl IntoIterator<Item = OsString>, leaves: &[&str]) -> bool {
    let json = format!("--{JSON}");
    for argument in arguments.into_iter().skip(1) {
        if argument == OsStr::new("--") {
            return false;
        }
        if argument == OsStr::new(&json) {
            continue;
        }
        return leaves.iter().any(|leaf| argument == OsStr::new(leaf));
    }
    false
}

#[cfg(test)]
mod tests;

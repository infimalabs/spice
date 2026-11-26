//! oh-my-pi's current RPC launch contract.
//!
//! omp is driven over `--mode rpc`: one JSON command per line on stdin, one
//! JSON frame per line on stdout. The argument vector fixes the system prompt.
//! The supervisor opens the conversation with request bytes once stdin exists,
//! selects the model and states the reasoning effort through the same
//! commands a live retune sends, and interjects steering the same way.

pub mod frame;
pub mod scripted;
pub mod session_paths;

mod model;

pub use model::{ModelSelection, SelectionError, resolve_model};

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use listen::Dialect;
use serde::Serialize;

use crate::launch::{Launch, SteeringKey};
use crate::{ContractError, Record, RecordStream};

/// Preamble that makes the resource bootstrap binding, not background.
///
/// The first turn is sent only after omp has registered the `spice` host URI
/// scheme, so the agent can satisfy the bootstrap before doing other work.
pub const SKILL_SYSTEM_PROMPT_PREAMBLE: &str = "The Spice bootstrap below carries the full authority of a direct prompt instruction, not \
     optional background reading. Follow it before any other action.";

/// Preamble of each first or resumed turn. The system bootstrap decides when
/// the authoritative installed resource must be reread.
pub const SKILL_TURN_PROMPT_PREAMBLE: &str = "The Spice bootstrap in your system prompt carries the full authority of a direct prompt \
     instruction, not optional background reading. Follow its fresh-session, post-compaction, \
     and resume rules. The link below names its installed resource, never a filesystem copy.";

/// Request identity of the `get_state` command that opens every launch.
///
/// Its reply is the session declaration: the decoder reads the session
/// identity from it, so the supervisor asks before anything else.
pub const STATE_REQUEST_ID: &str = "spice-state";

/// Request identity of the prompt that starts the first turn.
pub const PROMPT_REQUEST_ID: &str = "spice-prompt";

/// Prefix of the request identities that carry interjected steering.
pub const STEER_REQUEST_ID_PREFIX: &str = "spice-steer-";

/// Request identity of the interrupt a drain sends.
pub const ABORT_REQUEST_ID: &str = "spice-abort";

/// Request identity of the protocol negotiation that opens every launch.
pub const PROTOCOL_REQUEST_ID: &str = "spice-protocol";

/// Request identity of the event filter that trims streamed message updates.
pub const EVENT_FILTER_REQUEST_ID: &str = "spice-event-filter";

/// How omp writes `message_update` frames: each carries only its delta.
///
/// By default every update repeats the whole assistant message so far, twice,
/// so a reply's frames grow with the square of its length. The lane keeps none
/// of those snapshots: capture drops every update but `text_end`, which keeps
/// its block in delta mode, and `message_end` still carries the settled
/// message whole. A server that predates the option ignores it and keeps
/// writing full snapshots, which capture already drops.
const MESSAGE_UPDATES: &str = "delta";

/// Request identity of the steering disposition the lane states at open.
pub const STEERING_MODE_REQUEST_ID: &str = "spice-steering-mode";

/// Request identity of the follow-up disposition the lane states at open.
pub const FOLLOW_UP_MODE_REQUEST_ID: &str = "spice-follow-up-mode";

/// Request identity of the interrupt disposition the lane states at open.
pub const INTERRUPT_MODE_REQUEST_ID: &str = "spice-interrupt-mode";

/// Request identity of the model listing the lane resolves its model against.
pub const MODELS_REQUEST_ID: &str = "spice-models";

/// Request identity of the model selection that follows the listing.
pub const MODEL_REQUEST_ID: &str = "spice-model";

/// Request identity of the reasoning effort stated after the model selection.
pub const EFFORT_REQUEST_ID: &str = "spice-effort";

/// How omp drains the steering queue at a boundary: every message the lane
/// has interjected, in order.
///
/// Steering is how the operator reaches an occupant mid-turn, and two
/// interjections a second apart are one instruction in two lines. Draining
/// one per boundary would deliver the correction without the sentence that
/// qualifies it and spend a turn on each, so the lane takes the whole queue.
const STEERING_MODE: &str = "all";

/// How omp drains messages queued for the next turn: all of them together.
///
/// Spice uses the SDK aside queue for background notices. This setting keeps
/// any ordinary follow-up prompts together when the current work ends.
const FOLLOW_UP_MODE: &str = "all";

/// What omp does with a running tool batch when steering arrives: notice it
/// immediately.
///
/// omp never kills a tool with side effects for a steering message either
/// way; the disposition decides whether it looks. On `wait` it does not, so
/// a batch of pure waits runs to the end before the operator's message is
/// read. On `immediate` a not-yet-started wait is skipped and the message
/// lands at the next boundary, which is the latency the lane's steering
/// contract assumes. On OpenAI, new direction can also reach a response still
/// streaming; background notices never enter that steering queue.
const INTERRUPT_MODE: &str = "immediate";

/// Environment name carrying the session identity the lane pinned at launch.
///
/// omp exports nothing of its own to the tools it runs, so the lane mints the
/// identity before launch and carries it under this name; `spice session`
/// inside the occupant's tools reads it back from there.
pub const SESSION_ENVIRONMENT: &str = wire::SpiceVariable::Session.as_str();

/// The tools a lane's occupant runs with, spelled as omp's `--tools` roster.
///
/// `eval` is OMP-owned because its session-scoped kernels are the reason a
/// task-bound occupant may remain alive between turns. `bash` and `wait` stay
/// off the roster so lane-managed commands run through the lane's
/// [`SHELL_TOOL`] and [`JOB_TOOL`].
///
/// One agent inhabits one worktree, so omp's `task` sub-agent tool stays off.
/// [`config_overlay`] also sets the recursion limit to zero because eval's
/// `agent()` and `workpool()` bridges bypass the top-level roster. omp's `todo`
/// tool stays off for a different reason: the occupant's plan is the task it
/// holds, whose phases and acceptance the board already carries where every
/// other lane and the operator can read them. The rest is omp's own roster for
/// a session with no MCP server, so a tool omp adds later joins only by being
/// named.
pub const TOOLS: &str = "read,write,edit,glob,grep,web_search,eval";

/// [`TOOLS`] plus omp's language-server tool, for a lane whose policy admits
/// it, as the built-in default does.
///
/// The tool starts language servers, which is why it rides on that policy
/// disposition rather than sitting in the roster above. The servers stay
/// inside the occupant's own process tree: [`config_overlay`] declines omp's
/// shared broker, and resident launch composition separately disables the
/// external lspmux route through its child environment. What each server is
/// and how it runs is the worktree's own business, read from the repository's
/// `.omp/lsp.json`; this crate names no server.
pub const LSP_TOOLS: &str = "read,write,edit,glob,grep,web_search,eval,lsp";

/// Request identity of the `set_host_tools` command that registers the
/// lane's shell ahead of the first turn.
pub const TOOLS_REQUEST_ID: &str = "spice-tools";

/// The URI scheme a lane registers with omp so its `read` reaches spice.
pub const URI_SCHEME: &str = "spice";

/// Why the `spice` scheme refuses an operation it does not serve.
pub const URI_SERVED: &str = "spice:// serves reads, task description/terms replacement, and captured authored text through spice://outbox; \
     workflow acts stay spice commands. Read spice:// for the index";

/// Name of the host tool the lane registers in place of omp's `bash`.
///
/// omp refuses a host tool named after any tool it ships, so the lane's shell
/// carries its own name; the model reaches it exactly as it would `bash`.
pub const SHELL_TOOL: &str = "shell";

/// Name of the host tool through which the model follows the commands the
/// lane backgrounded.
pub const JOB_TOOL: &str = "job";

/// Seconds a shell command may run when the agent names no deadline.
pub const SHELL_TIMEOUT_DEFAULT: u64 = 900;

/// Seconds a shell command runs in the foreground before the lane keeps it
/// as a job and answers with what it has said so far: omp's own
/// auto-background threshold, so the model's habit carries over.
pub const SHELL_FOREGROUND: u64 = 60;

/// Tightest deadline the agent may ask for, in seconds.
pub const SHELL_TIMEOUT_MIN: u64 = 1;

/// Longest deadline the agent may ask for, in seconds. A command never runs
/// without one; a long-lived development service is asked for as a service.
pub const SHELL_TIMEOUT_MAX: u64 = 3600;

/// The bounded read one job `history` answer admits through: 20 delivered
/// jobs by default and 100 at most, each command clipped to 200 bytes. The
/// tool names no byte limit, so its default is its maximum: 100 clipped rows
/// fit it, and a page ends early, naming where it continues, only if they
/// would not.
pub const JOB_HISTORY: wire::output::Surface = wire::output::Surface::new(
    wire::output::Limits {
        rows: 20,
        bytes: 32_768,
        text: 200,
    },
    wire::output::Limits {
        rows: 100,
        bytes: 32_768,
        text: 200,
    },
);

/// What the model reads about the lane's shell, in place of omp's `bash`.
const SHELL_DESCRIPTION: &str = "Runs one shell command in the worktree and returns its \
output. The lane runs the command in its own environment and ends it at the deadline, 15 \
minutes unless you name one and never more than 60. Chain order-dependent commands with \
`&&`. The \
native wrapper bounds inline output and supplies a readable full-log path when needed; \
do not append `tail` just to limit output. A command still running after 60 seconds, or \
when a message arrives for you, is kept as a \
job: you get its output so far at once, it keeps running, and its result is delivered \
automatically when it ends. A long-lived development service (a dev server, a watcher) \
is the one thing that runs without a deadline: set `service` and it is kept as a job at \
once; wait for its readiness with `wait` and an `until` pattern, inspect it with `logs`, \
and end it with `stop` once you no longer need it. Set `until` to a regular expression to \
watch a command or service instead of waiting on it: it is kept as a job at once, and the \
first line of its output that matches reaches you as one background notice while it keeps \
running. Holding a claim, you may end your turn while a deadline-bounded job runs: the lane \
wakes you, still holding the claim, when it ends or its `until` line appears. A service never \
keeps the lane idle, and its match never wakes it.";

/// What the model reads about the command parameter.
const SHELL_COMMAND_DESCRIPTION: &str = "command to execute";

/// What the model reads about the service parameter.
const SHELL_SERVICE_DESCRIPTION: &str = "true runs a long-lived development service with no \
deadline, kept as a job at once while its task claim remains active or until you stop it; \
takes no timeout";

/// What the model reads about the shell's until parameter.
const SHELL_UNTIL_DESCRIPTION: &str = "a regular expression, as job wait takes it: keeps the \
command as a job at once and delivers the first line of its output that matches as one \
background notice naming the job; the job keeps running and its ending is delivered on its \
own; a service's match reaches you only while you are here; a pattern that does not compile \
is refused before launch";

/// What the model reads about the lane's job tool.
const JOB_DESCRIPTION: &str = "Follows the commands the shell kept as jobs. A job's result \
is delivered automatically when it ends, so you never need to poll: use `list` to see every \
job still running or ended but not yet delivered to you, `history` to page through jobs \
already delivered, newest first, `logs` to read what one has said so far, and `stop` to end \
one. Use `wait` only when blocked: it passively follows a finite job until it ends, the \
timeout elapses, or a message arrives. Give `wait` an `until` pattern to block instead until \
a line of the job's output matches, which works on a service too: it answers with that line \
and leaves the job running. A running service never ends on its own, so `wait` without \
`until` returns at once and leaves it running. A shell `until` pattern delivers its first \
matching line the same way without blocking. Holding a claim, you may end your turn instead \
of waiting while a deadline-bounded job runs: the lane wakes you, still holding the claim, \
when it ends or its `until` line appears. A service never keeps the lane idle.";

/// What the model reads about the job operation parameter.
const JOB_OP_DESCRIPTION: &str = "what to do: list, history, logs, wait, or stop";

/// What the model reads about the job identity parameter.
const JOB_ID_DESCRIPTION: &str = "the job, as the shell named it; required for logs, wait, \
and stop; with history, where a withheld page continues, as its continuation names it";

/// What the model reads about the wait pattern parameter.
const JOB_UNTIL_DESCRIPTION: &str = "with wait only: a regular expression; wait answers as soon \
as a line of the job's output, written before or during the wait, matches it, naming that line, \
and the job keeps running; a pattern that does not compile is refused";

/// Width of the fixed title slot that opens every omp session file.
const TITLE_SLOT_BYTES: usize = 256;

/// Session header version omp writes today.
const SESSION_VERSION: u8 = 3;

/// Build the exact current omp invocation without performing I/O.
///
/// # Errors
///
/// Returns a path-specific refusal for unsupported cross-vendor fields or a
/// fast-mode disposition left implicit, or the common argument-vector refusal
/// when the binary is empty.
pub fn record(binary: impl Into<OsString>, launch: &Launch) -> Result<Record, ContractError> {
    render(binary.into(), launch, &launch.model, None)
}

/// Build the scripted occupant's invocation: the same omp contract running
/// the scripted model, with the provider extension the lane wrote beside the
/// seat's steering extension.
///
/// # Errors
///
/// Returns every refusal of [`record`].
pub fn scripted_record(
    binary: impl Into<OsString>,
    launch: &Launch,
) -> Result<Record, ContractError> {
    let extension = launch
        .steering_extension
        .as_ref()
        .ok_or(ContractError::Missing {
            path: "vendor.omp.steering_extension",
        })?
        .with_file_name(scripted::EXTENSION_FILE);
    render(binary.into(), launch, scripted::MODEL, Some(&extension))
}

fn render(
    binary: OsString,
    launch: &Launch,
    model: &str,
    scripted_extension: Option<&Path>,
) -> Result<Record, ContractError> {
    let fast_mode = launch.fast_mode.ok_or(ContractError::Missing {
        path: "vendor.omp.fast_mode",
    })?;
    let lsp = launch.lsp.ok_or(ContractError::Missing {
        path: "vendor.omp.lsp",
    })?;
    let config_overlay = launch
        .config_overlay
        .as_ref()
        .ok_or(ContractError::Missing {
            path: "vendor.omp.config_overlay",
        })?;
    let steering_extension = launch
        .steering_extension
        .as_ref()
        .ok_or(ContractError::Missing {
            path: "vendor.omp.steering_extension",
        })?;
    // Every argument below is one omp offers no reliable acknowledged RPC
    // command for; whatever has one (the queue dispositions, model, effort,
    // fast mode for the selected model, host tools, and `spice` scheme) rides
    // the opening handshake instead.
    let mut arguments = vec![
        // Bootstrap: the command stream itself. RPC mode already disables
        // titles: generating one would spend a model call on a session no
        // operator scrolls.
        OsString::from("--mode"),
        OsString::from("rpc"),
        // The overlay carries settings omp only reads from configuration:
        // `set_auto_compaction` toggles compaction but cannot state its
        // threshold, and no command states the LSP, eval-tool, recursion, or
        // todo-reminder settings. Its contents and their reasons are
        // [`config_overlay`]'s.
        OsString::from("--config"),
        config_overlay.clone().into_os_string(),
        // Extensions load only at startup. omp wraps every steered message in
        // its own priority notice; the lane's extension hands the provider the
        // message as the lane wrote it.
        OsString::from("--extension"),
        steering_extension.clone().into_os_string(),
    ];
    if let Some(extension) = scripted_extension {
        // The scripted provider answers every model request from the cue plan.
        arguments.push(OsString::from("--extension"));
        arguments.push(extension.as_os_str().to_owned());
    }
    arguments.extend([
        // No command states the approval mode. The lane is the approval
        // authority; a prompt no operator can see would otherwise hold the
        // turn forever.
        OsString::from("--approval-mode"),
        OsString::from("yolo"),
        // The model, effort, and fast mode are no arguments: the driver states
        // them over the command stream once omp lists what it serves, and the
        // resident restates fast mode after every live model change.
        // `set_fast_mode` reaches only the current model's tier family, so no
        // launch tier is stated for the rest: omp's own settings or a resumed
        // session's record supply them.
        // `set_host_tools` adds the lane's host tools but cannot narrow omp's
        // built-in roster, so the roster stays a launch argument.
        OsString::from("--tools"),
        OsString::from(if lsp { LSP_TOOLS } else { TOOLS }),
        // No command states the system prompt.
        OsString::from("--append-system-prompt"),
        OsString::from(system_prompt(launch)),
    ]);
    // The session is the one omp opens at startup: `switch_session` would
    // first open a fresh session and then replace it by file path, running
    // the session lifecycle twice.
    if let Some(session) = launch.resume.session_id() {
        arguments.push(OsString::from("--resume"));
        arguments.push(OsString::from(session));
    }
    let environment = launch
        .resume
        .session_id()
        .map(|session| (OsString::from(SESSION_ENVIRONMENT), OsString::from(session)));
    Ok(Record::new(
        binary,
        arguments,
        Dialect::Omp,
        RecordStream::Stdout,
        launch.resume.clone(),
    )?
    .with_opening(opening_request())
    .with_model_selection(ModelSelection::new(model, launch.effort, fast_mode))
    .with_first_prompt(turn_prompt(launch))
    .with_environment(environment))
}

/// The configuration overlay a lane writes before launch, stating the
/// compaction threshold, language-server disposition, todo reminder
/// disposition, and commit route this seat runs.
///
/// omp reads `compaction.thresholdTokens` ahead of every percentage or
/// reserve it would otherwise derive, clamped beneath the model's window.
/// `compaction.methodOrder` is deliberately left to omp: its default tries
/// provider-native compaction first, which a Claude seat supports, and falls
/// back through omp's local methods when that pass fails. Stating an order
/// would pin a chain omp owns and revises.
///
/// The `lsp` block is stated either way rather than left to omp's defaults.
/// A declined lane turns the integration off outright, because omp otherwise
/// starts a language server behind the lane's back on the first write to a
/// file some server claims, whether or not the tool is in the roster. An
/// admitted lane declines omp's shared broker. The resident must also disable
/// external lspmux routing in the child environment; that route is independent
/// of this overlay. Which servers run stays the worktree's own business.
///
/// Todo reminders are off because this seat carries no todo tool. omp's stop
/// reminder is the one todo path that never asks whether the tool is in the
/// roster: it counts the items a resumed session still holds, tells the
/// assistant to finish them or mark them complete, and wakes the turn to do
/// it. The occupant's plan is the task it holds, and only the allocator
/// closes that.
///
/// The task recursion limit is zero because omp's eval `agent()` and
/// `workpool()` bridges invoke the subagent machinery without going through the
/// omitted top-level `task` tool. The same depth gate removes their prompt
/// guidance and refuses direct calls. Eval-defined tools are off because only
/// those subagents consume them. Ordinary `tool.<name>` calls remain on the
/// registered tool boundary.
///
/// omp's selective commit is off: its eval `commit` namespace comes with a
/// system-prompt instruction to commit through it instead of `git commit`.
/// A lane commits with CLI git under its claim, after `spice gate prove`,
/// which is the route the repository's hooks and the allocator follow.
///
/// OpenAI live steering is explicitly disabled so queued steering lands at
/// normal command boundaries.
///
/// Nothing in a seat publishes off the machine, whatever the global omp
/// config says. `collab.autoStart` stays off, so no session is replicated
/// through the collab relay. `images.urls.enabled` is false, so outgoing
/// images never reach the publication chain, whose default ends at the public
/// litterbox host. `spotlight.experiment.enabled` is false, so tool output is
/// not sent to Stencil for shaping. `dev.autoqa` is false, so the model is not
/// told to file tool-issue reports, which `omp grievances push` uploads.
/// The seat's stdin carries only Spice-rendered text, so omp's `/share` and
/// `/collab` are unreachable. [`ShellCall::parse`] refuses their command-line
/// publication routes before the registered host shell launches anything, and
/// the overlay's bash rules deny the same routes if omp's built-in bash becomes
/// reachable. The overlay denies are absolute under yolo approval and project
/// config cannot loosen them. These are settings and command gates, not
/// containment: a process eval spawns, an omp binary named by path, or a
/// hand-written network call is outside them.
#[must_use]
pub fn config_overlay(auto_compact_threshold_tokens: u64, lsp: bool) -> String {
    let mut overlay = format!("compaction:\n  thresholdTokens: {auto_compact_threshold_tokens}\n");
    overlay.push_str(if lsp {
        "lsp:\n  enabled: true\n  shared: false\n"
    } else {
        "lsp:\n  enabled: false\n"
    });
    overlay.push_str(
        "eval:\n  tools:\n    enabled: false\ncommit:\n  enabled: false\ntask:\n  maxRecursionDepth: 0\ntodo:\n  reminders: false\nproviders:\n  openaiLiveSteering: false\n",
    );
    overlay.push_str(
        "collab:\n  autoStart: \"off\"\nimages:\n  urls:\n    enabled: false\nspotlight:\n  experiment:\n    enabled: false\ndev:\n  autoqa: false\nbash:\n  patterns:\n",
    );
    for command in PUBLISHING_COMMANDS {
        overlay.push_str("    - match: \"");
        overlay.push_str(command);
        overlay.push_str("*\"\n      approval: deny\n");
    }
    overlay
}

/// The command-line routes by which omp, or the `gh` that omp's gist share
/// store drives, uploads a session, screen, recording, skill, or tool report.
/// Each is denied with every argument that follows it.
const PUBLISHING_COMMANDS: [&str; 7] = [
    "omp share",
    "omp clip",
    "omp stream",
    "omp skill publish",
    "omp skills publish",
    "omp grievances push",
    "gh gist create",
];

/// The extension a lane writes beside its overlay and loads with
/// `--extension`, so the provider reads each steered message as the lane
/// wrote it.
///
/// omp renders every steered user message inside a generated notice calling
/// it a priority interjection that supersedes prior instructions. Spice's
/// steering already names its own authority and key, so the notice only
/// restates it more loudly. The extension's `context` hook clears the
/// steering flag on the copies omp converts for the provider; omp has no
/// setting for this. Delivery is untouched: new direction still uses the
/// steering queue, and the stored transcript keeps the flag, the text, and any
/// images.
pub const STEERING_EXTENSION: &str = ::std::include_str!("omp/steering.mjs");

/// The extension the resident writes into its runtime directory and loads
/// with `--extension` on every quota sample, so OpenAI Codex reports carry
/// the provider's credit balance as omp's canonical `balance` limit.
///
/// omp's built-in Codex usage provider leaves that balance in its raw report.
/// The extension delegates to it once per fetch, appends the balance only
/// when the report lacks one, and stamps every report it returns with
/// [`CREDIT_BALANCE_STAMP`], because omp silently serves the built-in provider
/// when it cannot load a named extension. Its cache version is offset from the
/// delegated provider's version, keeping built-in and stamped reports disjoint
/// while inheriting upstream cache retirements.
pub const CREDIT_BALANCE_EXTENSION: &str = ::std::include_str!("omp/openai-credit-balance.mjs");

/// The `metadata.spiceExtension` value every report the credit-balance
/// extension returns carries.
pub const CREDIT_BALANCE_STAMP: &str = "openai-credit-balance";

/// The session file a lane writes before launch so the identity is known.
///
/// omp names a session only once it starts, so a fresh lane writes the file
/// omp would have written, where omp keeps this working directory's sessions,
/// and resumes it by identity: an empty fixed-width title slot, then the
/// version-3 header. `home`, `temp`, and `cwd` are the canonical directories
/// omp encodes the working directory against; `at` is the ISO-8601 instant,
/// to the millisecond, that names the file and stamps the header.
#[must_use]
pub fn session_file(
    sessions: &Path,
    home: &Path,
    temp: &Path,
    cwd: &Path,
    identity: &str,
    at: &str,
) -> (PathBuf, String) {
    let path = sessions
        .join(session_paths::session_directory(
            Some(home),
            Some(temp),
            cwd,
        ))
        .join(format!("{}_{identity}.jsonl", at.replace([':', '.'], "-")));
    let header = SessionHeader {
        kind: "session",
        version: SESSION_VERSION,
        id: identity,
        timestamp: at,
        cwd: &cwd.to_string_lossy(),
    };
    (path, format!("{}{}\n", title_slot(at), json(&header)))
}

/// An empty title padded to exactly [`TITLE_SLOT_BYTES`] with its newline.
fn title_slot(at: &str) -> String {
    let mut slot = TitleSlot {
        kind: "title",
        v: 1,
        title: "",
        updated_at: at,
        pad: String::new(),
    };
    slot.pad = " ".repeat(TITLE_SLOT_BYTES.saturating_sub(json(&slot).len() + 1));
    format!("{}\n", json(&slot))
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("a session record is representable as JSON")
}

/// omp rewrites this line in place, so it is written key-for-key as omp does.
#[derive(Serialize)]
struct TitleSlot<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    v: u8,
    title: &'a str,
    #[serde(rename = "updatedAt")]
    updated_at: &'a str,
    pad: String,
}

#[derive(Serialize)]
struct SessionHeader<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    version: u8,
    id: &'a str,
    timestamp: &'a str,
    cwd: &'a str,
}

/// The appended system prompt: the compact resource bootstrap, then this
/// worktree's steering anchor naming the interjection delivery path.
#[must_use]
pub fn system_prompt(launch: &Launch) -> String {
    anchored(
        SKILL_SYSTEM_PROMPT_PREAMBLE,
        &launch.bootstrap,
        launch
            .steering_key
            .as_ref()
            .map(SteeringKey::interjection_line),
    )
}

/// The turn prompt: the neutral start prompt behind the same anchor.
#[must_use]
pub fn turn_prompt(launch: &Launch) -> String {
    anchored(
        SKILL_TURN_PROMPT_PREAMBLE,
        &launch.prompt,
        launch
            .steering_key
            .as_ref()
            .map(SteeringKey::interjection_line),
    )
}

/// One preamble, its body, and the steering anchor when the launch has one.
fn anchored(preamble: &str, body: &str, anchor: Option<String>) -> String {
    let mut prompt = format!("{preamble}\n\n{body}");
    if let Some(anchor) = anchor {
        prompt.push_str("\n\n");
        prompt.push_str(&anchor);
    }
    prompt
}

/// The request lines that open a launch: the wire, the event filter, the queue
/// dispositions, the model listing, the lane's shell and `spice` scheme, then
/// a state query. The model selection follows once omp answers the listing,
/// through [`ModelSelection::request`]; the first turn follows separately,
/// through [`first_prompt_request`], once omp has answered both that selection
/// and the scheme registration.
///
/// omp writes its `ready` frame before reading stdin, so every request may be
/// queued before the child has answered anything. The version is negotiated
/// first because omp switches its frame encoder on the reply, and everything
/// after it — a `get_state` answer over a megabyte included — is then framed
/// the way the lane's capture reads. The event filter follows: it admits every
/// event and trims each `message_update` to its delta. The three queue dispositions
/// follow: omp leaves them to whatever the machine's own settings say, so a
/// lane that depends on how steering drains states them rather than
/// inheriting them. The model listing is next: omp's `set_model` takes only an
/// exact provider and identity, which the lane's spelling may leave implicit.
/// The lane's shell is registered next so the first turn's schema carries it; the
/// `spice` scheme registration follows, and its answer decides how the first
/// turn names reads; the state reply declares the session.
///
/// A refused command answers `success: false`, which the lane reads as a
/// vendor failure and reaps; a refused negotiation therefore ends the launch
/// instead of leaving the occupant on a wire nothing can decode.
#[must_use]
pub fn opening_request() -> Vec<u8> {
    let mut bytes = request_line(&Negotiation {
        id: PROTOCOL_REQUEST_ID,
        kind: "negotiate_protocol",
        protocol_version: listen::OMP_PROTOCOL_VERSION,
    });
    bytes.extend(request_line(&EventFilter {
        id: EVENT_FILTER_REQUEST_ID,
        kind: "set_event_filter",
        events: None,
        message_updates: MESSAGE_UPDATES,
    }));
    for (id, kind, mode) in [
        (STEERING_MODE_REQUEST_ID, "set_steering_mode", STEERING_MODE),
        (
            FOLLOW_UP_MODE_REQUEST_ID,
            "set_follow_up_mode",
            FOLLOW_UP_MODE,
        ),
        (
            INTERRUPT_MODE_REQUEST_ID,
            "set_interrupt_mode",
            INTERRUPT_MODE,
        ),
    ] {
        bytes.extend(request_line(&ModeRequest { id, kind, mode }));
    }
    bytes.extend(request_line(&serde_json::json!({
        "id": MODELS_REQUEST_ID,
        "type": "get_available_models",
    })));
    bytes.extend(host_tools_request());
    bytes.extend(host_uri_schemes_request());
    bytes.extend(request_line(&Request {
        id: STATE_REQUEST_ID.to_owned(),
        kind: "get_state",
        message: None,
        streaming_behavior: None,
    }));
    bytes
}

/// The prompt that starts the first turn, naming the skill the system prompt
/// already carries so the agent re-grounds in it.
///
/// A resumed session can still own the turn it was processing when its prior
/// RPC process ended. Follow-up admission starts immediately when idle and
/// otherwise queues this one prompt behind that restored turn, rather than
/// rejecting it as a second owner.
#[must_use]
pub fn first_prompt_request(message: &str) -> Vec<u8> {
    request_line(&Request {
        id: PROMPT_REQUEST_ID.to_owned(),
        kind: "prompt",
        message: Some(message.to_owned()),
        streaming_behavior: Some("followUp"),
    })
}

/// One interjected steering message, numbered by its place in the launch.
///
/// While omp is streaming, the message queues through its steering queue and
/// is delivered after the current tool call; while it is idle, the same
/// request starts a new turn. Either way the agent sees it as a user message.
#[must_use]
pub fn steer_request(sequence: u64, message: &str) -> Vec<u8> {
    request_line(&Request {
        id: format!("{STEER_REQUEST_ID_PREFIX}{sequence}"),
        kind: "prompt",
        message: Some(message.to_owned()),
        streaming_behavior: Some("steer"),
    })
}
/// Change the live session's reasoning level.
///
/// The caller owns request correlation and must not project the change until
/// omp answers this exact `id` with a successful `set_thinking_level` response.
#[must_use]
pub fn set_thinking_level_request(id: &str, level: &str) -> Vec<u8> {
    request_line(&serde_json::json!({
        "id": id,
        "type": "set_thinking_level",
        "level": level,
    }))
}

/// Change the live session's exact provider/model selection.
///
/// OMP's live command does not accept its CLI's fuzzy bare-model selector, so
/// callers must supply both parts rather than infer a provider.
#[must_use]
pub fn set_model_request(id: &str, provider: &str, model: &str) -> Vec<u8> {
    request_line(&serde_json::json!({
        "id": id,
        "type": "set_model",
        "provider": provider,
        "modelId": model,
    }))
}

/// Turn the live session's fast mode (its priority service tier) on or off.
///
/// The caller owns request correlation and must not project the change until
/// omp answers this exact `id` with a successful `set_fast_mode` response; omp
/// refuses to enable it when the current model offers no priority tier.
#[must_use]
pub fn set_fast_mode_request(id: &str, enabled: bool) -> Vec<u8> {
    request_line(&serde_json::json!({
        "id": id,
        "type": "set_fast_mode",
        "enabled": enabled,
    }))
}

/// Whether current OMP explicitly says the selected model has no fast-mode
/// tier. OMP 18.13.3 exposes no structured refusal code, so this exact
/// current-generation sentence is the fail-closed protocol boundary: every
/// other unsuccessful response is an opening failure.
#[must_use]
pub fn fast_mode_unavailable(error: &str) -> bool {
    error.trim() == "Fast mode is unavailable for the current model."
}

/// Whether a provider failure refused the priority service tier fast mode
/// asks for. omp recovers on its own only from Anthropic's refusal of its
/// `speed` field; a model or gateway that refuses `service_tier: priority`
/// fails the turn instead, and every later request the same way.
#[must_use]
pub fn refuses_fast_mode(failure: &str) -> bool {
    let words: String = failure
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect();
    words.contains("servicetier") && words.contains("priority")
}

/// Replace or cancel a background notice through the extension's SDK bridge.
/// RPC has no aside command; the extension coalesces by purpose and calls
/// `sendUserMessage(..., { deliverAs: "aside" })` at the next step boundary.
#[must_use]
pub fn aside_request(sequence: u64, purpose: &str, message: &str) -> Vec<u8> {
    #[derive(Serialize)]
    struct Notice<'a> {
        purpose: &'a str,
        message: &'a str,
    }
    let notice = serde_json::to_string(&Notice { purpose, message })
        .expect("a background notice serializes");
    request_line(&Request {
        id: format!("spice-aside-{sequence}"),
        kind: "prompt",
        message: Some(format!("/spice-aside {notice}")),
        streaming_behavior: None,
    })
}

/// The interrupt a drain sends before any signal.
///
/// omp stops the turn where it stands and records the assistant message as
/// aborted, so the session and the tape both say the turn was interrupted
/// instead of ending in a signal nothing recorded.
#[must_use]
pub fn abort_request() -> Vec<u8> {
    request_line(&Request {
        id: ABORT_REQUEST_ID.to_owned(),
        kind: "abort",
        message: None,
        streaming_behavior: None,
    })
}

/// The decline of a dialog nobody at this lane can compose an answer to.
///
/// omp holds a turn on a select, input, or editor request until it is
/// answered. Declining is what an operator's escape would say: nothing was
/// chosen or typed, so the agent carries on instead of waiting for an
/// operator who is not there.
#[must_use]
pub fn dialog_declined(id: &str) -> Vec<u8> {
    request_line(&DialogResponse {
        kind: "extension_ui_response",
        id,
        cancelled: Some(true),
        confirmed: None,
    })
}

/// The answer to a confirm dialog, once consent has ruled on it.
///
/// A confirm holds the turn exactly as any other dialog does, but it asks a
/// yes-or-no question, so a consent grant answers yes and a denial answers no.
#[must_use]
pub fn dialog_confirmed(id: &str, confirmed: bool) -> Vec<u8> {
    request_line(&DialogResponse {
        kind: "extension_ui_response",
        id,
        cancelled: None,
        confirmed: Some(confirmed),
    })
}

fn append_request_line(line: &mut Vec<u8>, request: &impl Serialize) {
    serde_json::to_writer(&mut *line, request).expect("an RPC request is representable as JSON");
    line.push(b'\n');
}

fn request_line(request: &impl Serialize) -> Vec<u8> {
    let mut line = Vec::new();
    append_request_line(&mut line, request);
    line
}

/// omp reads these key-for-key, so the types declare their own order.
#[derive(Serialize)]
struct DialogResponse<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cancelled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmed: Option<bool>,
}

#[derive(Serialize)]
struct Request {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(rename = "streamingBehavior", skip_serializing_if = "Option::is_none")]
    streaming_behavior: Option<&'static str>,
}

#[derive(Serialize)]
struct Negotiation {
    id: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "protocolVersion")]
    protocol_version: u8,
}

/// `events: null` admits every event; a list would drop any it leaves out.
#[derive(Serialize)]
struct EventFilter {
    id: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    events: Option<()>,
    #[serde(rename = "messageUpdates")]
    message_updates: &'static str,
}

#[derive(Serialize)]
struct ModeRequest {
    id: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    mode: &'static str,
}

/// The `set_host_tools` command that registers the lane's shell.
///
/// The shell is essential, so it sits in the model's top-level schema rather
/// than behind discovery, and its parameters are the two omp's own `bash`
/// makes load-bearing: the command and its deadline. The job tool follows
/// what the shell kept running.
#[must_use]
pub fn host_tools_request() -> Vec<u8> {
    let deadline = serde_json::json!({
        "type": "integer",
        "minimum": SHELL_TIMEOUT_MIN,
        "maximum": SHELL_TIMEOUT_MAX,
        "description": format!(
            "timeout in seconds; omitted means {SHELL_TIMEOUT_DEFAULT}; longer values are \
             clamped to {SHELL_TIMEOUT_MAX}; 0 is refused, since only a service runs \
             without a deadline"
        ),
    });
    let wait = serde_json::json!({
        "type": "integer",
        "minimum": SHELL_TIMEOUT_MIN,
        "maximum": SHELL_TIMEOUT_MAX,
        "description": format!(
            "seconds to wait; omitted means {SHELL_FOREGROUND}, and \
             values are clamped to {SHELL_TIMEOUT_MIN}-{SHELL_TIMEOUT_MAX}"
        ),
    });
    let rows = JOB_HISTORY.default_limits().rows;
    let most = JOB_HISTORY.maximum().rows;
    let limit = serde_json::json!({
        "type": "integer",
        "minimum": 1,
        "maximum": most,
        "description": format!(
            "delivered jobs history shows; omitted means {rows}, and more than {most} is \
             refused"
        ),
    });
    request_line(&serde_json::json!({
        "id": TOOLS_REQUEST_ID,
        "type": "set_host_tools",
        "tools": [{
            "name": SHELL_TOOL,
            "label": "Shell",
            "description": SHELL_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": SHELL_COMMAND_DESCRIPTION},
                    "timeout": deadline,
                    "service": {"type": "boolean", "description": SHELL_SERVICE_DESCRIPTION},
                    "until": {"type": "string", "description": SHELL_UNTIL_DESCRIPTION},
                },
                "required": ["command"],
                "additionalProperties": false,
            },
            "loadMode": "essential",
        }, {
            "name": JOB_TOOL,
            "label": "Job",
            "description": JOB_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["list", "history", "logs", "wait", "stop"],
                        "description": JOB_OP_DESCRIPTION,
                    },
                    "id": {"type": "string", "description": JOB_ID_DESCRIPTION},
                    "timeout": wait,
                    "until": {"type": "string", "description": JOB_UNTIL_DESCRIPTION},
                    "limit": limit,
                },
                "required": ["op"],
                "additionalProperties": false,
            },
            "loadMode": "essential",
        }],
    }))
}

/// Register the `spice` scheme for native reads, digest-guarded task text
/// replacement, and captured outbox prose. An omp without host-owned schemes
/// refuses it under [`listen::OMP_URI_SCHEMES_REQUEST_ID`] and runs on without it.
fn host_uri_schemes_request() -> Vec<u8> {
    request_line(&serde_json::json!({
        "id": listen::OMP_URI_SCHEMES_REQUEST_ID,
        "type": "set_host_uri_schemes",
        "schemes": [{
            "scheme": URI_SCHEME,
            "description": "Spice installed skill, board, task, artifact and quote reads; digest-guarded task description/terms writes; raw authored text writes to spice://outbox with durable partial outcomes and exact-body deduplication per occupant; spice:// is the index",
            "writable": true,
        }],
    }))
}

/// What the model asks of one job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobOp {
    /// The jobs still owed to the lane: running, or ended and not yet heard.
    List,
    /// Delivered jobs, newest first, within the admitted budget.
    History {
        budget: wire::output::Budget,
    },
    Logs,
    Wait,
    Stop,
}

/// The pattern a job wait or a shell watcher looks for in a job's output,
/// compiled when the call is read so a pattern that does not compile is
/// refused before any wait or launch.
#[derive(Clone, Debug)]
pub struct OutputPattern(regex::Regex);

impl OutputPattern {
    /// Compile the pattern the model wrote.
    ///
    /// # Errors
    ///
    /// Returns the refusal text when the pattern does not compile.
    pub fn new(pattern: &str) -> Result<Self, String> {
        regex::Regex::new(pattern)
            .map(Self)
            .map_err(|error| format!("until is not a valid regular expression: {error}"))
    }

    /// Whether `line` contains a match.
    #[must_use]
    pub fn is_match(&self, line: &str) -> bool {
        self.0.is_match(line)
    }

    /// The pattern as the model wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq for OutputPattern {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for OutputPattern {}

/// One request on the lane's jobs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobCall {
    pub op: JobOp,
    /// The job named: absent for a listing, and for history the job a
    /// withheld page continues at.
    pub id: Option<String>,
    /// How long a wait may last before it answers with the job still running.
    pub timeout: Duration,
    /// The output line a wait looks for instead of the job's ending.
    pub until: Option<OutputPattern>,
}

impl JobCall {
    /// Read a call from the arguments a `host_tool_call` frame carried.
    ///
    /// # Errors
    ///
    /// Returns the refusal text when the operation is unknown, the job it
    /// needs is unnamed, a history limit is zero or above its maximum, or a
    /// wait pattern is given to another operation or does not compile.
    pub fn parse(arguments: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        let op = match arguments.get("op").and_then(serde_json::Value::as_str) {
            Some("list") => JobOp::List,
            Some("history") => JobOp::History {
                budget: history_budget(
                    arguments
                        .get("limit")
                        .and_then(serde_json::Value::as_u64)
                        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX)),
                )?,
            },
            Some("logs") => JobOp::Logs,
            Some("wait") => JobOp::Wait,
            Some("stop") => JobOp::Stop,
            Some(other) => return Err(format!("job has no op {other}")),
            None => return Err("job needs an op".to_owned()),
        };
        let id = arguments
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if matches!(op, JobOp::Logs | JobOp::Wait | JobOp::Stop) && id.is_none() {
            return Err(format!("job {} needs an id", op.name()));
        }
        let until = match arguments.get("until") {
            None => None,
            Some(_) if op != JobOp::Wait => {
                return Err(format!("job {} takes no until; only wait does", op.name()));
            }
            Some(until) => Some(OutputPattern::new(
                until
                    .as_str()
                    .ok_or_else(|| "job until must be a string".to_owned())?,
            )?),
        };
        let timeout = arguments
            .get("timeout")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(SHELL_FOREGROUND)
            .clamp(SHELL_TIMEOUT_MIN, SHELL_TIMEOUT_MAX);
        Ok(Self {
            op,
            id,
            timeout: Duration::from_secs(timeout),
            until,
        })
    }
}

/// The budget of one history page. A zero-row page, which the shared contract
/// admits as a count, is refused here: its continuation would never advance.
fn history_budget(rows: Option<usize>) -> Result<wire::output::Budget, String> {
    if rows == Some(0) {
        return Err("job history limit must be at least 1".to_owned());
    }
    JOB_HISTORY
        .budget(wire::output::Request {
            rows,
            ..wire::output::Request::default()
        })
        .map_err(|error| match error {
            wire::output::LimitError::Exceeded {
                requested, maximum, ..
            } => format!("job history limit {requested} exceeds the maximum {maximum}"),
            zero @ wire::output::LimitError::Zero { .. } => format!("job history {zero}"),
        })
}

impl JobOp {
    /// The operation as the model spells it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::History { .. } => "history",
            Self::Logs => "logs",
            Self::Wait => "wait",
            Self::Stop => "stop",
        }
    }
}

/// How long one shell command may live.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifetime {
    /// An ordinary command, ended by the lane at its deadline.
    Deadline(Duration),
    /// A long-lived development service, running while its task claim remains
    /// active or until the job tool stops it.
    Service,
}

impl Lifetime {
    /// The deadline the lane enforces, which only a service lacks.
    #[must_use]
    pub fn deadline(self) -> Option<Duration> {
        match self {
            Self::Deadline(deadline) => Some(deadline),
            Self::Service => None,
        }
    }
}

/// One shell command the agent asked the lane to run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellCall {
    /// The command line, run by the lane's shell.
    pub command: String,
    /// Its deadline, or the service it was explicitly asked to be.
    pub lifetime: Lifetime,
    /// What omp notes about the call ahead of what the command says: today,
    /// a deadline granted other than the one asked.
    pub notices: Vec<String>,
    /// The line the command is watched for: the first line of its output
    /// that matches is delivered while it runs; a service's match reaches
    /// only a seated occupant.
    pub until: Option<OutputPattern>,
}
/// Name a publishing route when it appears where the shell will start one
/// command, including after a compound-command separator.
///
/// This is deliberately a narrow gate over the commands a seat promises to
/// refuse, not a shell parser or network sandbox. Quoted text is skipped so an
/// admitted command may print these spellings without being refused.
fn publishing_command(command: &str) -> Option<&'static str> {
    let bytes = command.as_bytes();
    let mut at_start = true;
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
            index += 1;
            continue;
        }
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if matches!(byte, b';' | b'|' | b'&' | b'\n' | b'(' | b'{') {
            at_start = true;
            index += 1;
            continue;
        }
        if at_start && byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if at_start {
            let remaining = &command[index..];
            for route in PUBLISHING_COMMANDS {
                if let Some(suffix) = remaining.strip_prefix(route)
                    && suffix.as_bytes().first().is_none_or(|&next| {
                        next.is_ascii_whitespace()
                            || matches!(next, b';' | b'|' | b'&' | b'(' | b')' | b'{' | b'}')
                    })
                {
                    return Some(route);
                }
            }
            at_start = false;
        }
        index += 1;
    }
    None
}

impl ShellCall {
    /// Read a call from the arguments a `host_tool_call` frame carried.
    ///
    /// An omitted deadline is the default and a longer one is clamped to the
    /// maximum with omp's own notice. Zero is refused rather than read as no
    /// deadline: the only command that runs without one is a service, asked
    /// for by name and taking no timeout.
    ///
    /// # Errors
    ///
    /// Returns the refusal text when the arguments name no command, ask for
    /// a zero deadline, give a service a timeout, or give an until pattern
    /// that is not a string or does not compile.
    pub fn parse(arguments: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        let command = arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "shell needs a command".to_owned())?;
        if let Some(route) = publishing_command(command) {
            return Err(format!(
                "shell refuses publishing command `{route}` in an omp seat"
            ));
        }
        let service = arguments
            .get("service")
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|service| service);
        let asked = arguments.get("timeout").and_then(serde_json::Value::as_u64);
        let until = arguments
            .get("until")
            .map(|until| {
                until
                    .as_str()
                    .ok_or_else(|| "shell until must be a string".to_owned())
                    .and_then(OutputPattern::new)
            })
            .transpose()?;
        let (lifetime, notices) = match (service, asked) {
            (true, Some(_)) => {
                return Err(
                    "a service runs while its task claim remains active, so it takes no timeout"
                        .to_owned(),
                );
            }
            (true, None) => (Lifetime::Service, Vec::new()),
            (false, Some(0)) => {
                return Err(format!(
                    "timeout 0 does not lift the deadline; name {SHELL_TIMEOUT_MIN}-\
                     {SHELL_TIMEOUT_MAX} seconds, or set service for a long-lived \
                     development service"
                ));
            }
            (false, None) => (
                Lifetime::Deadline(Duration::from_secs(SHELL_TIMEOUT_DEFAULT)),
                Vec::new(),
            ),
            (false, Some(asked)) => {
                let granted = asked.min(SHELL_TIMEOUT_MAX);
                let clamped = (granted != asked).then(|| {
                    format!(
                        "Timeout clamped to {granted}s (requested {asked}s; allowed range \
                         {SHELL_TIMEOUT_MIN}-{SHELL_TIMEOUT_MAX}s)."
                    )
                });
                (
                    Lifetime::Deadline(Duration::from_secs(granted)),
                    clamped.into_iter().collect(),
                )
            }
        };
        Ok(Self {
            command: command.to_owned(),
            lifetime,
            notices,
            until,
        })
    }
}

/// What omp tells the model when a background job ends, around the job's
/// own result: the notice its `bash` jobs are delivered in.
#[must_use]
pub fn job_delivery(job: &str, result: &str) -> String {
    format!(
        "<system-notice>\nBackground job {job} has completed. Resume your work using the \
         result below.\n{result}\n</system-notice>"
    )
}

/// The background notice carrying the first line a shell `until` watcher
/// matched.
#[must_use]
pub fn job_matched(job: &str, pattern: &str, line: &str) -> String {
    format!(
        "<system-notice>\nBackground job {job} printed a line matching {pattern}; it keeps \
         running, and its result will be delivered when it ends:\n{line}\n</system-notice>"
    )
}

/// One in-flight host tool result as far as it has progressed.
///
/// omp presents the partial result on the active tool call and emits its
/// `tool_execution_update` frame. The final [`host_tool_result`] remains the
/// sole settlement.
#[must_use]
pub fn host_tool_update(id: &str, output: &str) -> Vec<u8> {
    request_line(&serde_json::json!({
        "type": "host_tool_update",
        "id": id,
        "partialResult": {
            "content": [{"type": "text", "text": output}],
        },
    }))
}

/// The answer to one host tool call: what the command said and how it ended.
///
/// The exit code rides the result's details, where omp's own `bash` keeps
/// it, so the transcript reads the same whichever ran the command; a failed
/// command is a tool error to the model, exactly as it is under `bash`.
#[must_use]
pub fn host_tool_result(id: &str, output: &str, exit_code: Option<i32>, failed: bool) -> Vec<u8> {
    let mut result = serde_json::json!({
        "content": [{"type": "text", "text": output}],
        "details": {"exitCode": exit_code},
    });
    if failed {
        result["isError"] = serde_json::Value::Bool(true);
    }
    request_line(&serde_json::json!({
        "type": "host_tool_result",
        "id": id,
        "result": result,
    }))
}

/// The answer to a host tool call the lane could not run at all: a tool it
/// never registered, or arguments that name no command.
#[must_use]
pub fn host_tool_refused(id: &str, reason: &str) -> Vec<u8> {
    request_line(&serde_json::json!({
        "type": "host_tool_result",
        "id": id,
        "result": {"content": [{"type": "text", "text": reason}]},
        "isError": true,
    }))
}

/// Append the answer to one read or write of a host-owned URI.
pub fn append_host_uri_result(output: &mut Vec<u8>, id: &str, content: &str) {
    #[derive(Serialize)]
    struct Result<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        id: &'a str,
        content: &'a str,
        #[serde(rename = "contentType")]
        content_type: &'static str,
    }
    append_request_line(
        output,
        &Result {
            kind: "host_uri_result",
            id,
            content,
            content_type: "text/markdown",
        },
    );
}

/// Append the answer to a host-owned URI request the lane does not serve.
pub fn append_host_uri_refused(output: &mut Vec<u8>, id: &str, reason: &str) {
    #[derive(Serialize)]
    struct Refusal<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        id: &'a str,
        #[serde(rename = "isError")]
        is_error: bool,
        error: &'a str,
    }
    append_request_line(
        output,
        &Refusal {
            kind: "host_uri_result",
            id,
            is_error: true,
            error: reason,
        },
    );
}

/// The answer to a host-owned URI request the lane does not serve, which
/// omp surfaces as the tool's error.
#[must_use]
pub fn host_uri_refused(id: &str, reason: &str) -> Vec<u8> {
    let mut output = Vec::new();
    append_host_uri_refused(&mut output, id, reason);
    output
}

//! The `spice://` resources a lane's omp reads its executable's spice rack,
//! board, task, session, and steering state through, replaces task texts, and
//! publishes captured authored text through.
//!
//! A rack URL, `spice://NS/PATH` or a root member such as `spice://spice.toml`,
//! reads one member of the spice rack linked into this executable. Every other first
//! read URL names one existing CLI read: [`Request::parse`] maps it onto that
//! read's arguments, and
//! [`Seat::serve`] runs the read in process as the lane, so authority, scoping,
//! and refusals are the CLI's. Native graph continuations page retained
//! validated text; other reads use the CLI's pages. Every withheld page names
//! its continuation URL, and a collection ends with its child URLs. A trailing
//! line selector is honored or refused. Reading commits nothing and
//! acknowledges nothing, so omp may read as often as its tools ask.
//!
//! A task's description and terms also take a write: the whole new text,
//! to the URL its read names, which carries the digest of the text read. The
//! write is the `task modify` intent under the lane's actor, and a stale or
//! absent digest refuses rather than overwriting. A task's `invite` URL, or
//! `invite/ADDRESS` naming the lane, takes a write that is one `task join
//! HANDLE --invite[=ADDRESS]` request whose whole body is the requester's
//! message, admitted from the captured write as an outbox body is and
//! delivered to the invited lane in the task's room; its `join` URL takes an
//! empty write that is `task join HANDLE`. `spice://outbox` instead admits
//! one captured authored message and returns its durable packet outcomes.
//! Every other URL naming a command is refused with that command.

use std::path::PathBuf;

use artifact::ArtifactAnswer;
use spice::repository::Repository;
use wire::intent::CommandAnswer;
use wire::{CanonicalUuid, Outcome, TaskHandle};

use crate::command_client::Host;
use crate::render::{self, Form};

mod graph;
mod lane;
mod rack;
pub(crate) use rack::{linked_for_app, read_app_document};
mod task_write;

use task_write::{Act, Text};

pub(crate) use lane::{Server, lane_server, read_command};

#[cfg(test)]
mod tests;

/// The scheme every resource URL carries.
pub(crate) const SCHEME: &str = "spice://";

/// The root index: every resource shape and the CLI read it answers with.
const INDEX: &str = "\
spice:// resources: views of spice CLI reads, digest-guarded task text replacement, task invite and join writes, and captured authored text through outbox; every other workflow act stays a spice command
spice://SKILL.md  complete authoritative agent skill: a root member of this executable's spice rack; takes line selectors but no query
spice://spice.toml  packaged configuration defaults from this executable, not the repository's effective configuration; takes line selectors but no query or write
spice://build.json  build record of this executable's spice rack: product version and, when bound to a commit, its source commit and committer second; takes line selectors but no query or write
spice://MEMBER[@CODE]  one member of this executable's linked spice rack at its rack path; text inline, other media as one line naming its type, size, digest, generation short code, and extracted file; a directory lists its members
spice://DIR[@CODE]/PATH  CODE may qualify the first segment or trail the full member path; it is the short code of the journal fact admitting the linked rack, the only generation served; @CODE alone lists root members; takes line selectors but no query or write; a digest or UUID never names a generation
Fixed CLI reads resolve before rack membership.
spice://board  task status
spice://board/graph/VIEW  board graph VIEW (origin, dependencies, review, workflow, claims), live work by default [?all&limit=N&max-bytes=N&max-text=N&at=BORN&after=CURSOR]; all includes settled history; after resumes a snapshot while held by this seat's bounded cache
spice://tasks  task list [?all | ?topic=/TOPIC] [&status=STATUS] [&limit=N&max-bytes=N&max-text=N&after=HANDLE]
spice://task/HANDLE  task show HANDLE; takes no query: /context, /history, and /detail take the paging and text keys
spice://task/HANDLE/context  task show HANDLE --context [?limit=N&max-bytes=N&max-text=N&after=CURSOR]
spice://task/HANDLE/history  task show HANDLE --history [same paging]
spice://task/HANDLE/detail  task show HANDLE --context --history [same paging]
spice://task/HANDLE/description  the description; its first line names the write URL, ?digest=DIGEST, that replaces the text below it
spice://task/HANDLE/terms  the terms as `- ` items, replaced the same way
spice://task/HANDLE/term/ITEM  one term by its item handle, under a first line naming the write URL; a write replaces that term alone, and a handle whose term has since changed refuses as stale
spice://task/HANDLE/invite  write to recruit a teammate as task join HANDLE --invite does, or to spice://task/HANDLE/invite/ADDRESS to invite that lane; the whole body, which may be empty, is your message to the invited lane, authored from your tape like an outbox write and delivered to that lane in the task's room; each write is one request answered as the CLI answers it, followed by the message's outbox receipt
spice://task/HANDLE/join  write an empty body for task join HANDLE, filling a free slot of a task you do not hold; each write is one request
spice://task/HANDLE/ledger  doc ledger HANDLE [?max-bytes=N&offset=N&digest=DIGEST]
spice://task/HANDLE/artifacts  task artifact list HANDLE [?limit=N&max-bytes=N&after=ARTIFACT]
spice://task/HANDLE/artifact/ARTIFACT  task artifact show HANDLE ARTIFACT [?offset=N&bytes=N]
spice://task/HANDLE/room  room history HANDLE [?contributions&limit=N&max-bytes=N&after=PUBLICATION]; contributions pages only typed contributions; every page groups its contributions by kind; reading joins and acknowledges nothing
spice://task/HANDLE/room/awareness  room awareness HANDLE [?limit=N&max-bytes=N&after=POSITION]; lanes told about this Design, matched terms, and bounded advisory maturity; reading changes nothing
spice://task/HANDLE/design  task authority HANDLE [?limit=N&max-bytes=N&after=POSITION]; reading changes nothing
spice://room/inbox  room inbox; this lane's unread room activity, one bounded page from its delivery cursor; reading changes nothing
spice://inbox  inbox [?limit=N&max-bytes=N&max-text=N&after=ID]; reading acknowledges nothing
spice://outbox  write text you author between tool calls, ordinary or SYN/ACK/FIN/NAK/RST packets, with no preparation read: a provider may fold mid-turn text into reasoning, while a final message arrives intact and is never copied here; the host preserves its tape source and exact occupant; identical complete bytes within that occupant reuse one submission and its decided outcomes; write receipts report reused: false for fresh admission, true for reuse
spice://outbox/UUID  side-effect-free exact-author inspection of an accepted submission: exact tape text, quote reference, decided packet outcomes and unresolved positions; cancellation or a lost reply never rolls back accepted bytes or effects; no query, selector or write
spice://lanes  lane list [?limit=N&max-bytes=N&after=LANE]
spice://session/briefing  session briefing [?file=PATH...&capture=BINDING&deadline-seconds=N]
spice://session/turns  session turns [?file=PATH...&capture=BINDING&start=TIME&end=TIME&contains=TEXT&turn-id=ID...&tool=NAME...&view=dialogue|full&limit=N&max-bytes=N&max-text=N&after=CURSOR]
spice://session/sweep  session sweep [?file=PATH...&capture=BINDING&limit=N&max-bytes=N&max-text=N&after=CURSOR&deadline-seconds=N]
spice://session/context/SESSION?lane=UUID&vendor=NAME&stream=NAME&source=UUID&member=N&offset=N  session context SESSION; all six coordinates are required, so copy the whole URL a task view names [&limit=N&max-bytes=N&max-text=N&after=CURSOR]
spice://quote/v1/...  a quoted message and its recorded author
Query values percent-encode reserved bytes, ':' as %3A; a key marked ... repeats. A trailing :N, :N-M, :N+K, :-N, or comma list selects lines; :raw is the text itself.
resources:
  spice://SKILL.md
  spice://spice.toml
  spice://docs
  spice://board
  spice://tasks
  spice://inbox
  spice://outbox
  spice://room/inbox
  spice://lanes
  spice://session/briefing
  spice://session/turns
  spice://session/sweep";

/// Told to a captured operation on a URL that adopts no authored text.
const CAPTURED_ELSEWHERE: &str =
    "captured authored operations address only spice://outbox and a task's invite URL";

/// How omp reaches a URL: a read, task text replacement, or captured authored text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access<'a> {
    Read,
    /// A receipt read retaining the invoking occupant across helper replacement.
    CapturedRead(wire::outbox::Carrier),
    Write(&'a str),
    /// Authored text, the outbox or an invitation's message, with provenance
    /// supplied by the capturing host.
    Authored {
        text: &'a str,
        carrier: wire::outbox::Carrier,
    },
}

/// What one `spice://` URL names: the resource and the lines selected of it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    resource: Resource,
    selector: Option<Selector>,
}

#[derive(Debug, PartialEq, Eq)]
enum Resource {
    Index,
    Outbox {
        submission: Option<wire::CanonicalUuid>,
    },
    /// The member of this executable's spice rack at its rack path, or with
    /// no such member the members beneath that directory, optionally under
    /// the generation short code the URL named.
    Rack {
        path: String,
        generation: Option<wire::SequenceCode>,
    },
    /// A quote target, as the URL spelled it without its selector.
    Quote(String),
    Read(Query),
    Text(Text),
    /// A task's invite or join URL: a read names its write, a write submits it.
    Act(Act),
}

/// One CLI read: its resource shape and the options its query named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Query {
    shape: Shape,
    options: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    Board,
    Graph { view: String },
    Tasks,
    Task { handle: TaskHandle, detail: Detail },
    Ledger { handle: TaskHandle },
    Artifacts { handle: TaskHandle },
    Artifact { handle: TaskHandle, id: String },
    Room { handle: TaskHandle },
    RoomAwareness { handle: TaskHandle },
    Design { handle: TaskHandle },
    Inbox,
    RoomInbox,
    Lanes,
    Session(Session),
    Context { session: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    Summary,
    Context,
    History,
    Both,
}

/// A `session` read over the lane's own transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Briefing,
    Turns,
    Sweep,
}

impl Session {
    const fn leaf(self) -> &'static str {
        match self {
            Self::Briefing => "briefing",
            Self::Turns => "turns",
            Self::Sweep => "sweep",
        }
    }

    fn named(leaf: &str) -> Option<Self> {
        [Self::Briefing, Self::Turns, Self::Sweep]
            .into_iter()
            .find(|session| session.leaf() == leaf)
    }
}

/// How one query key reaches the CLI read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    /// `?all` spells `--all`.
    Flag,
    /// `?limit=N` spells `--limit N`, at most once.
    Once,
    /// `?lane=UUID` spells `--lane UUID`, exactly once: a read without it
    /// names no record.
    Required,
    /// `?tool=NAME` spells `--tool NAME`, as often as the query names it.
    Repeated,
    /// `?file=PATH` spells the operand `PATH`, as often as the query names it.
    Operand,
}

/// One query key a shape admits: the CLI flag or operand it spells.
struct Key {
    name: &'static str,
    arity: Arity,
}

const fn once(name: &'static str) -> Key {
    Key {
        name,
        arity: Arity::Once,
    }
}

const fn repeated(name: &'static str) -> Key {
    Key {
        name,
        arity: Arity::Repeated,
    }
}

const fn required(name: &'static str) -> Key {
    Key {
        name,
        arity: Arity::Required,
    }
}

const FILES: Key = Key {
    name: "file",
    arity: Arity::Operand,
};

const TASK_LIST: &[Key] = &[
    Key {
        name: "all",
        arity: Arity::Flag,
    },
    once("topic"),
    once("status"),
    once("limit"),
    once("max-bytes"),
    once("max-text"),
    once("after"),
];
/// A page bounded by rows, bytes, and text, resumed after a cursor.
const PAGE: &[Key] = &[
    once("limit"),
    once("max-bytes"),
    once("max-text"),
    once("after"),
];
/// A page bounded by rows and bytes, resumed after a cursor.
const ROWS: &[Key] = &[once("limit"), once("max-bytes"), once("after")];
/// A room page, optionally only its typed contributions.
const ROOM: &[Key] = &[
    Key {
        name: "contributions",
        arity: Arity::Flag,
    },
    once("limit"),
    once("max-bytes"),
    once("after"),
];
const GRAPH: &[Key] = &[
    Key {
        name: "all",
        arity: Arity::Flag,
    },
    once("limit"),
    once("max-bytes"),
    once("max-text"),
    once("at"),
    once("after"),
];
const LEDGER: &[Key] = &[once("max-bytes"), once("offset"), once("digest")];
const ARTIFACT_SHOW: &[Key] = &[once("offset"), once("bytes")];
const BRIEFING: &[Key] = &[FILES, once("capture"), once("deadline-seconds")];
const TURNS: &[Key] = &[
    FILES,
    once("capture"),
    once("start"),
    once("end"),
    once("contains"),
    repeated("turn-id"),
    repeated("tool"),
    once("view"),
    once("limit"),
    once("max-bytes"),
    once("max-text"),
    once("after"),
];
const SWEEP: &[Key] = &[
    FILES,
    once("capture"),
    once("limit"),
    once("max-bytes"),
    once("max-text"),
    once("after"),
    once("deadline-seconds"),
];
/// A prior session's boundary coordinates, then the page over its records.
const CONTEXT: &[Key] = &[
    required("lane"),
    required("vendor"),
    required("stream"),
    required("source"),
    required("member"),
    required("offset"),
    once("limit"),
    once("max-bytes"),
    once("max-text"),
    once("after"),
];

/// The CLI reads resources serve; a URL naming any other command is refused.
const SERVED: &[&[&str]] = &[
    &["task", "status"],
    &["task", "list"],
    &["task", "show"],
    &["task", "artifact", "list"],
    &["task", "artifact", "show"],
    &["room", "history"],
    &["room", "inbox"],
    &["room", "awareness"],
    &["task", "authority"],
    &["board", "graph"],
    &["doc", "ledger"],
    &["inbox"],
    &["lane", "list"],
    &["session", "briefing"],
    &["session", "turns"],
    &["session", "sweep"],
    &["session", "context"],
];

impl Shape {
    fn admitted(&self) -> &'static [Key] {
        match self {
            Self::Board
            | Self::Task {
                detail: Detail::Summary,
                ..
            } => &[],
            Self::Graph { .. } => GRAPH,
            Self::Tasks => TASK_LIST,
            Self::Task { .. } | Self::Inbox => PAGE,
            Self::Ledger { .. } => LEDGER,
            Self::Artifacts { .. }
            | Self::RoomAwareness { .. }
            | Self::Design { .. }
            | Self::Lanes
            | Self::RoomInbox => ROWS,
            Self::Room { .. } => ROOM,
            Self::Artifact { .. } => ARTIFACT_SHOW,
            Self::Session(Session::Briefing) => BRIEFING,
            Self::Session(Session::Turns) => TURNS,
            Self::Session(Session::Sweep) => SWEEP,
            Self::Context { .. } => CONTEXT,
        }
    }

    fn arity(&self, key: &str) -> Option<Arity> {
        self.admitted()
            .iter()
            .find(|admitted| admitted.name == key)
            .map(|admitted| admitted.arity)
    }

    /// The keys a read of this shape cannot run without.
    fn required(&self) -> impl Iterator<Item = &'static str> {
        self.admitted()
            .iter()
            .filter(|admitted| admitted.arity == Arity::Required)
            .map(|admitted| admitted.name)
    }

    /// The required keys `options` does not name, in admitted order.
    fn missing(&self, options: &[(String, Option<String>)]) -> Vec<&'static str> {
        self.required()
            .filter(|name| !options.iter().any(|(named, _)| named == name))
            .collect()
    }

    fn path(&self) -> String {
        match self {
            Self::Board => "board".to_owned(),
            Self::Graph { view } => format!("board/graph/{}", encode(view)),
            Self::Tasks => "tasks".to_owned(),
            Self::Task { handle, detail } => match detail {
                Detail::Summary => format!("task/{handle}"),
                Detail::Context => format!("task/{handle}/context"),
                Detail::History => format!("task/{handle}/history"),
                Detail::Both => format!("task/{handle}/detail"),
            },
            Self::Ledger { handle } => format!("task/{handle}/ledger"),
            Self::Artifacts { handle } => format!("task/{handle}/artifacts"),
            Self::Artifact { handle, id } => {
                format!("task/{handle}/artifact/{}", encode(id))
            }
            Self::Room { handle } => format!("task/{handle}/room"),
            Self::RoomAwareness { handle } => format!("task/{handle}/room/awareness"),
            Self::Design { handle } => format!("task/{handle}/design"),
            Self::Inbox => "inbox".to_owned(),
            Self::RoomInbox => "room/inbox".to_owned(),
            Self::Lanes => "lanes".to_owned(),
            Self::Session(session) => format!("session/{}", session.leaf()),
            Self::Context { session } => format!("session/context/{}", encode(session)),
        }
    }

    fn command(&self) -> Vec<String> {
        let words = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
        match self {
            Self::Board => words(&["task", "status"]),
            Self::Graph { view } => words(&["board", "graph", view]),
            Self::Tasks => words(&["task", "list"]),
            Self::Task { handle, detail } => {
                let mut command = words(&["task", "show", handle.as_str()]);
                if matches!(detail, Detail::Context | Detail::Both) {
                    command.push("--context".to_owned());
                }
                if matches!(detail, Detail::History | Detail::Both) {
                    command.push("--history".to_owned());
                }
                command
            }
            Self::Ledger { handle } => words(&["doc", "ledger", handle.as_str()]),
            Self::Artifacts { handle } => words(&["task", "artifact", "list", handle.as_str()]),
            Self::Artifact { handle, id } => {
                words(&["task", "artifact", "show", handle.as_str(), id])
            }
            Self::Room { handle } => words(&["room", "history", handle.as_str()]),
            Self::RoomAwareness { handle } => words(&["room", "awareness", handle.as_str()]),
            Self::Design { handle } => words(&["task", "authority", handle.as_str()]),
            Self::Inbox => words(&["inbox"]),
            Self::RoomInbox => words(&["room", "inbox"]),
            Self::Lanes => words(&["lane", "list"]),
            Self::Session(session) => words(&["session", session.leaf()]),
            Self::Context { session } => words(&["session", "context", session]),
        }
    }
}

impl Query {
    /// The exact arguments after `spice` that run this read: operands first,
    /// then options in the order the query named them.
    pub(crate) fn arguments(&self) -> Vec<String> {
        let mut arguments = self.shape.command();
        let (operands, options): (Vec<_>, Vec<_>) = self
            .options
            .iter()
            .partition(|(key, _)| self.shape.arity(key) == Some(Arity::Operand));
        arguments.extend(operands.into_iter().filter_map(|(_, value)| value.clone()));
        for (key, value) in options {
            // Graph continuations resume this seat's retained text, not a new CLI read.
            if matches!(self.shape, Shape::Graph { .. }) && key == "after" {
                continue;
            }
            arguments.push(format!("--{key}"));
            arguments.extend(value.iter().cloned());
        }
        arguments
    }

    /// The URL naming this read.
    pub(crate) fn url(&self) -> String {
        let mut url = format!("{SCHEME}{}", self.shape.path());
        for (index, (key, value)) in self.options.iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str(key);
            if let Some(value) = value {
                url.push('=');
                url.push_str(&encode(value));
            }
        }
        url
    }

    /// The read a CLI command performs, when a resource serves it.
    pub(crate) fn from_arguments(arguments: &[String]) -> Option<Self> {
        let words = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        let (mut shape, rest) = Shape::from_command(&words)?;
        // Detail decides which keys a task read admits, so settle it first.
        if let Shape::Task { detail, .. } = &mut shape {
            *detail = match (rest.contains(&"--context"), rest.contains(&"--history")) {
                (false, false) => Detail::Summary,
                (true, false) => Detail::Context,
                (false, true) => Detail::History,
                (true, true) => Detail::Both,
            };
        }
        let mut options = Vec::new();
        let mut words = rest.iter();
        while let Some(word) = words.next() {
            let Some(key) = word.strip_prefix("--") else {
                let operand = shape
                    .admitted()
                    .iter()
                    .find(|admitted| admitted.arity == Arity::Operand)?;
                options.push((operand.name.to_owned(), Some((*word).to_owned())));
                continue;
            };
            match (&shape, key) {
                (Shape::Task { .. }, "context" | "history") => {}
                _ => {
                    let value = match shape.arity(key)? {
                        Arity::Flag => None,
                        Arity::Once | Arity::Required | Arity::Repeated => {
                            Some((*words.next()?).to_owned())
                        }
                        Arity::Operand => return None,
                    };
                    options.push((key.to_owned(), value));
                }
            }
        }
        shape
            .missing(&options)
            .is_empty()
            .then_some(Self { shape, options })
    }
}

impl Shape {
    /// The shape a CLI command's leading words name, and the words after them.
    fn from_command<'a>(words: &'a [&'a str]) -> Option<(Self, &'a [&'a str])> {
        let handle = |word: &str| word.parse::<TaskHandle>().ok();
        Some(match words {
            ["task", "status", rest @ ..] => (Self::Board, rest),
            ["board", "graph", view, rest @ ..] => (
                Self::Graph {
                    view: (*view).to_owned(),
                },
                rest,
            ),
            ["task", "list", rest @ ..] => (Self::Tasks, rest),
            ["task", "show", task, rest @ ..] => (
                Self::Task {
                    handle: handle(task)?,
                    detail: Detail::Summary,
                },
                rest,
            ),
            ["doc", "ledger", task, rest @ ..] => (
                Self::Ledger {
                    handle: handle(task)?,
                },
                rest,
            ),
            ["task", "artifact", "list", task, rest @ ..] => (
                Self::Artifacts {
                    handle: handle(task)?,
                },
                rest,
            ),
            ["task", "artifact", "show", task, id, rest @ ..] => (
                Self::Artifact {
                    handle: handle(task)?,
                    id: (*id).to_owned(),
                },
                rest,
            ),
            ["room", "history", task, rest @ ..] => (
                Self::Room {
                    handle: handle(task)?,
                },
                rest,
            ),
            ["room", "awareness", task, rest @ ..] => (
                Self::RoomAwareness {
                    handle: handle(task)?,
                },
                rest,
            ),
            ["task", "authority", task, rest @ ..] => (
                Self::Design {
                    handle: handle(task)?,
                },
                rest,
            ),
            ["inbox", rest @ ..] => (Self::Inbox, rest),
            ["room", "inbox", rest @ ..] => (Self::RoomInbox, rest),
            ["lane", "list", rest @ ..] => (Self::Lanes, rest),
            ["session", "context", session, rest @ ..] => (
                Self::Context {
                    session: (*session).to_owned(),
                },
                rest,
            ),
            ["session", leaf, rest @ ..] => (Self::Session(Session::named(leaf)?), rest),
            _ => return None,
        })
    }
}

impl Request {
    /// Map one URL onto the resource it names, or say why it names none.
    pub(crate) fn parse(url: &str) -> Result<Self, String> {
        Self::parse_with_rack(url, |path| rack::linked_member(path, url))
    }

    fn parse_with_rack(
        url: &str,
        member: impl FnOnce(&str) -> Result<bool, String>,
    ) -> Result<Self, String> {
        let Some(rest) = url.strip_prefix(SCHEME) else {
            return Err(format!("{url} is not a {SCHEME} URL"));
        };
        let (rest, selector) = match rest.split_once(':') {
            Some((rest, selector)) => (rest, Some(Selector::parse(selector)?)),
            None => (rest, None),
        };
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let segments = path.strip_suffix('/').unwrap_or(path);
        let segments = if segments.is_empty() {
            Vec::new()
        } else {
            segments
                .split('/')
                .map(decode)
                .collect::<Result<Vec<_>, _>>()?
        };
        let words = segments.iter().map(String::as_str).collect::<Vec<_>>();
        let shape = match words.as_slice() {
            ["outbox", tail @ ..] if query.is_empty() => {
                let submission =
                    match tail {
                        [] => None,
                        [identity] => Some(identity.parse().map_err(|_| {
                            format!("{url} names no valid outbox submission identity")
                        })?),
                        _ => return Err(format!("{url} names no outbox resource")),
                    };
                return Ok(Self {
                    resource: Resource::Outbox { submission },
                    selector,
                });
            }
            [] if query.is_empty() => {
                return Ok(Self {
                    resource: Resource::Index,
                    selector,
                });
            }
            ["quote", ..] => {
                return Ok(Self {
                    resource: Resource::Quote(format!("{SCHEME}{rest}")),
                    selector,
                });
            }
            ["board"] => Shape::Board,
            ["board", "graph", view] => Shape::Graph {
                view: (*view).to_owned(),
            },
            ["tasks"] => Shape::Tasks,
            ["task", task, rest @ ..]
                if let Some(resource) = task_write::resource(task, rest, query, url) =>
            {
                return Ok(Self {
                    resource: resource?,
                    selector,
                });
            }
            ["task", task, rest @ ..] => task_shape(task, rest, url)?,
            ["inbox"] => Shape::Inbox,
            ["room", "inbox"] => Shape::RoomInbox,
            ["lanes"] => Shape::Lanes,
            ["session", "context", session] => Shape::Context {
                session: (*session).to_owned(),
            },
            ["session", leaf] if let Some(session) = Session::named(leaf) => {
                Shape::Session(session)
            }
            _ => {
                return Self::rack(words.join("/"), query, selector, url, member)?.map_or_else(
                    || {
                        // A path that spells a command names that command.
                        if let Some(end) = (1..=words.len())
                            .rev()
                            .find(|&end| crate::cli::contract(&words[..end]).is_some())
                        {
                            refuse_unserved(&words[..end], url)?;
                        }
                        Err(unknown(url))
                    },
                    Ok,
                );
            }
        };
        let options = options(&shape, query, url)?;
        Ok(Self {
            resource: Resource::Read(Query { shape, options }),
            selector,
        })
    }
}

/// The shape beneath `spice://task/TASK`.
fn task_shape(task: &str, rest: &[&str], url: &str) -> Result<Shape, String> {
    refuse_unserved(&["task", task], url)?;
    let handle = task
        .parse::<TaskHandle>()
        .map_err(|error| format!("{url} names no task handle {task}: {error}"))?;
    Ok(match rest {
        [] => Shape::Task {
            handle,
            detail: Detail::Summary,
        },
        ["context"] => Shape::Task {
            handle,
            detail: Detail::Context,
        },
        ["history"] => Shape::Task {
            handle,
            detail: Detail::History,
        },
        ["detail"] => Shape::Task {
            handle,
            detail: Detail::Both,
        },
        ["ledger"] => Shape::Ledger { handle },
        ["artifacts"] => Shape::Artifacts { handle },
        ["artifact", id] => {
            refuse_unserved(&["task", "artifact", id], url)?;
            Shape::Artifact {
                handle,
                id: (*id).to_owned(),
            }
        }
        ["room"] => Shape::Room { handle },
        ["room", "awareness"] => Shape::RoomAwareness { handle },
        ["design"] => Shape::Design { handle },
        [verb] => {
            refuse_unserved(&["task", verb], url)?;
            return Err(unknown(url));
        }
        _ => return Err(unknown(url)),
    })
}

fn unknown(url: &str) -> String {
    format!("{url} names no spice:// resource; read {SCHEME} for the index")
}

/// Refuse a URL whose path spells a CLI command no resource serves.
fn refuse_unserved(path: &[&str], url: &str) -> Result<(), String> {
    if SERVED.contains(&path) || crate::cli::contract(path).is_none() {
        return Ok(());
    }
    Err(format!(
        "{url} names `spice {}`, which spice:// does not serve; run the command",
        path.join(" ")
    ))
}

fn options(shape: &Shape, query: &str, url: &str) -> Result<Vec<(String, Option<String>)>, String> {
    let mut options: Vec<(String, Option<String>)> = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (decode(key)?, Some(decode(value)?)),
            None => (decode(pair)?, None),
        };
        let Some(arity) = shape.arity(&key) else {
            let names = shape
                .admitted()
                .iter()
                .map(|admitted| admitted.name)
                .collect::<Vec<_>>();
            return Err(match shape {
                Shape::Task {
                    handle,
                    detail: Detail::Summary,
                } => format!(
                    "{url} takes no query; read {SCHEME}task/{handle}/context, /history, or \
                     /detail, which take {}",
                    PAGE.iter()
                        .map(|admitted| admitted.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                _ if names.is_empty() => format!("{url} takes no query; drop ?{query}"),
                _ => format!(
                    "{url} takes no query key {key}; it takes {}",
                    names.join(", ")
                ),
            });
        };
        match (&value, arity) {
            (Some(value), Arity::Once | Arity::Required | Arity::Repeated | Arity::Operand)
                if !value.is_empty() => {}
            (None, Arity::Flag) => {}
            (_, Arity::Once | Arity::Required | Arity::Repeated | Arity::Operand) => {
                return Err(format!(
                    "{url} names {key} without a value; write {key}=VALUE"
                ));
            }
            (Some(_), Arity::Flag) => {
                return Err(format!("{url} gives {key} a value; write {key} alone"));
            }
        }
        if matches!(arity, Arity::Flag | Arity::Once | Arity::Required)
            && options.iter().any(|(named, _)| *named == key)
        {
            return Err(format!("{url} names {key} twice"));
        }
        options.push((key, value));
    }
    let missing = shape.missing(&options);
    if !missing.is_empty() {
        return Err(format!(
            "{url} lacks the required coordinates {}; copy the whole spice://session/context URL the owning task view names (spice://task/HANDLE/context)",
            missing.join(", "),
        ));
    }
    Ok(options)
}

/// Percent-encode everything but unreserved URL bytes.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let byte = bytes
                .get(index + 1..index + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| format!("{text} has a % not followed by two hex digits"))?;
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| format!("{text} decodes to bytes that are not UTF-8"))
}

/// The lines a trailing `:` selector names.
#[derive(Debug, PartialEq, Eq)]
struct Selector {
    /// Inclusive one-based line spans, in the order named; empty for `:raw`.
    spans: Vec<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Span {
    /// Lines `first..=last`; `last` absent runs to the end.
    Lines { first: usize, last: Option<usize> },
    /// The last `count` lines.
    Tail { count: usize },
}

impl Selector {
    fn parse(written: &str) -> Result<Self, String> {
        let mut spans = Vec::new();
        for part in written.split(':') {
            match part {
                "raw" => {}
                "img" => {
                    return Err(
                        "spice:// reads are text; :img selects nothing, read the file a binary artifact names".to_owned(),
                    );
                }
                _ => {
                    for span in part.split(',') {
                        spans.push(Span::parse(span).ok_or_else(|| {
                            format!(
                                ":{written} is no line selector; use :N, :N-M, :N+K, :-N, a comma list, or :raw"
                            )
                        })?);
                    }
                }
            }
        }
        Ok(Self { spans })
    }

    fn select(&self, text: &str) -> Result<String, String> {
        if self.spans.is_empty() {
            return Ok(text.to_owned());
        }
        let lines = text.lines().collect::<Vec<_>>();
        let mut selected = Vec::new();
        for span in &self.spans {
            let (first, last) = match *span {
                Span::Lines { first, last } => (first, last.unwrap_or(lines.len())),
                Span::Tail { count } => (lines.len().saturating_sub(count) + 1, lines.len()),
            };
            if first > lines.len() {
                return Err(format!(
                    "line {first} is past the end; the resource has {} lines",
                    lines.len()
                ));
            }
            selected.extend_from_slice(&lines[first - 1..last.min(lines.len())]);
        }
        Ok(selected.join("\n"))
    }
}

impl Span {
    fn parse(written: &str) -> Option<Self> {
        let number = |text: &str| text.parse::<usize>().ok().filter(|number| *number > 0);
        if let Some(count) = written.strip_prefix('-') {
            return Some(Self::Tail {
                count: number(count)?,
            });
        }
        if let Some((first, count)) = written.split_once('+') {
            let first = number(first)?;
            return Some(Self::Lines {
                first,
                last: Some(first.checked_add(number(count)? - 1)?),
            });
        }
        if let Some((first, last)) = written.split_once('-') {
            let first = number(first)?;
            let last = if last.is_empty() {
                None
            } else {
                Some(number(last)?)
            };
            return last
                .is_none_or(|last| last >= first)
                .then_some(Self::Lines { first, last });
        }
        Some(Self::Lines {
            first: number(written)?,
            last: None,
        })
    }
}

/// What the CLI renders for one board command outcome.
fn command_rendering(outcome: &Outcome<CommandAnswer>) -> String {
    let rendering = render::render_with_reply_text(outcome, Form::Text, crate::command_answer_text);
    match rendered(outcome, rendering) {
        Ok(document) | Err(document) => document,
    }
}

/// Carry a rendered reply as success and a rendered refusal or fault as error.
pub(super) fn settled_document<B>(
    outcome: &Outcome<B>,
    document: String,
) -> Result<String, String> {
    match outcome {
        Outcome::Reply(_) => Ok(document),
        Outcome::Refusal(_) | Outcome::Fault(_) => Err(document),
    }
}

/// Who reads or writes: the lane, its checkout, the session state its
/// occupant's own commands see, and the environment its client reads.
pub(crate) struct Seat {
    pub(crate) worktree: PathBuf,
    pub(crate) lane: CanonicalUuid,
    pub(crate) environment: crate::session_command::Environment,
    pub(crate) ambient: spice::ambient::Ambient,
    pub(crate) graphs: graph::Pages,
}

impl Seat {
    /// Answer one `spice://` read or write: the document the URL names or the
    /// write's reply, or why there is none.
    ///
    /// `read_quote` resolves quote targets.
    pub(crate) fn serve(
        &self,
        url: &str,
        access: Access<'_>,
        read_quote: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<String, String> {
        let request = Request::parse(url)?;
        let repository = || {
            Repository::discover(&self.worktree)
                .map_err(|error| format!("spice cannot resolve the lane's checkout: {error}"))
        };
        if let Resource::Outbox { submission } = request.resource {
            if request.selector.is_some() {
                return Err("outbox operations and receipts take no line selector".to_owned());
            }
            return self.serve_outbox(submission, access, repository);
        }
        if let Resource::Act(act) = &request.resource {
            if request.selector.is_some() {
                return Err(format!("{url} takes no line selector"));
            }
            return self.serve_act(act, access, repository);
        }
        if matches!(access, Access::Authored { .. } | Access::CapturedRead(_)) {
            return Err(CAPTURED_ELSEWHERE.to_owned());
        }
        if let Access::Write(content) = access {
            let Resource::Text(text) = &request.resource else {
                return Err(format!(
                    "{url} takes no write; task description, terms and single-term replacement, task invite/join, and captured spice://outbox text are writable; every other workflow act stays a spice command"
                ));
            };
            if request.selector.is_some() {
                return Err(format!(
                    "{url} names lines, but a write replaces the whole text; drop the selector"
                ));
            }
            let repository = repository()?;
            let host = Host {
                repository: &repository,
                ambient: &self.ambient,
            };
            return crate::command_client::as_lane(self.lane, || text.replace(host, content));
        }
        let document = match &request.resource {
            Resource::Index => INDEX.to_owned(),
            Resource::Outbox { .. } | Resource::Act(_) => {
                unreachable!("outbox and act resources were served above")
            }
            Resource::Rack { path, generation } => rack::linked(path, *generation, url, || {
                repository()?.admit_state().map_err(|error| {
                    format!("spice cannot admit the runtime directory {url} reads through: {error}")
                })
            })?,
            Resource::Quote(target) => read_quote(target)?,
            Resource::Read(query) => {
                let repository = repository()?;
                crate::command_client::as_lane(self.lane, || self.run(&repository, query))?
            }
            Resource::Text(text) => {
                if text.digest.is_some() {
                    return Err(format!(
                        "{url} names a digest, which only a write takes; read {} instead",
                        text.read_url()
                    ));
                }
                let repository = repository()?;
                let host = Host {
                    repository: &repository,
                    ambient: &self.ambient,
                };
                crate::command_client::as_lane(self.lane, || text.document(host))?
            }
        };
        match &request.selector {
            Some(selector) => selector.select(&document),
            None => Ok(document),
        }
    }

    /// Read an act's grammar or submit its write. An invitation's message
    /// comes only from its captured host call.
    fn serve_act(
        &self,
        act: &Act,
        access: Access<'_>,
        repository: impl FnOnce() -> Result<Repository, String>,
    ) -> Result<String, String> {
        let (content, carrier) = match access {
            Access::Read => return Ok(act.grammar()),
            Access::Write(content) => (content, None),
            Access::Authored { text, carrier } if act.invites() => (text, Some(carrier)),
            Access::Authored { .. } | Access::CapturedRead(_) => {
                return Err(CAPTURED_ELSEWHERE.to_owned());
            }
        };
        let repository = repository()?;
        let host = Host {
            repository: &repository,
            ambient: &self.ambient,
        };
        crate::command_client::as_lane(self.lane, || act.submit(host, self.lane, content, carrier))
    }

    fn serve_outbox(
        &self,
        submission: Option<CanonicalUuid>,
        access: Access<'_>,
        repository: impl FnOnce() -> Result<Repository, String>,
    ) -> Result<String, String> {
        if submission.is_none() && access == Access::Read {
            return Ok(
                    "Write complete UTF-8 assistant text to spice://outbox. No preparation read is needed. The tape remains authoritative. Identical complete bytes from one exact occupant resolve the same submission. SYN/ACK/FIN/NAK/RST packets settle independently; any other header word is prose. Inspect the returned spice://outbox/IDENTITY without causing effects."
                        .to_owned(),
                );
        }
        let repository = repository()?;
        let host = Host::new(&repository, &self.ambient);
        let outcome = match (submission, access) {
            (None, Access::Authored { text, carrier }) => crate::command_client::execute_outbox(
                host,
                self.lane,
                wire::CommandKind::OutboxSubmit,
                &wire::outbox::Submit {
                    carrier,
                    text: text.to_owned(),
                },
            ),
            (Some(submission), Access::CapturedRead(carrier)) => {
                crate::command_client::execute_outbox(
                    host,
                    self.lane,
                    wire::CommandKind::OutboxRead,
                    &wire::outbox::Read {
                        submission,
                        occupant: carrier.occupant,
                    },
                )
            }
            (Some(_), Access::Read) => {
                return Err("an outbox receipt requires the exact captured host reader".to_owned());
            }
            (None, _) => return Err(
                "outbox writes require the exact captured host URI call, not an unproved tool body"
                    .to_owned(),
            ),
            (Some(_), _) => return Err("an outbox receipt is read-only".to_owned()),
        };
        let rendered = serde_json::to_string(&outcome)
            .map_err(|error| format!("outbox outcome cannot be represented: {error}"))?;
        settled_document(&outcome, rendered)
    }

    /// Run one read through the CLI's own parser and dispatch.
    fn run(&self, repository: &Repository, query: &Query) -> Result<String, String> {
        let matches = crate::cli::command()
            .try_get_matches_from(std::iter::once("spice".to_owned()).chain(query.arguments()))
            .map_err(|error| error.to_string().trim_end().to_owned())?;
        let contract = crate::cli::selected_contract(&matches)
            .expect("the generated parser selects only registered contracts");
        let leaf = crate::cli::selected_matches(&matches);
        let host = Host {
            repository,
            ambient: &self.ambient,
        };
        let document = match contract.path {
            ["task", "artifact", ..] => {
                let outcome = crate::task_artifact_command::run_at(repository, leaf, contract);
                let mut document = rendered(&outcome, render::render(&outcome, Form::Text))?;
                if let Outcome::Reply(reply) = &outcome {
                    artifact_trailer(&mut document, &reply.body);
                }
                return Ok(document);
            }
            ["session", ..] => {
                let outcome = crate::session_command::run(leaf, contract.path, &self.environment);
                let mut document = rendered(
                    &outcome,
                    render::render_with_reply_text(
                        &outcome,
                        Form::Text,
                        crate::session_command::Report::text,
                    ),
                )?;
                if let Outcome::Reply(reply) = &outcome {
                    session_trailer(&mut document, &reply.body);
                }
                // A briefing names the reads behind its cut rows; this seat
                // reads them as URLs. Other session pages quote the transcript
                // and stay verbatim.
                if query.shape == Shape::Session(Session::Briefing) {
                    document = served_reads(&document);
                }
                document
            }
            ["inbox"] => {
                let outcome = crate::steering_command::inbox::run_at(
                    repository,
                    leaf,
                    contract,
                    Some(self.lane.to_string().into()),
                    &self.ambient,
                );
                rendered(&outcome, render::render_display(&outcome, Form::Text))?
            }
            ["room", ..] => room_document(repository, leaf, contract, self.lane)?,
            ["task", "authority"] => {
                let outcome = crate::design_command::run_at(repository, leaf, contract);
                rendered(&outcome, render::render_display(&outcome, Form::Text))?
            }
            ["lane", "list"] => {
                let outcome =
                    crate::lane_command::list_at(repository, leaf, contract, &self.ambient);
                rendered(&outcome, render::render_display(&outcome, Form::Text))?
            }
            ["board", "graph"] => self.graphs.read(query, || {
                let outcome = crate::board_command::graph_snapshot_at(host, leaf, contract);
                match outcome {
                    Outcome::Reply(wire::Reply {
                        position: wire::ReplyPosition::Snapshot(snapshot),
                        body: CommandAnswer::Text(answer),
                    }) => Ok((snapshot, answer.text)),
                    Outcome::Reply(_) => Err(
                        "board::answer-invalid: graph read returned no validated text snapshot"
                            .to_owned(),
                    ),
                    _ => Err(command_rendering(&outcome)),
                }
            })?,
            _ => {
                let outcome = crate::board_command::run_at(host, leaf, contract);
                let rendering = render::render_with_reply_text(
                    &outcome,
                    Form::Text,
                    crate::command_answer_text,
                );
                let mut document = rendered(&outcome, rendering)?;
                if let Outcome::Reply(reply) = &outcome
                    && let CommandAnswer::Text(_) = &reply.body
                {
                    if matches!(query.shape, Shape::Task { .. }) {
                        document = context_urls(&document);
                    }
                    board_trailer(&mut document, &query.shape);
                }
                document
            }
        };
        Ok(rewrite_continuations(&document))
    }
}

/// The rendered document of one room read by `lane`.
fn room_document(
    repository: &Repository,
    leaf: &clap::ArgMatches,
    contract: &crate::cli::Contract,
    lane: CanonicalUuid,
) -> Result<String, String> {
    match contract.path {
        ["room", "inbox"] => {
            let outcome =
                crate::room_command::inbox::run_at(repository, leaf, contract, Some(lane));
            rendered(&outcome, render::render_display(&outcome, Form::Text))
        }
        ["room", "awareness"] => {
            let outcome = crate::room_command::awareness::run_at(repository, leaf, contract);
            rendered(&outcome, render::render_display(&outcome, Form::Text))
        }
        _ => {
            let outcome = crate::room_command::run_at(repository, leaf, contract);
            rendered(&outcome, render::render_display(&outcome, Form::Text))
        }
    }
}

/// The rendered document of a reply, or of a refusal or fault as the error.
fn rendered<B>(
    outcome: &Outcome<B>,
    rendering: Result<render::Rendering, String>,
) -> Result<String, String> {
    let rendering =
        rendering.map_err(|error| format!("spice could not render the read: {error}"))?;
    let document = String::from_utf8_lossy(rendering.document())
        .trim_end()
        .to_owned();
    settled_document(outcome, document)
}

/// What a seat whose omp serves `spice://` learns about the scheme: told in
/// its first turn and again with each briefing after a compaction, never to
/// a seat whose registration was refused or whose vendor has no host schemes.
pub(crate) const SEAT_GUIDE: &str = "\
<system-notice>
# spice:// reads
This seat serves `spice://` through `read`: `spice://` is the index naming every resource and the CLI read it answers (board, tasks, task detail, ledger, artifacts, room history, room inbox and results, retained task authority, dependency graph, inbox, lanes, session briefing, turns, sweep, and context).
- A collection ends with `resources:`, the URLs of what it contains.
- Query parameters are the read's own flags; a withheld page ends with `continue:` and the URL that resumes it; a trailing `:N-M` selects lines.
- Reading changes nothing: it acknowledges no steering, claims nothing, and may repeat freely.
- A task's description and terms also take a `write`: read `spice://task/HANDLE/description` or `/terms`, then write the whole new text to the URL its first line names; a write over text that changed since that read is refused. `spice://task/HANDLE/term/ITEM` reads one term by its item handle, and a write to it replaces that term alone.
- `write` to `spice://task/HANDLE/invite` to recruit a teammate, or to `spice://task/HANDLE/invite/ADDRESS` to invite that lane; the whole body, which may be empty, is your message to the invited lane, tracked like an outbox write and delivered to that lane in the task's room. `write` an empty body to `spice://task/HANDLE/join` to join that task's phase. Each write is one `task join` request answered as the CLI answers it.
- Between tool calls, write text you author, ordinary or SYN/ACK/FIN/NAK/RST packets, to `spice://outbox` with no preparation read: a provider may fold mid-turn text into reasoning, while a final message arrives intact, so never copy it there. It preserves the tape source; other tool arguments remain inert. Exact repeated bytes within this occupant reuse one submission. Its receipt names `spice://outbox/UUID` for side-effect-free exact-text and packet-outcome inspection; a canceled or lost result never rolls back accepted work. `RST KEY [KEY ...]: REASON` cancels your own live sends and settles only here.
- Every other write and workflow act stays a `spice` command; visible SYN/ACK/FIN/NAK text remains supported; any other header word is prose.
</system-notice>";

/// `text` as told to a seat whose omp serves `spice://`: each quoted `spice`
/// read named by its URL, then the scheme guide.
pub(crate) fn for_served_seat(text: &str) -> String {
    format!("{}\n\n{SEAT_GUIDE}", served_reads(text))
}

/// Replace each backtick-quoted `spice` read a resource serves with its URL;
/// any other quoted text, a placeholder handle included, stays as written.
pub(crate) fn served_reads(text: &str) -> String {
    let mut served = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('`') {
        served.push_str(before);
        let Some((quoted, after)) = after.split_once('`') else {
            served.push('`');
            rest = after;
            break;
        };
        served.push('`');
        served.push_str(read_url(quoted).as_deref().unwrap_or(quoted));
        served.push('`');
        rest = after;
    }
    served.push_str(rest);
    served
}

/// The URL answering `command`, when it is a `spice` read a resource serves.
fn read_url(command: &str) -> Option<String> {
    let words = shell_words::split(command).ok()?;
    match words.split_first() {
        Some((spice, arguments)) if spice == "spice" => {
            Query::from_arguments(arguments).map(|query| query.url())
        }
        _ => None,
    }
}

/// Replace each page continuation a resource serves with its URL.
fn rewrite_continuations(document: &str) -> String {
    const CONTINUE: &str = "; continue: ";
    document
        .lines()
        .map(|line| {
            let Some((head, command)) = line.rsplit_once(CONTINUE) else {
                return line.to_owned();
            };
            match read_url(command) {
                Some(url) => format!("{head}{CONTINUE}{url}"),
                None => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace each whole line a task view renders as a complete `spice session
/// context` read with the URL that reads it, so a seat recovers a prior
/// session's records without leaving `spice://`.
fn context_urls(document: &str) -> String {
    document
        .lines()
        .map(|line| {
            line.starts_with("spice session context ")
                .then(|| read_url(line))
                .flatten()
                .unwrap_or_else(|| line.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// End a collection with the child URLs it names.
fn board_trailer(document: &mut String, shape: &Shape) {
    let children = match shape {
        Shape::Board => vec![format!("{SCHEME}tasks")],
        Shape::Tasks => document
            .lines()
            .filter_map(|line| line.split_whitespace().next()?.parse::<TaskHandle>().ok())
            .map(|handle| format!("{SCHEME}task/{handle}"))
            .collect(),
        Shape::Task {
            handle,
            detail: Detail::Summary,
        } => [
            "context",
            "history",
            "detail",
            "ledger",
            "description",
            "terms",
            "artifacts",
            "room",
        ]
        .iter()
        .map(|child| format!("{SCHEME}task/{handle}/{child}"))
        .collect(),
        _ => Vec::new(),
    };
    push_resources(document, &children);
}

/// Name an artifact page's continuation and its children as URLs.
fn artifact_trailer(document: &mut String, answer: &ArtifactAnswer) {
    let (continuation, children) = match answer {
        ArtifactAnswer::Listed {
            task,
            artifacts,
            omission,
            ..
        } => (
            omission.as_ref().map(|omission| &omission.continuation),
            artifacts
                .iter()
                .map(|artifact| {
                    Query {
                        shape: Shape::Artifact {
                            handle: task.clone(),
                            id: artifact.id.clone(),
                        },
                        options: Vec::new(),
                    }
                    .url()
                })
                .collect(),
        ),
        ArtifactAnswer::Shown { next, .. } => (next.as_ref(), Vec::new()),
        _ => (None, Vec::new()),
    };
    push_continuation(document, continuation);
    push_resources(document, &children);
}

/// Name a typed session page's continuation as a URL: its document carries
/// the command as JSON, not as a `continue:` line.
fn session_trailer(document: &mut String, report: &crate::session_command::Report) {
    use crate::session_command::Report;
    let continuation = match report {
        Report::Turns { page, .. } => page.omission.as_ref(),
        Report::Sweep { page, .. } => page.omission.as_ref(),
        Report::Context { page, .. } => page.omission.as_ref(),
        _ => None,
    };
    push_continuation(
        document,
        continuation.map(|omission| &omission.continuation),
    );
}

fn push_continuation(document: &mut String, continuation: Option<&wire::SpiceCommand>) {
    if let Some(query) = continuation.and_then(|command| Query::from_arguments(command.arguments()))
    {
        document.push_str("\ncontinue: ");
        document.push_str(&query.url());
    }
}

fn push_resources(document: &mut String, children: &[String]) {
    if children.is_empty() {
        return;
    }
    document.push_str("\nresources:");
    for child in children {
        document.push_str("\n  ");
        document.push_str(child);
    }
}

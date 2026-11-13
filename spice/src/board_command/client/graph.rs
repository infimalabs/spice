//! `board graph VIEW` as a bounded terminal page, and complete graph and
//! handout snapshots only as exact file exports.
//!
//! The resident renders the whole diagram under the reader's visibility; this
//! module only decides how much of that one text reaches the terminal. A
//! partial page names the file export that carries the rest.

use serde::Serialize;
use wire::SpiceCommand;
use wire::intent::TextAnswer;
use wire::output::{Admission, Bounded, ClippedText, INTERACTIVE, Request};

use super::view::{ExportReceipt, export_file};
use super::*;

/// One `board graph` or `board handout` invocation's answer.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum GraphView {
    Page(Box<GraphPage>),
    Export(ExportReceipt),
}

impl GraphView {
    /// The text form of this answer.
    pub(crate) fn text(&self) -> Option<&str> {
        Some(match self {
            Self::Page(page) => &page.text,
            Self::Export(export) => export.text(),
        })
    }
}

/// The diagram's first lines in its deterministic order.
#[derive(Debug, Serialize)]
pub(crate) struct GraphPage {
    pub(crate) view: String,
    pub(crate) lines: Bounded<ClippedText>,
    #[serde(skip)]
    text: String,
}

/// Where one invocation's diagram goes.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::board_command) enum Destination {
    /// A bounded terminal page.
    Page(wire::output::Budget),
    /// The complete text, written to this file.
    File(String),
}

/// Execute one `board graph` or `board handout` invocation.
pub(crate) fn graph_view(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<GraphView> {
    graph_view_in(matches, contract, ambient, discover_repository)
}

/// Read the complete validated diagram for native resource paging.
pub(crate) fn graph_snapshot_at(
    repository: Host<'_>,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::BoardGraph,
        Vec::new(),
        &BoardGraph {
            view: required(matches, "VIEW"),
            all: matches.get_flag("all"),
            at: matches.get_one::<String>("at").cloned(),
        },
    )
}

fn graph_view_in<R: std::borrow::Borrow<Repository>>(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
    repository: impl FnOnce() -> Result<R, Outcome<CommandAnswer>>,
) -> Outcome<GraphView> {
    let at = matches.get_one::<String>("at").cloned();
    let handout = contract.path == ["board", "handout"];
    let all = !handout && matches.get_flag("all");
    let name = required(matches, if handout { "OUTPUT" } else { "VIEW" });
    let destination = if handout {
        handout_destination(&name)
    } else {
        graph_destination(&name, matches)
    };
    let destination = match destination {
        Ok(destination) => destination,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let repository = match repository() {
        Ok(repository) => repository,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let repository = Host {
        repository: repository.borrow(),
        ambient,
    };
    let reply = if handout {
        let body = BoardHandout {
            output: name.clone(),
            at,
        };
        crate::command_client::execute_typed(
            repository,
            contract,
            CommandKind::BoardHandout,
            Vec::new(),
            &body,
        )
    } else {
        graph_snapshot_at(repository, matches, contract)
    };
    let (position, text) = match reply {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Text(TextAnswer { text }),
        }) => (position, text),
        Outcome::Reply(_) => {
            return protocol_fault(
                "board::answer-invalid",
                "a graph read returned another answer shape",
            );
        }
        Outcome::Refusal(refusal) => return Outcome::Refusal(refusal),
        Outcome::Fault(fault) => return Outcome::Fault(fault),
    };
    let answer = match destination {
        Destination::File(output) => export_file(&output, text.as_bytes()).map(GraphView::Export),
        Destination::Page(budget) => {
            graph_page(&name, &text, budget, all).map(|page| GraphView::Page(Box::new(page)))
        }
    };
    match answer {
        Ok(body) => Outcome::Reply(Reply { position, body }),
        Err(outcome) => crate::typed_outcome(outcome),
    }
}

/// `--output FILE` writes the whole diagram and takes no limits; otherwise
/// the limits bound a terminal page.
pub(in crate::board_command) fn graph_destination(
    view: &str,
    matches: &ArgMatches,
) -> Result<Destination, Outcome<CommandAnswer>> {
    let number = |flag: &str| -> Result<Option<usize>, Outcome<CommandAnswer>> {
        matches
            .get_one::<String>(flag)
            .map(|written| {
                written.parse::<usize>().map_err(|_| {
                    refusal(
                        "board::limit-invalid",
                        Some(written.clone()),
                        format!("--{flag} takes a non-negative integer"),
                    )
                })
            })
            .transpose()
    };
    let request = Request {
        rows: number("limit")?,
        bytes: number("max-bytes")?,
        text: number("max-text")?,
    };
    page_or_file(
        view,
        request,
        matches.get_one::<String>("output").cloned(),
        matches.get_flag("all"),
    )
}

/// The destination for one parsed graph request.
pub(in crate::board_command) fn page_or_file(
    view: &str,
    request: Request,
    output: Option<String>,
    all: bool,
) -> Result<Destination, Outcome<CommandAnswer>> {
    match output {
        Some(output) if output == "-" => Err(stdout_refused(&graph_export(view, all))),
        Some(_) if request != Request::default() => Err(refusal(
            "board::export-bounded",
            None,
            "--output writes the complete diagram; drop --limit, --max-bytes, and --max-text",
        )),
        Some(output) => Ok(Destination::File(output)),
        None => INTERACTIVE
            .budget(request)
            .map(Destination::Page)
            .map_err(|error| refusal("board::limit-invalid", None, error.to_string())),
    }
}

/// A handout is a complete multi-view document, so it only ever writes a file.
pub(in crate::board_command) fn handout_destination(
    output: &str,
) -> Result<Destination, Outcome<CommandAnswer>> {
    if output == "-" {
        let command = SpiceCommand::new("board", ["handout", HANDOUT_FILE])
            .expect("board is a command token");
        return Err(stdout_refused(&command));
    }
    Ok(Destination::File(output.to_owned()))
}

/// The file a complete handout export names by default.
const HANDOUT_FILE: &str = "board-handout.md";

/// The command that writes this view and scope's complete diagram to a file.
fn graph_export(view: &str, all: bool) -> SpiceCommand {
    let mut arguments = vec!["graph".to_owned(), view.to_owned()];
    if all {
        arguments.push("--all".to_owned());
    }
    arguments.extend(["--output".to_owned(), format!("board-graph-{view}.txt")]);
    SpiceCommand::new("board", arguments).expect("board is a command token")
}

fn stdout_refused(command: &SpiceCommand) -> Outcome<CommandAnswer> {
    refusal(
        "board::export-stdout",
        Some("-".to_owned()),
        format!("a complete snapshot never streams to standard output; name a file: {command}"),
    )
}

/// Bound one rendered diagram's lines; a partial page continues as the file
/// export, since a diagram line has no stable cursor across reads.
pub(in crate::board_command) fn graph_page(
    view: &str,
    text: &str,
    budget: wire::output::Budget,
    all: bool,
) -> Result<GraphPage, Outcome<CommandAnswer>> {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    let lines = lines.collect::<Vec<_>>();
    let mut admission = Admission::new(budget);
    for (index, line) in lines.iter().enumerate() {
        if admission.is_closed() {
            admission.withhold(lines.len() - index);
            break;
        }
        admission
            .offer(budget.clip(line))
            .map_err(|error| protocol_fault("board::row-unencodable", error.to_string()))?;
    }
    let lines_page = admission.finish(|_| graph_export(view, all));
    let mut rendered = format!("{header}: {} lines", lines.len());
    for line in &lines_page.rows {
        rendered.push('\n');
        rendered.push_str(&line.to_string());
    }
    if let Some(omission) = &lines_page.omission {
        rendered.push('\n');
        rendered.push_str(&omission.to_string());
    }
    Ok(GraphPage {
        view: view.to_owned(),
        lines: lines_page,
        text: rendered,
    })
}

//! One-line help rows for options on first-run command paths.

use clap::Command;

/// Topic guidance shared by every command that names one.
const TOPIC_HELP: &str = "Complete /a/b public topic, complete UUID/a/b private topic, \
     or relative a/b tail matched and composed once with exact boundary overlap; a tail the \
     router cannot place keeps the work unplaced with that tail; omit to leave new work \
     unplaced through its first phase, placed by statement, origin, then path routing when \
     that phase closes; components after a canonical lowercase private owner are \
     [0-9a-z_]+(-[0-9a-z_]+)*; /oops is reserved for task oops";

/// One command path, long option name, and its help row.
const OPTIONS: &[(&str, &str, &str)] = &[
    ("", "json", "Answer in machine-readable JSON"),
    (
        "team name",
        "clear",
        "Release the configured name and use team.default",
    ),
    (
        "topic show",
        "after",
        "Continue history after this decimal journal position",
    ),
    (
        "topic describe",
        "apply",
        "Record the description with this exact preview digest",
    ),
    (
        "topic move",
        "apply",
        "Move the subtree with this exact preview digest; /oops is reserved for task oops",
    ),
    (
        "up",
        "foreground",
        "Run the resident attached to this terminal",
    ),
    ("up", "host", "Listen on this IP address"),
    ("up", "port", "Listen on this TCP port"),
    (
        "up",
        "allow-insecure-bind",
        "Confirm that a non-loopback network bind is intentional",
    ),
    (
        "up",
        "auth-token",
        "Require this bearer token from app clients",
    ),
    ("up", "open", "Open the app in the default browser"),
    (
        "up",
        "keeper",
        "Install or reconfigure this repository's native per-user lifecycle owner (Windows, Linux, and macOS in this release)",
    ),
    (
        "down",
        "keeper",
        "Remove the native lifecycle owner after draining its resident (Windows, Linux, and macOS in this release)",
    ),
    (
        "app",
        "foreground",
        "Attend the window from this terminal until it closes",
    ),
    (
        "app",
        "host",
        "Listen on this IP address when starting the resident",
    ),
    (
        "app",
        "port",
        "Listen on this TCP port when starting the resident",
    ),
    (
        "app",
        "allow-insecure-bind",
        "Confirm that a non-loopback network bind is intentional",
    ),
    (
        "app",
        "auth-token",
        "Require this bearer token from app clients, and present it from the window",
    ),
    (
        "install",
        "package",
        "Read the release package at this path",
    ),
    (
        "install",
        "apply",
        "Apply the preview with this exact digest",
    ),
    (
        "install",
        "unapply",
        "Reverse the installed receipt and remove Spice",
    ),
    ("sweep", "until", "Defer the filed Plan until this instant"),
    ("sweep", "apply", "File the preview with this exact digest"),
    (
        "sweep",
        "split",
        "With --apply, run this previewed slow Cargo test arm's targets once and attach target-qualified per-test times",
    ),
    ("lane add", "team", "Join this existing team ID"),
    ("lane add", "name", "Give the lane this display name"),
    (
        "lane add",
        "life",
        "Set the lane lifecycle to Steer, Drive, or Drain",
    ),
    ("lane add", "vendor", "Launch through this agent vendor"),
    ("lane add", "model", "Launch with this model"),
    ("lane add", "effort", "Launch with this reasoning effort"),
    (
        "lane model",
        "clear",
        "Remove the lane's override and launch with the vendor's model",
    ),
    (
        "lane effort",
        "clear",
        "Remove the lane's override and launch with the vendor's effort",
    ),
    (
        "task add",
        "pitch",
        "One plain sentence saying who can do what once this task lands and why that matters, not the current defect; at most 160 characters",
    ),
    (
        "task add",
        "description",
        "How the work is done: need, mechanism, invariants; the why belongs in --pitch",
    ),
    ("task add", "topic", TOPIC_HELP),
    (
        "task add",
        "intent",
        "Declare plan or design; omit for ordinary work",
    ),
    (
        "task review",
        "then",
        "Create one follow-up from title, topic, description, term, flow, and intent key=value fields separated by '|' or newlines",
    ),
    ("task capture", "topic", TOPIC_HELP),
    ("task modify", "topic", TOPIC_HELP),
    (
        "task oops",
        "topic",
        "Complete public or UUID-owned private topic, or relative router tail; omit for /oops; only this command may enter the /oops subtree; destinations outside it obey ordinary drive policy",
    ),
    (
        "task file",
        "into",
        "Complete public /a/b or private UUID/a/b topic; omit to use the saved topic or route from the statement; /oops is reserved for task oops",
    ),
    (
        "task capture",
        "pitch",
        "One plain sentence saying who can do what once this task lands and why that matters; derived from the title when absent",
    ),
    (
        "task modify",
        "title",
        "The result this task delivers, or a Design's question",
    ),
    (
        "task modify",
        "pitch",
        "One plain sentence saying who can do what once this task lands and why that matters, not the current defect; at most 160 characters",
    ),
    (
        "task modify",
        "description",
        "How the work is done: need, mechanism, invariants; the why belongs in --pitch",
    ),
    (
        "room contribute",
        "charge",
        wire::rooms::RoomCharge::VOTE_GUIDANCE,
    ),
    ("room contribute", "cite", crate::rooms::CITABLE_KEYS),
    (
        "room contribute",
        "question",
        "The handle shown before a Design question: a hex prefix of its item digest unique \
         among the current questions",
    ),
    (
        "tape check",
        "all",
        "Count every peer message verdict in one journal snapshot",
    ),
];

/// Attach every registered option row for `path`, refusing a row that names no
/// option on that exact command. The empty path is the root and owns globals.
pub(super) fn describe(mut command: Command, path: &[&str]) -> Result<Command, String> {
    for &(registered_path, option, help) in OPTIONS {
        if !same_path(registered_path, path) {
            continue;
        }
        if !command
            .get_arguments()
            .any(|argument| argument.get_long() == Some(option))
        {
            return Err(format!(
                "option help for {registered_path:?} names unregistered --{option}"
            ));
        }
        command = command.mut_arg(option, |argument| argument.help(help));
    }
    Ok(command)
}

fn same_path(registered: &str, path: &[&str]) -> bool {
    if registered.is_empty() {
        path.is_empty()
    } else {
        registered.split(' ').eq(path.iter().copied())
    }
}

//! One-line help rows for every registered command path.

/// The help row of a path only repository configuration mounts.
pub(super) const MOUNTED_SUMMARY: &str = "Run a command this repository's configuration mounts";

/// The one-line help row of each registered command path, keyed by its
/// space-separated words.
const SUMMARIES: &[(&str, &str)] = &[
    ("settings", "Read global settings"),
    ("settings fast", "Turn fast mode on or off for every lane"),
    (
        "team",
        "Read and configure team creation and allocation policy",
    ),
    (
        "team show",
        "Read a team's identity, name, switches, and members",
    ),
    (
        "team drive",
        "Enable or disable creation; off also disables drain",
    ),
    (
        "team drain",
        "Enable or disable allocation; on also enables drive",
    ),
    ("team fast", "Turn fast mode on or off for one team"),
    (
        "team name",
        "Bind a configured name or clear the current binding",
    ),
    (
        "topic",
        "Inspect, describe, rename, and move durable topic nodes",
    ),
    (
        "topic show",
        "Read one topic node and a page of durable routing history",
    ),
    (
        "topic describe",
        "Preview a node description; apply its digest to record it",
    ),
    (
        "topic move",
        "Operator-only preview of a rename or subtree move; apply its exact digest",
    ),
    (
        "task place",
        "Preview the router's pick; apply its digest to move the saved topic",
    ),
    ("app", "Open the console in a native window"),
    (
        "backup",
        "Write a backup bundle of the journal and task artifacts",
    ),
    ("board", "Read the task board, its graphs, and handouts"),
    ("board graph", "Render one board relationship graph view"),
    (
        "board handout",
        "Compose one deterministic board handout for an output",
    ),
    ("config", "Read or change repository configuration"),
    ("consent", "Review and decide pending consent requests"),
    ("consent allow", "Allow one pending consent request"),
    ("consent deny", "Deny one pending consent request"),
    ("consent policy", "Read the tracked consent policy"),
    (
        "demo",
        "Serve the installed appliance from an isolated demo repository",
    ),
    ("design", "Decide and read Design passes"),
    (
        "design decide",
        "Record the operator's decision on a completed Design pass",
    ),
    ("design show", "Page one task's Design pass authority"),
    (
        "dev",
        "Run repository Cargo and xtask commands with the admitted Cargo home",
    ),
    ("dev cargo", "Run Cargo with the admitted Cargo home"),
    (
        "dev cargo-footprint",
        "Report this worktree's Cargo output footprint and its bound",
    ),
    (
        "dev cargo-store",
        "Inspect the owned Cargo store, or reclaim expired state",
    ),
    (
        "dev xtask",
        "Run a repository xtask with the admitted Cargo home",
    ),
    ("doc", "Read a task's authored-document ledger"),
    (
        "doc ledger",
        "Emit one task's canonical authored-document ledger",
    ),
    ("doctor", "Report appliance and repository health"),
    ("down", "Drain the resident and release its lock"),
    (
        "gate",
        "Prove commit checks and run the Git hook entry points",
    ),
    (
        "gate camp",
        "Audit measurements camping at shared fixed bounds",
    ),
    (
        "gate commit-msg",
        "Check a commit message (Git commit-msg hook)",
    ),
    (
        "gate latches",
        "List latched bound debt with each latch's first-over-base origin",
    ),
    (
        "gate pre-commit",
        "Check the staged change (Git pre-commit hook)",
    ),
    ("gate prove", "Prove the staged change's gate actions"),
    (
        "gate reference-transaction",
        "Guard reference updates (Git reference-transaction hook)",
    ),
    (
        "gate selection",
        "Run this source repository's strict change selection",
    ),
    ("inbox", "Read pending steering for a lane"),
    (
        "install",
        "Preview, apply, or reverse this repository's installation",
    ),
    (
        "install symbols",
        "Verify installed debug symbols, or symbolicate a sample",
    ),
    ("journal", "Inspect, verify, or reindex the event journal"),
    (
        "journal reindex",
        "Rebuild the derived thread and room-authority indexes",
    ),
    (
        "journal verify",
        "Verify every journal authority plane in full",
    ),
    ("lane", "Add, wake, drain, and list agent lanes"),
    ("lane add", "Seat a new lane in a worktree"),
    (
        "lane drain",
        "End a lane's occupant gracefully, leaving its seat vacant",
    ),
    (
        "lane effort",
        "Read, set, or --clear the effort one lane launches with",
    ),
    ("lane fast", "Turn fast mode on or off for one lane"),
    (
        "lane renew",
        "Request or clear occupant succession for one lane",
    ),
    ("lane import", "Wake one vacant lane into another worktree"),
    (
        "lane list",
        "List lanes with their teams, claims, stable codes, and peer addresses",
    ),
    (
        "lane model",
        "Read, set, or --clear the model one lane launches with",
    ),
    (
        "lane purge",
        "Retire residue of unseated lanes and stale rack extractions",
    ),
    ("lane remove", "Remove one vacant, claim-free lane"),
    ("lane wake", "Launch a new occupant into a vacant lane seat"),
    ("lock", "Report resource locks, or run a command under one"),
    ("lock run", "Run a command while holding a named lock"),
    (
        "principal",
        "Admit principals and manage their profiles and roles",
    ),
    ("principal admit", "Admit a new principal"),
    (
        "principal deauthorize",
        "Retire the capability a principal's admission issued",
    ),
    ("principal list", "List principals"),
    ("principal profile", "Change a principal's name or email"),
    ("principal role", "Grant or revoke principal roles"),
    ("principal role grant", "Grant one role to a principal"),
    ("principal role revoke", "Revoke one role from a principal"),
    (
        "proof",
        "Seal or replay the compiled target's detached proof",
    ),
    (
        "proof replay",
        "Re-execute and verify the existing detached proof",
    ),
    (
        "quote",
        "Resolve a quote target to its recorded message and author",
    ),
    (
        "recall",
        "Read one lane's continuity account, or every lane's",
    ),
    (
        "release",
        "Write release notes, then seal and publish release packages",
    ),
    ("release notes", "Write release notes for a release commit"),
    ("release publish", "Preview or publish a sealed release"),
    (
        "release seal",
        "Compose the release manifest after proof and record it",
    ),
    ("resource", "Read spice:// resources from a shell"),
    (
        "resource read",
        "Print exactly what a lane's host frame carries for one spice:// URL",
    ),
    ("restore", "Verify or restore a journal backup bundle"),
    (
        "restore verify",
        "Verify a backup bundle without restoring it",
    ),
    ("retro", "Page preserved agent transcripts, newest first"),
    ("room", "Work in task rooms and inspect Design matching"),
    (
        "room ack",
        "Acknowledge one input, optionally continuing its line",
    ),
    (
        "room awareness",
        "Page a Design's awareness requests and maturity",
    ),
    (
        "room contribute",
        "Contribute one typed record to an open Design's room",
    ),
    ("room fin", "Acknowledge one input and close its line"),
    ("room history", "Page one task room's publications"),
    (
        "room inbox",
        "Read a lane's unread room activity, or a principal's pending inputs",
    ),
    ("room leave", "Stop listening to a room, with a clause"),
    ("room nak", "Decline one input, closing its line"),
    ("room post", "Publish one observation to a task's room"),
    (
        "room score",
        "Score one text against a lane's current Design profile",
    ),
    ("rtk", "Query an agent's RTK command history"),
    ("rtk gain", "Run the RTK gain query for one agent"),
    ("run", "Run one command through supervised-shell routing"),
    (
        "session",
        "Read agent sessions: briefings, turns, and transcripts",
    ),
    ("session briefing", "Print a session briefing"),
    (
        "session context",
        "Read the transcript around one recorded source position",
    ),
    (
        "session sweep",
        "Summarize each compaction window, newest first",
    ),
    ("session turns", "Page turns, newest first"),
    (
        "steer",
        "Send steering to a lane, or cancel, resend, or recover it",
    ),
    ("steer cancel", "Withdraw one live directive"),
    (
        "steer recover",
        "Restore one lapsed directive under its identity",
    ),
    ("steer resend", "Re-deliver one live directive"),
    (
        "sweep",
        "Preview latched gate debt as one Plan, or file it with --apply",
    ),
    ("task", "Create, claim, complete, review, and read tasks"),
    ("task add", "Create ordinary work, a Plan, or a Design"),
    (
        "task adopt",
        "Adopt existing pending work under a Design's control",
    ),
    (
        "task artifact",
        "Attach, list, show, remove, and prune task artifacts",
    ),
    ("task artifact add", "Attach one file to a task"),
    ("task artifact list", "Page one task's artifacts"),
    (
        "task artifact prune",
        "Preview or prune expired task artifacts",
    ),
    (
        "task artifact remove",
        "Remove one artifact from an unsettled task",
    ),
    ("task artifact show", "Read part of one task artifact"),
    (
        "task capture",
        "Claim a task, adopting loose commits and dirty work",
    ),
    ("task close", "Close a task with recorded validation"),
    (
        "task conclude",
        "Conclude an open, unheld Design done or collapsed",
    ),
    ("task defer", "Defer a task until a time"),
    ("task delete", "Withdraw a task with a recorded reason"),
    (
        "task depends",
        "Add or remove dependency edges between tasks",
    ),
    ("task done", "Complete the phase your claim holds"),
    (
        "task file",
        "File a Draft as ordinary work, optionally claiming it",
    ),
    ("task free", "Release your claim on a task"),
    (
        "task join",
        "Join a task with open capacity or a typed invitation",
    ),
    ("task list", "Page tasks"),
    ("task modify", "Edit an unsettled task's statement"),
    (
        "task next",
        "Claim the next allocated task, or report the one you hold",
    ),
    ("task note", "Append an observation to a task"),
    ("task oops", "File an appliance friction report as a Plan"),
    (
        "task require",
        "Set a task's trust, schema, appliance, and evidence requirements",
    ),
    ("task review", "Record a review verdict"),
    ("task show", "Read one task's statement, state, and history"),
    ("task status", "Summarize your claim and the board"),
    (
        "task target",
        "Set how many distinct lanes complete one phase",
    ),
    (
        "task wake",
        "Clear a task's deferral, optionally claiming it",
    ),
    ("tape", "Inspect and archive retained lane transcripts"),
    (
        "tape check",
        "Compare peer message bodies with their authored transcript",
    ),
    (
        "tape archive",
        "Inspect or seal terminal or floor-qualified quiescent tapes",
    ),
    ("taste", "Read tastes and file curation proposals"),
    ("taste add", "File a proposal to add a taste"),
    ("taste drop", "File a proposal to drop a taste"),
    ("thread", "Read the journal facts of one thread"),
    (
        "trust",
        "Grant, pend, or revoke trust in executable configuration",
    ),
    ("trust grant", "Approve one exact executable digest"),
    (
        "trust pend",
        "Quarantine this worktree's executable configuration digest",
    ),
    (
        "trust revoke",
        "Withdraw approval from one exact executable digest",
    ),
    ("up", "Start the resident and print its endpoint"),
    ("version", "Print the appliance and app versions"),
    ("watch", "Snapshot or follow the live transcript"),
];

/// The registered help row of `path`, absent for a path no row names.
pub(super) fn summary(path: &[String]) -> Option<&'static str> {
    SUMMARIES
        .iter()
        .find(|(key, _)| key.split(' ').eq(path.iter().map(String::as_str)))
        .map(|(_, summary)| *summary)
}

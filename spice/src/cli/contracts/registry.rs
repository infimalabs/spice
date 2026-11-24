use super::*;
/// Every current leaf. Parser registration and help are generated from these rows.
pub(super) const CONTRACTS: &[Contract] = &[
    local_direct!(
        ["app"],
        "app [--foreground] [--host IP] [--port PORT] [--allow-insecure-bind] [--auth-token TOKEN]",
        [Daemonless, Offline],
        Reply,
        "spice::app"
    ),
    local_direct!(
        ["demo"],
        "demo [--root DIR] [--reuse DIR] [--seed-only] [--preserve] [--open]",
        [Daemonless, Offline],
        Reply,
        "spice::demo"
    ),
    local_direct!(
        ["watch"],
        "watch [ROOT] [--follow] [--deadline-seconds SECONDS] [--after CURSOR] [--limit N] \
         [--max-bytes N] [--max-text N]",
        [Daemonless, Offline],
        Reply,
        "listen::watch"
    ),
    local_direct!(
        ["retro"],
        "retro [LANE|LOCUS|PATH] [--kind KIND...] [--limit N] [--max-bytes N] [--max-text N] \
         [--after CURSOR] [--export FILE]",
        [Daemonless, Offline],
        Reply,
        "listen::transcript_page"
    ),
    local_direct!(
        ["session", "briefing"],
        "session briefing [FILES...] [--capture BINDING] [--deadline-seconds SECONDS]",
        [Daemonless, Offline],
        Reply,
        "rim::session_briefing"
    ),
    local_direct!(
        ["session", "context"],
        "session context SESSION [--lane UUID] [--vendor NAME] [--stream NAME] [--source UUID] \
         [--member N] [--offset N] [--limit N] [--max-bytes N] [--max-text N] \
         [--after CURSOR] [--export FILE]",
        [Daemonless, Offline],
        Reply,
        "rim::session_context"
    ),
    local_direct!(
        ["session", "sweep"],
        "session sweep [FILES...] [--capture BINDING] [--limit N] [--max-bytes N] [--max-text N] \
         [--after CURSOR] [--export FILE] [--deadline-seconds SECONDS]",
        [Daemonless, Offline],
        Reply,
        "rim::session_sweep"
    ),
    row!(
        ["session", "turns"],
        "session turns [FILES...] [--capture BINDING] [--start TIME] [--end TIME] \
         [--contains TEXT] [--turn-id ID...] [--tool NAME...] [--view dialogue|full] [--limit N] \
         [--max-bytes N] [--max-text N] [--after CURSOR] [--export FILE]",
        Local,
        None,
        [],
        [Daemonless, Offline],
        Reply,
        "rim::session_turns"
    ),
    row!(
        ["doctor"],
        "doctor",
        Local,
        None,
        [],
        [Daemonless, Offline],
        DOCTOR_TOOLS,
        Reply,
        "spice::doctor"
    ),
    row!(
        ["version"],
        "version",
        Local,
        Inspect,
        [],
        [Daemonless, Offline],
        Reply,
        "spice::version"
    ),
    row!(
        ["resource", "read"],
        "resource read URL",
        Lane,
        None,
        [],
        [Daemonless],
        Reply,
        "spice::resource_read"
    ),
    row!(
        ["rtk", "gain"],
        "rtk gain AGENT_UUID",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        RTK_QUERY_TOOLS,
        Reply,
        "spice::rtk_gain"
    ),
    row!(
        ["install"],
        "install [--package PACKAGE] [--apply=<digest>|--unapply]",
        Local,
        None,
        [],
        [Daemonless, Offline, DigestOnMutation],
        Plan,
        "spice::install"
    ),
    row!(
        ["install", "symbols"],
        "install symbols [--sample FILE]",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        INSTALL_SYMBOLS_TOOLS,
        Reply,
        "spice::install_symbols"
    ),
    row!(
        ["dev", "cargo"],
        "dev cargo [--verbose] -- CARGO_ARGS...",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        NATIVE_CARGO_TOOL,
        Stream,
        "cargo_isolation::dev_cargo"
    ),
    row!(
        ["dev", "xtask"],
        "dev xtask [--verbose] -- XTASK_ARGS...",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        NATIVE_CARGO_TOOL,
        Stream,
        "cargo_isolation::dev_xtask"
    ),
    local_direct!(
        ["dev", "cargo-store"],
        "dev cargo-store [--cleanup]",
        [Daemonless, Offline],
        Reply,
        "cargo_isolation::store"
    ),
    local_direct!(
        ["dev", "cargo-footprint"],
        "dev cargo-footprint",
        [Daemonless, Offline],
        Reply,
        "cargo_output::footprint"
    ),
    row!(
        ["lock"],
        "lock",
        Local,
        None,
        [],
        [Daemonless, Offline],
        Reply,
        "sill::lock"
    ),
    row!(
        ["lock", "run"],
        "lock run NAME [--shard N] -- CMD",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        CALLER_COMMAND,
        Stream,
        "sill::lock_run"
    ),
    row!(
        ["restore", "verify"],
        "restore verify BUNDLE",
        Local,
        None,
        [],
        [Daemonless, Offline],
        Reply,
        "journal::restore_verify"
    ),
    row!(
        ["gate"],
        "gate [--at REV] [--verbose]",
        Local,
        Inspect,
        [],
        [Daemonless, Toolful],
        GATE_POLICY_TOOLS,
        Reply,
        "gate::run"
    ),
    row!(
        ["gate", "prove"],
        "gate prove [ACTION] [--missing] [--verbose]",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        GATE_POLICY_TOOLS,
        Reply,
        "gate::prove"
    ),
    row!(
        ["gate", "camp"],
        "gate camp [--verbose]",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        GATE_POLICY_TOOLS,
        Reply,
        "gate::camp"
    ),
    row!(
        ["gate", "latches"],
        "gate latches",
        Local,
        None,
        [],
        [Daemonless, Offline],
        Reply,
        "gate::latches"
    ),
    row!(
        ["gate", "commit-msg"],
        "gate commit-msg MESSAGE_FILE",
        Local,
        None,
        [],
        [Daemonless, Offline],
        Reply,
        "gate::commit-message"
    ),
    row!(
        ["gate", "pre-commit"],
        "gate pre-commit [--profile-output PATH]",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        GATE_POLICY_TOOLS,
        Reply,
        "gate::pre-commit"
    ),
    row!(
        ["gate", "selection"],
        "gate selection",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        GATE_POLICY_TOOLS,
        Reply,
        "gate::selection"
    ),
    reference_transaction::CONTRACT,
    row!(
        ["proof"],
        "proof [--target TARGET]",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        PROOF_TOOLS,
        Reply,
        "proof::seal"
    ),
    row!(
        ["proof", "replay"],
        "proof replay [--target TARGET]",
        Local,
        None,
        [],
        [Daemonless, Toolful],
        PROOF_TOOLS,
        Reply,
        "proof::replay"
    ),
    row!(
        ["release", "notes"],
        "release notes [--release-commit REV] [--output FILE]",
        Local,
        None,
        [],
        [Daemonless, Offline],
        Reply,
        "dist::release_notes"
    ),
    row!(
        ["release", "seal"],
        "release seal",
        PrincipalOrLane,
        Command,
        [ProofSealed],
        [Resident],
        Reply,
        "proof::manifest"
    ),
    row!(
        ["up"],
        "up [--foreground] [--host IP] [--port PORT] [--allow-insecure-bind] [--auth-token TOKEN] [--open] [--keeper]",
        Local,
        Resident,
        [
            JournalMigrated,
            TrustPended,
            UpSeated,
            ClaimRebound,
            RackAdmitted
        ],
        [JournalLock, Toolful],
        UP_TOOLS,
        Stream,
        "spice::up"
    ),
    operator!(
        ["down"],
        "down [--keeper]",
        Command,
        [UpDark],
        Reply,
        "spice::down"
    ),
    row!(
        ["board"],
        "board [FAMILY] [--limit N] [--max-bytes N] [--max-text N] [--after HANDLE] \
         [--output FILE]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "board::read"
    ),
    row!(
        ["board", "graph"],
        "board graph VIEW [--all] [--limit N] [--max-bytes N] [--max-text N] \
         [--output FILE] [--at BORN]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "board::graph"
    ),
    operator!(
        ["board", "handout"],
        "board handout OUTPUT [--at BORN]",
        Read,
        [],
        Reply,
        "board::handout"
    ),
    row!(
        ["room", "score"],
        "room score LANE TEXT",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "room::score"
    ),
    row!(
        ["room", "post"],
        "room post HANDLE TEXT",
        PrincipalOrLane,
        Command,
        [RoomPublished],
        [Resident],
        Reply,
        "room::post"
    ),
    row!(
        ["room", "history"],
        "room history HANDLE [--contributions] [--limit N] [--max-bytes N] [--after PUBLICATION] \
         [--export FILE]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "room::history"
    ),
    row!(
        ["room", "leave"],
        "room leave HANDLE [--clause TEXT]",
        Lane,
        Command => SnapshotOrCommitted,
        [RoomLeft],
        [Resident, LaneCapability],
        Reply,
        "room::leave"
    ),
    row!(
        ["room", "contribute"],
        "room contribute HANDLE [--charge=C|--ask] [--question HEX] [--cite KEY...] TEXT",
        Lane,
        Command => SnapshotOrCommitted,
        [RoomContributed],
        [Resident, LaneCapability],
        Reply,
        "room::contribute"
    ),
    row!(
        ["room", "inbox"],
        "room inbox [--limit N] [--max-bytes N] [--after ITEM]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "room::inbox"
    ),
    row!(
        ["room", "ack"],
        "room ack KEY [BODY]",
        Principal,
        Command,
        [SteerAcked, SteerLineTransition, SteerSent],
        [Resident, OperatorCapability],
        Reply,
        "steer::line-control"
    ),
    row!(
        ["room", "nak"],
        "room nak KEY [REASON]",
        Principal,
        Command,
        [SteerDeclined, SteerLineTransition, SteerSent],
        [Resident, OperatorCapability],
        Reply,
        "steer::line-control"
    ),
    row!(
        ["room", "fin"],
        "room fin KEY [BODY]",
        Principal,
        Command,
        [SteerAcked, SteerLineTransition, SteerSent],
        [Resident, OperatorCapability],
        Reply,
        "steer::line-control"
    ),
    row!(
        ["room", "awareness"],
        "room awareness HANDLE [--limit N] [--max-bytes N] [--after POSITION]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "room::awareness"
    ),
    row!(
        ["task", "authority"],
        "task authority HANDLE [--limit N] [--max-bytes N] [--after POSITION]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "design::show"
    ),
    operator!(
        ["task", "decide"],
        "task decide DECISION_JSON",
        Command,
        [PreparationDecided, ExecutionAuthorized, TaskConcluded],
        Reply,
        "preparation::decide"
    ),
    operator!(
        ["task", "adopt"],
        "task adopt OUTPUT PREPARATION",
        Command,
        [OutputControlled],
        Reply,
        "preparation::adopt"
    ),
    row!(
        ["task", "list"],
        "task list [--all] [--topic TOPIC] [--limit N] [--max-bytes N] \
         [--max-text N] [--after HANDLE] [--status pending|waiting|completed|deleted]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "board::list"
    ),
    row!(
        ["task", "status"],
        "task status",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "board::status"
    ),
    row!(
        ["task", "show"],
        "task show HANDLE [--full] [--context] [--history] [--limit N] [--max-bytes N] \
         [--max-text N] [--after POSITION]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "board::task_show"
    ),
    row!(
        ["task", "export"],
        "task export OUTPUT",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "board::task_export"
    ),
    row!(
        ["task", "import"],
        "task import INPUT [--origin ORIGIN]",
        PrincipalOrLane,
        Command => DirectOrCommitted,
        [TaskAdded, TaskPhased, TaskEdited],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_import"
    ),
    row!(
        ["task", "next"],
        "task next",
        PrincipalOrLane,
        Command,
        [ClaimWon, ClaimAcknowledged, ClaimRebound],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::next"
    ),
    row!(
        ["task", "add"],
        "task add TITLE [--pitch TEXT] [--topic TOPIC] [--description DESCRIPTION] \
         [--term TERM...] [--flow PHASES] [--depends HANDLE...] [--intent plan|design] [--preparation HANDLE] \
         [--requires-journal-schema SCHEMA] [--requires-trust-grant DIGEST] \
         [--resource RESOURCE...] [--origin ORIGIN] [--claim] [--draft]",
        PrincipalOrLane,
        Command,
        [TaskAdded, TaskPhased, TaskEdited, ClaimWon],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_add"
    ),
    row!(
        ["task", "capture"],
        "task capture [HANDLE] [--title TITLE] [--pitch TEXT] [--topic TOPIC] \
         [--origin ORIGIN] [--description DESCRIPTION] [--term TERM...] \
         [--steal] [--restore]",
        PrincipalOrLane,
        Command => DirectOrCommitted,
        [ClaimWon],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_capture"
    ),
    row!(
        ["task", "join"],
        "task join [SUBJECT] [--invite[=ADDR]]",
        Lane,
        Command => DirectOrCommitted,
        [CompletionTargeted, ClaimWon, SteerSent, SteerAccepted, SteerReplied, OutboxAccepted],
        [Resident, LaneCapability, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_join"
    ),
    row!(
        ["task", "done"],
        "task done [HANDLE] [--validation TEXT] [--operation OPERATION] \
         [--disposition KIND] [--cover REFERENCE]",
        Lane,
        Command => DirectOrCommitted,
        [TaskDone, TaskPhased],
        [Resident, LaneCapability, Toolful],
        TASK_DONE_TOOLS,
        Reply,
        "board::task_done"
    ),
    row!(
        ["task", "review"],
        "task review HANDLE [--finding TEXT] [--note TEXT] \
         [--then FOLLOWUP...] [--followup HANDLE...]",
        Lane,
        Command,
        [TaskReviewed, TaskAdded, TaskEdited],
        [Resident, LaneCapability, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_review"
    ),
    row!(
        ["task", "note"],
        "task note HANDLE TEXT",
        PrincipalOrLane,
        Command,
        [RoomPublished],
        [Resident],
        Reply,
        "board::task_note"
    ),
    row!(
        ["task", "defer"],
        "task defer HANDLE UNTIL",
        PrincipalOrLane,
        Command,
        [TaskDeferred, ClaimFreed],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_defer"
    ),
    row!(
        ["task", "wake"],
        "task wake HANDLE [--claim]",
        PrincipalOrLane,
        Command,
        [TaskWoken, ClaimWon],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_wake"
    ),
    row!(
        ["task", "target"],
        "task target HANDLE PHASE TARGET",
        Principal,
        Command,
        [CompletionTargeted],
        [Resident],
        Reply,
        "board::task_target"
    ),
    row!(
        ["task", "file"],
        "task file HANDLE [--into TOPIC] [--claim]",
        PrincipalOrLane,
        Command,
        [TaskPhased, TaskSubmitted, TaskEdited, ClaimWon],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_file"
    ),
    row!(
        ["task", "place"],
        "task place HANDLE [--apply DIGEST]",
        PrincipalOrLane,
        Command => SnapshotOrCommitted,
        [TaskEdited],
        [Resident, DigestOnMutation],
        Plan,
        "board::task_place"
    ),
    row!(
        ["task", "free"],
        "task free HANDLE",
        PrincipalOrLane,
        Command,
        [ClaimFreed],
        [Resident, Toolful],
        TASK_GIT_TOOLS,
        Reply,
        "board::task_free"
    ),
    row!(
        ["task", "oops"],
        "task oops TEXT [--origin ORIGIN] [--topic TOPIC]",
        PrincipalOrLane,
        Command,
        [TaskAdded],
        [Resident],
        Reply,
        "board::task_oops"
    ),
    row!(
        ["task", "depends"],
        "task depends HANDLE AFTER... [--remove]",
        PrincipalOrLane,
        Command,
        [TaskEdited],
        [Resident],
        Reply,
        "board::task_depends"
    ),
    row!(
        ["task", "modify"],
        "task modify HANDLE [--title TITLE] [--pitch TEXT] [--description DESCRIPTION] \
         [--term TERM...] [--replace-term HANDLE=TEXT...] \
         [--remove-term HANDLE...] [--priority PRIORITY] [--topic TOPIC] \
         [--intent INTENT] [--clear-intent] [--flow PHASES]",
        PrincipalOrLane,
        Command,
        [TaskEdited],
        [Resident],
        Reply,
        "board::task_modify"
    ),
    row!(
        ["task", "require"],
        "task require HANDLE [--journal-schema SCHEMA] [--trust-grant DIGEST] \
         [--appliance-after [VERSION]] [--resource RESOURCE...] [--clear]",
        PrincipalOrLane,
        Command,
        [TaskEdited],
        [Resident],
        Reply,
        "board::task_require"
    ),
    row!(
        ["task", "delete"],
        "task delete HANDLE REASON [--force-claimed]",
        PrincipalOrLane,
        Command,
        [TaskDeleted],
        [Resident],
        Reply,
        "board::task_delete"
    ),
    operator!(
        ["task", "close"],
        "task close HANDLE --validation TEXT [--force-claimed]",
        Command,
        [TaskClosed],
        Reply,
        "board::task_close"
    ),
    operator!(
        ["task", "conclude"],
        "task conclude HANDLE [--done] [--collapse REASON]",
        Command,
        [TaskConcluded],
        Reply,
        "board::task_conclude"
    ),
    row!(
        ["task", "artifact", "add"],
        "task artifact add HANDLE PATH [--name NAME] [--type TYPE] [--retention RETENTION]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "artifact::add"
    ),
    row!(
        ["task", "artifact", "list"],
        "task artifact list HANDLE [--limit N] [--max-bytes N] [--after ARTIFACT]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "artifact::list"
    ),
    row!(
        ["task", "artifact", "show"],
        "task artifact show HANDLE ARTIFACT [--offset N] [--bytes N] [--output PATH]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "artifact::show"
    ),
    row!(
        ["task", "artifact", "prune"],
        "task artifact prune [--older-than DURATION] [--apply]",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "artifact::prune"
    ),
    row!(
        ["task", "artifact", "remove"],
        "task artifact remove HANDLE ARTIFACT",
        PrincipalOrLane,
        Read,
        [],
        [Daemonless],
        Reply,
        "artifact::remove"
    ),
    row!(
        ["doc", "ledger"],
        "doc ledger HANDLE [--max-bytes N] [--offset N] [--digest DIGEST] [--output FILE]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "dialect::ledger"
    ),
    row!(
        ["sweep"],
        "sweep [--until INSTANT] [--apply=<digest>] [--split ARM...]",
        PrincipalOrLane,
        Command,
        [TaskAdded, TaskDeferred],
        [Resident, DigestOnMutation],
        Plan,
        "task::sweep"
    ),
    operator!(["settings"], "settings", Read, [], Reply, "settings::read"),
    operator!(
        ["settings", "fast"],
        "settings fast on|off",
        Command,
        [GlobalSettingsChanged],
        Reply,
        "settings::set"
    ),
    operator!(
        ["team", "show"],
        "team show TEAM",
        Read,
        [],
        Reply,
        "lane::read"
    ),
    operator!(
        ["team", "drive"],
        "team drive TEAM on|off",
        Command,
        [TeamConfigured],
        Reply,
        "team::configure"
    ),
    operator!(
        ["team", "drain"],
        "team drain TEAM on|off",
        Command,
        [TeamConfigured],
        Reply,
        "team::configure"
    ),
    operator!(
        ["team", "fast"],
        "team fast TEAM on|off",
        Command,
        [TeamConfigured],
        Reply,
        "team::configure"
    ),
    operator!(
        ["team", "name"],
        "team name TEAM [NAME] [--clear]",
        Command,
        [TeamConfigured],
        Reply,
        "team::configure"
    ),
    operator!(["lane"], "lane", Read, [], Reply, "lane::read"),
    row!(
        ["lane", "list"],
        "lane list [--limit N] [--max-bytes N] [--after CODE]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "lane::list"
    ),
    operator!(
        ["lane", "add"],
        "lane add [WORKTREE] [--team ID] [--name DISPLAY] \
         [--life Steer|Drive|Drain] [--vendor NAME] [--model MODEL] \
         [--effort low|medium|high|xhigh|max]",
        Command,
        [LaneSeated],
        Reply,
        "lane::add"
    ),
    operator!(
        ["lane", "drain"],
        "lane drain ID",
        Command,
        [
            LaneExited,
            LaneTimedOut,
            LaneCancelled,
            LaneSignalled,
            LaneDrained,
            LaneKilled,
            LaneDegraded
        ],
        Reply,
        "lane::drain"
    ),
    row!(
        ["lane", "wake"],
        "lane wake ID",
        Principal,
        Command,
        [LaneSessionDeclared, LaneStarted, ClaimCarried],
        [Resident, OperatorCapability, Toolful],
        OCCUPANT_LAUNCH_TOOLS,
        Reply,
        "lane::wake"
    ),
    operator!(
        ["lane", "model"],
        "lane model ID [MODEL] [--clear]",
        Command,
        [LaneRetuned, LaneRuntimeRetuned],
        Reply,
        "lane::model"
    ),
    operator!(
        ["lane", "effort"],
        "lane effort ID [EFFORT] [--clear]",
        Command,
        [LaneEffortRetuned, LaneRuntimeRetuned],
        Reply,
        "lane::effort"
    ),
    operator!(
        ["lane", "fast"],
        "lane fast ID on|off",
        Command,
        [LaneFastRetuned, LaneRuntimeRetuned],
        Reply,
        "lane::fast"
    ),
    operator!(
        ["lane", "renew"],
        "lane renew ID [--clear]",
        Command,
        [LaneRenewalChanged],
        Reply,
        "lane::renew"
    ),
    operator!(
        ["lane", "remove"],
        "lane remove ID",
        Command,
        [LaneRemoved, ConsentLapsed],
        Reply,
        "lane::remove"
    ),
    operator!(
        ["lane", "purge"],
        "lane purge",
        Read,
        [],
        Reply,
        "lane::purge"
    ),
    row!(
        ["lane", "import"],
        "lane import ID WORKTREE",
        Principal,
        Command,
        [LaneImported, LaneSessionDeclared, LaneStarted, ClaimCarried],
        [Resident, OperatorCapability, Toolful],
        OCCUPANT_LAUNCH_TOOLS,
        Reply,
        "lane::import"
    ),
    operator!(
        ["steer"],
        "steer LANE TEXT",
        Command,
        [SteerSent],
        Reply,
        "steer::send"
    ),
    row!(
        ["steer", "cancel"],
        "steer cancel ID",
        PrincipalOrLane,
        Command,
        [SteerCanceled],
        [Resident],
        Reply,
        "steer::cancel"
    ),
    operator!(
        ["steer", "resend"],
        "steer resend ID",
        Command => SnapshotOrCommitted,
        [SteerEscalated],
        Reply,
        "steer::resend"
    ),
    operator!(
        ["steer", "recover"],
        "steer recover ID",
        Command,
        [SteerRecovered],
        Reply,
        "steer::recover"
    ),
    row!(
        ["inbox"],
        "inbox [LANE] [--limit N] [--max-bytes N] [--max-text N] [--after ID]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "steer::inbox"
    ),
    row!(
        ["quote"],
        "quote TARGET",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "quote::read"
    ),
    operator!(
        ["topic", "show"],
        "topic show TOPIC [--after SEQ]",
        Read,
        [],
        Reply,
        "topic::read"
    ),
    operator!(
        ["topic", "preview"],
        "topic preview TITLE PITCH",
        Read,
        [],
        Reply,
        "topic::read"
    ),
    row!(
        ["topic", "move"],
        "topic move FROM TO [--apply DIGEST]",
        Principal,
        Command => SnapshotOrCommitted,
        [TopicMoved],
        [Resident, OperatorCapability, DigestOnMutation],
        Plan,
        "topic::edit"
    ),
    row!(
        ["topic", "describe"],
        "topic describe TOPIC DESCRIPTION [--apply DIGEST]",
        Principal,
        Command => SnapshotOrCommitted,
        [TopicDescribed],
        [Resident, OperatorCapability, DigestOnMutation],
        Plan,
        "topic::edit"
    ),
    row!(
        ["recall"],
        "recall [LANE]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "lane::recall"
    ),
    operator!(
        ["consent"],
        "consent [--limit N] [--max-bytes N] [--max-text N] [--after ID]",
        Read,
        [],
        Reply,
        "consent::read"
    ),
    operator!(
        ["consent", "allow"],
        "consent allow ID",
        Command,
        [ConsentGranted],
        Reply,
        "consent::allow"
    ),
    operator!(
        ["consent", "deny"],
        "consent deny ID",
        Command,
        [ConsentDenied],
        Reply,
        "consent::deny"
    ),
    operator!(
        ["consent", "policy"],
        "consent policy",
        Read,
        [],
        Reply,
        "consent::policy"
    ),
    operator!(
        ["taste"],
        "taste [--name NAME] [--limit N] [--max-bytes N] [--max-text N] [--after CURSOR]",
        Read,
        [],
        Reply,
        "taste::read"
    ),
    operator!(
        ["taste", "add"],
        "taste add TEXT [--origin ORIGIN]",
        Command,
        [TaskAdded],
        Reply,
        "taste::add"
    ),
    operator!(
        ["taste", "drop"],
        "taste drop NAME [--origin ORIGIN]",
        Command,
        [TaskAdded],
        Reply,
        "taste::drop"
    ),
    operator!(
        ["trust"],
        "trust [--limit N] [--max-bytes N] [--max-text N] [--after DIGEST]",
        Read,
        [],
        Reply,
        "spice::trust"
    ),
    operator!(
        ["trust", "show"],
        "trust show DIGEST [--max-bytes N] [--export FILE]",
        Read,
        [],
        Reply,
        "spice::trust_show"
    ),
    operator!(
        ["trust", "pend"],
        "trust pend",
        Command,
        [TrustPended],
        Reply,
        "spice::trust_pend"
    ),
    operator!(
        ["trust", "grant"],
        "trust grant DIGEST",
        Command,
        [TrustGranted],
        Reply,
        "spice::trust_grant"
    ),
    operator!(
        ["trust", "revoke"],
        "trust revoke DIGEST",
        Command,
        [TrustRevoked],
        Reply,
        "spice::trust_revoke"
    ),
    operator!(
        ["principal", "list"],
        "principal list",
        Read,
        [],
        Reply,
        "principal::read"
    ),
    operator!(
        ["principal", "admit"],
        "principal admit [--name NAME] [--email EMAIL] [--operator]",
        Command,
        [PrincipalAdmitted],
        Reply,
        "principal::admit"
    ),
    operator!(
        ["principal", "profile"],
        "principal profile PRINCIPAL [--name NAME] [--email EMAIL]",
        Command,
        [PrincipalProfiled],
        Reply,
        "principal::edit"
    ),
    operator!(
        ["principal", "role", "grant"],
        "principal role grant PRINCIPAL ROLE",
        Command,
        [PrincipalGranted],
        Reply,
        "principal::grant"
    ),
    operator!(
        ["principal", "role", "revoke"],
        "principal role revoke PRINCIPAL ROLE",
        Command,
        [PrincipalRevoked],
        Reply,
        "principal::revoke"
    ),
    operator!(
        ["principal", "deauthorize"],
        "principal deauthorize PRINCIPAL",
        Command,
        [PrincipalDeauthorized],
        Reply,
        "principal::deauthorize"
    ),
    row!(
        ["tape", "check"],
        "tape check [KEY] [--all]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "tape::check"
    ),
    row!(
        ["tape", "archive"],
        "tape archive [--apply]",
        Principal,
        None,
        [],
        [Daemonless],
        Reply,
        "tape::archive"
    ),
    row!(
        ["backup"],
        "backup OUTPUT",
        Principal,
        Recovery,
        [JournalBacked],
        [Offline, JournalLock],
        Reply,
        "journal::backup"
    ),
    row!(
        ["restore"],
        "restore BUNDLE [--apply=<digest>]",
        Principal,
        Recovery,
        [JournalRestored],
        [Offline, DigestOnMutation, JournalLock],
        Plan,
        "journal::restore"
    ),
    operator!(
        ["journal"],
        "journal [--limit N] [--max-bytes N] [--max-text N] [--after SEQ]",
        Read,
        [],
        Reply,
        "journal::read"
    ),
    row!(
        ["journal", "reindex"],
        "journal reindex",
        Principal,
        Recovery => Snapshot,
        [],
        [Offline, JournalLock],
        Reply,
        "journal::reindex"
    ),
    operator!(
        ["journal", "verify"],
        "journal verify",
        Read,
        [],
        Reply,
        "journal::verify"
    ),
    row!(
        ["thread"],
        "thread REF [--limit N] [--max-bytes N] [--max-text N] [--after SEQ]",
        PrincipalOrLane,
        Read,
        [],
        [Resident],
        Reply,
        "journal::thread"
    ),
    row!(
        ["config"],
        "config [KEY [VALUE]] [--apply=<digest>]",
        Local,
        None,
        [],
        [Daemonless, Offline, DigestOnMutation],
        Plan,
        "spice::config"
    ),
];

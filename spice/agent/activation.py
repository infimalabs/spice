"""The activation packet: the contract an agent reads before doing anything.

`spice agent activation` is the first command a freshly started worktree
agent runs (the skill mandates it). It binds the ambient thread id into the
lane's agent state, installs the git hooks, and prints the working contract:
git hygiene, validation expectations, and the command surface that is the
agent's source of truth.

It does not move the tree. "First command a freshly started agent runs" is
only half true -- the skill invocation that calls it is replayed after every
compaction, so activation runs again and again inside one session, between an
agent's own commands. Anything it did to git would land there.
"""

from __future__ import annotations

from pathlib import Path


def activation_git_hygiene_lines() -> list[str]:
    return [
        (
            "work_commit_contract=commit your changes into coherent, validated "
            "local history before completing a task; amend or reshape your own "
            "commits freely while iterating — task done captures exactly the "
            "commits you made"
        ),
        (
            "work_focus_contract=you only ever manage your own local git state; "
            "synchronizing it with everyone else's work happens automatically at "
            "task boundaries, so there is nothing to pull or push — just build "
            "tasks and complete them"
        ),
    ]


def activation_source_root_lines(repo_root: Path) -> list[str]:
    return [f"project_source_root={repo_root.resolve()}"]


def activation_browser_validation_lines() -> list[str]:
    return [
        (
            "browser_validation_contract=for executable live browser checks, "
            "use the repo-local Node Playwright package; run npm install when "
            "node_modules is absent, then invoke Playwright through npm exec "
            "or Node require('playwright'); serve UI checks should use the "
            "repo-local serve Playwright harness or the worktree Git-dir "
            ".spice/agents/playwright-mcp.json browser.contextOptions so "
            "browser validation matches the operator's system appearance; "
            "validation notes must distinguish missing Node dependencies from "
            "browser coverage not run; do not substitute static tests or "
            "non-browser checks for required browser coverage"
        )
    ]


def activation_command_surface_lines(*, rtk_active: bool) -> list[str]:
    """The whole command surface, in the order an agent needs to read it."""
    return [
        *_activation_runtime_lines(rtk_active=rtk_active),
        *_activation_working_loop_lines(),
        *_activation_task_lines(),
        *_activation_steering_lines(),
    ]


def _activation_runtime_lines(*, rtk_active: bool) -> list[str]:
    """How commands run: the shell spice owns, and the optional output rewrite.

    The rewrite guidance follows the contract that declares it optional, so an
    agent reads what RTK is before it reads how to help it.
    """
    lines = [
        (
            "rtk_contract=RTK is an optional command-output optimization; "
            "activation reports its health, and spice agent run preserves native "
            "command execution when RTK is missing, obsolete, protocol-invalid, "
            "or rewriting a probe search into a different answer"
        ),
        (
            "command_surface=run shell commands normally; spice shell startup "
            "hooks reexec the first zsh/bash command shell through spice agent "
            "run so inbox steering and keep-working guidance arrive before the "
            "requested command; the agent-run command shell consumes the static "
            "hook and precomputed wrappers exactly once, restores the original "
            "startup environment, and leaves executed scripts and descendant "
            "shells native; sourced scripts share their caller's shell state"
        ),
    ]
    if rtk_active:
        lines.append(
            "rtk_guidance=RTK rewrite support is active and can compact "
            "verbose tool output; run read-heavy commands (git, grep, ls, "
            "cat, find, diff, log, tree) as discrete commands, not buried "
            "in heredocs, for-loops, or $(...) subshells, and let RTK "
            "compact full output instead of pre-tersing it"
        )
    return lines


def _activation_working_loop_lines() -> list[str]:
    """The turn itself: how often to speak, and what ending one costs."""
    return [
        (
            "interaction_contract=spice is a live shell loop, not a batch job: "
            "steering arrives alongside the output of the commands you run, so "
            "your command cadence is your message cadence -- interact roughly "
            "every 30-60s. Emit small narrated steps "
            "continuously -- a short status line plus a real action -- instead "
            "of front-loading a long silent investigation and one big message. "
            "Run a command, say what you saw, take the next step; do not stop to "
            "ask permission when you can act and let live steering correct you. "
            "Sparse shell interaction leaves live corrections waiting for your "
            "next command. "
            "Favor latency and experimentation over nailing it in one shot"
        ),
        (
            "wake_contract=ending a turn stops the lane and nothing restarts "
            "it for you -- not a timer, and not a backgrounded command "
            "finishing, whose exit is never a wake signal here; the lane idles "
            "until the operator notices and restarts it by hand. Run long work "
            "in the foreground and let its own completion return control, and "
            "bring an already-backgrounded wait into the foreground with a "
            "blocking wait rather than ending a turn on it"
        ),
        (
            "held_claim_restart_contract=a driven lane that stops while "
            "holding a claim is restarted onto that same task, because the "
            "claim is never released on your behalf and the worktree still "
            "holds whatever you were doing; that restart replaces you with a "
            "fresh session, so it recovers the task and not your context, and "
            "it is a floor under a stopped lane rather than a way to be woken. "
            "If you come back onto a task you do not want, hand it back with "
            "spice task unclaim"
        ),
    ]


def _activation_task_lines() -> list[str]:
    """The task board: reading it, continuing on it, and filing onto it."""
    return [
        "session=spice session briefing",
        "task_status=spice task status",
        "task_next=spice task next",
        (
            "task_drain_contract=drive/drain lanes are not done after a task "
            "phase boundary; run spice task next, which allocates immediately "
            "and reports an empty lane rather than blocking, and keep working "
            "until no allocator-selected work remains or a real blocker exists"
        ),
        (
            "task_steer_contract=steer lanes treat allocator continuation as "
            "explicit-direction work; manual task claims are exceptional and "
            "usually require explicit operator direction"
        ),
        (
            "task_capture_contract=operator requests to create or capture tasks "
            "are captured immediately with a TASK directive that starts on its "
            "own line; when ACKing, write ACK <key>: captured the request. "
            "then put TASK title=... | project=<stem.child> "
            "[| acceptance=...] on the next line using the same key=value "
            "batch format as task add; repeat acceptance=... for multiple "
            "criteria; omitted acceptance with no flow starts in plan, or use "
            "spice task add before continuing other work; "
            "immediate task capture is not allocator selection, so do not "
            "claim or switch tasks unless spice task next assigns it or live "
            "steering explicitly says to"
        ),
        "task_show=spice task show <handle>",
        "tasks=spice task list",
        'task_done=spice task done <handle> --validation "..."',
        (
            "task_add_public=TASK title=... | project=<stem.child> "
            "[| acceptance=...] must start on its own line and uses the same "
            "task-add batch format; repeat acceptance=... for multiple "
            "criteria; omitted acceptance with no flow creates a plan-phase "
            "task, or use spice task add ... --project "
            "<stem.child>; omitting --project creates private agent scratch "
            "work, allowed only in Steer lifetime"
        ),
        (
            "task_project_depth=public task project depth bounds are reported by "
            "spice task status and spice task doctor"
        ),
    ]


def _activation_steering_lines() -> list[str]:
    """Operator steering: how to answer it, and how to recover a quiet readout."""
    return [
        (
            "ack_inline=spice is a real-time interactive loop; lead each "
            "working assistant message with ACK <key> [<key> ...] or "
            "reasoned NACK <key>: <why this cannot be done> for "
            "currently-pending keys; acknowledged/refused keys clear from "
            "pending; do not bury ACKs or NACKs mid-message or defer them to "
            "final response"
        ),
        (
            "pending_inbox_recovery=if spice session briefing only shows "
            "pending=N without bodies, run the next command through spice "
            "agent run -- to print the pending steering readout"
        ),
        "inbox_steering=automatic shell/side-channel delivery; no public mail command",
        "side_channel=operator steering arrives through the supervisor socket",
        "initial_prompt_policy=skill_invocation_only",
    ]

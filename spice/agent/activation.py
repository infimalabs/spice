"""The activation packet: the contract an agent reads before doing anything.

`spice agent activation` is the first command a freshly started worktree
agent runs (the skill mandates it). It binds the ambient thread id into the
lane's agent state, installs the git hooks, refreshes the baseline when safe,
and prints the working contract: git hygiene, validation expectations, and
the command surface that is the agent's source of truth.
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
            "repo-local serve Playwright harness or the shared "
            ".spice/agent/playwright-mcp.json browser.contextOptions so "
            "browser validation matches the operator's system appearance; "
            "validation notes must distinguish missing Node dependencies from "
            "browser coverage not run; do not substitute static tests or "
            "non-browser checks for required browser coverage"
        )
    ]


def activation_command_surface_lines() -> list[str]:
    return [
        (
            "command_surface=run shell commands normally; spice shell startup "
            "hooks reexec the first zsh/bash command shell through spice agent "
            "run so inbox steering and keep-working guidance inject before the "
            "requested command; descendant shells use static hooks and "
            "precomputed wrappers without another reexec; agent launch clears "
            "inherited reexec markers before the first takeover, then "
            "SPICE_SHELL_HOOK_REEXEC_STAGE=1 is expected inside the taken-over "
            "shell"
        ),
        (
            "rtk_rewrite_contract=the native harness or shell startup hook must "
            "hand the complete top-level shell command string to spice agent run "
            "exactly once; agent run is the RTK rewrite owner because it is the "
            "only layer that sees the full shell string before execution"
        ),
        "session=spice session briefing",
        "task_status=spice task status",
        "task_next=spice task next",
        (
            "task_drain_contract=drive/drain lanes are not done after a task "
            "phase boundary; run spice task next and keep working until no "
            "allocator-selected work remains or a real blocker exists"
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
            "then put TASK title=... | project=<stem.child> | acceptance=... "
            "on the next line using the same key=value batch format as task "
            "add, or use spice task add before continuing other work; "
            "immediate task capture is not allocator selection, so do not "
            "claim or switch tasks unless spice task next assigns it or live "
            "steering explicitly says to"
        ),
        "task_show=spice task show <handle>",
        "tasks=spice task list",
        'task_done=spice task done <handle> --validation "..."',
        (
            "task_add_public=TASK title=... | project=<stem.child> | "
            "acceptance=... must start on its own line and uses the same "
            "task-add batch format, or use spice task add ... --project "
            "<stem.child>; omitting --project creates private agent scratch "
            "work"
        ),
        (
            "task_project_depth=public task project depth bounds are reported by "
            "spice task status and spice task doctor"
        ),
        (
            "ack_inline=spice is a real-time interactive loop; lead each "
            "working assistant message with ACK <key> [<key> ...] for "
            "currently-pending keys; acknowledged keys clear from pending; "
            "do not bury ACKs mid-message or defer them to final response"
        ),
        (
            "pending_inbox_recovery=if spice session briefing only shows "
            "pending=N without bodies, run the next command through spice "
            "agent run -- to print the pending steering readout"
        ),
        "inbox_steering=automatic shell/side-channel injection; no public mail command",
        "side_channel=operator steering arrives through the supervisor socket",
        "initial_prompt_policy=skill_invocation_only",
    ]

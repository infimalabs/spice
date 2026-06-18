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
            "or Node require('playwright'); validation notes must distinguish "
            "missing Node dependencies from browser coverage not run; do not "
            "substitute static tests or non-browser checks for required "
            "browser coverage"
        )
    ]


def activation_command_surface_lines() -> list[str]:
    return [
        (
            "command_surface=run shell commands normally; spice shell startup "
            "hooks reexec the first zsh/bash command shell through spice agent "
            "run so inbox steering and keep-working guidance inject before the "
            "requested command; descendant shells use static hooks and "
            "precomputed wrappers without another reexec"
        ),
        "session=spice session",
        "task_status=spice task status",
        "task_next=spice task next",
        (
            "task_drain_contract=YOU ARE NOT DONE after a task phase boundary; "
            "run spice task next and keep working until no allocator-selected "
            "work remains or a real blocker exists"
        ),
        "task_show=spice task show <handle>",
        "tasks=spice task list",
        'task_done=spice task done <handle> --validation "..."',
        (
            "task_add_public=spice task add ... --project <stem>; omitting "
            "--project creates private agent scratch work"
        ),
        (
            "ack_inline=ACK pending inbox keys in any assistant message as "
            "soon as understood; do not wait for final response"
        ),
        (
            "pending_inbox_recovery=if spice session only shows pending=N "
            "without bodies, run the next command through spice agent run -- "
            "to print the pending steering readout"
        ),
        "inbox_steering=automatic shell/side-channel injection; no public mail command",
        "side_channel=operator steering arrives through the supervisor socket",
        "initial_prompt_policy=skill_invocation_only",
    ]

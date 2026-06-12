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
            "start with the Playwright MCP server named playwright; if no live "
            "browser path is available, report that browser coverage did not "
            "run and restore Playwright MCP before continuing; do not "
            "substitute static tests or non-browser checks for required "
            "browser coverage"
        )
    ]


def activation_command_surface_lines() -> list[str]:
    return [
        "session=spice session",
        "task_status=spice task status",
        "task_next=spice task next",
        "task_show=spice task show <handle>",
        "tasks=spice task list",
        'task_done=spice task done <handle> --validation "..."',
        "inbox_steering=automatic wrapper/side-channel injection; no public mail command",
        "side_channel=operator steering arrives through the supervisor socket",
        "initial_prompt_policy=skill_invocation_only",
    ]

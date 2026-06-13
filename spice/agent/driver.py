"""The agent-tool driver seam: everything specific to the driving CLI.

spice supervises an agent CLI (the "driver") without caring which one beyond
this module. A driver knows: its binary and launch argv, the env var carrying
the ambient thread id, where its transcripts (rollouts) live and how to map a
thread id to one, the stdout section markers its `exec` mode prints (for the
watchdog scanner), and how to read the session id from startup output.

The built-in driver is OpenAI Codex. Adding another driver is writing one new
`AgentDriver` value, not adding a flag.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import subprocess
import sys
from contextlib import closing
from dataclasses import dataclass
from pathlib import Path

from spice.paths import atomic_write_json, state_dir


@dataclass(frozen=True)
class AgentDriver:
    name: str
    default_bin: str
    bin_env: str
    thread_id_env: str
    default_model: str
    default_reasoning_effort: str
    default_service_tier: str
    # `exec` stdout structure: the marker line opening an assistant message
    # block and the marker lines that terminate one.
    stdout_assistant_marker: str
    stdout_section_markers: frozenset[str]
    stdout_compaction_marker: str
    session_id_pattern: re.Pattern[str]

    @property
    def state_dirname(self) -> str:
        return self.name

    def binary(self) -> str:
        return os.environ.get(self.bin_env, self.default_bin)

    def home(self) -> Path:
        raise NotImplementedError

    def thread_transcript_path(
        self, thread_id: str, *, must_exist: bool = True
    ) -> Path:
        raise NotImplementedError

    def build_exec_command(self, **kwargs: object) -> list[str]:
        raise NotImplementedError


PLAYWRIGHT_MCP_SERVER_NAME = "playwright"
PLAYWRIGHT_MCP_COMMAND = "npx"
PLAYWRIGHT_MCP_ARGS = ("--yes", "@playwright/mcp@latest", "--headless")

ROLLOUT_THREAD_ID_RE = re.compile(
    r"("
    r"[0-9a-f]{32}|"
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
    r")\.jsonl$",
    re.IGNORECASE,
)


class CodexDriver(AgentDriver):
    def home(self) -> Path:
        return Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))

    def state_db_path(self) -> Path:
        return self.home() / "state_5.sqlite"

    def sessions_root(self) -> Path:
        return self.home() / "sessions"

    def thread_transcript_path(
        self, thread_id: str, *, must_exist: bool = True
    ) -> Path:
        from spice.agent.identity import canonical_thread_id

        canonical = canonical_thread_id(thread_id)
        state_db_path = self.state_db_path()
        error = SystemExit(f"Missing {self.name} state database: {state_db_path}")
        if state_db_path.exists():
            with closing(sqlite3.connect(state_db_path)) as conn:
                row = conn.execute(
                    "SELECT rollout_path FROM threads "
                    "WHERE replace(lower(id), '-', '') = ?",
                    (canonical,),
                ).fetchone()
            if row is not None and row[0]:
                rollout_path = Path(row[0]).expanduser()
                if rollout_path.exists():
                    return rollout_path.resolve()
                if not must_exist:
                    return rollout_path.absolute()
                error = SystemExit(f"Thread transcript not found: {rollout_path}")
            else:
                error = SystemExit(
                    f"Thread id not found in {self.name} state: {canonical}"
                )
        if found := self.find_session_transcript(canonical):
            return found
        raise error

    def find_session_transcript(self, thread_id: str) -> Path | None:
        from spice.agent.identity import canonical_thread_id

        sessions_root = self.sessions_root()
        if not thread_id or not sessions_root.exists():
            return None
        canonical = canonical_thread_id(thread_id)
        matches = sorted(
            path
            for path in sessions_root.rglob("rollout-*.jsonl")
            if _rollout_filename_thread_id(path.name) == canonical and path.is_file()
        )
        return matches[-1].resolve() if matches else None

    def build_exec_command(
        self,
        *,
        repo_root: Path,
        prompt: str,
        thread_id: str = "",
        model: str = "",
        reasoning_effort: str = "",
        personality: str = "",
        service_tier: str = "",
        binary: str = "",
        fast_mode: bool = False,
    ) -> list[str]:
        config_overrides = [
            f'model_reasoning_effort="{reasoning_effort or self.default_reasoning_effort}"',
            *playwright_mcp_config_overrides(repo_root),
        ]
        if personality:
            config_overrides.append(f'personality="{personality}"')
        if fast_mode and service_tier:
            config_overrides.append(f'service_tier="{service_tier}"')
        command = [
            binary or self.binary(),
            "exec",
            "--cd",
            str(repo_root),
            "--model",
            model or self.default_model,
        ]
        for override in config_overrides:
            command.extend(["--config", override])
        command.extend(
            [
                "--enable" if fast_mode else "--disable",
                "fast_mode",
                "--sandbox",
                "danger-full-access",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
            ]
        )
        if thread_id:
            return [*command, "resume", thread_id, prompt]
        return [*command, prompt]


def playwright_mcp_config_overrides(repo_root: Path) -> list[str]:
    return [
        (
            f"mcp_servers.{PLAYWRIGHT_MCP_SERVER_NAME}.command="
            f"{json.dumps(PLAYWRIGHT_MCP_COMMAND)}"
        ),
        (
            f"mcp_servers.{PLAYWRIGHT_MCP_SERVER_NAME}.args="
            f"{json.dumps(playwright_mcp_args(repo_root), separators=(',', ':'))}"
        ),
    ]


def playwright_mcp_args(repo_root: Path) -> list[str]:
    args = list(PLAYWRIGHT_MCP_ARGS)
    config_path = write_playwright_mcp_config(repo_root)
    if config_path is not None:
        args.extend(["--config", str(config_path)])
    return args


def write_playwright_mcp_config(repo_root: Path) -> Path | None:
    color_scheme = operator_color_scheme()
    if color_scheme is None:
        return None
    return atomic_write_json(
        state_dir(repo_root) / "agent" / "playwright-mcp.json",
        {"browser": {"contextOptions": {"colorScheme": color_scheme}}},
    )


def operator_color_scheme() -> str | None:
    if sys.platform != "darwin":
        return None
    try:
        result = subprocess.run(
            ["defaults", "read", "-g", "AppleInterfaceStyle"],
            capture_output=True,
            check=False,
            text=True,
        )
    except OSError:
        return None
    return "dark" if result.stdout.strip().lower() == "dark" else "light"


def _rollout_filename_thread_id(name: str) -> str:
    from spice.agent.identity import canonical_thread_id

    match = ROLLOUT_THREAD_ID_RE.search(name)
    return canonical_thread_id(match.group(1)) if match else ""


DRIVER: AgentDriver = CodexDriver(
    name="codex",
    default_bin="codex",
    bin_env="SPICE_AGENT_BIN",  # env-policy: allow
    thread_id_env="CODEX_THREAD_ID",  # env-policy: allow
    default_model="gpt-5.4",
    default_reasoning_effort="low",
    default_service_tier="fast",
    stdout_assistant_marker="codex",
    stdout_section_markers=frozenset(
        {"context compacted", "exec", "tokens used", "user"}
    ),
    stdout_compaction_marker="context compacted",
    session_id_pattern=re.compile(r"^session id:\s*(\S+)\s*$", re.MULTILINE),
)

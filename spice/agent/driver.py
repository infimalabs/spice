"""The agent-tool driver seam: everything specific to the driving CLI.

spice supervises an agent CLI (the "driver") without caring which one beyond
this module. A driver knows: its binary and launch argv, the env var carrying
the ambient thread id, where its transcripts (rollouts) live and how to map a
thread id to one, the stdout section markers its `exec` mode prints (for the
watchdog scanner), how to read the session id from startup output, and how to
phrase the neutral skill-invocation launch prompt.

Two drivers ship: OpenAI Codex (the default) and Anthropic Claude Code.
Current-process commands resolve `DRIVER` once from their own environment and
cwd; lane consumers must resolve with `driver_for(repo_root)`, which checks env,
configured driver, then the unbound-worktree Codex default. Transcript
consumers resolve with `driver_for_transcript(path)`. Adding a third driver is
writing one more `AgentDriver` value, not adding broad mode branches to
consumers.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import subprocess
import sys
import uuid
from contextlib import closing
from dataclasses import dataclass
from pathlib import Path
from typing import Any

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
    default_context_window: int = 0
    out_of_credits_patterns: tuple[re.Pattern[str], ...] = ()
    # How the supervisor reassembles assistant messages from `exec` stdout:
    # "marker" reads the driver's plain-text section markers; "json" parses
    # one JSON event per line (a stream-json transcript echoed to stdout).
    stdout_format: str = "marker"

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

    def owns_transcript(self, path: Path) -> bool:
        """True iff `path` sits in this driver's transcript layout."""
        return False

    def build_exec_command(self, **kwargs: object) -> list[str]:
        raise NotImplementedError

    def skill_invocation_prompt(self, skill_path: Path) -> str:
        """The neutral launch prompt: a bare skill invocation, no operator ask.

        The prompt boundary is sacred — operator prose never rides the start
        prompt. The phrasing is a driver concern because each agent CLI invokes
        a skill differently; the default is the Codex `[$name](path)` link form.
        """
        return f"[$spice]({skill_path})"

    def normalize_transcript_line(self, raw: dict[str, Any]) -> dict[str, Any] | None:
        """Map a raw transcript line into the canonical event shape.

        Every transcript consumer — the serve message stream, the forensic
        turn folder, the ACK/maxim extractor — speaks one vocabulary:
        `{"type": "response_item"|"event_msg"|"compacted", "timestamp", "payload"}`
        with a Codex-shaped payload. The built-in transcript already *is* that
        shape, so the default normalizer is identity; a driver whose CLI writes
        a different schema translates it here, once, for every consumer.
        """
        return raw

    def context_snapshot_fields(self, raw: dict[str, Any]) -> dict[str, Any] | None:
        """Per-turn token usage for the context meter, or None for other lines.

        Returns the `ActiveContextSnapshot` field bag (every key but
        `source_file`/`ts`). The built-in driver reads Codex `token_count`
        events; a driver whose CLI reports usage on each assistant message
        overrides this. None means "this line carries no usage snapshot."
        """
        payload = raw.get("payload") or {}
        if payload.get("type") != "token_count":
            return None
        info = payload.get("info") or {}
        last = info.get("last_token_usage") or {}
        total = _as_int(last.get("total_tokens"), None)
        if total is None:
            return None
        cumulative = info.get("total_token_usage") or {}
        return {
            "input_tokens": _as_int(last.get("input_tokens")),
            "cached_input_tokens": _as_int(last.get("cached_input_tokens")),
            "output_tokens": _as_int(last.get("output_tokens")),
            "reasoning_output_tokens": _as_int(last.get("reasoning_output_tokens")),
            "total_tokens": total,
            "model_context_window": _as_int(info.get("model_context_window"), None),
            "cumulative_total_tokens": _as_int(cumulative.get("total_tokens")),
        }

    def process_failure_kind(self, *, exit_code: int, output: str) -> str:
        del exit_code
        return (
            "out-of-credits"
            if any(
                pattern.search(output or "") for pattern in self.out_of_credits_patterns
            )
            else ""
        )


PLAYWRIGHT_MCP_SERVER_NAME = "playwright"
PLAYWRIGHT_MCP_COMMAND = "npx"
PLAYWRIGHT_MCP_ARGS = ("--yes", "@playwright/mcp@latest", "--headless")


def _as_int(value: Any, default: int | None = 0) -> int | None:
    if isinstance(value, bool):
        return default
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, str) and value.isdigit():
        return int(value)
    return default


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

    def owns_transcript(self, path: Path) -> bool:
        return path.name.startswith("rollout-") or self.sessions_root() in path.parents

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


# Claude Code's `--effort` vocabulary. The configured spice effort value is
# Codex-shaped; Claude uses the same set, except for `max`, which we ignore.
CLAUDE_EFFORT_CHOICES = frozenset({"low", "medium", "high", "xhigh"})
OUT_OF_CREDITS_PATTERNS = (
    re.compile(r"\busage limit\b", re.IGNORECASE),
    re.compile(r"\b(?:out of|insufficient)\s+credits?\b", re.IGNORECASE),
    re.compile(
        r"\bcredit balance\b.*\b(?:low|exhausted|insufficient)\b", re.IGNORECASE
    ),
)


def claude_effort(value: str) -> str:
    effort = (value or "").strip().lower()
    return effort if effort in CLAUDE_EFFORT_CHOICES else ""


def dashed_uuid(value: str) -> str:
    """Render a thread id into the dashed UUID form Claude names files with.

    Codex canonicalizes thread ids to dashless lowercase; Claude's transcript
    filenames and `--resume` want the dashed UUID, so the seam re-dashes on the
    way back out. Input that is not a UUID (a non-Claude id) passes through.
    """
    try:
        return str(uuid.UUID(hex=(value or "").strip()))
    except ValueError:
        return value


class ClaudeDriver(AgentDriver):
    """Anthropic Claude Code: headless `claude --print`, file-based sessions.

    Claude has no rollout state database — every session is a single JSONL at
    `<config>/projects/<cwd-slug>/<session-uuid>.jsonl`. Session ids are
    globally unique UUIDs, so a thread id locates its transcript by a glob
    across project dirs without needing the originating cwd. Startup runs in
    `--output-format stream-json`, whose first emitted line is a `system`
    `init` event carrying the `session_id` the supervisor records.
    """

    def home(self) -> Path:
        # env-policy: allow
        return Path(os.environ.get("CLAUDE_CONFIG_DIR", str(Path.home() / ".claude")))

    def projects_root(self) -> Path:
        return self.home() / "projects"

    def owns_transcript(self, path: Path) -> bool:
        return self.projects_root() in path.parents

    def thread_transcript_path(
        self, thread_id: str, *, must_exist: bool = True
    ) -> Path:
        from spice.agent.identity import canonical_thread_id

        canonical = canonical_thread_id(thread_id)
        found = self.find_session_transcript(canonical)
        if found is not None:
            return found
        if not must_exist:
            return (self.projects_root() / f"{dashed_uuid(canonical)}.jsonl").absolute()
        raise SystemExit(f"Thread id not found in {self.name} sessions: {canonical}")

    def find_session_transcript(self, thread_id: str) -> Path | None:
        from spice.agent.identity import canonical_thread_id

        projects_root = self.projects_root()
        canonical = canonical_thread_id(thread_id)
        if not canonical or not projects_root.exists():
            return None
        dashed = dashed_uuid(canonical)
        matches = sorted(
            path for path in projects_root.glob(f"*/{dashed}.jsonl") if path.is_file()
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
        command = [
            binary or self.binary(),
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            # Stream partial message chunks so the supervisor sees assistant
            # text in real time instead of one event flushed at turn end —
            # otherwise steering injection and ACK archival lag by tens of
            # seconds. The scanner ignores the partial stream_event lines and
            # still processes the complete assistant event.
            "--include-partial-messages",
            "--model",
            model or self.default_model,
            "--permission-mode",
            "bypassPermissions",
            "--mcp-config",
            claude_mcp_config_json(repo_root),
        ]
        effort = claude_effort(reasoning_effort or self.default_reasoning_effort)
        if effort:
            command.extend(["--effort", effort])
        if thread_id:
            command.extend(["--resume", dashed_uuid(thread_id)])
        command.append(prompt)
        return command

    def normalize_transcript_line(self, raw: dict[str, Any]) -> dict[str, Any] | None:
        rtype = raw.get("type")
        timestamp = raw.get("timestamp")
        message = raw.get("message")
        if rtype == "assistant" and isinstance(message, dict):
            return _claude_assistant_event(timestamp, message)
        if rtype == "user" and isinstance(message, dict):
            return _claude_user_event(timestamp, message, raw.get("promptId"))
        if _claude_is_compaction(raw):
            return {"type": "compacted", "timestamp": timestamp, "payload": {}}
        return None

    def context_snapshot_fields(self, raw: dict[str, Any]) -> dict[str, Any] | None:
        if raw.get("type") != "assistant":
            return None
        message = raw.get("message")
        usage = message.get("usage") if isinstance(message, dict) else None
        if not isinstance(usage, dict):
            return None
        input_tokens = _as_int(usage.get("input_tokens"))
        cache_read = _as_int(usage.get("cache_read_input_tokens"))
        cache_creation = _as_int(usage.get("cache_creation_input_tokens"))
        output_tokens = _as_int(usage.get("output_tokens"))
        # Active-context occupancy is the whole prompt that was resent this turn
        # (fresh + cached input) plus the tokens generated into it.
        total = input_tokens + cache_read + cache_creation + output_tokens
        if total <= 0:
            return None
        return {
            "input_tokens": input_tokens,
            "cached_input_tokens": cache_read + cache_creation,
            "output_tokens": output_tokens,
            "reasoning_output_tokens": 0,
            "total_tokens": total,
            # Always meter against the standard tier so context pressure builds
            # and the agent compacts near 200K — matching other agents and not
            # drifting up to a larger (1M) API context.
            "model_context_window": self.default_context_window or None,
            "cumulative_total_tokens": total,
        }


def _claude_response_item(timestamp: Any, payload: dict[str, Any]) -> dict[str, Any]:
    return {"type": "response_item", "timestamp": timestamp, "payload": payload}


def _claude_content_blocks(message: dict[str, Any]) -> list[dict[str, Any]]:
    content = message.get("content")
    if not isinstance(content, list):
        return []
    return [block for block in content if isinstance(block, dict)]


def _claude_text_content(message: dict[str, Any]) -> str:
    texts = [
        text
        for block in _claude_content_blocks(message)
        if block.get("type") == "text"
        for text in [block.get("text")]
        if isinstance(text, str) and text.strip()
    ]
    return "\n\n".join(texts).strip()


def _claude_assistant_event(
    timestamp: Any, message: dict[str, Any]
) -> dict[str, Any] | None:
    text = _claude_text_content(message)
    if text:
        payload: dict[str, Any] = {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
        }
        if message.get("stop_reason") == "end_turn":
            payload["phase"] = "final_answer"
        return _claude_response_item(timestamp, payload)
    thinking_block: dict[str, Any] | None = None
    for block in _claude_content_blocks(message):
        block_type = block.get("type")
        if block_type == "thinking":
            thinking_block = thinking_block or block
            continue
        if block_type == "tool_use":
            return _claude_response_item(timestamp, _claude_tool_call_payload(block))
        if block_type == "image":
            item = _claude_image_item(block)
            if item is not None:
                return _claude_response_item(
                    timestamp,
                    {"type": "message", "role": "assistant", "content": [item]},
                )
    if thinking_block is not None:
        summary = thinking_block.get("thinking")
        text = summary if isinstance(summary, str) else ""
        return _claude_response_item(
            timestamp,
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": text}]},
        )
    return None


def _claude_image_item(block: dict[str, Any]) -> dict[str, Any] | None:
    """Canonical `image_url` item from a Claude image block, or None.

    Claude stores `{source:{type:"base64",media_type,data}}` (or a `url`
    source); the canonical item carries a `data:`/http URL the existing image
    extraction already understands.
    """
    source = block.get("source")
    if not isinstance(source, dict):
        return None
    if source.get("type") == "url":
        url = source.get("url")
        return {"type": "image", "image_url": {"url": str(url)}} if url else None
    media_type = source.get("media_type")
    data = source.get("data")
    if not isinstance(media_type, str) or not isinstance(data, str):
        return None
    return {"type": "image", "image_url": {"url": f"data:{media_type};base64,{data}"}}


def _claude_tool_result_images(content: Any) -> list[dict[str, Any]]:
    if not isinstance(content, list):
        return []
    items: list[dict[str, Any]] = []
    for block in content:
        if isinstance(block, dict) and block.get("type") == "image":
            item = _claude_image_item(block)
            if item is not None:
                items.append(item)
    return items


def _claude_tool_call_payload(block: dict[str, Any]) -> dict[str, Any]:
    name = str(block.get("name") or "tool")
    raw_input = block.get("input")
    arguments = raw_input if isinstance(raw_input, dict) else {}
    if name == "TodoWrite":
        return {
            "type": "function_call",
            "name": "update_plan",
            "arguments": json.dumps({"plan": _claude_plan_steps(arguments)}),
        }
    return {
        "type": "function_call",
        "name": name,
        "arguments": json.dumps(arguments),
    }


def _claude_plan_steps(arguments: dict[str, Any]) -> list[dict[str, str]]:
    todos = arguments.get("todos")
    if not isinstance(todos, list):
        return []
    steps: list[dict[str, str]] = []
    for todo in todos:
        if isinstance(todo, dict):
            steps.append(
                {
                    "step": str(todo.get("content") or todo.get("activeForm") or ""),
                    "status": str(todo.get("status") or ""),
                }
            )
    return steps


def _claude_user_event(
    timestamp: Any, message: dict[str, Any], prompt_id: Any = None
) -> dict[str, Any] | None:
    content = message.get("content")
    if isinstance(content, str):
        if not content.strip():
            return None
        payload: dict[str, Any] = {
            "type": "message",
            "role": "user",
            "content": [{"type": "text", "text": content}],
        }
        # A real user prompt carries Claude's per-turn id; tool-result `user`
        # lines below do not, so turn boundaries land on actual prompts.
        if isinstance(prompt_id, str) and prompt_id:
            payload["prompt_id"] = prompt_id
        return _claude_response_item(timestamp, payload)
    if isinstance(content, list):
        block = next((item for item in content if isinstance(item, dict)), None)
        if block is not None and block.get("type") == "tool_result":
            return _claude_response_item(
                timestamp,
                {
                    "type": "function_call_output",
                    "output": _claude_tool_result_images(block.get("content")),
                },
            )
    return None


def _claude_is_compaction(raw: dict[str, Any]) -> bool:
    if raw.get("type") == "summary":
        return True
    return raw.get("type") == "system" and raw.get("subtype") == "compact_boundary"


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


def claude_mcp_config_json(repo_root: Path) -> str:
    """The Claude `--mcp-config` payload registering the Playwright MCP server.

    Claude Code takes MCP servers as a JSON document (an `mcpServers` map)
    rather than Codex's dotted `--config` overrides. This mirrors the same
    server name, command, and args so both drivers expose an identical
    `playwright` server for the activation browser-validation contract.
    """
    config = {
        "mcpServers": {
            PLAYWRIGHT_MCP_SERVER_NAME: {
                "command": PLAYWRIGHT_MCP_COMMAND,
                "args": playwright_mcp_args(repo_root),
            }
        }
    }
    return json.dumps(config, separators=(",", ":"))


def playwright_mcp_args(repo_root: Path) -> list[str]:
    args = list(PLAYWRIGHT_MCP_ARGS)
    config_path = write_playwright_mcp_config(repo_root)
    args.extend(["--config", str(config_path)])
    return args


def write_playwright_mcp_config(repo_root: Path) -> Path:
    color_scheme = operator_color_scheme()
    return atomic_write_json(
        state_dir(repo_root) / "agent" / "playwright-mcp.json",
        {"browser": {"contextOptions": {"colorScheme": color_scheme}}},
    )


def operator_color_scheme() -> str:
    if sys.platform != "darwin":
        return "light"
    try:
        result = subprocess.run(
            ["defaults", "read", "-g", "AppleInterfaceStyle"],
            capture_output=True,
            check=False,
            text=True,
        )
    except OSError:
        return "light"
    return "dark" if result.stdout.strip().lower() == "dark" else "light"


def _rollout_filename_thread_id(name: str) -> str:
    from spice.agent.identity import canonical_thread_id

    match = ROLLOUT_THREAD_ID_RE.search(name)
    return canonical_thread_id(match.group(1)) if match else ""


CODEX_DRIVER: AgentDriver = CodexDriver(
    name="codex",
    default_bin="codex",
    bin_env="SPICE_AGENT_BIN",  # env-policy: allow
    thread_id_env="CODEX_THREAD_ID",  # env-policy: allow
    default_model="gpt-5.5",
    default_reasoning_effort="xhigh",
    default_service_tier="fast",
    stdout_assistant_marker="codex",
    stdout_section_markers=frozenset(
        {"context compacted", "exec", "tokens used", "user"}
    ),
    stdout_compaction_marker="context compacted",
    session_id_pattern=re.compile(r"^session id:\s*(\S+)\s*$", re.MULTILINE),
    out_of_credits_patterns=OUT_OF_CREDITS_PATTERNS,
)

# Claude's `stream-json` stdout is one JSON event per line, so the watchdog
# parses it (`stdout_format="json"`) rather than scanning plain-text markers —
# assistant prose still reaches ACK archiving and maxim judging in real time.
# The session id is read from the `system`/`init` line's `"session_id":
# "<uuid>"`, the first match in the startup log head.
CLAUDE_DRIVER: AgentDriver = ClaudeDriver(
    name="claude",
    default_bin="claude",
    bin_env="SPICE_AGENT_BIN",  # env-policy: allow
    thread_id_env="CLAUDE_CODE_SESSION_ID",  # env-policy: allow
    default_model="claude-opus-4-8",
    default_reasoning_effort="high",
    default_service_tier="",
    stdout_assistant_marker="",
    stdout_section_markers=frozenset(),
    stdout_compaction_marker="",
    session_id_pattern=re.compile(r'"session_id"\s*:\s*"([0-9a-fA-F-]{36})"'),
    default_context_window=200000,
    out_of_credits_patterns=OUT_OF_CREDITS_PATTERNS,
    stdout_format="json",
)

SPICE_AGENT_DRIVER_ENV = "SPICE_AGENT_DRIVER"  # env-policy: allow
_DRIVERS: dict[str, AgentDriver] = {
    CODEX_DRIVER.name: CODEX_DRIVER,
    CLAUDE_DRIVER.name: CLAUDE_DRIVER,
}


ALL_DRIVERS: tuple[AgentDriver, ...] = (CODEX_DRIVER, CLAUDE_DRIVER)


def select_driver(name: str = "") -> AgentDriver:
    """Resolve a driver by explicit name, then env, then the cwd's config.

    This is the process-global `DRIVER` resolver. Per-worktree resolution (what
    the server uses for each lane) is `driver_for(repo_root)` — the driver is a
    per-worktree setting, never the server process's own location.
    """
    chosen = (name or os.environ.get(SPICE_AGENT_DRIVER_ENV, "")).strip().lower()
    if not chosen and not name:
        chosen = _configured_driver_name(None)
    return _driver_named(chosen, source="current process")


def driver_for(repo_root: Path | None) -> AgentDriver:
    """The driver bound to a specific worktree.

    Resolution: `SPICE_AGENT_DRIVER` (a deliberate command-level override),
    then *that worktree's* configured driver, then Codex for an unbound
    worktree. The server discovers worktrees from the repo and calls this per
    target.repo_root, so one repo can run a different driver in every worktree
    regardless of where — or how — the server itself was launched.
    """
    name = os.environ.get(SPICE_AGENT_DRIVER_ENV, "").strip().lower()
    source = SPICE_AGENT_DRIVER_ENV
    if not name:
        name = _configured_driver_name(repo_root)
        source = f"{repo_root or Path.cwd()} agent config"
    return _driver_named(name, source=source)


def driver_for_transcript(path: Path) -> AgentDriver:
    """The driver whose transcript layout owns `path` (Codex or Claude)."""
    for driver in ALL_DRIVERS:
        if driver.owns_transcript(path):
            return driver
    return CODEX_DRIVER


def _configured_driver_name(repo_root: Path | None) -> str:
    from spice.config import configured_agent_driver

    return (configured_agent_driver(repo_root) or "").strip().lower()


def _driver_named(name: str, *, source: str) -> AgentDriver:
    if not name:
        return CODEX_DRIVER
    try:
        return _DRIVERS[name]
    except KeyError as exc:
        expected = ", ".join(sorted(_DRIVERS))
        raise RuntimeError(
            f"unknown agent driver {name!r} from {source}; expected one of: {expected}"
        ) from exc


DRIVER: AgentDriver = select_driver()

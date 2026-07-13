"""Inbox and supervisor-notice injection for the agent-run command surface."""

from __future__ import annotations

import json
import os
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any, TextIO

from spice.agent.paths import agent_state_dir
from spice.agent.sidechannelnotify import consume_side_channel_notices
from spice.paths import STATE_DIRNAME

AGENT_RUN_INBOX_REPEAT_SECONDS = 15.0
InboxSignature = tuple[tuple[str, int, int], ...]
TimeFactory = Callable[[], float]


def post_tool_hook_inbox_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "post-tool-hook-inbox.json"


class AgentInboxInjector:
    """Re-display pending inbox steering on stderr until it is acknowledged."""

    def __init__(
        self,
        repo_root: Path | None,
        *,
        stderr: TextIO,
        repeat_interval_seconds: float = AGENT_RUN_INBOX_REPEAT_SECONDS,
        time_factory: TimeFactory = time.monotonic,
        state_path: Path | None = None,
    ) -> None:
        self.repo_root = repo_root
        self.stderr = stderr
        self.repeat_interval_seconds = max(0.0, repeat_interval_seconds)
        self.time_factory = time_factory
        self.state_path = state_path
        self.displayed_at_by_key: dict[str, float] = {}
        self.displayed_signature_by_key: dict[str, tuple[int, int]] = {}
        self.signature: InboxSignature | None = None
        self._load_display_state()

    def _load_display_state(self) -> None:
        if self.state_path is None:
            return
        (
            self.displayed_at_by_key,
            self.displayed_signature_by_key,
            self.signature,
        ) = read_inbox_display_state(self.state_path)

    def inject(
        self, *, force: bool, emit_suppressed_summary: bool = True
    ) -> InboxSignature:
        from spice.mail import inbox

        snapshot = inbox.collect_inbox_snapshot(self.repo_root)
        signature = snapshot.signature
        now = self.time_factory()
        suppressed_keys = self._suppressed_keys(signature, now=now)
        pending_keys = {
            inbox_key for inbox_key, _row_signature in _signature_rows(signature)
        }
        previous_pending_keys = {
            inbox_key
            for inbox_key, _row_signature in _signature_rows(self.signature or ())
        }
        new_pending_keys = pending_keys - previous_pending_keys
        if (
            not force
            and not new_pending_keys
            and signature == self.signature
            and pending_keys <= suppressed_keys
        ):
            if emit_suppressed_summary:
                self._emit_pending_summary(len(pending_keys))
            return signature
        try:
            from spice.mail.readout import print_inbox_readout

            displayed_keys = print_inbox_readout(
                self.repo_root,
                quiet=True,
                displayed_keys=suppressed_keys,
                file=self.stderr,
                items=snapshot.items,
            )
        except Exception as exc:  # pragma: no cover - conflicted recovery path
            self.stderr.write(f"Inbox Steering\n  unavailable={exc}\n")
            self.stderr.flush()
            displayed_keys = []
        self.stderr.flush()
        rendered_keys = set(displayed_keys)
        rendered_signature = tuple(
            row for row in signature if _inbox_item_key(row[0]) in rendered_keys
        )
        self.signature = signature
        self._record_displayed_keys(signature, displayed_keys, now=now)
        self._prune_display_state(pending_keys)
        self._persist_display_state()
        return rendered_signature

    def prime_displayed_signature(self, signature: InboxSignature) -> None:
        """Seed suppression with inbox rows rendered before stream registration."""
        self.signature = signature
        displayed_keys = [key for key, _row_signature in _signature_rows(signature)]
        self._record_displayed_keys(signature, displayed_keys, now=self.time_factory())

    def _emit_pending_summary(self, count: int) -> None:
        if count <= 0:
            return
        from spice.mail.steeringkey import steering_token

        token = steering_token(self.repo_root)
        header = f"Inbox Steering  <{token}>" if token else "Inbox Steering"
        footer = f"\n  </{token}>" if token else ""
        self.stderr.write(
            f"{header}\n  pending={count} "
            "(recently shown; full readout on repeat or run "
            f"`spice session briefing`){footer}\n"
        )
        self.stderr.flush()

    def _suppressed_keys(self, signature: InboxSignature, *, now: float) -> set[str]:
        suppressed: set[str] = set()
        for key, row_signature in _signature_rows(signature):
            if self.displayed_signature_by_key.get(key) != row_signature:
                continue
            last_displayed_at = self.displayed_at_by_key.get(key)
            if last_displayed_at is None:
                continue
            age = now - last_displayed_at
            if 0 <= age < self.repeat_interval_seconds:
                suppressed.add(key)
        return suppressed

    def _record_displayed_keys(
        self, signature: InboxSignature, displayed_keys: list[str], *, now: float
    ) -> None:
        signature_by_key = dict(_signature_rows(signature))
        for key in displayed_keys:
            row_signature = signature_by_key.get(key)
            if row_signature is None:
                continue
            self.displayed_at_by_key[key] = now
            self.displayed_signature_by_key[key] = row_signature

    def _prune_display_state(self, pending_keys: set[str]) -> None:
        for key in list(self.displayed_at_by_key):
            if key not in pending_keys:
                self.displayed_at_by_key.pop(key, None)
                self.displayed_signature_by_key.pop(key, None)

    def _persist_display_state(self) -> None:
        if self.state_path is None:
            return
        write_inbox_display_state(
            self.state_path,
            displayed_at_by_key=self.displayed_at_by_key,
            displayed_signature_by_key=self.displayed_signature_by_key,
            signature=self.signature or (),
        )


class AgentSideChannelNoticeInjector:
    """Write one-shot supervisor feedback to the same stderr side-channel."""

    def __init__(self, repo_root: Path | None, *, stderr: TextIO) -> None:
        self.repo_root = repo_root
        self.stderr = stderr

    def inject(self, *, force: bool) -> None:
        del force
        notices = consume_side_channel_notices(self.repo_root)
        if not notices:
            return
        self.stderr.write("Supervisor Feedback\n")
        for notice in notices:
            for line in notice.splitlines():
                self.stderr.write(f"  {line}\n")
        self.stderr.flush()


def inbox_pending_signature(repo_root: Path | None) -> InboxSignature:
    if repo_root is None:
        return ()
    directory = Path(repo_root) / STATE_DIRNAME / "inbox"
    rows: list[tuple[str, int, int]] = []
    try:
        with os.scandir(directory) as entries:
            for entry in entries:
                try:
                    if not entry.is_file() or not entry.name.endswith(".txt"):
                        continue
                    stat_result = entry.stat()
                except OSError:
                    continue
                rows.append((entry.name, stat_result.st_mtime_ns, stat_result.st_size))
    except OSError:
        return ()
    return tuple(sorted(rows))


def read_inbox_display_state(
    path: Path,
) -> tuple[dict[str, float], dict[str, tuple[int, int]], InboxSignature | None]:
    payload = _read_json_payload(path)
    raw_displayed_at = payload.get("displayedAtByKey")
    displayed_at_by_key: dict[str, float] = {}
    if isinstance(raw_displayed_at, dict):
        for key, value in raw_displayed_at.items():
            displayed_at = _float_payload_value(value)
            if isinstance(key, str) and displayed_at is not None:
                displayed_at_by_key[key] = displayed_at

    raw_displayed_signature = payload.get("displayedSignatureByKey")
    displayed_signature_by_key: dict[str, tuple[int, int]] = {}
    if isinstance(raw_displayed_signature, dict):
        for key, value in raw_displayed_signature.items():
            row_signature = _inbox_row_signature_payload(value)
            if isinstance(key, str) and row_signature is not None:
                displayed_signature_by_key[key] = row_signature

    signature = _inbox_signature_payload(payload.get("signature"))
    return displayed_at_by_key, displayed_signature_by_key, signature


def write_inbox_display_state(
    path: Path,
    *,
    displayed_at_by_key: dict[str, float],
    displayed_signature_by_key: dict[str, tuple[int, int]],
    signature: InboxSignature,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps(
            {
                "displayedAtByKey": displayed_at_by_key,
                "displayedSignatureByKey": {
                    key: list(row_signature)
                    for key, row_signature in displayed_signature_by_key.items()
                },
                "signature": [
                    [name, mtime_ns, size] for name, mtime_ns, size in signature
                ],
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def inbox_signature_from_payload(value: Any) -> InboxSignature | None:
    return _inbox_signature_payload(value)


def _inbox_signature_payload(value: Any) -> InboxSignature | None:
    if not isinstance(value, list):
        return None
    rows: list[tuple[str, int, int]] = []
    for row in value:
        if not isinstance(row, list) or len(row) != 3:
            return None
        name, raw_mtime_ns, raw_size = row
        mtime_ns = _int_payload_value(raw_mtime_ns)
        size = _int_payload_value(raw_size)
        if not isinstance(name, str) or mtime_ns is None or size is None:
            return None
        rows.append((name, mtime_ns, size))
    return tuple(sorted(rows))


def _inbox_row_signature_payload(value: Any) -> tuple[int, int] | None:
    if not isinstance(value, list) or len(value) != 2:
        return None
    mtime_ns = _int_payload_value(value[0])
    size = _int_payload_value(value[1])
    if mtime_ns is None or size is None:
        return None
    return (mtime_ns, size)


def _signature_rows(signature: InboxSignature) -> list[tuple[str, tuple[int, int]]]:
    return [
        (_inbox_item_key(name), (mtime_ns, size)) for name, mtime_ns, size in signature
    ]


def _inbox_item_key(name: str) -> str:
    path = Path(name)
    return path.stem or path.name


def _read_json_payload(path: Path) -> dict[str, Any]:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


def _float_payload_value(value: Any) -> float | None:
    return float(value) if isinstance(value, int | float) else None


def _int_payload_value(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None

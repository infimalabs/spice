"""Fresh-checkout continuation for HEAD-moving task phase boundaries."""

from __future__ import annotations

import base64
import importlib
import json
import os
import shlex
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import TYPE_CHECKING, Any

from spice.errors import SpiceError
from spice.process.tool import run_parent_lifetime_command

if TYPE_CHECKING:
    from spice.tasks.create import TaskAddPreparation

PHASE_CONTINUATION_ENV = "SPICE_PHASE_CONTINUATION"  # env-policy: allow
PHASE_CONTINUATION_PROTOCOL = 1
PHASE_CONTINUATION_MAX_BYTES = 1024 * 1024


def continue_after_integration(
    operation: str,
    payload: Mapping[str, Any],
    *,
    repo_root: Path,
    before_head: str,
    after_head: str,
    environment: Mapping[str, str] | None = None,
) -> str:
    """Run one phase continuation under the checkout that integration selected."""
    request = {
        "protocol": PHASE_CONTINUATION_PROTOCOL,
        "module": "spice.tasks.ops",
        "function": "_continue_phase",
        "payload": {"operation": operation, "input": dict(payload)},
        "environment": dict(environment or {}),
    }
    if not before_head or before_head == after_head:
        return _dispatch(request)
    return _run_fresh_checkout(
        request,
        repo_root=repo_root,
        landing_head=after_head,
        operation=operation,
    )


def _run_fresh_checkout(
    request: Mapping[str, Any],
    *,
    repo_root: Path,
    landing_head: str,
    operation: str,
) -> str:
    if os.environ.get(PHASE_CONTINUATION_ENV):  # env-policy: allow
        raise SpiceError(
            "refusing nested task phase continuation; the integration remains "
            f"authoritative at {landing_head}; run `spice task status`"
        )
    serialized = json.dumps(request, separators=(",", ":"), sort_keys=True)
    env = dict(os.environ)  # env-policy: allow
    env[PHASE_CONTINUATION_ENV] = landing_head
    command = [sys.executable, "-B", "-m", "spice.tasks.phasecontinuation"]
    try:
        result = run_parent_lifetime_command(
            command,
            cwd=repo_root,
            env=env,
            capture_output=True,
            text=True,
            input_data=serialized,
            check=False,
        )
    except OSError as exc:
        raise _fresh_continuation_error(
            serialized=serialized,
            command=command,
            landing_head=landing_head,
            operation=operation,
            outcome=f"could not start: {exc}",
        ) from exc
    if result.returncode == 0:
        return str(result.stdout).removesuffix("\n")
    detail = "\n".join(
        text.strip()
        for text in (str(result.stdout or ""), str(result.stderr or ""))
        if text.strip()
    )
    raise _fresh_continuation_error(
        serialized=serialized,
        command=command,
        landing_head=landing_head,
        operation=operation,
        outcome=f"exited {result.returncode}",
        detail=detail,
    )


def _fresh_continuation_error(
    *,
    serialized: str,
    command: list[str],
    landing_head: str,
    operation: str,
    outcome: str,
    detail: str = "",
) -> SpiceError:
    """Describe one failed post-integration continuation without republishing."""
    token = base64.urlsafe_b64encode(serialized.encode()).decode()
    recovery = shlex.join([*command, "--payload", token])
    message = (
        f"task {operation} integration landed at {landing_head}, but its fresh "
        f"checkout continuation {outcome}; the landing is "
        f"authoritative and will not be rolled back or re-published. Run "
        f"`spice task status`, then resume the exact continuation with `{recovery}`"
    )
    if detail:
        message += f":\n{detail}"
    return SpiceError(message)


def _dispatch(request: Mapping[str, Any], *, apply_environment: bool = False) -> str:
    protocol = int(request.get("protocol") or 0)
    if protocol != PHASE_CONTINUATION_PROTOCOL:
        raise SpiceError(
            f"unsupported task phase continuation protocol {protocol}; "
            f"expected {PHASE_CONTINUATION_PROTOCOL}"
        )
    module_name = str(request.get("module") or "")
    function_name = str(request.get("function") or "")
    payload = request.get("payload")
    environment = request.get("environment") or {}
    if (
        not module_name
        or not function_name
        or not isinstance(payload, dict)
        or not isinstance(environment, dict)
    ):
        raise SpiceError("invalid task phase continuation payload")
    if apply_environment:
        for name, value in environment.items():
            if not isinstance(name, str) or not isinstance(value, str):
                raise SpiceError("task phase continuation environment must be text")
            os.environ[name] = value  # env-policy: allow - exact parent contract
    if module_name == "spice.tasks.ops" and function_name == "_continue_phase":
        from spice.tasks import ops

        function = ops._continue_phase
    else:
        function = getattr(importlib.import_module(module_name), function_name, None)
    if not callable(function):
        raise SpiceError(
            f"task phase continuation target is unavailable: "
            f"{module_name}.{function_name}"
        )
    result = function(payload)
    if not isinstance(result, str):
        raise SpiceError(
            f"task phase continuation {module_name}.{function_name} returned "
            f"{type(result).__name__}, expected str"
        )
    return result


def serialize_prepared_followup(
    prepared: TaskAddPreparation,
) -> dict[str, Any]:
    """Carry one validated follow-up identity across a checkout reload."""
    return {
        "args": list(prepared.args),
        "actor": prepared.actor,
        "claim": prepared.claim,
        "incepted": prepared.incepted,
        "project": prepared.project,
        "system_project": prepared.system_project,
        "title": prepared.title,
        "wording_matches": [
            {
                "source": match.source,
                "matched": match.matched,
                "trigger_family": match.trigger_family,
                "reason": match.reason,
            }
            for match in prepared.wording_matches
        ],
    }


def deserialize_prepared_followup(raw: object) -> TaskAddPreparation:
    """Reconstitute the exact validated follow-up under the fresh checkout."""
    if not isinstance(raw, dict):
        raise SpiceError("invalid prepared review follow-up continuation")
    from spice.tasks import create, wording

    matches = raw.get("wording_matches") or []
    if not isinstance(matches, list):
        raise SpiceError("invalid prepared review follow-up wording")
    return create.TaskAddPreparation(
        args=tuple(str(item) for item in raw.get("args") or []),
        actor=str(raw.get("actor") or ""),
        claim=bool(raw.get("claim")),
        incepted=str(raw.get("incepted") or ""),
        project=str(raw.get("project") or ""),
        system_project=bool(raw.get("system_project")),
        title=str(raw.get("title") or ""),
        wording_matches=tuple(
            wording.TaskWordingMatch(
                source=str(item.get("source") or ""),
                matched=str(item.get("matched") or ""),
                trigger_family=str(item.get("trigger_family") or ""),
                reason=str(item.get("reason") or ""),
            )
            for item in matches
            if isinstance(item, dict)
        ),
    )


def _request_from_process(argv: list[str]) -> Mapping[str, Any]:
    if argv:
        if len(argv) != 2 or argv[0] != "--payload":
            raise SpiceError("task phase continuation accepts only --payload TOKEN")
        try:
            raw = base64.urlsafe_b64decode(argv[1].encode()).decode()
        except (ValueError, UnicodeError) as exc:
            raise SpiceError("invalid task phase continuation recovery token") from exc
    else:
        raw = sys.stdin.read(PHASE_CONTINUATION_MAX_BYTES + 1)
        if len(raw) > PHASE_CONTINUATION_MAX_BYTES:
            raise SpiceError("task phase continuation exceeds the 1 MiB protocol limit")
    try:
        request = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SpiceError("invalid task phase continuation JSON") from exc
    if not isinstance(request, dict):
        raise SpiceError("task phase continuation request must be an object")
    return request


def main(argv: list[str] | None = None) -> int:
    try:
        output = _dispatch(
            _request_from_process(list(argv or sys.argv[1:])),
            apply_environment=True,
        )
    except SpiceError as exc:
        print(f"spice: {exc}", file=sys.stderr)
        return 2
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

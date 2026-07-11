"""`spice-judge` — the portable in-box maxim judge adapter.

This is the default maxim judge off macOS, where the Apple Foundation Models
``afm-cli`` binary does not exist. It conforms to the published judge CLI
contract in ``CONFIG.md``: it is launched as the exact argv ``[spice-judge]``
with no command-line arguments, reads one prompt on stdin, and writes a plain
``YES``/``NO`` verdict to stdout, exiting ``0``.

The adapter is a thin wrapper: it delegates the actual judgement to a portable
local model command, obtainable off macOS. The default command runs a small
local model through `Ollama <https://ollama.com>`_; ``SPICE_JUDGE_MODEL_CMD``
overrides it with any argv that reads a prompt on stdin and writes an answer to
stdout. There is one deterministic path and no silent no-op: when the model is
absent, crashes, or exceeds its deadline, the adapter exits non-zero with an
actionable message on stderr, which spice folds into its judge error detail.
"""

from __future__ import annotations

import os
import re
import shlex
import subprocess
import sys
from collections.abc import Sequence

JUDGE_MODEL_COMMAND_ENV = "SPICE_JUDGE_MODEL_CMD"  # env-policy: allow
JUDGE_TIMEOUT_ENV = "SPICE_JUDGE_TIMEOUT"  # env-policy: allow

DEFAULT_JUDGE_MODEL = "llama3.2"
DEFAULT_MODEL_COMMAND: tuple[str, ...] = ("ollama", "run", DEFAULT_JUDGE_MODEL)
DEFAULT_TIMEOUT_SECONDS = 60.0

EXIT_SUCCESS = 0
EXIT_FAILURE = 2

_VERDICT_RE = re.compile(r"\b(YES|NO)\b")


def resolve_model_command() -> list[str]:
    """Return the argv of the local model command backing the adapter.

    ``SPICE_JUDGE_MODEL_CMD`` wins when set; otherwise the documented default
    runs a small local model through Ollama.
    """
    override = os.environ.get(JUDGE_MODEL_COMMAND_ENV, "").strip()  # env-policy: allow
    if override:
        return shlex.split(override)
    return list(DEFAULT_MODEL_COMMAND)


def resolve_timeout() -> float | None:
    """Return the per-verdict model deadline in seconds, or ``None`` to disable.

    A missing value uses the default deadline; a non-positive value disables the
    deadline explicitly; a non-numeric value is a violated assumption that fails
    loudly.
    """
    raw = os.environ.get(JUDGE_TIMEOUT_ENV, "").strip()  # env-policy: allow
    if not raw:
        return DEFAULT_TIMEOUT_SECONDS
    value = float(raw)
    return value if value > 0 else None


def extract_verdict(text: str) -> str | None:
    """Reduce a model reply to ``"YES"``, ``"NO"``, or ``None`` when ambiguous.

    A single distinct standalone verdict token resolves; a reply carrying both
    or neither is ambiguous and passes through for spice's own retry.
    """
    distinct = set(_VERDICT_RE.findall(text.upper()))
    if distinct == {"YES"}:
        return "YES"
    if distinct == {"NO"}:
        return "NO"
    return None


def main(argv: Sequence[str] | None = None) -> int:
    """Judge the maxim prompt on stdin with the portable local model."""
    del argv  # The judge contract launches [spice-judge] with no arguments.
    prompt = sys.stdin.read()
    try:
        command = resolve_model_command()
        timeout = resolve_timeout()
    except ValueError as exc:
        print(f"spice-judge: invalid {JUDGE_TIMEOUT_ENV}: {exc}", file=sys.stderr)
        return EXIT_FAILURE
    if not command:
        print(
            f"spice-judge: {JUDGE_MODEL_COMMAND_ENV} is set but empty; give an argv "
            "that reads a prompt on stdin and writes an answer to stdout",
            file=sys.stderr,
        )
        return EXIT_FAILURE

    try:
        completed = subprocess.run(
            command,
            input=prompt,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError:
        print(
            f"spice-judge: judge model command {command[0]!r} not found. Install "
            f"Ollama and run `ollama pull {DEFAULT_JUDGE_MODEL}`, set "
            f"{JUDGE_MODEL_COMMAND_ENV} to another conforming argv, or set an explicit "
            "[judge] bin.",
            file=sys.stderr,
        )
        return EXIT_FAILURE
    except subprocess.TimeoutExpired:
        print(
            f"spice-judge: judge model {command!r} exceeded its "
            f"{timeout:g}s deadline; raise {JUDGE_TIMEOUT_ENV} or use a faster model",
            file=sys.stderr,
        )
        return EXIT_FAILURE
    except OSError as exc:
        print(
            f"spice-judge: could not launch judge model {command!r}: {exc}",
            file=sys.stderr,
        )
        return EXIT_FAILURE

    if completed.returncode != 0:
        detail = (completed.stderr or "").strip()
        suffix = f": {detail}" if detail else ""
        print(
            f"spice-judge: judge model {command!r} exited "
            f"{completed.returncode}{suffix}",
            file=sys.stderr,
        )
        return EXIT_FAILURE

    verdict = extract_verdict(completed.stdout)
    if verdict is not None:
        print(verdict)
        return EXIT_SUCCESS
    # Ambiguous reply: emit the model's own text so spice's parser retries it
    # instead of treating a recoverable answer as a hard judge failure.
    sys.stdout.write(completed.stdout)
    return EXIT_SUCCESS


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())

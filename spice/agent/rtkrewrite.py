"""Deterministic RTK rewrite result handling and native-fallback diagnostics."""

from __future__ import annotations

import contextlib
import hashlib
import os
import shlex
import subprocess
import sys
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TextIO

from spice.config import values
from spice.config.trust import require_repository_config_approval
from spice.agent.identity import ambient_thread_id
from spice.agent.paths import agent_thread_state_dir
from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd
from spice.process.groups import ProcessDeadlineExceeded, run_bounded_process_group

RTK_REWRITE_SUBCOMMAND = "rewrite"
RTK_CANONICAL_EXECUTABLE = "rtk"
RTK_REWRITE_MATCH_EXIT_CODE = 3
RTK_REWRITE_NO_MATCH_EXIT_CODE = 1
RTK_REWRITE_SUCCESS_EXIT_CODES = frozenset((0, RTK_REWRITE_MATCH_EXIT_CODE))
RTK_REWRITE_SELECTOR_TIMEOUT_SECONDS = 5.0
RTK_DIAGNOSTIC_EXECUTABLE_CHARS = 160

# A search reads its pattern as an extended regular expression when its name says
# so or when a flag asks for it. These characters are operators in that dialect
# and literals in the basic one, so a substitution that carries one from an
# extended search into a basic one answers a different question than the one that
# was asked -- and answers it as "no matches" rather than as an error. Only the
# written command is named by the caller, so only there does a name decide the
# dialect; a substituted search reaching the check is already known by name to be
# a basic one, and only the flag can widen it back.
RTK_EXTENDED_REGEX_OPERATORS = frozenset("|+?(){}")
RTK_EXTENDED_REGEX_COMMANDS = frozenset(("rg", "egrep"))
RTK_BASIC_REGEX_COMMANDS = frozenset(("grep", "fgrep"))
RTK_EXTENDED_REGEX_FLAGS = frozenset(("-E", "--extended-regexp"))
RTK_DIALECT_FAILURE_CLASS = "regex-dialect-narrowed"
RTK_QUOTING_FAILURE_CLASS = "unquoted-argument"

RtkWarningKey = tuple[str, str, str]
_rtk_warned_keys: set[RtkWarningKey] = set()


@dataclass(frozen=True)
class RtkRewriteDecision:
    rewritten: str | None = None
    failure_class: str = ""
    failure_signature: str = ""


def remap_rewrite_frontend(command_text: str, rtk_executable: str) -> str:
    """Route a canonical RTK rewrite through its configured executable."""
    if rtk_executable == RTK_CANONICAL_EXECUTABLE:
        return command_text
    configured_word = shlex.quote(rtk_executable)
    if command_text == RTK_CANONICAL_EXECUTABLE:
        return configured_word
    canonical_prefix = f"{RTK_CANONICAL_EXECUTABLE} "
    if command_text.startswith(canonical_prefix):
        return configured_word + command_text[len(RTK_CANONICAL_EXECUTABLE) :]
    return command_text


def rewrite_command_text(
    *args: str,
    repo_root: Path | None = None,
    rtk_executable: str | None = None,
    env: Mapping[str, str] | None = None,
    stderr: TextIO | None = None,
    run: Callable[..., subprocess.CompletedProcess[str]] | None = None,
) -> str | None:
    """Return a usable RTK rewrite or select native execution with one warning."""
    executable = (
        values.configured_rtk_executable(repo_root)
        if rtk_executable is None
        else rtk_executable
    )
    command = [executable, RTK_REWRITE_SUBCOMMAND, "--", *args]
    resolved_root = repo_root or repo_root_from_cwd()
    if rtk_executable is None and resolved_root is not None:
        require_repository_config_approval(
            resolved_root,
            ("rtk", "executable"),
            command=shlex.join(command),
        )
    run_kwargs: dict[str, Any] = {
        "capture_output": True,
        "text": True,
        "check": False,
    }
    if env is not None:
        run_kwargs["env"] = dict(env)
    try:
        if run is None:
            completed = run_bounded_process_group(
                command,
                timeout_seconds=RTK_REWRITE_SELECTOR_TIMEOUT_SECONDS,
                phase="agent.rtk-rewrite-selector",
                input_label="command-selection",
                **run_kwargs,
            )
        else:
            completed = run(command, **run_kwargs)
    except ProcessDeadlineExceeded:
        failure_class = "deadline-exceeded"
        emit_rewrite_diagnostic(
            repo_root,
            stderr,
            executable=executable,
            failure_class=failure_class,
            failure_signature=(f"deadline={RTK_REWRITE_SELECTOR_TIMEOUT_SECONDS:g}s"),
        )
        return None
    except OSError as exc:
        failure_class = _launch_failure_class(exc)
        emit_rewrite_diagnostic(
            repo_root,
            stderr,
            executable=executable,
            failure_class=failure_class,
            failure_signature=f"{failure_class}:errno={exc.errno}",
        )
        return None
    decision = _reject_unquoted_argument(
        _reject_narrowed_regex_dialect(_classify_rewrite_result(completed), args),
        args,
    )
    if decision.failure_class:
        emit_rewrite_diagnostic(
            repo_root,
            stderr,
            executable=executable,
            failure_class=decision.failure_class,
            failure_signature=decision.failure_signature,
        )
    return decision.rewritten


def emit_rewrite_diagnostic(
    repo_root: Path | None,
    stderr: TextIO | None,
    *,
    executable: str,
    failure_class: str,
    failure_signature: str,
) -> None:
    """Emit one bounded warning for this thread/executable/failure signature."""
    if not _claim_warning_signature(
        repo_root,
        executable=executable,
        failure_signature=f"{failure_class}:{failure_signature}",
    ):
        return
    display = executable
    if len(display) > RTK_DIAGNOSTIC_EXECUTABLE_CHARS:
        display = display[: RTK_DIAGNOSTIC_EXECUTABLE_CHARS - 1] + "…"
    surface = stderr if stderr is not None else sys.stderr
    surface.write(
        "spice agent run: RTK rewrite degraded to native "
        f"executable={display!r} failure={failure_class}\n"
    )
    surface.flush()


def _classify_rewrite_result(completed: object) -> RtkRewriteDecision:
    returncode = getattr(completed, "returncode", None)
    stdout = getattr(completed, "stdout", None)
    if not isinstance(returncode, int) or not isinstance(stdout, (str, type(None))):
        return RtkRewriteDecision(
            failure_class="invalid-result-shape",
            failure_signature=(
                f"returncode={type(returncode).__name__}:stdout={type(stdout).__name__}"
            ),
        )
    rewritten = (stdout or "").strip()
    if returncode in RTK_REWRITE_SUCCESS_EXIT_CODES and rewritten:
        return RtkRewriteDecision(rewritten=rewritten)
    if returncode == RTK_REWRITE_NO_MATCH_EXIT_CODE and not rewritten:
        return RtkRewriteDecision()
    stdout_shape = "nonempty" if rewritten else "empty"
    if returncode not in (
        *RTK_REWRITE_SUCCESS_EXIT_CODES,
        RTK_REWRITE_NO_MATCH_EXIT_CODE,
    ):
        return RtkRewriteDecision(
            failure_class="unexpected-exit",
            failure_signature=f"exit={returncode}:stdout={stdout_shape}",
        )
    return RtkRewriteDecision(
        failure_class="invalid-result-pair",
        failure_signature=f"exit={returncode}:stdout={stdout_shape}",
    )


def _reject_narrowed_regex_dialect(
    decision: RtkRewriteDecision, args: Sequence[str]
) -> RtkRewriteDecision:
    """Select native execution when a rewrite reinterprets an extended pattern."""
    if decision.rewritten is None:
        return decision
    operators = _narrowed_regex_operators(args, decision.rewritten)
    if not operators:
        return decision
    return RtkRewriteDecision(
        failure_class=RTK_DIALECT_FAILURE_CLASS,
        failure_signature=f"operators={operators}",
    )


def _narrowed_regex_operators(args: Sequence[str], rewritten: str) -> str:
    """The extended operators a rewrite carries into a basic-dialect search."""
    try:
        written = _written_words(args)
        substituted = shlex.split(rewritten)
    except ValueError:
        return ""
    if not _reads_extended_regex(written):
        return ""
    if RTK_BASIC_REGEX_COMMANDS.isdisjoint(substituted):
        return ""
    if any(_requests_extended_regex(word) for word in substituted):
        return ""
    # Every operator the caller wrote is read differently by the substituted
    # search, whether the rewrite reproduced the pattern intact or split it into
    # separate words. Both outcomes report no matches, so neither is inspected
    # further; the written operators alone decide that the answer would change.
    found = {
        operator
        for word in written
        if not word.startswith("-")
        for operator in RTK_EXTENDED_REGEX_OPERATORS.intersection(word)
    }
    return "".join(sorted(found))


def _reject_unquoted_argument(
    decision: RtkRewriteDecision, args: Sequence[str]
) -> RtkRewriteDecision:
    """Select native execution when a rewrite reproduced one argv word as several."""
    if decision.rewritten is None or len(args) < 2:
        return decision
    try:
        substituted = shlex.split(decision.rewritten)
    except ValueError:
        return decision
    # Only the argv shape can be checked this way. Shell text arrives as a
    # single argument whose words are the command itself, so its spaces carry
    # no promise; an argv word's spaces are part of one pattern or one path,
    # and a rewrite that emits them bare searches for something else entirely.
    intact = frozenset(substituted)
    for index, arg in enumerate(args):
        if arg in intact:
            continue
        if arg.split() != [arg] or _split_across_words(arg, substituted):
            return RtkRewriteDecision(
                failure_class=RTK_QUOTING_FAILURE_CLASS,
                failure_signature=f"argument={index}",
            )
    return decision


def _split_across_words(arg: str, substituted: Sequence[str]) -> bool:
    """Whether consecutive substituted words spell one written word between them."""
    # A word can be broken without ever having carried a space: RTK spaces an
    # alternation operator away from its alternatives, so the halves rejoin to
    # the written word only once the spacing is discarded on both sides.
    target = "".join(arg.split())
    for start in range(len(substituted) - 1):
        joined = substituted[start]
        for word in substituted[start + 1 :]:
            joined += word
            if len(joined) > len(target):
                break
            if joined == target:
                return True
    return False


def _written_words(args: Sequence[str]) -> list[str]:
    """The caller's command as words, whether it arrived as text or as argv."""
    if len(args) == 1:
        return shlex.split(args[0])
    return list(args)


def _reads_extended_regex(words: Sequence[str]) -> bool:
    """Whether a search reads its pattern as an extended regular expression."""
    if not RTK_EXTENDED_REGEX_COMMANDS.isdisjoint(words):
        return True
    return any(_requests_extended_regex(word) for word in words)


def _requests_extended_regex(word: str) -> bool:
    if word in RTK_EXTENDED_REGEX_FLAGS:
        return True
    return word.startswith("-") and not word.startswith("--") and "E" in word


def _launch_failure_class(exc: OSError) -> str:
    if isinstance(exc, FileNotFoundError):
        return "launch-not-found"
    if isinstance(exc, PermissionError):
        return "launch-permission"
    return "launch-error"


def _claim_warning_signature(
    repo_root: Path | None, *, executable: str, failure_signature: str
) -> bool:
    thread_id = ambient_thread_id() or ""
    key = (thread_id, executable, failure_signature)
    if key in _rtk_warned_keys:
        return False
    if repo_root is not None and thread_id:
        digest = hashlib.sha256(
            f"{executable}\0{failure_signature}".encode("utf-8")
        ).hexdigest()
        try:
            directory = (
                agent_thread_state_dir(repo_root, thread_id) / "rtk" / "warnings"
            )
            directory.mkdir(parents=True, exist_ok=True)
            descriptor = os.open(
                directory / digest,
                os.O_CREAT | os.O_EXCL | os.O_WRONLY,
                0o600,
            )
        except FileExistsError:
            _rtk_warned_keys.add(key)
            return False
        except (OSError, SpiceError):
            pass
        else:
            with contextlib.suppress(OSError):
                os.close(descriptor)
    _rtk_warned_keys.add(key)
    return True

"""Safe rg shell wrapper routing for agent command compression."""

from __future__ import annotations

import subprocess
import sys
from collections.abc import Sequence

NATIVE_RG_FLAGS = {
    "--files",
    "--json",
    "--type-list",
    "--pcre2-version",
    "--debug",
    "--trace",
    "--help",
    "-h",
    "--version",
    "-V",
    "--count",
    "-c",
    "--files-with-matches",
    "-l",
    "--files-without-match",
    "-L",
    "--only-matching",
    "-o",
}

PATTERN_FLAGS = {"-e", "--regexp"}

VALUE_FLAGS = {
    "-A",
    "-B",
    "-C",
    "-g",
    "--glob",
    "--iglob",
    "-t",
    "--type",
    "-T",
    "--type-not",
    "-m",
    "--max-count",
    "--max-depth",
    "--context",
}

VALUELESS_FLAGS = {
    "--case-sensitive",
    "--crlf",
    "--fixed-strings",
    "--follow",
    "--heading",
    "--hidden",
    "--ignore-case",
    "--line-number",
    "--multiline",
    "--multiline-dotall",
    "--no-heading",
    "--no-ignore",
    "--no-ignore-vcs",
    "--no-messages",
    "--pcre2",
    "--search-zip",
    "--smart-case",
    "--stats",
    "--text",
    "--word-regexp",
    "-F",
    "-H",
    "-M",
    "-P",
    "-S",
    "-U",
    "-a",
    "-i",
    "-n",
    "-s",
    "-u",
    "-uu",
    "-uuu",
    "-w",
    "-z",
}


def run_agent_rg_wrapper(args: Sequence[str]) -> int:
    """Route ordinary rg searches through rtk grep; preserve native rg otherwise."""
    command = agent_rg_wrapper_command(args)
    return subprocess.run(command, check=False).returncode


def agent_rg_wrapper_command(args: Sequence[str]) -> list[str]:
    if args and args[0] == "--":
        args = args[1:]
    parsed = parse_rtk_grep_args(args)
    if parsed is None:
        return ["rg", *args]
    pattern, path, extra = parsed
    return ["rtk", "grep", pattern, path, *extra]


def parse_rtk_grep_args(args: Sequence[str]) -> tuple[str, str, list[str]] | None:
    pattern = ""
    path = ""
    extra: list[str] = []
    index = 0
    while index < len(args):
        arg = args[index]
        if arg == "--":
            return None
        if arg in PATTERN_FLAGS or any(
            arg.startswith(flag + "=") for flag in PATTERN_FLAGS
        ):
            return None
        if arg in NATIVE_RG_FLAGS or any(
            arg.startswith(flag + "=") for flag in NATIVE_RG_FLAGS
        ):
            return None
        if arg in VALUE_FLAGS:
            if index + 1 >= len(args):
                return None
            extra.extend([arg, args[index + 1]])
            index += 2
            continue
        if any(
            arg.startswith(flag + "=") for flag in VALUE_FLAGS if flag.startswith("--")
        ):
            extra.append(arg)
            index += 1
            continue
        if arg.startswith("-"):
            if arg in VALUELESS_FLAGS:
                extra.append(arg)
                index += 1
                continue
            return None
        if not pattern:
            pattern = arg
        elif not path:
            path = arg
        else:
            extra.append(arg)
        index += 1
    if not pattern:
        return None
    return pattern, path or ".", extra


def main() -> int:
    return run_agent_rg_wrapper(sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())

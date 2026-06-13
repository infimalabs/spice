"""Repo-configurable maxim conscience."""

from __future__ import annotations

import subprocess
from argparse import Namespace
from pathlib import Path

from spice.agent import maximcli, maxims, watchdog
from spice.agent.maxims import MaximVerdict
from spice.mail.inbox import collect_inbox_items


def test_repo_config_declares_new_maxim_bag_for_scan_and_watchdog(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.shortcuts]
words = ["shortcut", "shortcuts"]
message = "DO NOT take shortcuts; keep the direct route."
""",
    )

    bag = maxims.triggered_maxims(["Taking shortcuts here."], repo_root=repo)[0]
    assert bag.name == "shortcuts"
    assert maxims.configured_maxim("shortcut", repo_root=repo) == bag.message

    def judge_violation(maxim: str, statement: str) -> MaximVerdict:
        return MaximVerdict(
            maxim=maxim,
            statement=statement,
            prompt="",
            answer="NO",
            attempts=("NO", "NO"),
        )

    monkeypatch.setattr(watchdog, "evaluate_maxim_any_violation", judge_violation)

    paths = watchdog.publish_maxim_hits_as_inbox(repo, "Taking shortcuts here.")
    item = collect_inbox_items(repo)[0]

    assert len(paths) == 1
    assert paths[0].is_file()
    assert item.text == "[MAXIM] DO NOT take shortcuts; keep the direct route.\n"


def test_repo_config_overrides_builtin_trigger_words(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.fallbacks]
words = ["detour"]
""",
    )

    bag = maxims.resolved_maxim_bags(repo)["fallbacks"]
    hit = maxims.triggered_maxims(
        ["This detour hides the real problem."], repo_root=repo
    )[0]

    assert bag.words == frozenset({"detour"})
    assert hit.name == "fallbacks"
    assert hit.message == bag.message


def test_repo_config_overrides_builtin_message_for_show_and_meta_judge(
    tmp_path, monkeypatch, capsys
):
    repo = _init_repo(tmp_path / "repo")
    message = "DO NOT split this project into parallel behavior modes."
    _write_pyproject(
        repo,
        f"""
[tool.spice.maxims.modes]
message = "{message}"
""",
    )
    monkeypatch.chdir(repo)

    maximcli.run_maxim_show_cli(Namespace(name="mode"))
    shown = capsys.readouterr().out

    seen: list[str] = []

    def judge(maxim: str, statement: str, *, template: str) -> MaximVerdict:
        seen.append(maxim)
        return MaximVerdict(
            maxim=maxim,
            statement=statement,
            prompt=template,
            answer="NO",
            attempts=("NO",),
        )

    monkeypatch.setattr(maximcli, "evaluate_maxim", judge)
    code = maximcli.run_maxim_agree_cli(
        Namespace(
            maxim="all",
            statements=["This mode splits behavior."],
            prompt_file=None,
            quiet=True,
            output_format=None,
        )
    )

    assert shown == f"{message}\n"
    assert code == maximcli.CONDITION_UNMET_EXIT_CODE
    assert seen == [message]


def _write_pyproject(repo: Path, text: str) -> None:
    (repo / "pyproject.toml").write_text(text.strip() + "\n", encoding="utf-8")


def _init_repo(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "-b", "main"],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    )
    return path

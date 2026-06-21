"""Repo-configurable maxim conscience."""

from __future__ import annotations

import subprocess
from argparse import Namespace
from pathlib import Path

import pytest

from spice.agent import maximcli, maxims, watchdog
from spice.agent.maxims import MaximVerdict
from spice.errors import SpiceError
from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.inbox import collect_inbox_items, inbox_item_key


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


def test_maxim_reminder_gate_suppresses_same_combined_body_until_compaction(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    _make_every_maxim_violate(monkeypatch)
    gate = watchdog.MaximReminderGate()

    first_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )
    duplicate_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta again", reminder_gate=gate
    )
    archive_ackd_inbox_items(repo, [inbox_item_key(first_paths[0].name)])
    gate.note_compaction()
    after_compaction_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )

    assert len(first_paths) == 1
    assert duplicate_paths == []
    assert len(after_compaction_paths) == 1
    assert after_compaction_paths != first_paths
    assert [item.text for item in collect_inbox_items(repo)] == [
        "[MAXIM] FIRST reminder. SECOND reminder.\n",
    ]


def test_maxim_reminder_gate_allows_new_combined_body_with_existing_maxim(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    _make_every_maxim_violate(monkeypatch)
    gate = watchdog.MaximReminderGate()

    single_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha", reminder_gate=gate
    )
    combined_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )
    duplicate_combined_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta again", reminder_gate=gate
    )

    assert len(single_paths) == 1
    assert len(combined_paths) == 1
    assert duplicate_combined_paths == []
    assert [item.text for item in collect_inbox_items(repo)] == [
        "[MAXIM] FIRST reminder.\n",
        "[MAXIM] FIRST reminder. SECOND reminder.\n",
    ]


def test_maxim_publish_reuses_existing_pending_body_without_process_gate(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    _make_every_maxim_violate(monkeypatch)

    first_paths = watchdog.publish_maxim_hits_as_inbox(repo, "alpha beta")
    second_paths = watchdog.publish_maxim_hits_as_inbox(repo, "alpha beta again")

    assert first_paths == second_paths
    assert [item.name for item in collect_inbox_items(repo)] == [first_paths[0].name]


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


def test_builtin_phrase_trigger_matches_whole_phrase_across_punctuation():
    hits = maxims.triggered_maxims(
        [
            "This FALLS, back to a quiet path.",
            "A fallsback identifier should not match.",
        ]
    )
    misses = maxims.triggered_maxims(["This falls backwards instead."])

    assert [hit.name for hit in hits] == ["fallbacks"]
    assert misses == []
    assert maxims.configured_maxim("falls   back") == maxims.builtin_maxim("fallback")


def test_repo_config_declares_phrase_trigger_key(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.routes]
words = ["quiet route", "soft landing"]
message = "DO NOT take the quiet route."
""",
    )

    bag = maxims.resolved_maxim_bags(repo)["routes"]
    hits = maxims.triggered_maxims(
        ["This quiet-route had a soft\nLANDING."], repo_root=repo
    )
    misses = maxims.triggered_maxims(["This quietroute fallsback."], repo_root=repo)

    assert bag.words == frozenset({"quiet route", "soft landing"})
    assert [hit.name for hit in hits] == ["routes"]
    assert misses == []
    assert maxims.configured_maxim("Quiet   Route", repo_root=repo) == bag.message


def test_repo_config_rejects_non_alphabetic_phrase_trigger_key(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.routes]
words = ["falls-back"]
message = "DO NOT take the quiet route."
""",
    )

    with pytest.raises(SpiceError, match="alphabetic phrases"):
        maxims.resolved_maxim_bags(repo)


def test_builtin_fallback_maxim_allows_explicit_defaults_and_resolver_order():
    message = maxims.builtin_maxim("fallback")

    assert "quiet defensive secondary paths" in message
    assert "intentional defaults" in message
    assert "explicit resolver order" in message
    assert "fail loudly" in message


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


def test_maxim_show_quotes_phrase_trigger_keys(tmp_path, monkeypatch, capsys):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.routes]
words = ["quiet route", "detour"]
message = "DO NOT take the quiet route."
""",
    )
    monkeypatch.chdir(repo)

    maximcli.run_maxim_show_cli(Namespace(name=None))
    shown = capsys.readouterr().out

    assert 'routes (detour/"quiet route")' in shown


def _write_pyproject(repo: Path, text: str) -> None:
    (repo / "pyproject.toml").write_text(text.strip() + "\n", encoding="utf-8")


def _write_dual_maxim_config(repo: Path) -> None:
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.first]
words = ["alpha"]
message = "FIRST reminder."

[tool.spice.maxims.second]
words = ["beta"]
message = "SECOND reminder."
""",
    )


def _make_every_maxim_violate(monkeypatch) -> None:
    def judge_violation(maxim: str, statement: str) -> MaximVerdict:
        return MaximVerdict(
            maxim=maxim,
            statement=statement,
            prompt="",
            answer="NO",
            attempts=("NO",),
        )

    monkeypatch.setattr(watchdog, "evaluate_maxim_any_violation", judge_violation)


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

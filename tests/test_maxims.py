"""Repo-configurable maxim conscience."""

from __future__ import annotations

import io
import json
import subprocess
from argparse import Namespace
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from threading import Lock

import pytest

from spice import config
from spice.agent import maximcli, maxims, watchdog
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV, driver_choices
from spice.agent.maxims import MaximVerdict
from spice.cli import entry as cli_entry
from spice.errors import SpiceError
from spice.flexstate import git_state_path
from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.inbox import (
    collect_inbox_items,
    compose_inbox_text,
    inbox_item_key,
    write_inbox_item,
)

MAXIM_CORPUS_RECALL_FLOOR = 1.0
MAXIM_CORPUS_FALSE_POSITIVE_RATE_CEILING = 0.0


@dataclass(frozen=True)
class _MaximCorpusCase:
    maxim_name: str
    statement: str
    violates: bool


@dataclass(frozen=True)
class _MaximCorpusScore:
    violating: int
    detected: int
    compliant: int
    false_positives: int
    judge_calls: int

    @property
    def recall(self) -> float:
        return self.detected / self.violating

    @property
    def false_positive_rate(self) -> float:
        return self.false_positives / self.compliant


_LABELED_MAXIM_CORPUS = (
    _MaximCorpusCase(
        "polling",
        "I will add a retry loop that sleeps until the file appears.",
        True,
    ),
    _MaximCorpusCase(
        "polling",
        "I will react to the file watcher event and remove the wait.",
        False,
    ),
    _MaximCorpusCase(
        "fallbacks",
        "I will fall back to a quiet default if the contract is unclear.",
        True,
    ),
    _MaximCorpusCase(
        "fallbacks",
        "I will require the configured path and fail loudly when it is missing.",
        False,
    ),
    _MaximCorpusCase(
        "backwards-compat",
        "I will preserve backwards compatibility by keeping the old caller path.",
        True,
    ),
    _MaximCorpusCase(
        "backwards-compat",
        "I will update every caller to the new contract and remove the old path.",
        False,
    ),
    _MaximCorpusCase(
        "shims",
        "I will add a shim between the old response shape and the new one.",
        True,
    ),
    _MaximCorpusCase(
        "shims",
        "I will replace the old response shape directly and update every caller.",
        False,
    ),
    _MaximCorpusCase(
        "aliases",
        "I will add an alias so both command names work.",
        True,
    ),
    _MaximCorpusCase(
        "aliases",
        "I will rename the command in place and update every reference.",
        False,
    ),
    _MaximCorpusCase(
        "legacy",
        "I will keep the legacy branch around as commented history.",
        True,
    ),
    _MaximCorpusCase(
        "legacy",
        "I will delete the obsolete branch and use the current path.",
        False,
    ),
)


def test_configured_stub_judge_drives_maxim_agree_end_to_end(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    record_path = repo / "judge-call.json"
    judge_path = repo / "judge-stub"
    judge_path.write_text(
        "\n".join(
            [
                "#!/usr/bin/env python3",
                "import json",
                "import sys",
                "from pathlib import Path",
                f"record_path = Path({str(record_path)!r})",
                "record_path.write_text(",
                "    json.dumps({'argv': sys.argv[1:], 'prompt': sys.stdin.read()}),",
                "    encoding='utf-8',",
                ")",
                "print('YES')",
                "",
            ]
        ),
        encoding="utf-8",
    )
    judge_path.chmod(0o755)
    config.update_section(
        repo, config.JUDGE_KEY, {config.JUDGE_BIN_KEY: str(judge_path)}
    )
    monkeypatch.chdir(repo)

    maxim = "Prefer explicit contracts."
    statement = "I will publish the executable contract."
    code = cli_entry.main(["maxim", "agree", maxim, statement, "--quiet"])
    call = json.loads(record_path.read_text(encoding="utf-8"))

    assert code == maximcli.CONDITION_MET_EXIT_CODE
    assert call["argv"] == []
    assert '"Prefer explicit contracts"' in call["prompt"]
    assert '"I will publish the executable contract"' in call["prompt"]


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

    paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "Taking shortcuts here.", reminder_gate=watchdog.MaximReminderGate()
    )
    item = collect_inbox_items(repo)[0]

    assert len(paths) == 1
    assert paths[0].is_file()
    assert item.text == "[MAXIM] DO NOT take shortcuts; keep the direct route.\n"


def test_maxim_reminder_gate_suppresses_same_combined_body_until_compaction(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    gate = watchdog.MaximReminderGate()
    judged: list[tuple[str, str]] = []

    def judge_violation(maxim: str, statement: str) -> MaximVerdict:
        judged.append((maxim, statement))
        return MaximVerdict(
            maxim=maxim,
            statement=statement,
            prompt="",
            answer="NO",
            attempts=("NO",),
        )

    monkeypatch.setattr(watchdog, "evaluate_maxim_any_violation", judge_violation)

    first_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )
    first_body = first_paths[0].read_text(encoding="utf-8")
    first_judged = list(judged)
    duplicate_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta again", reminder_gate=gate
    )
    duplicate_judged = list(judged)
    archive_ackd_inbox_items(repo, [inbox_item_key(first_paths[0].name)])
    after_ack_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )
    after_ack_judged = list(judged)
    gate.note_compaction()
    after_compaction_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )

    assert len(first_paths) == 1
    assert first_judged == [
        ("FIRST reminder.", "alpha beta"),
        ("SECOND reminder.", "alpha beta"),
    ]
    assert duplicate_paths == []
    assert duplicate_judged == first_judged
    assert after_ack_paths == []
    assert after_ack_judged == first_judged
    assert len(after_compaction_paths) == 1
    assert after_compaction_paths[0].read_text(encoding="utf-8") == first_body
    assert judged == [
        ("FIRST reminder.", "alpha beta"),
        ("SECOND reminder.", "alpha beta"),
        ("FIRST reminder.", "alpha beta"),
        ("SECOND reminder.", "alpha beta"),
    ]
    assert after_compaction_paths != first_paths
    assert [item.text for item in collect_inbox_items(repo)] == [
        "[MAXIM] FIRST reminder. SECOND reminder.\n",
    ]


def test_maxim_reminder_gate_agreeing_first_pass_does_not_poison_later_violation(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    gate = watchdog.MaximReminderGate()
    answers = ["YES", "NO"]
    judged: list[tuple[str, str]] = []

    def judge_once_agree_then_violate(maxim: str, statement: str) -> MaximVerdict:
        judged.append((maxim, statement))
        answer = answers.pop(0)
        return MaximVerdict(
            maxim=maxim,
            statement=statement,
            prompt="",
            answer=answer,
            attempts=(answer,),
        )

    monkeypatch.setattr(
        watchdog, "evaluate_maxim_any_violation", judge_once_agree_then_violate
    )

    agreeing_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha", reminder_gate=gate
    )
    violating_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha again", reminder_gate=gate
    )

    assert agreeing_paths == []
    assert len(violating_paths) == 1
    assert judged == [
        ("FIRST reminder.", "alpha"),
        ("FIRST reminder.", "alpha again"),
    ]
    assert [item.text for item in collect_inbox_items(repo)] == [
        "[MAXIM] FIRST reminder.\n"
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


def test_maxim_publish_suppression_uses_in_memory_gate_not_pending_file_scan(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    _make_every_maxim_violate(monkeypatch)
    gate = watchdog.MaximReminderGate()

    first_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )
    second_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta again", reminder_gate=gate
    )
    archive_ackd_inbox_items(repo, [inbox_item_key(first_paths[0].name)])
    after_ack_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=gate
    )

    assert len(first_paths) == 1
    assert second_paths == []
    assert after_ack_paths == []
    assert collect_inbox_items(repo) == []


def test_maxim_reminder_gate_stores_rendered_body_separate_from_key(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    gate = watchdog.MaximReminderGate()
    reminder_key = "prejudge:first+second"
    body = "[MAXIM] FIRST reminder. SECOND reminder.\n"
    path = write_inbox_item(repo, None, body)

    gate.mark_sent(reminder_key, path, body)

    assert not gate.should_publish(reminder_key)
    assert gate.should_publish(body)
    assert gate.published_reminders() == ((path, body),)

    discarded = watchdog.discard_pending_maxim_reminders(repo, gate)

    assert discarded == [path]
    assert collect_inbox_items(repo) == []
    assert gate.published_reminders() == ()
    assert not gate.should_publish(reminder_key)

    gate.note_compaction()

    assert gate.should_publish(reminder_key)


def test_stdout_supervisor_discards_its_pending_maxim_reminders_on_shutdown(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    _write_dual_maxim_config(repo)
    _make_every_maxim_violate(monkeypatch)
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    write_inbox_item(
        repo,
        "20260103T000000000001Z.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    process = _FakeProcess(stdout=io.StringIO("codex\nalpha beta\nexec\n"))
    log_path = repo / "supervisor.log"

    watchdog._tee_agent_stdout(process, repo, log_path)

    items = collect_inbox_items(repo)
    assert [item.name for item in items] == ["20260103T000000000001Z.txt"]
    assert "operator steering" in items[0].text
    assert "spice maxim supervisor cleanup discarded inbox:" in log_path.read_text(
        encoding="utf-8"
    )


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
    assert bag.drivers == _all_driver_names()
    assert hit.name == "fallbacks"
    assert hit.message == bag.message


def test_builtin_maxim_bags_default_to_all_driver_scopes():
    expected = _all_driver_names()

    assert expected == frozenset({"claude", "codex"})
    assert all(bag.drivers == expected for bag in maxims.resolved_maxim_bags().values())


def test_repo_config_declares_maxim_driver_scope(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.routes]
words = ["quiet route"]
message = "DO NOT take the quiet route."
drivers = ["Codex", "codex"]
""",
    )

    bag = maxims.resolved_maxim_bags(repo)["routes"]
    hits = maxims.triggered_maxims(["This quiet route drifts."], repo_root=repo)

    assert bag.drivers == frozenset({"codex"})
    assert hits == [bag]
    assert maxims.triggered_maxims(
        ["This quiet route drifts."],
        repo_root=repo,
        driver_name="codex",
    ) == [bag]
    assert (
        maxims.triggered_maxims(
            ["This quiet route drifts."],
            repo_root=repo,
            driver_name="claude",
        )
        == []
    )


def test_watchdog_filters_maxim_bags_by_active_driver(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.codexonly]
words = ["codex route"]
message = "CODEX only reminder."
drivers = ["codex"]

[tool.spice.maxims.shared]
words = ["shared route"]
message = "Shared reminder."
""",
    )
    _make_every_maxim_violate(monkeypatch)

    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "codex")
    codex_paths = watchdog.publish_maxim_hits_as_inbox(
        repo,
        "The codex route and shared route both drift.",
        reminder_gate=watchdog.MaximReminderGate(),
    )
    codex_items = collect_inbox_items(repo)

    for item in codex_items:
        item.source_path.unlink()
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    claude_paths = watchdog.publish_maxim_hits_as_inbox(
        repo,
        "The codex route and shared route both drift.",
        reminder_gate=watchdog.MaximReminderGate(),
    )
    claude_items = collect_inbox_items(repo)

    assert len(codex_paths) == 1
    assert codex_items[0].text == "[MAXIM] CODEX only reminder. Shared reminder.\n"
    assert len(claude_paths) == 1
    assert claude_items[0].text == "[MAXIM] Shared reminder.\n"


def test_repo_config_rejects_unknown_maxim_driver_scope(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.routes]
words = ["quiet route"]
message = "DO NOT take the quiet route."
drivers = ["codex", "ghost"]
""",
    )

    with pytest.raises(SpiceError, match="known agent drivers"):
        maxims.resolved_maxim_bags(repo)


def test_worktree_disabled_maxim_bag_stops_publish_without_silencing_enabled_bag(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    _commit_all(repo)
    _make_every_maxim_violate(monkeypatch)

    disabled = maxims.set_maxim_bag_disabled("first", disabled=True, repo_root=repo)
    paths = watchdog.publish_maxim_hits_as_inbox(
        repo, "alpha beta", reminder_gate=watchdog.MaximReminderGate()
    )

    state_path = git_state_path(maxims.DISABLED_MAXIM_BAGS_GIT_PATH, root=repo)
    assert disabled == frozenset({"first"})
    assert state_path.is_file()
    assert repo / ".git" in state_path.parents
    assert [bag.name for bag in maxims.resolved_maxim_bags(repo).values()] == [
        "polling",
        "fallbacks",
        "backwards-compat",
        "shims",
        "aliases",
        "legacy",
        "second",
    ]
    assert len(paths) == 1
    assert [item.text for item in collect_inbox_items(repo)] == [
        "[MAXIM] SECOND reminder.\n"
    ]
    subprocess.run(
        ["git", "diff", "--exit-code", "--", "pyproject.toml"],
        cwd=repo,
        capture_output=True,
        check=True,
        text=True,
    )


def test_worktree_disabled_maxim_bag_is_local_to_one_linked_worktree(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.routes]
words = ["quiet route"]
message = "DO NOT take the quiet route."
""",
    )
    _commit_all(repo)
    peer = tmp_path / "peer"
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "peer", str(peer)],
        cwd=repo,
        check=True,
    )

    maxims.set_maxim_bag_disabled("routes", disabled=True, repo_root=repo)

    assert maxims.disabled_maxim_bag_names(repo) == frozenset({"routes"})
    assert maxims.disabled_maxim_bag_names(peer) == frozenset()
    assert maxims.triggered_maxims(["This quiet route drifts."], repo_root=repo) == []
    assert [
        hit.name
        for hit in maxims.triggered_maxims(["This quiet route drifts."], repo_root=peer)
    ] == ["routes"]
    assert [
        hit.name
        for hit in maxims.triggered_maxims(["This falls back quietly."], repo_root=repo)
    ] == ["fallbacks"]


def test_watchdog_scopes_and_worktree_disable_compose_for_operator_behavior(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    _write_pyproject(
        repo,
        """
[tool.spice.maxims.codexonly]
words = ["codex route"]
message = "CODEX scoped reminder."
drivers = ["codex"]

[tool.spice.maxims.claudeonly]
words = ["claude route"]
message = "CLAUDE scoped reminder."
drivers = ["claude"]

[tool.spice.maxims.shared]
words = ["shared route"]
message = "Shared reminder."
""",
    )
    _commit_all(repo)
    peer = tmp_path / "peer"
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "peer", str(peer)],
        cwd=repo,
        check=True,
    )
    _make_every_maxim_violate(monkeypatch)
    statement = "The codex route, claude route, and shared route all drift."

    maxims.set_maxim_bag_disabled("shared", disabled=True, repo_root=repo)
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "codex")
    repo_paths = watchdog.publish_maxim_hits_as_inbox(
        repo, statement, reminder_gate=watchdog.MaximReminderGate()
    )
    peer_codex_paths = watchdog.publish_maxim_hits_as_inbox(
        peer, statement, reminder_gate=watchdog.MaximReminderGate()
    )
    peer_codex_items = collect_inbox_items(peer)

    for item in peer_codex_items:
        item.source_path.unlink()
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    peer_claude_paths = watchdog.publish_maxim_hits_as_inbox(
        peer, statement, reminder_gate=watchdog.MaximReminderGate()
    )

    assert len(repo_paths) == 1
    assert [item.text for item in collect_inbox_items(repo)] == [
        "[MAXIM] CODEX scoped reminder.\n"
    ]
    assert len(peer_codex_paths) == 1
    assert [item.text for item in peer_codex_items] == [
        "[MAXIM] CODEX scoped reminder. Shared reminder.\n"
    ]
    assert len(peer_claude_paths) == 1
    assert [item.text for item in collect_inbox_items(peer)] == [
        "[MAXIM] CLAUDE scoped reminder. Shared reminder.\n"
    ]
    subprocess.run(
        ["git", "diff", "--exit-code", "--", "pyproject.toml"],
        cwd=repo,
        capture_output=True,
        check=True,
        text=True,
    )


def test_worktree_maxim_disable_rejects_unknown_bag(tmp_path):
    repo = _init_repo(tmp_path / "repo")

    with pytest.raises(SpiceError, match="unknown maxim bag 'ghost'"):
        maxims.set_maxim_bag_disabled("ghost", disabled=True, repo_root=repo)


def test_maxim_disable_enable_cli_updates_worktree_state(tmp_path, monkeypatch, capsys):
    repo = _init_repo(tmp_path / "repo")
    _write_dual_maxim_config(repo)
    monkeypatch.chdir(repo)

    disable_code = maximcli.run_maxim_disable_cli(Namespace(name="first"))
    disable_output = capsys.readouterr().out
    enable_code = maximcli.run_maxim_enable_cli(Namespace(name="first"))
    enable_output = capsys.readouterr().out

    assert disable_code == 0
    assert "disabled maxim bags: first" in disable_output
    assert maximcli.SCOPE_DECISION_EVIDENCE_ROW in disable_output
    assert maxims.disabled_maxim_bag_names(repo) == frozenset()
    assert enable_code == 0
    assert "disabled maxim bags: none" in enable_output
    assert maximcli.SCOPE_DECISION_EVIDENCE_ROW in enable_output


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


@pytest.mark.parametrize(
    ("statement", "selector"),
    [
        ("Do not fall back to a quiet path.", "fall back"),
        ("The fall backs route hides the real problem.", "fall backs"),
        ("This falls back to a quiet path.", "falls back"),
    ],
)
def test_builtin_fallback_variants_trigger_fallback_maxim(statement, selector):
    hits = maxims.triggered_maxims([statement])

    assert [hit.name for hit in hits] == ["fallbacks"]
    assert maxims.configured_maxim(selector) == maxims.builtin_maxim("fallback")


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

    assert "single deterministic path" in message
    assert "explicit default" in message
    assert "documented resolver order" in message
    assert "fail loudly" in message
    assert "contract names outright" in message


def test_dual_judge_maxim_corpus_recall_and_false_positive_rate():
    score = _score_labeled_maxim_corpus(_LABELED_MAXIM_CORPUS)

    assert score.recall >= MAXIM_CORPUS_RECALL_FLOOR
    assert score.false_positive_rate <= MAXIM_CORPUS_FALSE_POSITIVE_RATE_CEILING
    assert (
        score.judge_calls == len(_LABELED_MAXIM_CORPUS) * maxims.PARALLEL_MAXIM_JUDGES
    )


def test_repo_config_declares_custom_mode_words_for_show_and_meta_judge(
    tmp_path, monkeypatch, capsys
):
    repo = _init_repo(tmp_path / "repo")
    message = "DO NOT split this project into parallel behavior modes."
    _write_pyproject(
        repo,
        f"""
[tool.spice.maxims.modes]
words = ["mode", "modes"]
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


def _all_driver_names() -> frozenset[str]:
    return frozenset(driver_choices())


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


def _score_labeled_maxim_corpus(
    corpus: Sequence[_MaximCorpusCase],
) -> _MaximCorpusScore:
    backend, judge_calls = _labeled_corpus_backend(corpus)
    detected = 0
    false_positives = 0
    violating = 0
    compliant = 0
    for case in corpus:
        verdict = maxims.evaluate_maxim_any_violation(
            maxims.builtin_maxim(case.maxim_name),
            case.statement,
            backend=backend,
            max_attempts=1,
        )
        predicted_violation = not verdict.agrees
        if case.violates:
            violating += 1
            if predicted_violation:
                detected += 1
        else:
            compliant += 1
            if predicted_violation:
                false_positives += 1
    return _MaximCorpusScore(
        violating=violating,
        detected=detected,
        compliant=compliant,
        false_positives=false_positives,
        judge_calls=judge_calls(),
    )


def _labeled_corpus_backend(
    corpus: Sequence[_MaximCorpusCase],
) -> tuple[maxims.JudgeBackend, Callable[[], int]]:
    lock = Lock()
    calls_by_statement: dict[str, int] = {}
    corpus_by_statement = {
        maxims.normalize_field(case.statement): case for case in corpus
    }

    def backend(prompt: str) -> str:
        statement = _prompt_corpus_statement(prompt, corpus_by_statement)
        case = corpus_by_statement[statement]
        with lock:
            call_index = calls_by_statement.get(statement, 0)
            calls_by_statement[statement] = call_index + 1
        if not case.violates:
            return "YES"
        return "YES" if call_index == 0 else "NO"

    def judge_calls() -> int:
        with lock:
            return sum(calls_by_statement.values())

    return backend, judge_calls


def _prompt_corpus_statement(
    prompt: str, corpus_by_statement: dict[str, _MaximCorpusCase]
) -> str:
    for statement in corpus_by_statement:
        if f'"{statement}"' in prompt:
            return statement
    raise AssertionError(f"prompt did not contain a labeled corpus statement: {prompt}")


class _FakeProcess:
    pid = 12345

    def __init__(self, *, stdout: io.StringIO) -> None:
        self.stdout = stdout


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


def _commit_all(repo: Path) -> None:
    subprocess.run(["git", "add", "."], cwd=repo, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Spice Test",
            "-c",
            "user.email=spice@example.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ],
        cwd=repo,
        check=True,
    )

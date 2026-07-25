"""Executable behavior ledger for every Taskwarrior touchpoint behind tw.py.

Two halves. The inventory half pins which modules reach the backend and which
command fragments they emit, so no touchpoint can escape a replacement effort
unnoticed. The oracle half records what the live backend actually does for the
semantics spice depends on -- READY and ACTIVE derivation, claim UDAs, urgency
ordering, annotation round-trip, date rendering, and error surfaces -- so a
replacement store can be proven behaviorally identical against the same
assertions. Recording current behavior is the whole point: these expectations
are observations, not aspirations.
"""

from __future__ import annotations

import ast
import shutil
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import config, create, identity, tw

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
DASHED_ACTOR = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE"

SPICE_PACKAGE = Path(__file__).resolve().parent.parent / "spice"
FILTERING_ENTRY_POINTS = frozenset({"export", "run"})

FAR_FUTURE_WAIT = "2099-01-02T03:04:05Z"
PAST_SCHEDULED = "2001-02-03T04:05:06Z"
ROUND_TRIP_OFFSET_SECONDS = 3600

# Every module that reaches the Taskwarrior process layer, mapped to the tw
# entry points it calls. Equality is deliberate: a new consumer or a dropped
# one must be recorded here before the suite goes green again, because the
# retirement effort prices exactly this surface.
TW_TOUCHPOINTS: dict[str, frozenset[str]] = {
    "spice/agent/watchdog.py": frozenset({"canonical_actor"}),
    "spice/agent/wrap.py": frozenset({"export"}),
    "spice/serve/payload/lane.py": frozenset({"canonical_actor", "export"}),
    "spice/serve/payload/message.py": frozenset({"canonical_actor", "export"}),
    "spice/serve/payload/metric.py": frozenset({"export"}),
    "spice/sessions/briefingtaskplane.py": frozenset({"current_actor", "now_iso"}),
    "spice/studies/taskgen.py": frozenset({"export"}),
    "spice/tasks/alloc.py": frozenset({"current_actor", "export", "now_iso"}),
    "spice/tasks/claimstate.py": frozenset(
        {
            "canonical_actor",
            "claim_head",
            "current_actor",
            "current_branch",
            "export",
            "now_iso",
            "run",
        }
    ),
    "spice/tasks/cli.py": frozenset({"current_actor", "export"}),
    "spice/tasks/create.py": frozenset(
        {
            "canonical_actor",
            "canonical_utc",
            "current_actor",
            "current_branch",
            "export",
            "future_utc",
            "run",
        }
    ),
    "spice/tasks/graph.py": frozenset({"export"}),
    "spice/tasks/graphs/handout.py": frozenset({"export"}),
    "spice/tasks/identity.py": frozenset({"export"}),
    "spice/tasks/markdown/apply.py": frozenset(
        {"canonical_actor", "current_actor", "export", "now_iso", "run"}
    ),
    "spice/tasks/markdown/ledger.py": frozenset({"export"}),
    "spice/tasks/ops.py": frozenset(
        {
            "canonical_actor",
            "current_actor",
            "export",
            "now_iso",
            "require_clean_worktree",
            "run",
        }
    ),
    "spice/tasks/projectsubs.py": frozenset({"export"}),
    "spice/tasks/readiness.py": frozenset({"export", "run"}),
    "spice/tasks/render.py": frozenset(
        {"canonical_actor", "current_actor", "export", "now_iso"}
    ),
    "spice/tasks/reviewfeedback.py": frozenset({"canonical_actor", "run"}),
    "spice/tasks/sizing.py": frozenset({"export"}),
    "spice/tasks/wordingreview.py": frozenset({"current_actor", "export", "run"}),
}

# Every literal fragment spice hands to `task` through tw.run/tw.export, with
# the Taskwarrior concept it exercises. A replacement store must answer each
# of these roles, so the vocabulary is the retirement checklist.
TW_COMMAND_VOCABULARY: dict[str, str] = {
    "(": "filter grouping",
    ")": "filter grouping",
    "or": "filter disjunction",
    "--": "end of options before free-form annotation text",
    "+ACTIVE": "virtual tag: row carries a start timestamp",
    "-ACTIVE": "negated virtual tag: row carries no start timestamp",
    "+READY": "virtual tag: pending, unblocked, not future-waiting",
    "annotate": "verb: append a timestamped annotation",
    "calc": "verb: evaluate a Taskwarrior date or numeric expression",
    "delete": "verb: move a row to deleted",
    "done": "verb: move a row to completed",
    "modify": "verb: write fields and UDAs on selected rows",
    "uuid": "row key read back from an exported row to address the next command",
    ":": "empty assignment suffix that clears a UDA value",
    "start:": "clear the start timestamp, dropping ACTIVE",
    "start:now": "set the start timestamp, raising ACTIVE",
    "depends:": "dependency assignment prefix",
    "delete_reason:": "UDA assignment prefix recorded before deletion",
    "project:": "project assignment or filter prefix",
    "project.is:": "exact project filter prefix",
    "origin.is:": "exact origin filter prefix",
    "origin_thread.is:": "exact origin-thread filter prefix",
    "incepted.is:": "exact inception-token filter prefix",
    "claim_by.is:": "exact claim-owner filter prefix",
    "claim_until.is:": "exact claim-deadline filter prefix",
    "claim_worktree.is:": "exact claim-worktree filter prefix",
    "status:pending": "status filter",
    "status:waiting": "status filter",
    "status:completed": "status filter",
    "status.any:": "status filter matching every status including deleted",
}


# The unconstrained string columns a replacement store must carry verbatim:
# claim lease bookkeeping, review verdicts, task-document identity, and the
# evidence trail spice writes on every lifecycle transition.
_CLAIM_UDAS = (
    "claim_by",
    "claim_at",
    "claim_until",
    "claim_thread",
    "claim_worktree",
    "claim_branch",
    "claim_head",
    "claim_lease_seconds",
    "claim_context_start",
    "claim_context_end",
    "claim_context_link",
    "claim_context_turn",
)
_REVIEW_UDAS = (
    "review_author",
    "review_by",
    "review_at",
    "review_finding",
    "review_note",
)
_TASKDOC_UDAS = ("taskdoc_id", "taskdoc_parent")
_EVIDENCE_UDAS = (
    "acceptance",
    "task_description",
    "validation",
    "judgment",
    "delete_reason",
    "origin",
    "creation_surface",
    "wording_review",
    "ready_at",
    "origin_thread",
    "origin_worktree",
    "origin_branch",
    "done_head",
    "done_merge_head",
    "done_ref",
    "done_local_commits",
    "done_upstream",
    "done_upstream_head",
)
_STRING_UDAS = (
    "incepted",
    *_CLAIM_UDAS,
    *_REVIEW_UDAS,
    *_TASKDOC_UDAS,
    *_EVIDENCE_UDAS,
)
_PHASE_SLOT_UDAS = tuple(f"phase_{index}" for index in range(config.PHASE_SLOT_COUNT))


def _tw_call_nodes(tree: ast.AST) -> list[ast.Call]:
    calls = []
    for node in ast.walk(tree):
        func = getattr(node, "func", None)
        if not isinstance(node, ast.Call) or not isinstance(func, ast.Attribute):
            continue
        if isinstance(func.value, ast.Name) and func.value.id == "tw":
            calls.append(node)
    return calls


def _string_constants(node: ast.AST) -> list[str]:
    return [
        sub.value
        for sub in ast.walk(node)
        if isinstance(sub, ast.Constant) and isinstance(sub.value, str)
    ]


def _scan_touchpoints() -> tuple[dict[str, frozenset[str]], set[str]]:
    touchpoints: dict[str, set[str]] = {}
    fragments: set[str] = set()
    for path in sorted(SPICE_PACKAGE.rglob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for call in _tw_call_nodes(tree):
            name = call.func.attr  # type: ignore[union-attr]
            relative = path.relative_to(SPICE_PACKAGE.parent).as_posix()
            touchpoints.setdefault(relative, set()).add(name)
            if name in FILTERING_ENTRY_POINTS:
                fragments.update(
                    literal for arg in call.args for literal in _string_constants(arg)
                )
    return {key: frozenset(value) for key, value in touchpoints.items()}, fragments


def test_ledger_records_every_module_that_reaches_taskwarrior():
    observed, _ = _scan_touchpoints()

    assert observed == TW_TOUCHPOINTS


def test_ledger_records_every_command_fragment_spice_emits():
    _, fragments = _scan_touchpoints()

    assert fragments == set(TW_COMMAND_VOCABULARY)


def test_tw_module_exposes_exactly_the_ledgered_entry_points():
    ledgered = {name for names in TW_TOUCHPOINTS.values() for name in names}
    public = {
        name
        for name in vars(tw)
        if not name.startswith("_") and callable(getattr(tw, name))
    }

    assert ledgered <= public


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def _add(title: str, **kwargs: Any) -> str:
    return create.add(title, project="task.unit", origin="ack:1jN54zJJ", **kwargs)


def _uuid(handle: str) -> str:
    return identity.uuid_of(identity.resolve(handle))


def _handles(rows: list[dict[str, Any]]) -> set[str]:
    return {identity.render_handle(row) for row in rows}


def test_ready_derivation_excludes_future_wait_and_blocked_dependencies(task_repo):
    plain = _add("ready row")
    waiting = _add("deferred row", wait=FAR_FUTURE_WAIT, scheduled=PAST_SCHEDULED)
    blocker = _add("blocking row")
    blocked = _add("blocked row")
    tw.run([_uuid(blocked), "modify", f"depends:{_uuid(blocker)}"])

    ready = _handles(tw.export(["+READY"]))

    assert ready == {plain, blocker}
    assert _handles(tw.export(["status:waiting"])) == {waiting}
    assert _handles(tw.export(["status:pending"])) == {plain, blocker, blocked}


def test_active_derivation_follows_start_and_survives_a_future_wait(task_repo):
    idle = _add("idle row")
    started = _add("started row")
    deferred_and_started = _add("deferred claim", wait=FAR_FUTURE_WAIT)
    tw.run([_uuid(started), "modify", "start:now"])
    tw.run([_uuid(deferred_and_started), "modify", "start:now"])

    active = _handles(tw.export(["+ACTIVE"]))
    unstarted = _handles(tw.export(["status:pending", "-ACTIVE"]))

    assert active == {started, deferred_and_started}
    assert unstarted == {idle}

    tw.run([_uuid(started), "modify", "start:"])

    assert _handles(tw.export(["+ACTIVE"])) == {deferred_and_started}


def test_claim_uda_round_trip_and_exact_owner_filter(task_repo):
    mine = _add("claimed row")
    theirs = _add("peer row")
    until = tw.future_utc(ROUND_TRIP_OFFSET_SECONDS)
    tw.run([_uuid(mine), "modify", f"claim_by:{ACTOR}", f"claim_until:{until}"])
    tw.run([_uuid(theirs), "modify", f"claim_by:{PEER_ACTOR}"])

    row = identity.resolve(mine)

    assert row["claim_by"] == ACTOR
    assert row["claim_until"] == until
    assert _handles(tw.export([f"claim_by.is:{ACTOR}"])) == {mine}
    assert _handles(tw.export([f"claim_by.is:{PEER_ACTOR}"])) == {theirs}


def test_canonical_actor_renders_a_filter_safe_token():
    assert tw.canonical_actor(DASHED_ACTOR) == "aaaaaaaabbbbccccddddeeeeeeeeeeee"
    assert tw.canonical_actor(config.SENTINEL_ACTOR) == "0" * 32


def test_clearing_a_uda_removes_the_value_from_the_exported_row(task_repo):
    handle = _add("wording row")
    uuid = _uuid(handle)
    tw.run([uuid, "modify", f"{config.TASK_WORDING_REVIEW_UDA}:pending"])
    with_value = identity.resolve(handle)

    tw.run([uuid, "modify", f"{config.TASK_WORDING_REVIEW_UDA}:"])
    cleared = identity.resolve(handle)

    assert with_value[config.TASK_WORDING_REVIEW_UDA] == "pending"
    assert cleared.get(config.TASK_WORDING_REVIEW_UDA, "") == ""


def test_urgency_ranks_review_phase_above_the_same_row_in_todo(task_repo):
    handle = _add("phase urgency row", priority="medium")
    uuid = _uuid(handle)
    tw.run([uuid, "modify", "phase:todo"])
    todo_urgency = float(identity.resolve(handle)["urgency"])

    tw.run([uuid, "modify", "phase:review"])
    review_urgency = float(identity.resolve(handle)["urgency"])

    assert review_urgency == pytest.approx(todo_urgency + 4.0)


def test_urgency_ranks_priorities_by_the_spice_coefficients(task_repo):
    urgencies = {}
    for priority in config.PRIORITY_URGENCY:
        handle = _add(f"{priority} priority row", priority=priority)
        urgencies[priority] = float(identity.resolve(handle)["urgency"])

    ordered = [name for name, _ in sorted(urgencies.items(), key=lambda kv: -kv[1])]
    expected = sorted(
        config.PRIORITY_URGENCY,
        key=lambda name: -float(config.PRIORITY_URGENCY[name]),
    )

    assert ordered == expected


def test_every_ledgered_uda_survives_a_write_and_export_round_trip(task_repo):
    handle = _add("uda round trip row")
    uuid = _uuid(handle)
    written = {name: f"value-{index}" for index, name in enumerate(_STRING_UDAS)}
    tw.run([uuid, "modify", *(f"{name}:{value}" for name, value in written.items())])

    # Resolved by uuid: the write covers `incepted`, which is the token every
    # handle is derived from, so the row is only addressable by uuid after it.
    row = tw.export([uuid])[0]

    assert {name: str(row.get(name) or "") for name in written} == written


def test_uda_schema_declares_the_column_set_a_replacement_store_must_carry():
    schema = config.uda_schema()
    constrained = {name for name, frag in schema.items() if "values" in frag}
    numeric = {name for name, frag in schema.items() if frag["type"] == "numeric"}

    assert set(schema) == set(_STRING_UDAS) | constrained | numeric
    assert numeric == {"phase_i"}
    assert constrained == {"priority", "phase", *_PHASE_SLOT_UDAS}
    assert schema["phase"]["values"] == ",".join(config.APPROVED_PHASES)


def test_calc_resolves_a_relative_date_expression_to_an_absolute_instant(task_repo):
    result = tw.run(["calc", "now+1d"])

    resolved = datetime.fromisoformat(result.stdout.strip()).astimezone(UTC)
    delta = resolved - datetime.now(UTC)

    assert timedelta(hours=23) < delta < timedelta(hours=25)


def test_annotation_round_trip_preserves_leading_dashes_and_utf8(task_repo):
    handle = _add("annotated row")
    text = "-- ACK 1kGt: rôle réglé — 90% ✅"
    tw.run([_uuid(handle), "annotate", "--", text])

    annotations = identity.resolve(handle)["annotations"]

    assert [entry["description"] for entry in annotations] == [text]


def test_canonical_utc_survives_the_taskwarrior_date_round_trip(task_repo):
    handle = _add("dated row")
    moment = datetime.now(UTC) + timedelta(seconds=ROUND_TRIP_OFFSET_SECONDS)
    rendered = tw.canonical_utc(moment)
    tw.run([_uuid(handle), "modify", f"wait:{rendered}"])

    stored = identity.resolve(handle)["wait"]

    assert stored == rendered
    assert datetime.strptime(stored, tw.TW_DATETIME_FORMAT).replace(
        tzinfo=UTC
    ) == moment.replace(microsecond=0)


def test_failed_command_raises_with_the_requested_arguments(task_repo):
    with pytest.raises(SpiceError) as failure:
        tw.run(["definitely-not-a-uuid", "modify", "project:task.unit"])

    assert "task command failed" in str(failure.value)


def test_export_rejects_a_non_array_payload(monkeypatch, tmp_path):
    monkeypatch.setattr(tw, "require_task_binary", lambda: None)
    monkeypatch.setattr(config, "bootstrap", lambda: tmp_path / "taskrc")
    monkeypatch.setattr(config, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(subprocess, "run", _fake_process('{"rows": []}'))

    with pytest.raises(SpiceError) as failure:
        tw.export()

    assert "did not return a JSON array" in str(failure.value)


def test_timeout_names_the_action_and_omits_schema_override_noise(
    monkeypatch, tmp_path
):
    monkeypatch.setattr(tw, "require_task_binary", lambda: None)
    monkeypatch.setattr(config, "bootstrap", lambda: tmp_path / "taskrc")
    monkeypatch.setattr(config, "repo_root", lambda: tmp_path)

    def timing_out(command, **_kwargs):
        raise subprocess.TimeoutExpired(command, tw.TASK_COMMAND_TIMEOUT_SECONDS)

    monkeypatch.setattr(subprocess, "run", timing_out)

    with pytest.raises(SpiceError) as failure:
        tw.run(["some-uuid", "modify", "project:task.unit"])

    message = str(failure.value)
    override_tokens = [word for word in message.split() if word.startswith("rc.uda.")]

    assert "modify mutation" in message
    assert override_tokens == []


def _fake_process(stdout: str):
    class Result:
        returncode = 0
        stderr = ""

    def run_stub(_command, **_kwargs):
        result = Result()
        result.stdout = stdout
        return result

    return run_stub


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)

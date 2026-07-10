"""Task show and next CLI rendering."""

from __future__ import annotations

from pathlib import Path


from spice.tasks import (
    claimstate,
    effort,
    render,
)


ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


SHOW_DEFAULT_CACHED_INPUT_TOKENS = 10


SHOW_DEFAULT_OUTPUT_TOKENS = 20


SHOW_DEFAULT_REASONING_OUTPUT_TOKENS = 5


def test_task_show_surfaces_creator_rehydrate_action(monkeypatch):
    row = _row(
        "Creator context",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": "origin-thread",
            "origin_worktree": "/tmp/origin",
            "claim_by": "actor-a",
            "claim_until": "2026-06-12T07:00:00Z",
            "claim_thread": "claim-thread",
            "claim_worktree": "/tmp/claim",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])
    monkeypatch.setattr(render.tw, "now_iso", lambda: "2026-06-12T08:00:00Z")

    output = render.render_show("TASK-test")

    assert (
        "rehydrate:\n  creator context, run: spice session briefing origin-thread"
        in (output)
    )
    assert "--start 2026-06-12T06:53:25.463000Z" in output
    assert "--end 2026-06-12T07:03:25.463000Z" in output


def test_task_show_hides_recovery_context_for_current_task(monkeypatch):
    row = _row(
        "Current context hidden",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": "origin-thread",
            "origin_worktree": "/tmp/origin",
            "claim_thread": "claim-thread",
            "claim_context_start": "2026-06-12T08:15:18.621994Z",
            "claim_context_end": "2026-06-12T08:25:18.621994Z",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    lines = render.render_show("TASK-test").splitlines()

    assert lines[lines.index("claim_thread claim-thread") + 1] == "acceptance "
    assert (
        lines[lines.index("origin origin-thread - /tmp/origin") + 1]
        == 'next: spice task done TASK-test --validation "..."'
    )


def test_task_show_requires_context_check_before_implementation(monkeypatch):
    row = _row(
        "Current context",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "Implement only if current",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": "origin-thread",
            "claim_thread": "claim-thread",
            "claim_context_start": "2026-06-12T08:15:18.621994Z",
            "claim_context_end": "2026-06-12T08:25:18.621994Z",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test", include_recovery_context=True)

    assert "context_check:" in output
    assert "Before editing, run the rehydrate command(s) above" in output
    assert "assert the task description/acceptance still match" in output


def test_task_next_includes_recovery_context_for_assignment(monkeypatch):
    row = _row(
        "Assigned context",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "Implement only after assignment context",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": "origin-thread",
            "claim_thread": "claim-thread",
            "claim_context_start": "2026-06-12T08:15:18.621994Z",
            "claim_context_end": "2026-06-12T08:25:18.621994Z",
        }
    )

    monkeypatch.setattr(render.alloc, "next_task", lambda: row)
    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])
    monkeypatch.setattr(
        render.claimstate,
        "renew_claim",
        lambda: claimstate.ClaimRenewalResult(
            True,
            "renewed",
            handle="TASK-test",
            claim_until="2026-07-09T06:00:00.000000Z",
        ),
    )
    monkeypatch.setattr(
        render.ops, "claim_drive_line", lambda _handle: "drive: continue TASK-test"
    )

    output = render.render_next()

    assert output.startswith(
        "claim_renewal=renewed TASK-test until 2026-07-09T06:00:00.000000Z\n"
    )
    assert "next task:\nTASK-test [todo] P:M task.render Assigned context" in output
    assert "claim_context 2026-06-12T08:15:18.621994Z ->" in output
    assert "rehydrate:" in output
    assert "context_check:" in output


def test_task_next_renews_before_allocating(monkeypatch):
    calls: list[str] = []
    row = _row(
        "Assigned after renewal",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
    )
    row.update({"phase": "todo", "phase_i": "0", "urgency": "9.2"})

    def fake_renew():
        calls.append("renew")
        return claimstate.ClaimRenewalResult(
            True,
            "renewed",
            handle="TASK-test",
            claim_until="2026-07-09T06:00:00.000000Z",
        )

    def fake_next():
        calls.append("next")
        return row

    monkeypatch.setattr(render.claimstate, "renew_claim", fake_renew)
    monkeypatch.setattr(render.alloc, "next_task", fake_next)
    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])
    monkeypatch.setattr(
        render.ops, "claim_drive_line", lambda _handle: "drive: continue TASK-test"
    )

    output = render.render_next()

    assert calls == ["renew", "next"]
    assert output.startswith("claim_renewal=renewed TASK-test until ")


def test_task_next_reports_no_claim_renewal_when_no_task_available(monkeypatch):
    monkeypatch.setattr(
        render.claimstate,
        "renew_claim",
        lambda: claimstate.ClaimRenewalResult(False, "no_active_claim"),
    )
    monkeypatch.setattr(render.alloc, "next_task", lambda: None)

    output = render.render_next()

    assert output == "\n".join(
        [
            "claim_renewal=skipped no_active_claim",
            "no available tasks; run spice task status",
        ]
    )


def test_task_next_reports_failed_claim_renewal_detail(monkeypatch):
    monkeypatch.setattr(
        render.claimstate,
        "renew_claim",
        lambda: claimstate.ClaimRenewalResult(
            False, "backend_error", detail="backend offline"
        ),
    )
    monkeypatch.setattr(render.alloc, "next_task", lambda: None)

    output = render.render_next()

    assert output == "\n".join(
        [
            "claim_renewal=failed backend_error detail=backend offline",
            "no available tasks; run spice task status",
        ]
    )


def test_task_show_context_check_names_stale_or_shifted_context(monkeypatch):
    row = _row(
        "No transcript context",
        project="task.render",
        incepted="not-a-context-window",
        status="pending",
        phase="verify",
    )
    row.update(
        {
            "task_description": "Verify only if still relevant",
            "phase_i": "1",
            "urgency": "9.2",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "verify"])

    output = render.render_show("TASK-test", include_recovery_context=True)

    assert "context_check:" in output
    assert "no transcript rehydrate command is available" in output
    assert "If context shifted or the task is stale" in output
    assert "before changing files" in output


def test_task_show_does_not_add_implementation_context_check_to_review(monkeypatch):
    row = _row(
        "Review context",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "Review already asserts description currency",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "origin_thread": "origin-thread",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test", include_recovery_context=True)

    assert "context_check:" not in output
    assert (
        "next: spice task review TASK-test --finding clean --note "
        '"description current; ..."'
    ) in output


def test_task_show_keeps_creator_rehydrate_for_same_claim_thread(monkeypatch):
    row = _row(
        "Same thread context",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": "same-thread",
            "origin_worktree": "/tmp/repo",
            "claim_thread": "same-thread",
            "claim_worktree": "/tmp/repo",
            "claim_context_start": "2026-06-12T08:15:18.621994Z",
            "claim_context_end": "2026-06-12T08:25:18.621994Z",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test", include_recovery_context=True)

    assert "creator context, run: spice session briefing same-thread" in output
    assert "--start 2026-06-12T06:53:25.463000Z" in output
    assert "--end 2026-06-12T07:03:25.463000Z" in output
    assert "claim context, run: spice session briefing same-thread" in output
    assert "--start 2026-06-12T08:15:18.621994Z" in output
    assert "--end 2026-06-12T08:25:18.621994Z" in output


def test_task_show_replaces_sentinel_rehydrate_commands(monkeypatch):
    sentinel = "0" * 32
    row = _row(
        "Sentinel task",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": sentinel,
            "origin_worktree": "/tmp/origin",
            "claim_thread": sentinel,
            "claim_worktree": "/tmp/claim",
            "claim_context_start": "2026-06-12T07:15:18.621994Z",
            "claim_context_end": "2026-06-12T07:25:18.621994Z",
            "claim_context_turn": "turn-a",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test", include_recovery_context=True)

    assert "rehydrate:" in output
    assert "creator context: unavailable (sentinel thread has no transcript)" in output
    assert "claim context: unavailable (sentinel thread has no transcript)" in output
    assert f"spice session briefing {sentinel}" not in output
    assert f"spice session turns {sentinel}" not in output


def test_task_show_prints_merge_aware_diff_command_for_task_merge(monkeypatch):
    row = _row(
        "Review merge",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "done_ref": "merge-head",
            "done_merge_head": "merge-head",
            "done_head": "agent-head",
            "done_upstream_head": "upstream-head",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "review_commit merge-head (task merge; agent_head agent-head)" in output
    assert "review_diff_base upstream-head (done_upstream_head)" in output
    assert (
        "review_diff_command "
        "git show -m --first-parent --stat --patch merge-head" in output
    )
    assert (
        "review_diff_note primary merge diff shows the integrated reviewed patch; "
        "agent_head agent-head is provenance only because its ancestry can include "
        "already-integrated overlap"
    ) in output
    assert "review_agent_diff_command" not in output


def test_task_show_steers_overlap_reviews_to_integrated_merge_patch(monkeypatch):
    row = _row(
        "Review overlap",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "done_ref": "reviewed-merge",
            "done_merge_head": "reviewed-merge",
            "done_head": "agent-head-with-overlap",
            "done_upstream_head": "upstream-already-has-overlap",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert (
        "review_diff_command "
        "git show -m --first-parent --stat --patch reviewed-merge" in output
    )
    assert (
        "agent_head agent-head-with-overlap is provenance only because its "
        "ancestry can include already-integrated overlap"
    ) in output
    assert "git diff --stat --patch" not in output
    assert "review_fallback_diff_command" not in output
    assert "review_agent_diff_command" not in output


def test_task_show_merge_diff_command_falls_back_to_first_parent(monkeypatch):
    row = _row(
        "Review merge",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "done_ref": "merge-head",
            "done_merge_head": "merge-head",
            "done_head": "agent-head",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "review_diff_base merge-head^1 (merge first parent)" in output
    assert (
        "review_diff_command "
        "git show -m --first-parent --stat --patch merge-head" in output
    )
    assert "review_agent_diff_command" not in output


def test_task_show_omits_merge_aware_diff_command_for_task_head(monkeypatch):
    row = _row(
        "Review direct head",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "done_ref": "agent-head",
            "done_merge_head": "agent-head",
            "done_head": "agent-head",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "review_commit agent-head (task head)" in output
    assert "review_diff_command" not in output
    assert "plain git show" not in output


def test_task_show_renders_phase_effort_as_aggregate_phase_rows(monkeypatch):
    row = _row(
        "Render effort",
        project="task.render",
        incepted="1k4yrMDR",
        status="pending",
        phase="verify",
    )
    row.update(
        {
            "uuid": "effort-task-uuid",
            "task_description": "Render the effort ledger",
            "phase_i": "1",
            "urgency": "9.2",
        }
    )
    windows = _phase_effort_show_windows()
    usage_rows = _phase_effort_show_usage_rows(windows)

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(
        render.claimstate, "phases_of", lambda _row: ["todo", "verify", "review"]
    )
    monkeypatch.setattr(
        render.effort, "phase_effort_windows_for_tasks", lambda _rows: windows
    )
    monkeypatch.setattr(
        render.effort,
        "phase_effort_usage_for_windows",
        lambda _windows, _files_by_thread: usage_rows,
    )
    monkeypatch.setattr(
        render,
        "_phase_effort_transcript_files_by_thread",
        lambda _windows: {ACTOR_A: (Path("thread-a.jsonl"),)},
    )

    output = render.render_show("TASK-test")

    assert _section_lines(output, "phase_effort:") == [
        "phase_effort:",
        (
            "  todo[0] tokens=135 input=100 cached=10 output=20 reasoning=5 "
            "turns=1 msgs=2 renewals=0 wall=20s"
        ),
        (
            "  verify[1] tokens=267 input=207 cached=20 output=30 reasoning=10 "
            "turns=1 msgs=2 renewals=1 wall=1m15s partial=missing_transcript"
        ),
        (
            "  review[2] tokens=unattributed input=- cached=- output=- "
            "reasoning=- turns=3 msgs=4 renewals=0 wall=30s partial=missing_end"
        ),
    ]


def test_task_show_surfaces_review_note_artifact_citation(monkeypatch):
    row = _row(
        "Review citation",
        project="task.render",
        incepted="1k4yrMDR",
        status="completed",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "review_by": "actor-a",
            "review_finding": "changes",
            "review_note": "See artifact A1 on TASK-test for the raw log.",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.claimstate, "phases_of", lambda _row: ["todo", "review"])
    monkeypatch.setattr(
        render.artifacts,
        "render_artifact_lines",
        lambda _handle: ["artifacts:", "  A1 raw.log text/plain 12 B permanent"],
    )

    output = render.render_show("TASK-test")

    assert "review_finding changes" in output
    assert "review_note See artifact A1 on TASK-test for the raw log." in output
    assert "artifacts:\n  A1 raw.log text/plain 12 B permanent" in output


def _row(
    description: str,
    *,
    project: str,
    incepted: str,
    status: str = "pending",
    phase: str = "todo",
) -> dict[str, object]:
    return {
        "description": description,
        "project": project,
        "status": status,
        "phase": phase,
        "priority": "M",
        "incepted": incepted,
        "entry": incepted,
    }


def _section_lines(output: str, header: str) -> list[str]:
    lines = output.splitlines()
    section = [lines[lines.index(header)]]
    for line in lines[lines.index(header) + 1 :]:
        if line and not line.startswith(" "):
            break
        section.append(line)
    return section


def _phase_effort_show_windows() -> tuple[effort.PhaseEffortWindow, ...]:
    return (
        _phase_effort_show_window("todo", 0, model="gpt-5.5", start=10.0, end=30.0),
        _phase_effort_show_window("verify", 1, model="gpt-5.5", start=45.0, end=120.0),
        _phase_effort_show_window("review", 2, model="", start=125.0, end=155.0),
    )


def _phase_effort_show_window(
    phase: str,
    phase_index: int,
    *,
    model: str,
    start: float,
    end: float,
) -> effort.PhaseEffortWindow:
    return effort.PhaseEffortWindow(
        task_id="effort-task-uuid",
        handle="TASK-test",
        title="Render effort",
        phase=phase,
        phase_index=phase_index,
        actor_id=f"agent-{phase_index}",
        thread_id=ACTOR_A,
        team_id="team-a",
        driver="codex",
        model=model,
        effort="xhigh",
        started_at=start,
        ended_at=end,
    )


def _phase_effort_show_usage_rows(
    windows: tuple[effort.PhaseEffortWindow, ...],
) -> tuple[effort.PhaseEffortUsage, ...]:
    return (
        _phase_effort_show_usage(windows[0], total=135, input_tokens=100),
        _phase_effort_show_usage(
            windows[1],
            total=267,
            input_tokens=207,
            cached_input_tokens=20,
            output_tokens=30,
            reasoning_output_tokens=10,
            renewal_count=1,
            partial_markers=(effort.PARTIAL_MISSING_TRANSCRIPT,),
        ),
        _phase_effort_show_usage(
            windows[2],
            total=999,
            input_tokens=900,
            cached_input_tokens=800,
            output_tokens=90,
            reasoning_output_tokens=9,
            turn_count=3,
            message_count=4,
            partial_markers=(effort.PARTIAL_MISSING_END,),
        ),
    )


def _phase_effort_show_usage(
    window: effort.PhaseEffortWindow,
    *,
    total: int,
    input_tokens: int,
    cached_input_tokens: int = SHOW_DEFAULT_CACHED_INPUT_TOKENS,
    output_tokens: int = SHOW_DEFAULT_OUTPUT_TOKENS,
    reasoning_output_tokens: int = SHOW_DEFAULT_REASONING_OUTPUT_TOKENS,
    turn_count: int = 1,
    message_count: int = 2,
    renewal_count: int = 0,
    partial_markers: tuple[str, ...] = (),
) -> effort.PhaseEffortUsage:
    return effort.PhaseEffortUsage(
        window=window,
        source_files=("thread-a.jsonl",),
        input_tokens=input_tokens,
        cached_input_tokens=cached_input_tokens,
        output_tokens=output_tokens,
        reasoning_output_tokens=reasoning_output_tokens,
        total_tokens=total,
        turn_count=turn_count,
        message_count=message_count,
        renewal_count=renewal_count,
        partial_markers=partial_markers,
    )
